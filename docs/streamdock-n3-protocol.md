# StreamDock N3 (USB `6603:1003`) protocol reference, for ESP32 hosts

This is a self-contained guide to driving the dock from a microcontroller acting as a **USB host**.
It is written for an agent building the lightweight spin-off: an ESP32 hosts the dock and exposes a
minimal web UI plus MQTT. Everything you need to talk to the dock is on this page.

- **Evidence and history:** [`docs/protocol.md`](protocol.md) (vendor SDKs, `mirajazz`, static analysis of
  the vendor macOS library, probe sessions).
- **Working implementation:** `crates/streamdock-hid` (Rust, on top of `mirajazz` 0.16.2), used by `streamdockd`.

Status tags: **[verified]** = observed on our unit; **[reference]** = taken from the vendor SDKs, the vendor
library or `mirajazz`, not yet observed on our unit; **[open]** = unknown, test it on the hardware.

## 1. The device

| Item | Value |
|---|---|
| Sold as | "FHOOU Stream Dock" (assumed to be a 293s; it is not) |
| Actual model | Mirabox **StreamDock N3**, firmware V2.5; same hardware as the Ajazz **AKP03** |
| USB VID:PID | `0x6603:0x1003` [verified] |
| Strings | manufacturer `HOTSPOTEKUSB`, product `HOTSPOTEKUSB HID DEMO`, serial e.g. `0300D0000001` [verified] |
| Firmware | `V25.293N3_E.01.004` [verified] |
| Controls | 6 LCD keys (3×2, 64×64 px each), 3 plain buttons, 3 rotary knobs (turn + press) |
| Power | bus powered, `bMaxPower` 100 mA declared [verified]. Real draw with backlight on is [open]: measure before relying on an ESP32 dev board's VBUS. |

Front layout, as used by the vendor app [reference] and our input mapping [verified]:

```
 ┌──────┬──────┬──────┐
 │ key1 │ key2 │ key3 │      LCD keys 1–6, row-major from the top left
 ├──────┼──────┼──────┤
 │ key4 │ key5 │ key6 │
 └──────┴──────┴──────┘
   btn7   btn8   btn9         plain buttons (no image)
        ( knob 2 )            knob 2: top / middle
 ( knob 1 )      ( knob 3 )   knob 1: bottom-left, knob 3: bottom-right
```

Knob positions follow the vendor SDK naming; the hands-on test matched it, but a re-check with one knob
at a time is still [open] (low risk).

## 2. USB level (what an ESP32 host sees)

Read from the device on 2026-10-18 with `lsusb -v` and raw `GET_DESCRIPTOR` requests [verified]:

| Item | Value |
|---|---|
| USB version | 2.00, **negotiates High Speed (480 Mbit/s)** on a PC |
| `bMaxPacketSize0` | 64 |
| Configurations | 1 (`wTotalLength` 66), bus powered, 100 mA |
| Device qualifier | present (`0a 06 00 02 00 00 00 40 01 00`): the device claims it can run at full speed |
| Other-speed configuration | **request STALLs**: the full-speed endpoint layout is not described [verified] |

**Interface 0: the one to use.** HID class, no subclass/protocol, report descriptor 54 bytes,
usage page `0xFFA0`, usage `0x01`.

| Endpoint | Type | `wMaxPacketSize` (HS) | `bInterval` | Use |
|---|---|---|---|---|
| `0x82` IN | interrupt | 512 | 1 | input reports, 512 bytes |
| `0x03` OUT | interrupt | 1024 | 1 | commands and image data, 1024 bytes |

HID reports on interface 0 [verified from the report descriptor]:
- **input report:** 512 bytes, **no report ID**
- **output report:** 1024 bytes, **no report ID**
- **feature report ID 1:** returns the firmware string (NUL padded), even though the descriptor declares no
  feature report. A host reads it with a `GET_REPORT` control request (feature, ID 1); at least 20 bytes [verified via Linux `HIDIOCGFEATURE`].

> **Report ID byte:** Linux `hidraw`, `hidapi` and `mirajazz` prepend a `0x00` "report ID" byte to every
> output buffer (1025 bytes in total). That byte is **not sent on the wire**. On an ESP32 you send exactly the
> **1024-byte payload** described below as one interrupt OUT transfer on `0x03`.

