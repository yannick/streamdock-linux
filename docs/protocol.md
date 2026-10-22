# StreamDock N3 (USB `6603:1003`) HID protocol

Our dock is sold as "FHOOU Stream Dock" and was assumed to be a 293s. Over USB it identifies as
**`6603:1003` "HOTSPOTEKUSB HID DEMO"**, which the vendor SDK lists as the **StreamDock N3**
(`USB_PID_STREAMDOCK_N3EN` in the Python SDK, `PID_STREAMDOCK_N3V25E` in the C++ SDK), not as a 293s.
The 293s is a 15+3 key device: its key maps, image sizes and PIDs (`5548:6670`, `6603:1014`, `6603:1005`)
do not apply to us.

Status legend: **verified** = observed on our device; **reference** = taken from the vendor SDK or
`mirajazz`, not yet observed; **open** = unknown.

Sources:
- vendor Python SDK `Python-SDK/src/StreamDock/Devices/StreamDockN3.py`, `ProductIDs.py`
- vendor C++ SDK `CPP-SDK/src/HotspotDevice/StreamDockN3V25/streamdockN3V25.cpp`
- Rust crate [`mirajazz` 0.16.2](https://github.com/4ndv/mirajazz) `src/device.rs`, `src/state.rs`
  (the wire framing; the vendor SDKs hide it in the closed `libtransport.so`)
- probe tool `tools/n3probe/n3probe.py` (this repo)

## Hardware layout

| Control                 | Count | Notes                                         |
|-------------------------|-------|-----------------------------------------------|
| LCD keys                | 6     | 2 rows × 3, each with its own 64×64 image     |
| Plain buttons           | 3     | below the LCD keys, no image                  |
| Rotary knobs            | 3     | rotate left/right and press                   |
| Background "touchscreen" | 1    | 320×240 per vendor SDK (open: whether it exists on our unit) |

The "9 buttons" in the original brief are the 6 LCD keys plus 3 plain buttons.

## USB / HID enumeration (verified)

| Item          | Value |
|---------------|-------|
| VID:PID       | `6603:1003` |
| Name          | `HOTSPOTEKUSB HOTSPOTEKUSB HID DEMO` |
| Serial        | 12 hex digits (present, so `mirajazz` doesn't fall back to protocol v0) |
| Firmware      | `V25.293N3_E.01.004` (feature report 1, NUL-padded; see below) |
| Interface 0   | vendor interface: usage page `0xFFA0`, usage `0x01`. **This is the one to use.** |
| Interface 1   | boot keyboard (usage page 1 / usage 6): used when the dock runs without a host app; ignore it |

Interface 0 report descriptor (`06a0ff0901a1010902a10006a1ff0903 0904 15 80 25 7f 35 00 45 ff 75 08 96 0002 81 02 0905 0906 … 96 0004 91 02 c0 c0`):

- **input report**: 512 bytes, no report ID
- **output report**: 1024 bytes, no report ID; on hidraw write `0x00` + 1024 bytes = 1025 bytes
  (matches the vendor `set_report_size(513, 1025, 0)`)
- no feature report is declared, but `HIDIOCGFEATURE` with report ID 1 still returns the firmware string

On Linux the dock appears as two hidraw nodes (here `/dev/hidraw9` = if0, `/dev/hidraw10` = if1).
Access without root needs a udev rule (`udev/70-streamdock.rules`, H15-4).

The firmware string matters: the vendor C++ SDK switches behaviour on it (`V25.N3` → single device, no
background GIF; `V3.N3` → "dual device"). Ours reports `V25.293N3_E…`, i.e. the V2.5 firmware.

## Output commands (host → dock)

Every command is one 1024-byte output report (zero padded) starting with the ASCII prefix
`CRT\0\0` (`43 52 54 00 00`), followed by a 3-letter opcode and arguments.

| Command        | Bytes after `CRT\0\0`                    | Meaning | Status |
|----------------|------------------------------------------|---------|--------|
| `DIS`          | `44 49 53`                               | wake the display / start the session; sent first | verified (part of the working upload) |
| `LIG`          | `4c 49 47 00 00 <pct>`                   | LCD backlight, `pct` 0–100. **Tiny range**: every step is visible, but 0 is not dark and 10 vs 100 differs only slightly | **verified** (H15-31) |
| `CLE`          | `43 4c 45 00 00 00 <key>`                | clear one key (`key` 1–6) or all | sent without error (on open); blanking not checked by eye |
| `CLE` + `DC`   | `43 4c 45 00 00 44 43`                   | clear everything before shutdown | reference |
| `BAT`          | `42 41 54 00 00 <len_hi> <len_lo> <key>` | announce a key image of `len` bytes for `key` (1-based); the JPEG follows in raw reports | verified |
| `STP`          | `53 54 50`                               | commit/flush: show what was uploaded or cleared | verified |
| `CONNECT`      | `43 4f 4e 4e 45 43 54`                   | keep-alive; `mirajazz` callers send it every ~10 s | verified (sent every 10 s, no error) |
| `HAN`          | `48 41 4e`                               | screen off / sleep; input keeps arriving. `DIS` + `LIG` wakes it, key images must be re-sent (see below) | **verified** (H15-24) |
| `MOD`          | `4d 4f 44 00 00 <0x30+mode>`             | device mode; only some models need it | open for N3 |
| `LBLIG`, `SETLB` | `4c 42 4c 49 47 <pct>`, `53 45 54 4c 42 <rgb…>` | knob LED brightness/colors | N3 has no RGB LEDs (vendor: `hasRGBLed = false`) |

The dock sends **no reply** to commands. During a 3 s listen while sending `LIG`, `BAT` and `STP`,
no input report arrived (verified).

### Init sequence

1. open the vendor interface (usage page `0xFFA0`)
2. read the firmware string (feature report 1), optional
3. `DIS`
4. `LIG 00 00 00 00` (`mirajazz` init; it sets brightness 0 until the first real `LIG`)
5. `LIG <brightness>`
6. `CLE … ff` + `STP` to start from a clean screen
7. while connected: `CONNECT` every ~10 s

### Image upload

1. Render the key image at **64×64** and rotate it **90° clockwise** (verified on the device on 2026-10-17:
   only the tiles rotated this way read upright; no mirroring). That is PIL `rotate(270)` / `rotate(-90)`,
   `image` crate `rotate90()`, `mirajazz` `ImageRotation::Rot90`. It matches the vendor Python SDK's −90°.
2. Encode as baseline **JPEG**: the vendor uses quality 95 without chroma subsampling, `mirajazz` quality 90.
3. Send `BAT` with the JPEG length (big-endian u16) and the key number (1–6).
4. Send the JPEG in chunks of 1024 bytes, each as a plain output report (`0x00` report ID + chunk, last one
   zero padded). There is no per-chunk header.
5. Repeat 3–4 for other keys, then send one `STP` to show them all.

A 64×64 JPEG is about 1.7–2 KB, so 2 chunks.

Background image (320×240, vendor `setBackgroundImgDualDevice`): framing is open. It is probably the `LOG` opcode
of other Mirabox devices; not needed for the first driver.

## Input reports (dock → host)

Reads are 512-byte input reports. A valid event starts with ASCII `ACK` (`41 43 4b`):

| Offset | Meaning |
|--------|---------|
| 0–2    | `ACK` |
| 3–8    | constant (open: `00 00 4f 4b 00 00` "OK" in other Mirabox devices) |
| 9      | input code (table below) |
| 10     | state: `0x01` = down, `0x00` = up (vendor C++); the vendor Python SDK says `0x02` = up |

Reports that don't start with `ACK` are ignored.

| Code   | Control                                   | State byte | Status    |
|--------|-------------------------------------------|------------|-----------|
| `0x01`…`0x06` | LCD key 1–6 (top-left → bottom-right) | down/up | **verified** |
| `0x25` | bottom button left (key 7)                | down/up    | **verified** |
| `0x30` | bottom button middle (key 8)              | down/up    | **verified** |
| `0x31` | bottom button right (key 9)               | down/up    | **verified** |
| `0x33` | knob 1 press                              | down/up    | **verified** |
| `0x35` | knob 2 press                              | down/up    | **verified** |
| `0x34` | knob 3 press                              | down/up    | **verified** |
| `0x90` / `0x91` | knob 1 counter-clockwise / clockwise | one report per detent | **verified** |
| `0x50` / `0x51` | knob 2 counter-clockwise / clockwise | one report per detent | **verified** |
| `0x60` / `0x61` | knob 3 counter-clockwise / clockwise | one report per detent | **verified** |

Verified on 2026-10-17 with `cargo run -p streamdock-hid --example probe`:
- every key and button reported down and then up, with presses lasting 150–300 ms
- every knob reported press and release, plus one report per detent in each direction
- no unknown codes appeared
- `…0` (e.g. `0x90`, `0x50`, `0x60`) is counter-clockwise and `…1` clockwise

Physical knob positions, as named in the vendor C++ SDK: knob 1 (`0x90`/`0x33`) is bottom-left, knob 2
(`0x50`/`0x35`) top, knob 3 (`0x60`/`0x34`) bottom-right. The hands-on turn order matched this. The maintainer was
"not 100% sure" of his order, so a re-check with one knob at a time would make it certain.

Rotation is relative: one report per detent, with no counter or velocity.

## Key index mapping

Image keys and input codes use the same numbering for the LCD keys (1–6, direct mapping, vendor `_IMAGE_KEY_MAP`).
`mirajazz` callers pass 0-based keys and the crate adds 1 in `BAT`/`CLE`.

## Can `streamdock-hid` (H15-5) wrap `mirajazz`?

Short answer: **yes, most likely**, with these parameters:

- `DeviceQuery::new(65440, 1, 0x6603, 0x1003)` (same usage page/usage as the 293S variants)
- `Device::connect(dev, protocol_version = 3, key_count = 9, encoder_count = 3)`:
  - protocol ≥ 2 → 1024-byte packets, which matches our 1024-byte output report
  - protocol 3 → reads the state byte, so key release events come through (our firmware reports down and up)
- our own `process_input(code, state)` mapping the table above to `ButtonStateChange` / `EncoderStateChange` /
  `EncoderTwist`
- `ImageFormat { mode: JPEG, size: (64, 64), rotation: Rot90, mirror: None }` (verified)

Of the 293S variants in the brief, `1003` speaks the **v2/v3 framing** (`6603:1014` HSV293SV3 / `6603:1005`
style: `CRT` commands, 1024-byte packets, `ACK` input reports). The 293S v1 (`5548:6670`) uses 512-byte packets.

Trade-offs:
- for wrapping: `mirajazz` already handles framing, chunking, device watching (hotplug) and state diffing, and it
  is used in production by the OpenDeck Mirabox/Ajazz plugins
- against: it pulls in `async-hid` + tokio + `image`, its API is shaped for OpenDeck (0-based keys, flush cache),
  and the background image isn't covered. The whole protocol is roughly 10 commands, so implementing it ourselves
  over `hidapi`/`async-hid` is ~300 lines if we ever need to.

Recommendation: wrap `mirajazz` behind our own `Device` trait, so we can replace it later without touching the daemon.

**Prior art confirming this:** the OpenDeck plugin [`opendeck-akp03`](https://github.com/4ndv/opendeck-akp03) 0.11.0
(`mirajazz` 0.16.2) already lists our dock as `N3_6603_1003` = "Mirabox N3 (6603:1003)" (`src/mappings.rs`):

- `protocol_version() = 3`
- `ImageFormat { JPEG, (64, 64), Rot90, None }`, the same as what we verified
- its `src/inputs.rs` decodes the same code table:
  - keys `1..=6`, buttons `0x25 → 7`, `0x30 → 8`, `0x31 → 9`
  - knob twist `0x90/0x91` (left knob), `0x50/0x51` (top), `0x60/0x61` (right), −1/+1 per report
  - knob press `0x33` (left), `0x35` (top), `0x34` (right)
  - pressed = state byte ≠ 0, i.e. release = `0x00`

We can copy that `process_input` almost one to one (MIT-licensed upstream; check before copying verbatim).

Knob naming: the vendor C++ SDK calls the knobs "bottom-left/bottom-right/top", the plugin "left/right/middle (top)".
Physical positions still need confirming on the device.

## Vendor macOS app, static analysis (H15-9)

Findings from a static read (disassembly only, nothing executed) of the device library in the vendor's macOS app,
version 1.0.25. Function names are the library's own symbols.

### Model identification

- `Utilities::isSoftwareSupportDevice(int vid, int pid)` is a VID/PID whitelist; `6603:1003` is accepted.
- The device is handled by a **model name**, not by PID. The N3 family is one branch in
  `Utilities::getLocationByCoordinates`: `MBox-N3`, `MBox-N3 `,
  `MBox-N3E`, `MBox-N3E `, `N3E`, `N3-R`, `SD12N3V25`, `TS16N3`, `TS16N3V25`, `VSDN3`, `AKP03`, `AKP03E`, `AKP03R`
  (plus unrelated K1Pro/MSD-TWO/… sharing the same layout). Our dock is therefore the same hardware as the
  **Ajazz AKP03**. The app's model list has `MBox-N3V25` / `MBox-N3V25E`, which matches our firmware
  `V25.293N3_E.01.004`.
- Layout for that family, as UI (column, row) → location:
  `(0..2, 0)` → 1–3 and `(0..2, 1)` → 4–6 (LCD keys, row-major 3×2), `(3,0)` → 7, `(3,1)` → 8, `(4,1)` → 9
  (buttons), `(0..2, 5)` → 10–12 (knobs).
- `SDDevice::isOld293Version()` reads an input report (`hid_get_input_report`, report ID from the
  device object) and reports "old 293" when no firmware string comes back. That is the legacy 293 path only; our dock
  answers the firmware query, so it is not "old".

### Commands (`SDDevice` builders)

All are `CRT\0\0` + opcode, matching our table above. Builders that put the HID report ID at byte 0 shift
everything by one.

| Opcode | Builder | Status for us |
|--------|-------------------|---------------|
| `DIS` | `addWakeUpScreenPack` | verified on device |
| `LIG` | `addAdjustBrightnessPack(int,bool)` | verified, same bytes as the vendor SDK (see "Brightness" below) |
| `CLE` | `getClearCommand(uchar)`, `getClearAllCommand` (key `0xff` at offset 11) | same as mirajazz |
| `STP` | `getFinishCommand` | verified. Byte 9 = `0x21/0x22/0x23` only for `StreamDock[321D]`, `H3`, `StreamDock[0108D]`, so **plain `STP` for N3** |
| `BAT` | `sendPicSizeCommand(int,uchar)` | verified |
| `CONNECT` | `addHeartbeatPack` | heartbeat |
| `HAN` | `addScreenOffPack` | screen off (= mirajazz `sleep`) |
| `CLE…DC` | `sendDisconnectCommand` | on app exit |
| `VER` | `sendGetHardwareFirmwareVersion` | firmware query (we use feature report 1 instead) |
| `MOD` | `addModeChangedCommand(uchar)` | device reports its mode back as `CRT…MOD`, `sigGetModeCmd(byte 10)` |
| `ULEND` | `getUploadFinishedCommand` | not in mirajazz; purpose open |
| `QUCMD` | `getQUCMDCommand(5×uchar)` | device settings; not in mirajazz |
| `SET` | `sendChangeDirectionCommand(uchar,uchar)` (args at offsets 11, 12 after a report-ID byte) | screen orientation; not in mirajazz |
| `LOG`, `PIXMAPEND` | `sendLogoSizeCommand`, `sendPixmapEndCommand` | boot logo upload |
| `DRA` | `sendTransparentBackground`, `sendSecondaryScreenPicInfo` | background/secondary screen |
| `BLEF` | `sendBLEFCommand` | open |
| raw `HAN` (no `CRT`) | `addHandshakePack` | only for `StreamDock[296]` |

### Input reports (`SDDevice::readDataFromHidDevice`)

- The first byte is dispatched by a jump table.
- **Key events:** `ACK` + `\0\0` + `OK` + `\0\0` + code (byte 9) + state (byte 10) → `sigHandleKeyEvents(code, state)`
 . That is exactly the layout in
  [Input reports](#input-reports-dock--host).
- **Byte 9 = 0 is not a key:** it is an upload ACK that wakes the send thread (log "收到ACK，唤醒发送数据线程").
  The app paces image uploads on these ACKs. Our device sent none within 3 s of a `BAT`+`STP` upload, so we don't wait
  for them either.
- Other replies: `ACK…CB…OK` (split-packet reply), `ACK…OK…LOG` (logo uploaded), `ACK…TF…` (TF-card capacity), and a
  `CRT…MOD` mode report. A resend request is a report whose first byte is in the jump table and which continues with
  `rror` ("接收到重发命令").
- Some models drop key events less than 200 ms apart while a sleep-intercept flag is set (`TimerTool`).

### Differences from the Python SDK and mirajazz

1. Release state: the vendor library passes the raw state byte through. The C++ SDK expects `0x00` on release, the Python SDK `0x02`.
   **Still to be checked on the device.**
2. Upload ACKs (`ACK` with code 0) exist; mirajazz ignores them by treating code 0 as no input. That is harmless.
3. Extra opcodes we don't need for the driver: `ULEND`, `QUCMD`, `SET`, `LOG`, `PIXMAPEND`, `DRA`, `BLEF`.

### Sleep and wake (verified on the dock, H15-24)

- After `HAN` the screen is dark and input still arrives. `DIS` followed by `LIG <brightness>` turns
  it back on; `streamdockd` then re-uploads the key images, which come back as before.
- **Firmware quirk:** while the screen is off, one physical press is reported as a **stray `KeyUp`
  first**, then the real `KeyDown` about 40 ms later and its `KeyUp` (observed:
  `KeyUp(1)` at t, `KeyDown(1)` at t+40 ms, `KeyUp(1)` at t+110 ms). A host that wakes on the first
  report and forwards the next ones triggers the key's action on the wake press. `streamdockd` ignores
  input for 400 ms after a wake and swallows the release of any press that started in that window.

## Brightness (H15-31)

**The bytes are right.** The transport library of the vendor's Linux (Python) SDK has
`Transport::setKeyBrightness(unsigned char)`. It:

- rejects values above 100 (`cmp $0x64`);
- builds a buffer from the 8-byte template `CRT\0\0LIG` ;
- stores the percent at index 10;
- sends it with `TransportSession::transact`.

That is `43 52 54 00 00 4c 49 47 00 00 <pct>`, byte for byte what `mirajazz` 0.16.2 sends after the report id
(`Device::set_brightness`). The vendor's C++ examples call `setKeyBrightness(100)` with no `STP` or refresh
afterwards.

**The effect is weak.** Test on 2026-10-18 on our dock, with values sent through the
production daemon's API and watched by the maintainer: 100 → 10 → 100 → 0 → 100 → 50 → 80. Every step was visible, but
with "very little difference", and 0 does not turn the backlight off. So `LIG` alone cannot dim the keys usefully
(e.g. at night). Darkening has to come from the key images themselves or from `HAN` (screen off).

## Open questions

1. ~~Key image rotation angle~~: 90° clockwise, verified.
2. ~~Key order~~: `0x01…0x06` = keys 1–6 in order (verified). Knob positions: 1 bottom-left, 2 top, 3 bottom-right (vendor naming; matched the hands-on order, low-risk re-check open).
3. ~~Release events~~: keys, buttons and knob presses all report up (verified). The exact byte (`0x00` or `0x02`) is still unrecorded; the driver treats anything other than `0x01` as up.
4. ~~Effect of `LIG 0`~~: minimum, not off; the screen stays readable (H15-31). Use `HAN` for dark.
5. Does the screen go dark without `CONNECT` keep-alives, and after how long?
6. Background image framing and whether our unit has one.
7. Does brightness persist across power cycles?

## Probe tool

`tools/n3probe/n3probe.py` (pure Python over hidraw, Pillow via `uv run`):

```sh
tools/n3probe/n3probe.py info              # node, descriptor, firmware
tools/n3probe/n3probe.py listen --raw      # decoded input events
tools/n3probe/n3probe.py brightness 60
tools/n3probe/n3probe.py image 1 red --label A   # default --rot 270 (PIL, counter-clockwise) = 90° clockwise
tools/n3probe/n3probe.py pattern           # keys 1-6 numbered, each rotated differently
tools/n3probe/n3probe.py clear             # all keys
tools/n3probe/n3probe.py raw 4c 49 47 00 00 32   # any CRT command
```
