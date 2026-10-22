//! The integer arithmetic of TensorFlow Lite Micro's int8 reference kernels (Apache-2.0), kept
//! bit-exact: `QuantizeMultiplier`, `MultiplyByQuantizedMultiplier` (the default double
//! rounding), gemmlowp's fixed-point helpers and its `logistic`, as `LOGISTIC` uses them.

/// `QuantizeMultiplier`: a real multiplier as a Q31 significand and a power-of-two shift.
#[allow(clippy::cast_possible_truncation, reason = "q_fixed ≤ 2^31 by construction")]
pub fn quantize_multiplier(multiplier: f64) -> (i32, i32) {
    if multiplier == 0.0 {
        return (0, 0);
    }
    let (q, mut shift) = frexp(multiplier);
    let mut q_fixed = (q * f64::from(1_u32 << 31)).round() as i64;
    if q_fixed == 1_i64 << 31 {
        q_fixed /= 2;
        shift += 1;
    }
    if shift < -31 {
        shift = 0;
        q_fixed = 0;
    }
    (q_fixed as i32, shift)
}

/// `std::frexp` for positive finite values: `x = m * 2^e` with `m` in `[0.5, 1)`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    reason = "exponent bits"
)]
fn frexp(x: f64) -> (f64, i32) {
    if x == 0.0 || !x.is_finite() {
        return (x, 0);
    }
    let bits = x.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i32;
    if exp == 0 {
        // Subnormal: scale into the normal range first.
        let (m, e) = frexp(x * f64::from(1_u32 << 31) * f64::from(1_u32 << 23));
        return (m, e - 54);
    }
    let mantissa = f64::from_bits((bits & !(0x7ff << 52)) | (1022 << 52));
    (mantissa, exp - 1022)
}

/// gemmlowp `SaturatingRoundingDoublingHighMul`.
#[allow(clippy::cast_possible_truncation, reason = "the high half fits")]
pub fn srdhm(a: i32, b: i32) -> i32 {
    if a == i32::MIN && b == i32::MIN {
        return i32::MAX;
    }
    let ab = i64::from(a) * i64::from(b);
    let nudge: i64 = if ab >= 0 { 1 << 30 } else { 1 - (1 << 30) };
    // C++ integer division truncates towards zero.
    ((ab + nudge) / (1_i64 << 31)) as i32
}

/// gemmlowp `RoundingDivideByPOT`: rounds half away from zero.
pub fn rounding_divide_by_pot(x: i32, exponent: i32) -> i32 {
    let mask = (1_i32 << exponent).wrapping_sub(1);
    let remainder = x & mask;
    let threshold = (mask >> 1) + i32::from(x < 0);
    (x >> exponent) + i32::from(remainder > threshold)
}

/// `MultiplyByQuantizedMultiplier` (not `TFLITE_SINGLE_ROUNDING`).
pub fn multiply_by_quantized_multiplier(x: i32, multiplier: i32, shift: i32) -> i32 {
    let left = shift.max(0);
    let right = (-shift).max(0);
    rounding_divide_by_pot(srdhm(x.wrapping_mul(1 << left), multiplier), right)
}

/// gemmlowp `SaturatingRoundingMultiplyByPOT<exponent>`.
fn saturating_rounding_multiply_by_pot(x: i32, exponent: i32) -> i32 {
    match exponent.cmp(&0) {
        std::cmp::Ordering::Equal => x,
        std::cmp::Ordering::Greater => {
            let threshold = (1_i32 << (31 - exponent)) - 1;
            if x > threshold {
                i32::MAX
            } else if x < -threshold {
                i32::MIN
            } else {
                x << exponent
            }
        }
        std::cmp::Ordering::Less => rounding_divide_by_pot(x, -exponent),
    }
}

/// `Rescale<dst>` of a fixed-point value with `src` integer bits.
fn rescale(x: i32, src_integer_bits: i32, dst_integer_bits: i32) -> i32 {
    saturating_rounding_multiply_by_pot(x, src_integer_bits - dst_integer_bits)
}

/// gemmlowp `RoundingHalfSum`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::manual_midpoint,
    reason = "the half sum fits; gemmlowp's exact rounding"
)]
fn rounding_half_sum(a: i32, b: i32) -> i32 {
    let sum = i64::from(a) + i64::from(b);
    let sign = if sum >= 0 { 1 } else { -1 };
    ((sum + sign) / 2) as i32
}

/// `exp_on_interval_between_negative_one_quarter_and_0_excl` (Q0.31 in and out).
fn exp_on_interval_between_negative_one_quarter_and_0_excl(a: i32) -> i32 {
    const CONSTANT_TERM: i32 = 1_895_147_668; // exp(-1/8)
    const ONE_THIRD: i32 = 715_827_883;
    let x = a.wrapping_add(1 << 28);
    let x2 = srdhm(x, x);
    let x3 = srdhm(x2, x);
    let x4 = srdhm(x2, x2);
    let x4_over_4 = saturating_rounding_multiply_by_pot(x4, -2);
    let poly = saturating_rounding_multiply_by_pot(
        srdhm(x4_over_4.wrapping_add(x3), ONE_THIRD).wrapping_add(x2),
        -1,
    );
    CONSTANT_TERM.wrapping_add(srdhm(CONSTANT_TERM, x.wrapping_add(poly)))
}