**Interface 1:** a boot keyboard (endpoint `0x81` IN, 8 bytes, `bInterval` 10). Presumably for a standalone
keyboard mode [assumption; on Linux it never sent anything while we used interface 0]. **Do not claim it**; ignore it.

### 2.1 Choosing the ESP32

USB host support exists on ESP32-S2, ESP32-S3 (USB OTG **full speed only**) and ESP32-P4 (USB OTG **high
speed** + full speed). Classic ESP32, C3 and C6 have no USB host.

| Option | Pros | Risk |
|---|---|---|
| **ESP32-P4** | Talks to the dock exactly like a PC does (high speed, 512/1024-byte packets) | Bigger, newer chip; needs a board with a USB-A host port and VBUS supply |
| **ESP32-S3** | Cheap, common, good Wi-Fi | The dock then runs at **full speed**, which nobody has tested. Its full-speed endpoint layout is unknown (the other-speed descriptor STALLs). Interrupt packets are at most 64 bytes at full speed, so a 1024-byte report becomes 16 packets in one transfer. Some HID firmwares refuse this or misbehave. |

**Recommendation:** the first hardware test of the spin-off is "does the dock enumerate and accept one `DIS` +
`CLE` + `STP` on an ESP32-S3?" If yes, stay on the S3. If not, use the P4. Test with a powered hub as well as
directly, and dump the configuration descriptor the dock reports at full speed.

### 2.2 ESP-IDF notes

- Use the **USB Host Library** (`usb/usb_host.h`): install the host, register a client, open the device
  with VID/PID `6603:1003`, `usb_host_interface_claim(…, 0, 0)` for interface 0 only.
- **Send commands as raw interrupt OUT transfers** on endpoint `0x03` (`usb_host_transfer_alloc(1024, …)`,
  `bEndpointAddress = 0x03`, `num_bytes = 1024`, `usb_host_transfer_submit`). Linux sends output reports the
  same way. The ESP-IDF HID host driver sends output reports as a `SET_REPORT` control request instead, and
  whether this firmware accepts a 1024-byte `SET_REPORT` is [open]. Prefer the raw transfer.
- **Read input** by keeping one interrupt IN transfer of 512 bytes on `0x82` submitted at all times;
  resubmit it in its callback.
- Read the firmware version with a control transfer: `bmRequestType 0xA1`, `bRequest 0x01` (GET_REPORT),
  `wValue 0x0301` (feature, ID 1), `wIndex 0`, `wLength 20` or more. Optional. The reply is the bare string,
  **not** prefixed by a report-ID byte (the descriptor declares no report IDs): byte 0 is the `V` of
  `V25.293N3_E.01.004` [verified; skipping byte 0 as a report ID loses the `V`].
- Handle `USB_HOST_CLIENT_EVENT_DEV_GONE` as unplug; re-run the init sequence on reconnect.

## 3. Output commands (host → dock)

Every command is **one 1024-byte OUT transfer**: an ASCII header, an opcode, arguments, and zeros up to
1024 bytes. Offsets below are into that 1024-byte payload (no report-ID byte).

| Offset | 0 | 1 | 2 | 3 | 4 | 5–… |
|---|---|---|---|---|---|---|
| Value | `0x43` `C` | `0x52` `R` | `0x54` `T` | `0x00` | `0x00` | opcode + arguments |

| Command | Bytes from offset 5 | Arguments | Purpose | Status |
|---|---|---|---|---|
| `DIS` | `44 49 53` | none | wake the display / start the session; send first | [verified] |
| `LIG` | `4c 49 47 00 00 PP` | `PP` @10 = brightness 0–100 | LCD backlight | sent without error; visible effect [open] |
| `CLE` | `43 4c 45 00 00 00 KK` | `KK` @11 = key 1–6, or `0xFF` = all | clear key image(s); needs `STP` to show | sent without error; blank keys not checked by eye [open] |
| `BAT` | `42 41 54 00 00 HH LL KK` | length of the JPEG that follows, big-endian: `HH` @10, `LL` @11; `KK` @12 = key 1–6 | announce a key image | [verified] |
| `STP` | `53 54 50` | none | commit: show everything uploaded or cleared since the last `STP` | [verified] |
| `CONNECT` | `43 4f 4e 4e 45 43 54` | none | keep-alive; send every ~10 s | [verified] (no error) |
| `HAN` | `48 41 4e` | none | screen off / sleep | [reference] |
| `CLE` + `DC` | `43 4c 45 00 00 44 43` | none | "disconnect": clear everything before the host goes away | [reference] |

