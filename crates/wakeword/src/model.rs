//! A microWakeWord `.tflite` model: parsed, checked against the op set and limits we support,
//! and run as an int8 streaming interpreter whose results equal TensorFlow Lite Micro's.
//!
//! The reference is TensorFlow Lite **Micro** (what Home Assistant's voice hardware runs), deliberately not
//! the full `TFLite` runtime `pymicro-wakeword` uses: its int8 kernels round differently and its
//! outputs differ by up to 6/255 on the same features (H15-68).
//!
//! The models are streaming CNNs: each invocation takes [`Model::stride`] feature steps and keeps
//! the convolution history in resource variables (`VAR_HANDLE` / `READ_VARIABLE` /
//! `ASSIGN_VARIABLE`), which a second subgraph initialises (`CALL_ONCE`).

use std::collections::HashMap;

use crate::Error;
use crate::flatbuffer::Table;
use crate::frontend::CHANNELS;
use crate::kernels::{
    activation_range, input_radius, logistic_q4, multiply_by_quantized_multiplier, quantize_multiplier,
    rounding_divide_by_pot,
};

/// Largest model file we accept (the built-in ones are 50–80 KB).
pub const MAX_MODEL_BYTES: usize = 1 << 20;
/// Largest tensor (elements) and number of operators.
const MAX_TENSOR_ELEMENTS: usize = 1 << 16;
const MAX_OPERATORS: usize = 1024;

const CONCATENATION: i32 = 2;
const CONV_2D: i32 = 3;
const DEPTHWISE_CONV_2D: i32 = 4;
const FULLY_CONNECTED: i32 = 9;
const LOGISTIC: i32 = 14;
const RESHAPE: i32 = 22;
const STRIDED_SLICE: i32 = 45;
const SPLIT_V: i32 = 102;
const QUANTIZE: i32 = 114;
const CALL_ONCE: i32 = 129;
const VAR_HANDLE: i32 = 142;
const READ_VARIABLE: i32 = 143;
const ASSIGN_VARIABLE: i32 = 144;

const TYPE_INT32: u8 = 2;
const TYPE_UINT8: u8 = 3;
const TYPE_INT8: u8 = 9;

/// A tensor as stored in the model.
#[derive(Debug, Clone)]
struct TensorDef {
    dtype: u8,
    shape: Vec<usize>,
    scale: Vec<f32>,
    zero_point: Vec<i64>,
    /// Constant data (empty for activations).
    data: Vec<u8>,
}

impl TensorDef {
    fn len(&self) -> usize {
        self.shape.iter().product()
    }

    fn scale(&self) -> Result<f32, Error> {
        self.scale
            .first()
            .copied()
            .ok_or(Error::Unsupported("tensor without quantization"))
    }

    fn zero_point(&self) -> Result<i32, Error> {
        let zp = self.zero_point.first().copied().unwrap_or(0);
        i32::try_from(zp).map_err(|_| Error::Malformed("zero point out of range"))
    }

    fn i8s(&self) -> Vec<i8> {
        self.data.iter().map(|&b| b.cast_signed()).collect()
    }

    fn i32s(&self) -> Vec<i32> {
        self.data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| i32::from_le_bytes(*c))
            .collect()
    }
}

/// An operator as stored in the model; `options` is its builtin options table.
#[derive(Debug, Clone, Copy)]
struct OpDef<'a> {
    code: i32,
    options: Option<Table<'a>>,
}

#[derive(Debug, Clone)]
struct GraphDef<'a> {
    tensors: Vec<TensorDef>,
    inputs: Vec<usize>,
    outputs: Vec<usize>,
    ops: Vec<(OpDef<'a>, Vec<Option<usize>>, Vec<usize>)>,
}

/// Requantization of an int32 accumulator to int8 with per-channel multipliers.
#[derive(Debug, Clone)]
struct Requant {
    multipliers: Vec<(i32, i32)>,
    input_offset: i32,
    output_offset: i32,
    min: i32,
    max: i32,
}

impl Requant {
    #[allow(clippy::cast_possible_truncation, reason = "clamped to int8")]
    fn apply(&self, acc: i32, channel: usize) -> i8 {
        let (m, s) = self.multipliers[channel.min(self.multipliers.len() - 1)];
        let v = multiply_by_quantized_multiplier(acc, m, s) + self.output_offset;
        v.clamp(self.min, self.max) as i8
    }
}

/// NHWC geometry of a convolution.
#[derive(Debug, Clone, Copy)]
struct Geometry {
    in_h: usize,
    in_w: usize,
    in_c: usize,
    out_h: usize,
    out_w: usize,
    out_c: usize,
    k_h: usize,
    k_w: usize,
    stride_h: usize,
    stride_w: usize,
    dil_h: usize,
    dil_w: usize,
    pad_h: usize,
    pad_w: usize,
}

/// One prepared operation.
#[derive(Debug, Clone)]
enum Step {
    Conv {
        input: usize,
        output: usize,
        filter: Vec<i8>,
        /// `Σ w` over the input channels, per output channel and kernel tap.
        tap_sums: Vec<i32>,
        bias: Vec<i32>,
        geometry: Geometry,
        requant: Requant,
    },
    Depthwise {
        input: usize,
        output: usize,
        filter: Vec<i8>,
        bias: Vec<i32>,
        geometry: Geometry,
        multiplier: usize,
        requant: Requant,
    },
    FullyConnected {
        input: usize,
        output: usize,
        filter: Vec<i8>,
        bias: Vec<i32>,
        depth: usize,
        filter_offset: i32,
        requant: Requant,
    },
    /// Concatenation and `SPLIT_V` as block copies: `(tensor, block length)` per part, repeated
    /// `outer` times.
    Concat {
        parts: Vec<(usize, usize)>,
        output: usize,
        outer: usize,
    },
    Split {
        input: usize,
        parts: Vec<(usize, usize)>,
        outer: usize,
    },
    /// `STRIDED_SLICE` as a gather of precomputed input indices.
    Gather {
        input: usize,
        output: usize,
        indices: Vec<usize>,
    },
    Copy {
        input: usize,
        output: usize,
    },
    Logistic {
        input: usize,
        output: usize,
        zero_point: i32,
        radius: i32,
        multiplier: i32,
        shift: i32,
    },
    Requantize {
        input: usize,
        output: usize,
        input_zp: i32,
        output_zp: i32,
        multiplier: i32,
        shift: i32,
        unsigned: bool,
    },
    Read {
        var: usize,
        output: usize,
    },
    Assign {
        var: usize,
        input: usize,
    },
}