/// `exp_on_negative_values` for a Q4.27 input, Q0.31 output.
fn exp_on_negative_values_q4(a: i32) -> i32 {
    const INTEGER_BITS: i32 = 4;
    const FRACTIONAL_BITS: i32 = 31 - INTEGER_BITS;
    const BARREL: [(i32, i32); 6] = [
        (-2, 1_672_461_947),
        (-1, 1_302_514_674),
        (0, 790_015_084),
        (1, 290_630_308),
        (2, 39_332_535),
        (3, 720_401),
    ];
    let one_quarter = 1_i32 << (FRACTIONAL_BITS - 2);
    let mask = one_quarter - 1;
    let a_mod_quarter_minus_one_quarter = (a & mask).wrapping_sub(one_quarter);
    let mut result = exp_on_interval_between_negative_one_quarter_and_0_excl(rescale(
        a_mod_quarter_minus_one_quarter,
        INTEGER_BITS,
        0,
    ));
    let remainder = a_mod_quarter_minus_one_quarter.wrapping_sub(a);
    for (exponent, multiplier) in BARREL {
        if remainder & (1 << (FRACTIONAL_BITS + exponent)) != 0 {
            result = srdhm(result, multiplier);
        }
    }
    if a == 0 { i32::MAX } else { result }
}

/// `one_over_one_plus_x_for_x_in_0_1` (Q0.31 in and out), by Newton-Raphson.
fn one_over_one_plus_x_for_x_in_0_1(a: i32) -> i32 {
    const F2_ONE: i32 = 1 << 29;
    let half_denominator = rounding_half_sum(a, i32::MAX);
    let mut x = 1_515_870_810_i32.wrapping_add(srdhm(half_denominator, -1_010_580_540));
    for _ in 0..3 {
        let half_denominator_times_x = srdhm(half_denominator, x);
        let one_minus = F2_ONE.wrapping_sub(half_denominator_times_x);
        x = x.wrapping_add(rescale(srdhm(x, one_minus), 4, 2));
    }
    // `ExactMulByPot<-1>` only relabels Q2.29 as Q1.30 (same raw value), then `Rescale<0>`.
    rescale(x, 1, 0)
}

/// gemmlowp `logistic` of a Q4.27 value, Q0.31 result.
pub fn logistic_q4(a: i32) -> i32 {
    if a == 0 {
        return 1 << 30;
    }
    let abs = if a > 0 { a } else { a.wrapping_neg() };
    let positive = one_over_one_plus_x_for_x_in_0_1(exp_on_negative_values_q4(abs.wrapping_neg()));
    if a > 0 {
        positive
    } else {
        i32::MAX.wrapping_sub(positive)
    }
}

/// The quantized range of a fused activation (`CalculateActivationRangeQuantized`, int8).
#[allow(clippy::cast_possible_truncation, reason = "TfLiteRound of a scaled value")]
pub fn activation_range(activation: u8, scale: f32, zero_point: i32) -> Option<(i32, i32)> {
    let quantize = |f: f32| zero_point + (f / scale).round() as i32;
    let (min, max) = (i32::from(i8::MIN), i32::from(i8::MAX));
    match activation {
        0 => Some((min, max)),
        1 => Some((min.max(quantize(0.0)), max)),
        2 => Some((min.max(quantize(-1.0)), max.min(quantize(1.0)))),
        3 => Some((min.max(quantize(0.0)), max.min(quantize(6.0)))),
        _ => None,
    }
}

/// `CalculateInputRadius`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "as in C"
)]
pub fn input_radius(integer_bits: i32, left_shift: i32) -> i32 {
    let max = f64::from((1 << integer_bits) - 1) * (1_i64 << (31 - integer_bits)) as f64
        / (1_i64 << left_shift) as f64;
    max.floor() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipliers() {
        assert_eq!(quantize_multiplier(1.0), (1 << 30, 1));
        assert_eq!(quantize_multiplier(0.5), (1 << 30, 0));
        assert_eq!(multiply_by_quantized_multiplier(100, 1 << 30, 1), 100);
        let (m, s) = quantize_multiplier(0.25);
        assert_eq!(
            multiply_by_quantized_multiplier(10, m, s),
            3,
            "2.5 rounds away from zero"
        );
        assert_eq!(multiply_by_quantized_multiplier(-10, m, s), -3);
    }

    #[test]
    fn rounding() {
        assert_eq!(rounding_divide_by_pot(5, 1), 3);
        assert_eq!(rounding_divide_by_pot(-5, 1), -3);
        assert_eq!(rounding_divide_by_pot(4, 2), 1);
        assert_eq!(srdhm(i32::MIN, i32::MIN), i32::MAX);
    }

    #[test]
    #[allow(clippy::cast_possible_truncation, reason = "test inputs are small")]
    fn logistic_is_close_to_the_real_one() {
        for x in [-7.5_f64, -3.0, -0.5, 0.0, 0.25, 1.0, 4.0, 7.9] {
            let raw = (x * f64::from(1 << 27)).round() as i32;
            let got = f64::from(logistic_q4(raw)) / f64::from(1_u32 << 31);
            let want = 1.0 / (1.0 + (-x).exp());
            assert!((got - want).abs() < 1e-6, "logistic({x}) = {got}, want {want}");
        }
    }
}