Opcodes the vendor app has but you don't need: `VER`, `MOD`, `ULEND`, `QUCMD`, `SET` (orientation), `LOG` +
`PIXMAPEND` (boot logo), `DRA` (background), `BLEF`, `LBLIG`/`SETLB` (RGB LEDs, which the N3 doesn't have).
See `protocol.md`.

**The dock sends no reply to commands** [verified: no input report within 3 s of `LIG`, `BAT` and `STP`].
The vendor app waits for an "upload ACK" (an `ACK` input report with code `0x00`) between image chunks, but our
unit sent none within 3 s of an upload in the one session where we logged raw reports, and `mirajazz` doesn't
wait either. Send back-to-back; if uploads ever corrupt, add a
1–2 ms pause between OUT transfers first.

### 3.1 Test vectors

Leading bytes of the 1024-byte payload; everything after them is `0x00`.

```
DIS                     43 52 54 00 00 44 49 53
LIG 80 %                43 52 54 00 00 4c 49 47 00 00 50
LIG 0 (init)            43 52 54 00 00 4c 49 47 00 00 00
CLE all keys            43 52 54 00 00 43 4c 45 00 00 00 ff
CLE key 3               43 52 54 00 00 43 4c 45 00 00 00 03
BAT key 2, 1834 bytes   43 52 54 00 00 42 41 54 00 00 07 2a 02
STP                     43 52 54 00 00 53 54 50
CONNECT                 43 52 54 00 00 43 4f 4e 4e 45 43 54
HAN                     43 52 54 00 00 48 41 4e
CLE DC                  43 52 54 00 00 43 4c 45 00 00 44 43
```

### 3.2 Builder (C)

```c
#define N3_OUT_LEN 1024

// Fills out[1024] with "CRT\0\0" + op + args, zero padded.
static void n3_cmd(uint8_t out[N3_OUT_LEN], const char *op, const uint8_t *args, size_t args_len) {
    memset(out, 0, N3_OUT_LEN);
    memcpy(out, "CRT\0\0", 5);
    size_t op_len = strlen(op);
    memcpy(out + 5, op, op_len);
    if (args_len) memcpy(out + 5 + op_len, args, args_len);
}

// LIG: n3_cmd(buf, "LIG", (uint8_t[]){0, 0, pct}, 3);
// CLE: n3_cmd(buf, "CLE", (uint8_t[]){0, 0, 0, key}, 4);          // key 1..6 or 0xFF
// BAT: n3_cmd(buf, "BAT", (uint8_t[]){0, 0, len >> 8, len & 0xFF, key}, 5);
```

## 4. Sequences

### 4.1 Init (after enumeration and claiming interface 0)

1. *(optional)* read the firmware string (feature report 1)
2. `DIS`
3. `LIG 0` (what `mirajazz` sends; harmless)
4. `LIG <brightness>` (e.g. 80)
5. `CLE 0xFF`, then `STP`: start from a clean screen
6. start the input reader (§5) and a 10 s keep-alive timer sending `CONNECT`

What happens when keep-alives stop (does the screen go dark, after how long?) is [open]. Send them anyway.

### 4.2 Setting a key image

1. Produce a **64×64 baseline JPEG**, already **rotated 90° clockwise** relative to how it should look on the
   key [verified: only tiles rotated this way read upright; no mirroring]. Quality 90 works (the vendor uses 95
   without chroma subsampling). A 64×64 JPEG is ~1.7–2 KB. Keep it **≤ 65535 bytes** (16-bit length); in
   practice it always fits in 2 chunks.
2. `BAT` with the JPEG length and the key number (1–6).
3. Send the JPEG in **1024-byte chunks**, each as its own OUT transfer, the last one zero padded. There is no
   per-chunk header [verified].
4. Repeat 2–3 for other keys, then one `STP` to show all of them at once.

On an ESP32, **don't encode JPEGs at runtime** unless you have to. Render the icons offline (the build or the
web UI can do the rotation and encoding) and store the finished JPEG bytes in flash or LittleFS, then upload
them unchanged. If you need dynamic images (status text, values), ESP-IDF's `esp_new_jpeg` encoder can encode
a 64×64 RGB888 buffer; rotate the buffer before encoding.