/// A loaded model, ready to run.
#[derive(Debug, Clone)]
pub struct Model {
    steps: Vec<Step>,
    /// Initial variable values (the `CALL_ONCE` subgraph's assignments).
    init: Vec<(usize, Vec<i8>)>,
    values: Vec<Vec<i8>>,
    vars: Vec<Vec<i8>>,
    input: usize,
    output: usize,
    stride: usize,
    input_scale: f32,
    input_zero_point: i32,
}

impl Model {
    /// Parses and checks a `.tflite` microWakeWord model.
    pub fn new(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > MAX_MODEL_BYTES {
            return Err(Error::Unsupported("model larger than 1 MiB"));
        }
        let root = Table::root(bytes)?;
        if root.u32(0, 0)? != 3 {
            return Err(Error::Unsupported("not a TFLite schema version 3 model"));
        }
        let codes = operator_codes(&root)?;
        let buffers = buffers(&root)?;
        let subgraphs = root.vector(2)?.ok_or(Error::Malformed("no subgraphs"))?;
        let graphs = (0..subgraphs.len())
            .map(|i| graph(&subgraphs.table(i)?, &codes, &buffers))
            .collect::<Result<Vec<_>, _>>()?;
        Builder::default().build(&graphs)
    }

    /// Feature steps per invocation (the input's second dimension, 3 for the built-in models).
    #[must_use]
    pub fn stride(&self) -> usize {
        self.stride
    }

    /// Back to the initial state (variables as the init subgraph sets them).
    pub fn reset(&mut self) {
        for (var, value) in &self.init {
            self.vars[*var].clone_from(value);
        }
    }

    /// Quantizes frontend features (as `u16`) for the model's input, as `pymicro-wakeword` does:
    /// `round_half_even(v * 0.0390625 / scale + zero_point)`.
    #[allow(clippy::cast_possible_truncation, reason = "clamped to int8")]
    #[must_use]
    pub fn quantize(&self, feature: u16) -> i8 {
        let x =
            f64::from(feature) * 0.039_062_5 / f64::from(self.input_scale) + f64::from(self.input_zero_point);
        x.round_ties_even().clamp(-128.0, 127.0) as i8
    }

    /// Runs one invocation on `stride * 40` quantized features; returns the probability byte
    /// (0–255).
    pub fn invoke(&mut self, input: &[i8]) -> Result<u8, Error> {
        if input.len() != self.stride * CHANNELS {
            return Err(Error::Input("wrong number of features"));
        }
        self.values[self.input].copy_from_slice(input);
        for i in 0..self.steps.len() {
            run(&self.steps[i], &mut self.values, &mut self.vars);
        }
        Ok(self.values[self.output][0].cast_unsigned())
    }
}

fn operator_codes(root: &Table<'_>) -> Result<Vec<i32>, Error> {
    let codes = root.vector(1)?.ok_or(Error::Malformed("no operator codes"))?;
    (0..codes.len())
        .map(|i| {
            let code = codes.table(i)?;
            Ok(i32::from(code.u8(0, 0)?).max(code.i32(3, 0)?))
        })
        .collect()
}

fn buffers<'a>(root: &Table<'a>) -> Result<Vec<&'a [u8]>, Error> {
    let Some(buffers) = root.vector(4)? else {
        return Ok(Vec::new());
    };
    (0..buffers.len())
        .map(|i| {
            Ok(buffers
                .table(i)?
                .vector(0)?
                .map(|v| v.bytes())
                .transpose()?
                .unwrap_or(&[]))
        })
        .collect()
}

fn indices(v: Option<crate::flatbuffer::Vector<'_>>) -> Result<Vec<Option<usize>>, Error> {
    let Some(v) = v else { return Ok(Vec::new()) };
    Ok(v.i32s()?.into_iter().map(|i| usize::try_from(i).ok()).collect())
}

