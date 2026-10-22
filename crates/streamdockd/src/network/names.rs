//! Names for Bluetooth numbers (H15-49): profile UUIDs and the class of device, as the
//! Bluetooth SIG's assigned numbers define them; company identifiers, GAP appearance and the
//! device ID (modalias) for the detail dialog (H15-53).

/// The base UUID that 16-bit assigned numbers extend.
const BASE: &str = "-0000-1000-8000-00805f9b34fb";

/// A profile's name, e.g. "A2DP sink"; empty for UUIDs we don't know (vendor ones).
#[must_use]
pub fn profile(uuid: &str) -> &'static str {
    let uuid = uuid.to_ascii_lowercase();
    let Some(short) = uuid.strip_suffix(BASE).and_then(|head| head.strip_prefix("0000")) else {
        return "";
    };
    match short {
        "1101" => "Serial port",
        "1105" => "OBEX object push",
        "1106" => "OBEX file transfer",
        "1108" => "Headset (HSP)",
        "110a" => "A2DP source",
        "110b" => "A2DP sink",
        "110c" => "AVRCP target",
        "110d" => "A2DP",
        "110e" => "AVRCP",
        "110f" => "AVRCP controller",
        "1112" => "Headset audio gateway",
        "1115" => "PAN user",
        "1116" => "PAN access point",
        "111e" => "Hands-free (HFP)",
        "111f" => "Hands-free audio gateway",
        "1124" => "HID",
        "112d" => "SIM access",
        "112f" => "Phonebook access",
        "1132" => "Message access",
        "1200" => "PnP information",
        "1800" => "Generic access",
        "1801" => "Generic attribute",
        "180a" => "Device information",
        "180f" => "Battery",
        "1812" => "HID over GATT",
        "184e" => "LE Audio stream control",
        "184f" => "LE Audio broadcast",
        "1850" => "LE Audio capabilities",
        "fe2c" => "Google Fast Pair",
        _ => "",
    }
}

/// The class of device in words, e.g. "Audio/video: headphones".
#[must_use]
pub fn class(class: u32) -> String {
    let major = (class >> 8) & 0x1f;
    let minor = (class >> 2) & 0x3f;
    let major_name = match major {
        0 => "Miscellaneous",
        1 => "Computer",
        2 => "Phone",
        3 => "Network access point",
        4 => "Audio/video",
        5 => "Peripheral",
        6 => "Imaging",
        7 => "Wearable",
        8 => "Toy",
        9 => "Health",
        _ => "Uncategorized",
    };
    let minor_name = if major == 4 {
        match minor {
            1 => "headset",
            2 => "hands-free",
            4 => "microphone",
            5 => "loudspeaker",
            6 => "headphones",
            7 => "portable audio",
            8 => "car audio",
            9 => "set-top box",
            10 => "HiFi audio",
            14 => "video monitor",
            15 => "video display and loudspeaker",
            18 => "gaming toy",
            _ => "",
        }
    } else {
        ""
    };
    if minor_name.is_empty() {
        major_name.to_owned()
    } else {
        format!("{major_name}: {minor_name}")
    }
}

/// A Bluetooth SIG company identifier's name, for the few vendors of audio gear and phones we are
/// likely to meet; empty for the rest (the dialog shows the number).
#[must_use]
pub fn company(id: u16) -> &'static str {
    match id {
        0x0006 => "Microsoft",
        0x000a => "Qualcomm",
        0x004c => "Apple",
        0x0057 => "Harman",
        0x0075 => "Samsung",
        0x0087 => "Garmin",
        0x009e => "Bose",
        0x00e0 => "Google",
        0x012d => "Sony",
        _ => "",
    }
}

/// A USB vendor id's name (the `usb:` device IDs in modaliases); empty for the rest.
fn usb_vendor(id: u16) -> &'static str {
    match id {
        0x045e => "Microsoft",
        0x04e8 => "Samsung",
        0x054c => "Sony",
        0x05ac => "Apple",
        0x18d1 => "Google",
        0x1d6b => "Linux Foundation",
        _ => "",
    }
}

/// "0x0941 (wearable audio device)": a GAP appearance with its category, where we know it.
#[must_use]
pub fn appearance(value: u16) -> String {
    let category = match value >> 6 {
        0x01 => "phone",
        0x02 => "computer",
        0x03 => "watch",
        0x05 => "display",
        0x06 => "remote control",
        0x0a => "media player",
        0x0f => "HID",
        0x21 => "audio sink",
        0x22 => "audio source",
        0x25 => "wearable audio device",
        _ => "",
    };
    if category.is_empty() {
        format!("{value:#06x}")
    } else {
        format!("{value:#06x} ({category})")
    }
}

/// The device ID from a modalias ("usb:v054Cp0D58d0100", "bluetooth:v012Dp…"): vendor, product
/// and version as the device reports them; empty when it is not one.
#[must_use]
pub fn device_id(modalias: &str) -> String {
    let Some((source, ids)) = modalias.split_once(':') else {
        return String::new();
    };
    let hex = |tag: char, len: usize| {
        let at = ids.find(tag)? + 1;
        u16::from_str_radix(ids.get(at..at + len)?, 16).ok()
    };
    let (Some(vendor), Some(product), Some(version)) = (hex('v', 4), hex('p', 4), hex('d', 4)) else {
        return String::new();
    };
    let (name, kind) = match source {
        "bluetooth" => (company(vendor), "Bluetooth"),
        "usb" => (usb_vendor(vendor), "USB"),
        _ => ("", source),
    };
    let vendor = if name.is_empty() {
        format!("{vendor:04X}")
    } else {
        format!("{name} ({vendor:04X})")
    };
    format!("{vendor}, product {product:04X}, version {version:04X} ({kind} IDs)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles() {
        assert_eq!(profile("0000110B-0000-1000-8000-00805F9B34FB"), "A2DP sink");
        assert_eq!(
            profile("0000111e-0000-1000-8000-00805f9b34fb"),
            "Hands-free (HFP)"
        );
        assert_eq!(profile("81c2e72a-0591-443e-a1ff-05f988593351"), "", "vendor UUID");
    }

    #[test]
    fn classes() {
        // WH-1000XM4.
        assert_eq!(class(0x24_0404), "Audio/video: headset");
        assert_eq!(class(0x24_0418), "Audio/video: headphones");
        assert_eq!(class(0x5a_020c), "Phone");
    }

    #[test]
    fn device_ids_and_appearance() {
        assert_eq!(
            device_id("usb:v054Cp0D58d0100"),
            "Sony (054C), product 0D58, version 0100 (USB IDs)"
        );
        assert_eq!(
            device_id("bluetooth:v012Dp0001d0002"),
            "Sony (012D), product 0001, version 0002 (Bluetooth IDs)"
        );
        assert_eq!(device_id(""), "");
        assert_eq!(device_id("usb:v12"), "");
        assert_eq!(appearance(0x0941), "0x0941 (wearable audio device)");
        assert_eq!(appearance(0x7fff), "0x7fff");
        assert_eq!(company(0x004c), "Apple");
    }
}