Clearing one key: `CLE <key>` + `STP`.

### 4.3 Shutdown (host going away)

`CLE DC`, then `HAN` [reference: what `mirajazz` and the vendor app send].

## 5. Input reports (dock → host)

Each IN transfer on `0x82` delivers one **512-byte** report. Only the first 11 bytes matter:

| Offset | Value | Meaning |
|---|---|---|
| 0–2 | `41 43 4b` (`ACK`) | event marker. **Ignore reports that don't start with `ACK`.** [verified] |
| 3–8 | `00 00 4f 4b 00 00` (`\0\0OK\0\0`) | constant [verified: `streamdock-hid` requires this exact header and parsed every event]. Check `OK` at 5–6: other `ACK` replies (`ACK…CB…OK`, `ACK…OK…LOG`, `ACK…TF…`) use different bytes here [reference: vendor library] |
| 9 | code | which control (table below). **`0x00` = not an input** (upload ACK); ignore [reference] |
| 10 | state | `0x01` = pressed; anything else = released [verified: releases arrive; the exact release byte is `0x00` per the vendor C++ SDK and `0x02` per the Python SDK, so treat every value ≠ `0x01` as "up"] |

Codes [all verified on our unit with every control, no unknown codes seen]:

| Code | Control | Event |
|---|---|---|
| `0x01` … `0x06` | LCD key 1–6 | down (`state = 1`) / up |
| `0x25` | button 7 (bottom left) | down / up |
| `0x30` | button 8 (bottom middle) | down / up |
| `0x31` | button 9 (bottom right) | down / up |
| `0x33` | knob 1 press (bottom left) | down / up |
| `0x35` | knob 2 press (top) | down / up |
| `0x34` | knob 3 press (bottom right) | down / up |
| `0x90` / `0x91` | knob 1 turn | counter-clockwise / clockwise, one report per detent |
| `0x50` / `0x51` | knob 2 turn | counter-clockwise / clockwise, one report per detent |
| `0x60` / `0x61` | knob 3 turn | counter-clockwise / clockwise, one report per detent |

Notes:
- Turns are **relative**: one report per detent, no counter, no velocity. Fast turns arrive as several reports;
  aggregate them yourself (e.g. sum the deltas over 50 ms before publishing).
- Turn reports carry a state byte too; ignore it.
- Presses last 150–300 ms between down and up in normal use [verified].
- Knob 2 and knob 3 press codes are **not** in numeric order (`0x35` is knob 2, `0x34` is knob 3).

Example reports (first 11 bytes, rest zeros):

```
key 4 down        41 43 4b 00 00 4f 4b 00 00 04 01
key 4 up          41 43 4b 00 00 4f 4b 00 00 04 00   (release byte 00 per vendor C++ SDK; may be 02)
knob 2 clockwise  41 43 4b 00 00 4f 4b 00 00 51 ??   (state byte not recorded; vendor C++ SDK expects 00; ignore it)
```

### 5.1 Parser (C)