fn graph<'a>(g: &Table<'a>, codes: &[i32], buffers: &[&'a [u8]]) -> Result<GraphDef<'a>, Error> {
    let tensors = g.vector(0)?.ok_or(Error::Malformed("subgraph without tensors"))?;
    let tensors = (0..tensors.len())
        .map(|i| tensor(&tensors.table(i)?, buffers))
        .collect::<Result<Vec<_>, _>>()?;
    let required = |v: Vec<Option<usize>>| {
        v.into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or(Error::Malformed("missing tensor"))
    };
    let inputs = required(indices(g.vector(1)?)?)?;
    let outputs = required(indices(g.vector(2)?)?)?;
    let Some(ops) = g.vector(3)? else {
        return Ok(GraphDef {
            tensors,
            inputs,
            outputs,
            ops: Vec::new(),
        });
    };
    if ops.len() > MAX_OPERATORS {
        return Err(Error::Unsupported("too many operators"));
    }
    let ops = (0..ops.len())
        .map(|i| {
            let op = ops.table(i)?;
            let code = *codes
                .get(op.u32(0, 0)? as usize)
                .ok_or(Error::Malformed("operator code index"))?;
            let outputs = required(indices(op.vector(2)?)?)?;
            Ok((
                OpDef {
                    code,
                    options: op.table(4)?,
                },
                indices(op.vector(1)?)?,
                outputs,
            ))
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let n = tensors.len();
    let in_range = |&i: &usize| i < n;
    let all_in_range = inputs.iter().all(in_range)
        && outputs.iter().all(in_range)
        && ops
            .iter()
            .all(|(_, i, o)| i.iter().flatten().all(in_range) && o.iter().all(in_range));
    if !all_in_range {
        return Err(Error::Malformed("tensor index out of range"));
    }
    Ok(GraphDef {
        tensors,
        inputs,
        outputs,
        ops,
    })
}

fn tensor(t: &Table<'_>, buffers: &[&[u8]]) -> Result<TensorDef, Error> {
    let shape = t
        .vector(0)?
        .map(|v| v.i32s())
        .transpose()?
        .unwrap_or_default()
        .into_iter()
        .map(|d| usize::try_from(d).map_err(|_| Error::Unsupported("dynamic tensor shape")))
        .collect::<Result<Vec<_>, _>>()?;
    let elements = shape.iter().try_fold(1_usize, |a, &d| a.checked_mul(d));
    if elements.is_none_or(|e| e > MAX_TENSOR_ELEMENTS) {
        return Err(Error::Unsupported("tensor too large"));
    }
    let q = t.table(4)?;
    let scale = q
        .map(|q| q.vector(2))
        .transpose()?
        .flatten()
        .map(|v| v.f32s())
        .transpose()?
        .unwrap_or_default();
    let zero_point = q
        .map(|q| q.vector(3))
        .transpose()?
        .flatten()
        .map(|v| v.i64s())
        .transpose()?
        .unwrap_or_default();
    let data = buffers
        .get(t.u32(2, 0)? as usize)
        .copied()
        .unwrap_or(&[])
        .to_vec();
    Ok(TensorDef {
        dtype: t.u8(1, 0)?,
        shape,
        scale,
        zero_point,
        data,
    })
}

/// Turns the parsed graphs into steps, with the variables shared by name across subgraphs.
#[derive(Debug, Default)]
struct Builder {
    var_ids: HashMap<String, usize>,
    steps: Vec<Step>,
}

impl Builder {
    fn build(mut self, graphs: &[GraphDef<'_>]) -> Result<Model, Error> {
        let main = graphs.first().ok_or(Error::Malformed("no subgraphs"))?;
        let (input, output) = check_io(main)?;
        let mut init = Vec::new();
        let mut resources = HashMap::new();
        for (op, ins, outs) in &main.ops {
            self.op(main, *op, ins, outs, &mut resources, graphs, &mut init)?;
        }
        let in_tensor = &main.tensors[input];
        let mut values: Vec<Vec<i8>> = main.tensors.iter().map(|t| vec![0; t.len()]).collect();
        for (v, t) in values.iter_mut().zip(&main.tensors) {
            if t.dtype == TYPE_INT8 && t.data.len() == t.len() {
                *v = t.i8s();
            }
        }
        let mut model = Model {
            steps: self.steps,
            vars: vec![Vec::new(); self.var_ids.len()],
            init,
            values,
            input,
            output,
            stride: in_tensor.shape[1],
            input_scale: in_tensor.scale()?,
            input_zero_point: in_tensor.zero_point()?,
        };
        model.reset();
        if model.vars.iter().any(Vec::is_empty) {
            return Err(Error::Unsupported("a variable without an initial value"));
        }
        Ok(model)
    }

    fn var(&mut self, op: &OpDef<'_>) -> Result<usize, Error> {
        let options = op.options.ok_or(Error::Malformed("VAR_HANDLE without options"))?;
        let name = options.string(1)?.unwrap_or_default().to_owned();
        let next = self.var_ids.len();
        Ok(*self.var_ids.entry(name).or_insert(next))
    }

    /// Runs the init subgraph statically: it may only assign constants to variables.
    fn call_once(
        &mut self,
        op: &OpDef<'_>,
        graphs: &[GraphDef<'_>],
        init: &mut Vec<(usize, Vec<i8>)>,
    ) -> Result<(), Error> {
        let options = op.options.ok_or(Error::Malformed("CALL_ONCE without options"))?;
        let index = usize::try_from(options.i32(0, 0)?).map_err(|_| Error::Malformed("subgraph index"))?;
        let g = graphs.get(index).ok_or(Error::Malformed("CALL_ONCE subgraph"))?;
        let mut handles = HashMap::new();
        for (op, ins, outs) in &g.ops {
            match (op.code, ins.as_slice(), outs.as_slice()) {
                (VAR_HANDLE, _, [out]) => {
                    handles.insert(*out, self.var(op)?);
                }
                (ASSIGN_VARIABLE, [Some(r), Some(v)], []) => {
                    let var = *handles
                        .get(r)
                        .ok_or(Error::Malformed("ASSIGN_VARIABLE without handle"))?;
                    let t = &g.tensors[*v];
                    if t.dtype != TYPE_INT8 || t.data.len() != t.len() {
                        return Err(Error::Unsupported("variables must start from int8 constants"));
                    }
                    init.push((var, t.i8s()));
                }
                _ => return Err(Error::Unsupported("the init subgraph may only assign variables")),
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments, reason = "the builder's context for one operator")]
    fn op(
        &mut self,
        g: &GraphDef<'_>,
        op: OpDef<'_>,
        ins: &[Option<usize>],
        outs: &[usize],
        resources: &mut HashMap<usize, usize>,
        graphs: &[GraphDef<'_>],
        init: &mut Vec<(usize, Vec<i8>)>,
    ) -> Result<(), Error> {
        let input = |i: usize| {
            ins.get(i)
                .copied()
                .flatten()
                .ok_or(Error::Malformed("missing operator input"))
        };
        let output = |i: usize| {
            outs.get(i)
                .copied()
                .ok_or(Error::Malformed("missing operator output"))
        };
        let step = match op.code {
            CALL_ONCE => return self.call_once(&op, graphs, init),
            VAR_HANDLE => {
                let var = self.var(&op)?;
                resources.insert(output(0)?, var);
                return Ok(());
            }
            READ_VARIABLE => Step::Read {
                var: resource(resources, input(0)?)?,
                output: output(0)?,
            },
            ASSIGN_VARIABLE => Step::Assign {
                var: resource(resources, input(0)?)?,
                input: input(1)?,
            },
            _ => compute_step(g, &op, ins, outs)?,
        };
        self.steps.push(step);
        Ok(())
    }
}

/// The step of an operator that only computes (no variables, no subgraphs).
fn compute_step(
    g: &GraphDef<'_>,
    op: &OpDef<'_>,
    ins: &[Option<usize>],
    outs: &[usize],
) -> Result<Step, Error> {
    let input = |i: usize| {
        ins.get(i)
            .copied()
            .flatten()
            .ok_or(Error::Malformed("missing operator input"))
    };
    let optional = |i: usize| ins.get(i).copied().flatten();
    let output = outs
        .first()
        .copied()
        .ok_or(Error::Malformed("missing operator output"))?;
    match op.code {
        CONV_2D => conv(g, op, input(0)?, input(1)?, optional(2), output, false),
        DEPTHWISE_CONV_2D => conv(g, op, input(0)?, input(1)?, optional(2), output, true),
        FULLY_CONNECTED => fully_connected(g, op, input(0)?, input(1)?, optional(2), output),
        CONCATENATION => concat(g, op, ins, output),
        SPLIT_V => split(g, input(0)?, input(1)?, input(2)?, outs),
        STRIDED_SLICE => strided_slice(g, op, [input(0)?, input(1)?, input(2)?, input(3)?], output),
        RESHAPE => reshape(g, input(0)?, output),
        LOGISTIC => logistic(g, input(0)?, output),
        QUANTIZE => requantize(g, input(0)?, output),
        _ => Err(Error::Unsupported("an operator outside the microWakeWord op set")),
    }
}

fn resource(resources: &HashMap<usize, usize>, tensor: usize) -> Result<usize, Error> {
    resources
        .get(&tensor)
        .copied()
        .ok_or(Error::Malformed("variable used before VAR_HANDLE"))
}

/// The model's one input (int8 `[1, stride, 40]`) and one output (uint8 `[1, 1]`).
fn check_io(g: &GraphDef<'_>) -> Result<(usize, usize), Error> {
    let ([input], [output]) = (g.inputs.as_slice(), g.outputs.as_slice()) else {
        return Err(Error::Unsupported(
            "the model needs exactly one input and one output",
        ));
    };
    let (i, o) = (&g.tensors[*input], &g.tensors[*output]);
    let input_ok = i.dtype == TYPE_INT8
        && i.shape.len() == 3
        && i.shape[0] == 1
        && i.shape[2] == CHANNELS
        && (1..=10).contains(&i.shape[1]);
    if !input_ok {
        return Err(Error::Unsupported("input must be int8 [1, stride, 40]"));
    }
    if o.dtype != TYPE_UINT8 || o.len() != 1 {
        return Err(Error::Unsupported("output must be one uint8 probability"));
    }
    Ok((*input, *output))
}

fn int8(t: &TensorDef) -> Result<(), Error> {
    if t.dtype == TYPE_INT8 {
        Ok(())
    } else {
        Err(Error::Unsupported("activations must be int8"))
    }
}

fn constant_i8(t: &TensorDef) -> Result<Vec<i8>, Error> {
    if t.dtype != TYPE_INT8 || t.data.len() != t.len() {
        return Err(Error::Unsupported("weights must be int8 constants"));
    }
    Ok(t.i8s())
}

fn constant_i32(t: &TensorDef) -> Result<Vec<i32>, Error> {
    if t.dtype != TYPE_INT32 || t.data.len() != 4 * t.len() {
        return Err(Error::Unsupported("biases and indices must be int32 constants"));
    }
    Ok(t.i32s())
}

fn requant(input: &TensorDef, output: &TensorDef, scales: &[f64], activation: u8) -> Result<Requant, Error> {
    let (out_scale, out_zp) = (output.scale()?, output.zero_point()?);
    let (min, max) =
        activation_range(activation, out_scale, out_zp).ok_or(Error::Unsupported("fused activation"))?;
    let in_scale = f64::from(input.scale()?);
    Ok(Requant {
        multipliers: scales
            .iter()
            .map(|&s| quantize_multiplier(in_scale * s / f64::from(out_scale)))
            .collect(),
        input_offset: -input.zero_point()?,
        output_offset: out_zp,
        min,
        max,
    })
}

/// `CONV_2D` and `DEPTHWISE_CONV_2D` (per-channel int8).
fn conv(
    g: &GraphDef<'_>,
    op: &OpDef<'_>,
    input: usize,
    filter: usize,
    bias: Option<usize>,
    output: usize,
    depthwise: bool,
) -> Result<Step, Error> {
    let o = op
        .options
        .ok_or(Error::Malformed("convolution without options"))?;
    let (padding, stride_w, stride_h) = (o.u8(0, 0)?, o.i32(1, 1)?, o.i32(2, 1)?);
    let (activation, dil_w, dil_h, multiplier) = if depthwise {
        (o.u8(4, 0)?, o.i32(5, 1)?, o.i32(6, 1)?, o.i32(3, 1)?)
    } else {
        (o.u8(3, 0)?, o.i32(4, 1)?, o.i32(5, 1)?, 1)
    };
    let (ti, tf, to) = (&g.tensors[input], &g.tensors[filter], &g.tensors[output]);
    int8(ti)?;
    int8(to)?;
    let dims_ok = ti.shape.len() == 4 && tf.shape.len() == 4 && to.shape.len() == 4;
    if !dims_ok {
        return Err(Error::Unsupported("convolutions must be 4-D"));
    }
    let geometry = geometry(
        &ti.shape,
        &tf.shape,
        &to.shape,
        padding,
        [stride_h, stride_w, dil_h, dil_w],
    )?;
    let channels_ok = if depthwise {
        tf.shape[0] == 1
            && tf.shape[3] == geometry.out_c
            && geometry.out_c == geometry.in_c * usize::try_from(multiplier).unwrap_or(0)
    } else {
        tf.shape[0] == geometry.out_c && tf.shape[3] == geometry.in_c
    };
    if !channels_ok || ti.shape[0] != 1 {
        return Err(Error::Unsupported("convolution channel layout"));
    }
    let bias = bias
        .map(|b| constant_i32(&g.tensors[b]))
        .transpose()?
        .unwrap_or_else(|| vec![0; geometry.out_c]);
    let scales: Vec<f64> = tf.scale.iter().map(|&s| f64::from(s)).collect();
    if bias.len() != geometry.out_c || !(scales.len() == 1 || scales.len() == geometry.out_c) {
        return Err(Error::Unsupported("convolution quantization"));
    }
    let requant = requant(ti, to, &scales, activation)?;
    let filter = constant_i8(tf)?;
    Ok(if depthwise {
        Step::Depthwise {
            input,
            output,
            filter,
            bias,
            geometry,
            multiplier: geometry.out_c / geometry.in_c,
            requant,
        }
    } else {
        let tap_sums = filter
            .chunks(geometry.in_c)
            .map(|w| w.iter().map(|&w| i32::from(w)).sum())
            .collect();
        Step::Conv {
            input,
            output,
            filter,
            tap_sums,
            bias,
            geometry,
            requant,
        }
    })
}

/// The convolution's geometry from NHWC input/output and `[_, kh, kw, _]` filter shapes, checked
/// against `TFLite`'s output size; padding as `ComputePaddingHeightWidth` computes it.
fn geometry(
    input: &[usize],
    filter: &[usize],
    output: &[usize],
    padding: u8,
    steps: [i32; 4],
) -> Result<Geometry, Error> {
    let positive = |v: i32| {
        usize::try_from(v)
            .ok()
            .filter(|&v| v > 0)
            .ok_or(Error::Malformed("stride or dilation"))
    };
    let [stride_h, stride_w, dil_h, dil_w] = [
        positive(steps[0])?,
        positive(steps[1])?,
        positive(steps[2])?,
        positive(steps[3])?,
    ];
    let (k_h, k_w) = (filter[1], filter[2]);
    let pad = |in_size: usize, k: usize, stride: usize, dil: usize, out: usize| -> Result<usize, Error> {
        let effective = (k - 1) * dil + 1;
        let expected = if padding == 0 {
            in_size.div_ceil(stride)
        } else {
            (in_size + stride).saturating_sub(effective) / stride
        };
        if expected != out {
            return Err(Error::Malformed("convolution output shape"));
        }
        Ok((((out - 1) * stride + effective).saturating_sub(in_size)) / 2)
    };
    Ok(Geometry {
        in_h: input[1],
        in_w: input[2],
        in_c: input[3],
        out_h: output[1],
        out_w: output[2],
        out_c: output[3],
        k_h,
        k_w,
        stride_h,
        stride_w,
        dil_h,
        dil_w,
        pad_h: pad(input[1], k_h, stride_h, dil_h, output[1])?,
        pad_w: pad(input[2], k_w, stride_w, dil_w, output[2])?,
    })
}

fn fully_connected(
    g: &GraphDef<'_>,
    op: &OpDef<'_>,
    input: usize,
    filter: usize,
    bias: Option<usize>,
    output: usize,
) -> Result<Step, Error> {
    let activation = op.options.map(|o| o.u8(0, 0)).transpose()?.unwrap_or(0);
    let (ti, tf, to) = (&g.tensors[input], &g.tensors[filter], &g.tensors[output]);
    int8(ti)?;
    int8(to)?;
    let [units, depth] = *tf.shape.as_slice() else {
        return Err(Error::Unsupported("fully connected weights must be 2-D"));
    };
    if ti.len() != depth || to.len() != units || tf.scale.len() != 1 {
        return Err(Error::Unsupported("fully connected shape or quantization"));
    }
    // GetQuantizedConvolutionMultipler: the scales are multiplied in float first.
    let product = f64::from(ti.scale()? * tf.scale()?);
    let mut requant = requant(ti, to, &[1.0], activation)?;
    requant.multipliers = vec![quantize_multiplier(product / f64::from(to.scale()?))];
    let bias = bias
        .map(|b| constant_i32(&g.tensors[b]))
        .transpose()?
        .unwrap_or_else(|| vec![0; units]);
    Ok(Step::FullyConnected {
        input,
        output,
        filter: constant_i8(tf)?,
        bias,
        depth,
        filter_offset: -tf.zero_point()?,
        requant,
    })
}

/// Splits a shape at `axis`: (outer elements, elements per index of the axis and below).
fn around(shape: &[usize], axis: usize) -> (usize, usize) {
    (shape[..axis].iter().product(), shape[axis + 1..].iter().product())
}

fn axis_of(axis: i32, rank: usize) -> Result<usize, Error> {
    let rank_i = i32::try_from(rank).map_err(|_| Error::Malformed("rank"))?;
    let a = if axis < 0 { axis + rank_i } else { axis };
    usize::try_from(a)
        .ok()
        .filter(|&a| a < rank)
        .ok_or(Error::Malformed("axis out of range"))
}

fn same_quantization(a: &TensorDef, b: &TensorDef) -> Result<(), Error> {
    if a.scale()?.to_bits() == b.scale()?.to_bits() && a.zero_point()? == b.zero_point()? {
        Ok(())
    } else {
        Err(Error::Unsupported(
            "int8 concatenation/split with different quantization",
        ))
    }
}

fn concat(g: &GraphDef<'_>, op: &OpDef<'_>, ins: &[Option<usize>], output: usize) -> Result<Step, Error> {
    let o = op
        .options
        .ok_or(Error::Malformed("CONCATENATION without options"))?;
    if o.u8(1, 0)? != 0 {
        return Err(Error::Unsupported("fused activation on concatenation"));
    }
    let to = &g.tensors[output];
    int8(to)?;
    let axis = axis_of(o.i32(0, 0)?, to.shape.len())?;
    let (outer, inner) = around(&to.shape, axis);
    let mut parts = Vec::new();
    for t in ins.iter().copied() {
        let t = t.ok_or(Error::Malformed("missing concatenation input"))?;
        let ti = &g.tensors[t];
        int8(ti)?;
        same_quantization(ti, to)?;
        if ti.shape.len() != to.shape.len() {
            return Err(Error::Malformed("concatenation ranks"));
        }
        parts.push((t, ti.shape[axis] * inner));
    }
    if parts.iter().map(|p| p.1).sum::<usize>() * outer != to.len() {
        return Err(Error::Malformed("concatenation sizes"));
    }
    Ok(Step::Concat { parts, output, outer })
}

fn split(g: &GraphDef<'_>, input: usize, sizes: usize, axis: usize, outs: &[usize]) -> Result<Step, Error> {
    let ti = &g.tensors[input];
    int8(ti)?;
    let axis = axis_of(
        *constant_i32(&g.tensors[axis])?
            .first()
            .ok_or(Error::Malformed("split axis"))?,
        ti.shape.len(),
    )?;
    let (outer, inner) = around(&ti.shape, axis);
    let sizes = constant_i32(&g.tensors[sizes])?;
    if sizes.len() != outs.len() {
        return Err(Error::Malformed("SPLIT_V sizes"));
    }
    let mut parts = Vec::new();
    for &o in outs {
        let to = &g.tensors[o];
        int8(to)?;
        same_quantization(ti, to)?;
        parts.push((o, to.shape.get(axis).copied().unwrap_or(0) * inner));
    }
    if parts.iter().map(|p| p.1).sum::<usize>() * outer != ti.len() {
        return Err(Error::Malformed("SPLIT_V sizes"));
    }
    Ok(Step::Split { input, parts, outer })
}

/// `STRIDED_SLICE` with constant begin/end/strides, resolved to input indices once.
fn strided_slice(g: &GraphDef<'_>, op: &OpDef<'_>, ins: [usize; 4], output: usize) -> Result<Step, Error> {
    let o = op
        .options
        .ok_or(Error::Malformed("STRIDED_SLICE without options"))?;
    let (begin_mask, end_mask, ellipsis, new_axis, shrink) = (
        o.i32(0, 0)?,
        o.i32(1, 0)?,
        o.i32(2, 0)?,
        o.i32(3, 0)?,
        o.i32(4, 0)?,
    );
    if ellipsis != 0 || new_axis != 0 || o.bool(5)? {
        return Err(Error::Unsupported("strided slice masks"));
    }
    let ti = &g.tensors[ins[0]];
    int8(ti)?;
    int8(&g.tensors[output])?;
    let (begin, end, strides) = (
        constant_i32(&g.tensors[ins[1]])?,
        constant_i32(&g.tensors[ins[2]])?,
        constant_i32(&g.tensors[ins[3]])?,
    );
    let rank = ti.shape.len();
    if begin.len() != rank || end.len() != rank || strides.len() != rank || strides.contains(&0) {
        return Err(Error::Malformed("strided slice parameters"));
    }
    let mut ranges = Vec::with_capacity(rank);
    for axis in 0..rank {
        let dim = i64::try_from(ti.shape[axis]).map_err(|_| Error::Malformed("dim"))?;
        let stride = i64::from(strides[axis]);
        let bit = |mask: i32| mask & (1 << axis) != 0;
        let resolve = |v: i32, lo: i64, hi: i64| {
            let v = i64::from(v);
            (if v < 0 { v + dim } else { v }).clamp(lo, hi)
        };
        let (lo, hi) = if stride > 0 { (0, dim) } else { (-1, dim - 1) };
        let start = if bit(begin_mask) {
            if stride > 0 { 0 } else { dim - 1 }
        } else {
            resolve(begin[axis], lo, hi)
        };
        let stop = if bit(shrink) {
            start + 1
        } else if bit(end_mask) {
            if stride > 0 { dim } else { -1 }
        } else {
            resolve(end[axis], lo, hi)
        };
        ranges.push((start, stop, stride));
    }
    let indices = gather_indices(&ti.shape, &ranges)?;
    if indices.len() != g.tensors[output].len() {
        return Err(Error::Malformed("strided slice output shape"));
    }
    Ok(Step::Gather {
        input: ins[0],
        output,
        indices,
    })
}

/// Flat input indices of a strided slice, in output order.
fn gather_indices(shape: &[usize], ranges: &[(i64, i64, i64)]) -> Result<Vec<usize>, Error> {
    let mut indices = vec![0_usize];
    for (axis, &(start, stop, stride)) in ranges.iter().enumerate() {
        let size = shape[axis + 1..].iter().product::<usize>();
        let mut next = Vec::new();
        for &base in &indices {
            let mut i = start;
            while (stride > 0 && i < stop) || (stride < 0 && i > stop) {
                let i_usize = usize::try_from(i).map_err(|_| Error::Malformed("slice index"))?;
                next.push(base + i_usize * size);
                i += stride;
            }
        }
        if next.len() > MAX_TENSOR_ELEMENTS {
            return Err(Error::Unsupported("slice too large"));
        }
        indices = next;
    }
    Ok(indices)
}

fn reshape(g: &GraphDef<'_>, input: usize, output: usize) -> Result<Step, Error> {
    let (ti, to) = (&g.tensors[input], &g.tensors[output]);
    int8(ti)?;
    int8(to)?;
    if ti.len() != to.len() {
        return Err(Error::Malformed("reshape sizes"));
    }
    Ok(Step::Copy { input, output })
}

fn logistic(g: &GraphDef<'_>, input: usize, output: usize) -> Result<Step, Error> {
    let (ti, to) = (&g.tensors[input], &g.tensors[output]);
    int8(ti)?;
    int8(to)?;
    if to.zero_point()? != -128 || ti.len() != to.len() {
        return Err(Error::Unsupported("LOGISTIC output must have zero point -128"));
    }
    // CalculateArithmeticOpData: 4 integer bits.
    let (multiplier, shift) = quantize_multiplier(f64::from(ti.scale()?) * f64::from(1_u32 << 27));
    Ok(Step::Logistic {
        input,
        output,
        zero_point: ti.zero_point()?,
        radius: input_radius(4, shift),
        multiplier,
        shift,
    })
}

fn requantize(g: &GraphDef<'_>, input: usize, output: usize) -> Result<Step, Error> {
    let (ti, to) = (&g.tensors[input], &g.tensors[output]);
    int8(ti)?;
    if !matches!(to.dtype, TYPE_INT8 | TYPE_UINT8) || ti.len() != to.len() {
        return Err(Error::Unsupported("QUANTIZE must go from int8 to int8 or uint8"));
    }
    let (multiplier, shift) = quantize_multiplier(f64::from(ti.scale()?) / f64::from(to.scale()?));
    Ok(Step::Requantize {
        input,
        output,
        input_zp: ti.zero_point()?,
        output_zp: to.zero_point()?,
        multiplier,
        shift,
        unsigned: to.dtype == TYPE_UINT8,
    })
}

// ---------- Execution ----------
//
// Every step writes into the output tensor's existing buffer (taken out and put back, so the
// input can be borrowed): no allocations per invocation.

fn run(step: &Step, values: &mut [Vec<i8>], vars: &mut [Vec<i8>]) {
    match step {
        Step::Conv { input, output, .. }
        | Step::Depthwise { input, output, .. }
        | Step::FullyConnected { input, output, .. }
        | Step::Gather { input, output, .. }
        | Step::Copy { input, output }
        | Step::Logistic { input, output, .. }
        | Step::Requantize { input, output, .. } => {
            let mut out = std::mem::take(&mut values[*output]);
            compute(step, &values[*input], &mut out);
            values[*output] = out;
        }
        Step::Concat { parts, output, outer } => concat_run(values, parts, *output, *outer),
        Step::Split { input, parts, outer } => split_run(values, *input, parts, *outer),
        Step::Read { var, output } => values[*output].clone_from(&vars[*var]),
        Step::Assign { var, input } => vars[*var].clone_from(&values[*input]),
    }
}

/// The steps with one input and one output.
fn compute(step: &Step, input: &[i8], out: &mut Vec<i8>) {
    out.clear();
    match step {
        Step::Conv {
            filter,
            tap_sums,
            bias,
            geometry,
            requant,
            ..
        } => {
            conv_2d(input, filter, tap_sums, bias, geometry, requant, out);
        }
        Step::Depthwise {
            filter,
            bias,
            geometry,
            multiplier,
            requant,
            ..
        } => {
            depthwise_conv_2d(input, filter, bias, geometry, *multiplier, requant, out);
        }
        Step::FullyConnected {
            filter,
            bias,
            depth,
            filter_offset,
            requant,
            ..
        } => {
            out.extend(bias.iter().enumerate().map(|(unit, &b)| {
                let row = &filter[unit * depth..(unit + 1) * depth];
                let acc = row.iter().zip(input).fold(0_i32, |acc, (&w, &x)| {
                    acc + (i32::from(w) + filter_offset) * (i32::from(x) + requant.input_offset)
                });
                requant.apply(acc + b, 0)
            }));
        }
        Step::Gather { indices, .. } => out.extend(indices.iter().map(|&i| input[i])),
        Step::Copy { .. } => out.extend_from_slice(input),
        Step::Logistic {
            zero_point,
            radius,
            multiplier,
            shift,
            ..
        } => {
            out.extend(
                input
                    .iter()
                    .map(|&x| logistic_int8(i32::from(x) - zero_point, *radius, *multiplier, *shift)),
            );
        }
        Step::Requantize {
            input_zp,
            output_zp,
            multiplier,
            shift,
            unsigned,
            ..
        } => {
            out.extend(
                input
                    .iter()
                    .map(|&x| requantize_one(x, *input_zp, *output_zp, *multiplier, *shift, *unsigned)),
            );
        }
        Step::Concat { .. } | Step::Split { .. } | Step::Read { .. } | Step::Assign { .. } => {
            unreachable!("not a one-input step")
        }
    }
}

fn concat_run(values: &mut [Vec<i8>], parts: &[(usize, usize)], output: usize, outer: usize) {
    let mut out = std::mem::take(&mut values[output]);
    out.clear();
    for o in 0..outer {
        for &(t, block) in parts {
            out.extend_from_slice(&values[t][o * block..(o + 1) * block]);
        }
    }
    values[output] = out;
}

fn split_run(values: &mut [Vec<i8>], input: usize, parts: &[(usize, usize)], outer: usize) {
    let total: usize = parts.iter().map(|p| p.1).sum();
    let mut offset = 0;
    for &(t, block) in parts {
        let mut out = std::mem::take(&mut values[t]);
        out.clear();
        for o in 0..outer {
            out.extend_from_slice(&values[input][o * total + offset..o * total + offset + block]);
        }
        values[t] = out;
        offset += block;
    }
}

/// `Σ x·w` of two int8 rows (vectorizable).
fn dot(x: &[i8], w: &[i8]) -> i32 {
    x.iter().zip(w).map(|(&x, &w)| i32::from(x) * i32::from(w)).sum()
}

/// The input positions (`(input offset, kernel tap)`) that one output position reads.
fn positions(g: &Geometry, oy: usize, ox: usize, into: &mut Vec<(usize, usize)>) {
    into.clear();
    for (iy, ky) in taps(oy, g.stride_h, g.pad_h, g.dil_h, g.k_h, g.in_h) {
        for (ix, kx) in taps(ox, g.stride_w, g.pad_w, g.dil_w, g.k_w, g.in_w) {
            into.push(((iy * g.in_w + ix) * g.in_c, ky * g.k_w + kx));
        }
    }
}

/// `reference_integer_ops::ConvPerChannel` (NHWC input, OHWI filter), with
/// `Σ w·(x + offset) = Σ w·x + offset·Σ w` (the same integers; `tap_sums` holds `Σ w` per output
/// channel and tap).
#[allow(clippy::too_many_arguments, reason = "the kernel's operands")]
fn conv_2d(
    input: &[i8],
    filter: &[i8],
    tap_sums: &[i32],
    bias: &[i32],
    g: &Geometry,
    requant: &Requant,
    out: &mut Vec<i8>,
) {
    let taps_per_filter = g.k_h * g.k_w;
    let mut used = Vec::with_capacity(taps_per_filter);
    for oy in 0..g.out_h {
        for ox in 0..g.out_w {
            positions(g, oy, ox, &mut used);
            for (oc, &b) in bias.iter().enumerate().take(g.out_c) {
                let filter_base = oc * taps_per_filter;
                let mut acc = 0_i32;
                let mut weights = 0_i32;
                for &(at, tap) in &used {
                    let w = &filter[(filter_base + tap) * g.in_c..][..g.in_c];
                    acc += dot(&input[at..][..g.in_c], w);
                    weights += tap_sums[filter_base + tap];
                }
                out.push(requant.apply(acc + requant.input_offset * weights + b, oc));
            }
        }
    }
}

/// `reference_integer_ops::DepthwiseConvPerChannel` (filter `[1, kh, kw, in_c * multiplier]`).
#[allow(clippy::too_many_arguments, reason = "the kernel's operands")]
fn depthwise_conv_2d(
    input: &[i8],
    filter: &[i8],
    bias: &[i32],
    g: &Geometry,
    multiplier: usize,
    requant: &Requant,
    out: &mut Vec<i8>,
) {
    let mut used = Vec::with_capacity(g.k_h * g.k_w);
    for oy in 0..g.out_h {
        for ox in 0..g.out_w {
            positions(g, oy, ox, &mut used);
            for (oc, &b) in bias.iter().enumerate().take(g.out_c) {
                let ic = oc / multiplier;
                let acc: i32 = used
                    .iter()
                    .map(|&(at, tap)| {
                        i32::from(filter[tap * g.out_c + oc])
                            * (i32::from(input[at + ic]) + requant.input_offset)
                    })
                    .sum();
                out.push(requant.apply(acc + b, oc));
            }
        }
    }
}

/// The (input index, kernel index) pairs of one output position that fall inside the input.
fn taps(
    out: usize,
    stride: usize,
    pad: usize,
    dil: usize,
    k: usize,
    size: usize,
) -> impl Iterator<Item = (usize, usize)> {
    (0..k).filter_map(move |kk| {
        let i = (out * stride + kk * dil).checked_sub(pad)?;
        (i < size).then_some((i, kk))
    })
}

/// `reference_integer_ops::Logistic` for one value (`x` already minus the zero point).
#[allow(clippy::cast_possible_truncation, reason = "clamped to int8")]
fn logistic_int8(x: i32, radius: i32, multiplier: i32, shift: i32) -> i8 {
    if x <= -radius {
        return i8::MIN;
    }
    if x >= radius {
        return i8::MAX;
    }
    let q4 = multiply_by_quantized_multiplier(x, multiplier, shift);
    let q23 = rounding_divide_by_pot(logistic_q4(q4), 31 - 8);
    (q23 - 128).clamp(-128, 127) as i8
}

/// `reference_ops::Requantize` for one value; uint8 results are stored as their bit pattern.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to the output type"
)]
fn requantize_one(x: i8, input_zp: i32, output_zp: i32, multiplier: i32, shift: i32, unsigned: bool) -> i8 {
    let v = multiply_by_quantized_multiplier(i32::from(x) - input_zp, multiplier, shift) + output_zp;
    if unsigned {
        (v.clamp(0, 255) as u8).cast_signed()
    } else {
        v.clamp(-128, 127) as i8
    }
}