```c
typedef enum { N3_NONE, N3_KEY, N3_KNOB_PRESS, N3_KNOB_TURN } n3_kind_t;
typedef struct { n3_kind_t kind; uint8_t index; int8_t value; } n3_event_t;
// N3_KEY:        index 1..9 (1-6 LCD keys, 7-9 buttons), value 1 = down, 0 = up
// N3_KNOB_PRESS: index 1..3, value 1 = down, 0 = up
// N3_KNOB_TURN:  index 1..3, value -1 = counter-clockwise, +1 = clockwise

static n3_event_t n3_parse(const uint8_t *r, size_t len) {
    n3_event_t e = { N3_NONE, 0, 0 };
    if (len < 11 || r[0] != 'A' || r[1] != 'C' || r[2] != 'K') return e;
    if (r[5] != 'O' || r[6] != 'K') return e;   // other ACK replies (split-packet, logo, TF card)
    uint8_t code = r[9];
    int8_t down = (r[10] == 0x01);
    switch (code) {
    case 0x01: case 0x02: case 0x03: case 0x04: case 0x05: case 0x06:
        return (n3_event_t){ N3_KEY, code, down };
    case 0x25: return (n3_event_t){ N3_KEY, 7, down };
    case 0x30: return (n3_event_t){ N3_KEY, 8, down };
    case 0x31: return (n3_event_t){ N3_KEY, 9, down };
    case 0x33: return (n3_event_t){ N3_KNOB_PRESS, 1, down };
    case 0x35: return (n3_event_t){ N3_KNOB_PRESS, 2, down };
    case 0x34: return (n3_event_t){ N3_KNOB_PRESS, 3, down };
    case 0x90: return (n3_event_t){ N3_KNOB_TURN, 1, -1 };
    case 0x91: return (n3_event_t){ N3_KNOB_TURN, 1, +1 };
    case 0x50: return (n3_event_t){ N3_KNOB_TURN, 2, -1 };
    case 0x51: return (n3_event_t){ N3_KNOB_TURN, 2, +1 };
    case 0x60: return (n3_event_t){ N3_KNOB_TURN, 3, -1 };
    case 0x61: return (n3_event_t){ N3_KNOB_TURN, 3, +1 };
    default:   return e;   // 0x00 = upload ACK, anything else unknown
    }
}
```

## 6. Suggested MQTT mapping for the spin-off (proposal, not fixed)

`<id>` = the dock's USB serial number (stable per unit).

| Topic | Direction | Payload |
|---|---|---|
| `streamdock/<id>/status` | dock → broker, retained | `online` / `offline` (MQTT last will) |
| `streamdock/<id>/key/<1-9>` | dock → broker | `down` / `up` |
| `streamdock/<id>/knob/<1-3>/press` | dock → broker | `down` / `up` |
| `streamdock/<id>/knob/<1-3>/turn` | dock → broker | signed detents, e.g. `+1`, `-3` (aggregated) |
| `streamdock/<id>/set/brightness` | broker → dock | `0`–`100` |
| `streamdock/<id>/set/key/<1-6>/image` | broker → dock | raw JPEG bytes, already 64×64 and rotated |
| `streamdock/<id>/set/key/<1-6>/clear` | broker → dock | anything |

Home Assistant MQTT discovery can announce keys as `event` entities and the brightness as a `number`.

## 7. Open questions to settle on the hardware

1. Does the dock work at **full speed** behind an ESP32-S3 (endpoint sizes, 1024-byte reports in 64-byte packets)?
2. Is a 1024-byte `SET_REPORT` control transfer accepted as an alternative to interrupt OUT?
3. Real current draw with the backlight at 100 %.
4. Visible effect of `LIG 0`: backlight off, or minimum?
5. Does the screen go dark without `CONNECT`, and after how long?
6. Does the brightness persist across power cycles?
7. The exact release byte (`0x00` or `0x02`); harmless as long as you treat "≠ 1" as up.
8. Knob positions, checked one knob at a time.
9. Does `CLE` + `STP` visibly blank the keys? (Sent without error; nobody has looked.)

## 8. Minimal bring-up checklist

1. Enumerate `6603:1003`, claim interface 0, find endpoints `0x82` (IN) and `0x03` (OUT).
2. Send `DIS`, `LIG 80`, `CLE FF`, `STP`: the keys should go blank and lit (blanking not yet checked by eye).
3. Submit a 512-byte IN transfer; press key 1 and expect `41 43 4b … 01 01`, then `… 01 00`.
4. Upload a pre-rendered 64×64 JPEG to key 1 (`BAT`, 2 chunks, `STP`); it should appear upright.
5. Add the 10 s `CONNECT` timer and unplug/replug handling.
