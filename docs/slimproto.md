# Slimproto: Lyrion Music Server player protocol (client side)

Specification of the protocol a **player** speaks with Lyrion Music Server (LMS, formerly Logitech Media Server /
Squeezebox Server), written for H15-47: a built-in Lyrion player inside `streamdockd` (Rust) that announces itself,
streams audio and reports its state.

Status legend: **reference** = read in a reference player's source (cited); **server** = behaviour of LMS itself,
known from the server code or community docs but not in our sources, still to be confirmed with a capture;
**open** = unknown.

Sources (read-only, GPL: this document describes the protocol in our own words, no code is copied):

| Short | Path under `streamdock/external-repos/` | What |
|-------|------------------------------------------|------|
| `SL`  | `squeezelite/` (ralph-irving, v2.0.0-1610) | reference C player; `slimproto.c`, `slimproto.h`, `stream.c`, `decode.c`, `output.c`, `pcm.c`, `utils.c` |
| `R2`  | `squeezelite-R2/` | older fork; only differences are noted |
| `LP`  | `LyrPlay/LMS_StreamTest/` | Swift iOS player; `SlimProtoClient.swift`, `SlimProtoCommandHandler.swift`, `ServerDiscoveryManager.swift`, `SlimProtoCoordinator.swift`, `SettingsManager.swift` |

Citations are `file:line` relative to those roots, e.g. `SL slimproto.c:357`.

## Overview

```
player                                   LMS
  │  UDP broadcast "e…" → :3483  ───────────▶  (discovery)
  │  ◀─────────────── "E" + TLVs  (unicast reply; sender address = server)
  │
  │  TCP connect :3483 ─────────────────────▶  slimproto control connection
  │  HELO ──────────────────────────────────▶
  │  ◀── setd / aude / audg / strm q / strm t …
  │  ◀── strm s (HTTP request to send)
  │  STAT STMf, STMc ───────────────────────▶
  │  TCP connect server:9000 (or a remote host), GET … ─▶  audio over HTTP
  │  RESP (response headers), STAT STMl/STMs/STMt/STMd/STMu … ─▶
```

Two connections per player: the long-lived **slimproto** TCP connection (commands and status) and a short-lived
**HTTP** connection per track (audio data). Everything multi-byte on the slimproto connection is **big-endian**
(network order), except where noted.

## 1. Discovery (UDP 3483)

### squeezelite's minimal discovery (reference)

Only used when no server is given (`-s`) at startup (`SL slimproto.c:844-850`):

1. Open a UDP socket with `SO_BROADCAST` (`SL slimproto.c:783-786`).
2. Send the single byte `e` (0x65) to `255.255.255.255:3483` (`SL slimproto.c:788-793,803`).
3. Wait up to 5 s for any datagram (`SL slimproto.c:807`). The content is **ignored** (it reads at most 10 bytes,
   `SL slimproto.c:808-810`); the **sender's IP address is the server**. The slimproto port is assumed to be 3483.
4. Repeat until an answer arrives (`SL slimproto.c:798-818`). If a server name was given but did not resolve, it is
   retried on every loop (`SL slimproto.c:814-816`).

Rediscovery also happens after more than 5 failed TCP connects when no server was configured
(`SL slimproto.c:932-935`).

### TLV discovery (reference: LyrPlay; tag meanings partly server)

Request: `e` followed by TLVs naming the fields wanted. Each request TLV is a 4-byte ASCII tag plus a 1-byte length
`0` (no value). LyrPlay sends `e NAME\0 JSON\0` (11 bytes) to every interface's broadcast address and to
255.255.255.255 (`LP ServerDiscoveryManager.swift:97-98,139-146,215-260`).

Reply: first byte `E` (0x45), then TLVs until the end of the datagram (`LP ServerDiscoveryManager.swift:177,268-310`):

| Offset | Size | Field |
|--------|------|-------|
| 0      | 1    | `E` |
| 1      | 4    | tag (ASCII) |
| 5      | 1    | value length *n* (0-255) |
| 6      | *n*  | value (UTF-8 text, not NUL terminated) |
| …      |      | next TLV |

| Tag    | Value | Status |
|--------|-------|--------|
| `NAME` | server name | reference (`LP ServerDiscoveryManager.swift:297-299`) |
| `JSON` | HTTP / JSON-RPC port as decimal text, e.g. `9000` | reference (`LP …:300-304`) |
| `IPAD` | server IP address as text | server |
| `UUID` | server UUID (36-char text) | server |
| `VERS` | server version, e.g. `9.0.2` | server |
| `JVID` | used by Jive controllers (device id) | server, ignore |

Rules: ask only for what you need, ignore unknown tags, take the server address from the datagram source (not
`IPAD`), and assume slimproto port 3483 (no TLV carries it). Listen ~2-5 s per round and collect several servers.
Older hardware used a `d` request / `D` reply with a fixed layout: **server**, not needed.

## 2. Connection and framing (TCP 3483)

### Connect

- TCP to `server:3483` (default `PORT 3483`, `SL slimproto.c:32,852-854`), non-blocking with a 5 s connect timeout
  (`SL slimproto.c:917-922`).
- On success send **HELO** immediately (`SL slimproto.c:964`), then run the receive loop.
- squeezelite adds the capability `loc` when its own address equals the server's (same host; LocalPlayer plugin)
  (`SL slimproto.c:947-955`). Not needed for us.

### Client → server frame

| Offset | Size | Field |
|--------|------|-------|
| 0      | 4    | opcode, ASCII (`HELO`, `STAT`, `RESP`, `META`, `DSCO`, `SETD`, `IR  `, …) |
| 4      | 4    | payload length *n*, u32 BE, **excludes** these 8 header bytes |
| 8      | *n*  | payload |

(`SL slimproto.h:31-104`; every sender sets `length = sizeof(pkt) - 8 [+ data]`, e.g. `SL slimproto.c:140,177,211,224`.)

### Server → client frame

| Offset | Size | Field |
|--------|------|-------|
| 0      | 2    | length *n*, u16 BE, **includes** the 4-byte opcode |
| 2      | 4    | opcode, ASCII (`strm`, `audg`, `aude`, `setd`, `serv`, `cont`, `codc`, `vers`, …) |
| 6      | *n*-4 | body |

squeezelite reads 2 bytes, then exactly *n* bytes into a buffer that starts with the opcode, and dispatches on the
first 4 bytes (`SL slimproto.c:563-600,528-539`). The handlers' `len` therefore counts the opcode: `setd` with
`len == 5` is "opcode + id only" (`SL slimproto.c:455`). All struct offsets below are given **from the opcode**
(as in `slimproto.h`) and, in a second column, **from the body** (opcode stripped, as LyrPlay indexes).

Limits: squeezelite treats *n* > 4096 as fatal and drops the connection (`SL slimproto.c:34,591-594`); LyrPlay caps at
10000 and discards the payload to stay aligned, and ignores *n* = 0 (`LP SlimProtoClient.swift:41-60`). We should
accept up to 65535 and skip unknown opcodes (squeezelite logs "unhandled" and continues, `SL slimproto.c:536-537`).

### Liveness and reconnect

- The server polls each player with `strm t` about every 5 s (**server**, `Slimproto.pm check_all_clients`, as noted
  in `LP SlimProtoCoordinator.swift:198-200`). squeezelite drops the connection after 35 consecutive 1 s waits
  without any server data (`SL slimproto.c:559,609-613`).
- After the connection ends, squeezelite waits 100 ms, reconnects and sends HELO with the **reconnect flag** set
  (`SL slimproto.c:966-972`, flag in §3). Audio already buffered keeps playing; the stream/decoder state is kept.
- Connect failure: sleep 5 s and retry (`SL slimproto.c:927-930`); after more than 5 failures without a configured
  server, rediscover (`SL slimproto.c:932-935`).
- After `serv` (switch server), connect to the new IP; if that fails, revert to the previous server
  (`SL slimproto.c:909-915,924-926`).
- On reconnect LMS restarts a player that was playing by itself (re-sends `strm s` and seeks): do not also restart
  playback from the client side (`LP SlimProtoCoordinator.swift:183-189`).

## 3. HELO (client → server)

Sent once per TCP connection, first thing (`SL slimproto.c:122-156`, struct `SL slimproto.h:31-42`).

| Offset (payload) | Size | Field | Value |
|------------------|------|-------|-------|
| 0  | 1  | device id | `12` (squeezeplay; LMS + `Model=squeezelite` → squeezelite player class) (`SL slimproto.c:141`, `LP SlimProtoClient.swift:310`) |
| 1  | 1  | firmware revision | `0` (`SL slimproto.c:142`) |
| 2  | 6  | MAC address | player id in LMS; must be stable and unique (`SL slimproto.c:146`) |
| 8  | 16 | UUID | all zero in both reference players (`SL slimproto.c:138`, `LP SlimProtoClient.swift:330-331`) |
| 24 | 2  | wlan channel list, u16 BE | `0x4000` if this is a **reconnect**, else `0x0000` (`SL slimproto.c:143`) |
| 26 | 4  | bytes received, high 32 bits, BE | bytes of the current stream so far (`SL slimproto.c:144`) |
| 30 | 4  | bytes received, low 32 bits, BE | (`SL slimproto.c:145`) |
| 34 | 2  | language | squeezelite: `00 00`; LyrPlay: `en` (`LP SlimProtoClient.swift:341-342`) |
| 36 | …  | capabilities | ASCII, comma separated, no terminator; length implied by the frame length |

The reconnect bit plus bytes received let the server resume the running stream instead of restarting it
(**server**). Other device ids (**server**): 2 Squeezebox, 4 Squeezebox2, 5 Transporter, 7 Receiver, 9 Controller,
10 Boom, 12 SqueezePlay.

The MAC comes from `-m` or the first interface (`SL utils.c:136-283`, `main.c:813-827`). For us: derive a stable,
locally administered MAC (bit 0x02 of the first byte set) from the machine id, so we don't collide with a real
squeezelite on the same host, and persist it.

### Capabilities string

squeezelite builds `[CanHTTPS=1,]` + base + fixed + variable parts (`SL slimproto.c:123-136,883-895,944-962`), e.g.

```
CanHTTPS=1,Model=squeezelite,AccuratePlayPoints=1,HasDigitalOut=1,HasPolarityInversion=1,Balance=1,
Firmware=2.0.0-1610,ModelName=SqueezeLite,MaxSampleRate=192000,aac,ogg,flc,aif,pcm,mp3
```

| Key | Meaning | Source |
|-----|---------|--------|
| `Model=` | player model; `squeezelite` gets LMS's squeezelite player class (LyrPlay sends `LyrPlay`) | `SL slimproto.c:123`, `LP SettingsManager.swift:296-298` |
| `ModelName=` | display name of the model (`-M`, default `SqueezeLite`) | `SL slimproto.c:883`, `SL squeezelite.h:38-44` |
| `MaxSampleRate=` | highest rate the output accepts; LMS resamples/transcodes above it | `SL slimproto.c:883-888` |
| codec list | the `types` of every registered decoder: `pcm`, `aif`, `wav`, `flc`, `ogf`, `mp3`, `aac`, `ogg`, `ops`, `alc`, `wma`, `wmap`, `wmal`, `dsf`, `dff` (see §7) | `SL slimproto.c:890-895` |
| `AccuratePlayPoints=1` | the elapsed time in STAT is exact; the server trusts it | `SL slimproto.c:123` |
| `HasDigitalOut=1` | enables the digital output settings | `SL slimproto.c:123` |
| `HasPolarityInversion=1` | enables the polarity setting (strm flags 0x03) | `SL slimproto.c:123` |
| `Balance=1` | server sends different L/R gains for balance | `SL slimproto.c:123` (absent in `R2 slimproto.c:111`) |
| `Firmware=` | client version string | `SL slimproto.c:123` |
| `CanHTTPS=1` | client can fetch `https://` streams itself (direct streaming of HTTPS radio/services) | `SL slimproto.c:124-136` |
| `loc` | same host as the server (LocalPlayer plugin, file path instead of HTTP) | `SL slimproto.c:952-955` |
| `SyncgroupID=NNNNNNNNNN` | sync group handed over by `serv`, sent to the next server once | `SL slimproto.c:480-510,957-962`, `LP SlimProtoClient.swift:348-354` |

The player name is **not** part of HELO: it is sent with `SETD` (§5) when the server asks.

## 4. Server → client commands

Unknown opcodes must be ignored. Handled by squeezelite: `strm`, `cont`, `codc`, `aude`, `audg`, `setd`, `serv`
(`SL slimproto.c:517-526`).

### `strm`: stream control

Struct `SL slimproto.h:107-128` (28 bytes incl. opcode, then the HTTP request).

| Opcode off. | Body off. | Size | Field | Meaning |
|------|------|---|-------|---------|
| 0  | –  | 4 | `strm` | |
| 4  | 0  | 1 | command | `s` start, `p` pause, `u` unpause, `q` stop, `t` status, `f` flush, `a` skip ahead |
| 5  | 1  | 1 | autostart | ASCII `0`-`3`, see below |
| 6  | 2  | 1 | format | ASCII codec id (§7); `?` = unknown, decided later by `codc` |
| 7  | 3  | 1 | pcm sample size | ASCII `0`=8, `1`=16, `2`=24, `3`=32 bit; `?` = from header (`SL pcm.c:441`); for format `f`, `o` = Ogg FLAC (`SL slimproto.c:377`) |
| 8  | 4  | 1 | pcm sample rate | ASCII index into the rate table below (`SL pcm.c:65-67,442`) |
| 9  | 5  | 1 | pcm channels | ASCII `1` mono, `2` stereo (`SL pcm.c:443`) |
| 10 | 6  | 1 | pcm endianness | ASCII `0` big, `1` little (`SL pcm.c:444`) |
| 11 | 7  | 1 | threshold | KiB of compressed data to buffer before the decoder may start (`SL slimproto.c:373,378`) |
| 12 | 8  | 1 | spdif enable | `0` auto, `1` on, `2` off (**server**); ignored by squeezelite |
| 13 | 9  | 1 | transition period | seconds for fades/crossfade (`SL slimproto.c:386`) |
| 14 | 10 | 1 | transition type | ASCII `0` none, `1` crossfade, `2` fade in, `3` fade out, `4` fade in+out (`SL slimproto.c:385`, `SL squeezelite.h:640`) |
| 15 | 11 | 1 | flags | bit field, below |
| 16 | 12 | 1 | output threshold | output buffer to fill before playback starts, in **tenths of a second** (`SL slimproto.c:383`, `SL output.c:63`, `SL decode.c:266-270`) |
| 17 | 13 | 1 | slaves | sync slave count; ignored |
| 18 | 14 | 4 | replay gain / interval / jiffies, u32 BE | `s`: replay gain 16.16 (0 = none); `p`/`a`: interval in ms; `u`: start time in jiffies; `t`: server timestamp to echo |
| 22 | 18 | 2 | server port, u16 BE | HTTP port of the stream source |
| 24 | 20 | 4 | server IP, u32 BE | IPv4 of the stream source; `0` = the slimproto server itself (`SL slimproto.c:348-350`) |
| 28 | 24 | … | HTTP request | the complete request to send verbatim (only meaningful with `s`) |

For every command other than `s`, the format and pcm bytes are fillers (LMS packs `m`, `?` etc.): never validate them
(`LP SlimProtoCommandHandler.swift:163-168,201-205`).

PCM rate table (`SL pcm.c:65-67`, byte = `'0'` + index):

| Byte | `0` | `1` | `2` | `3` | `4` | `5` | `6` | `7` | `8` | `9` | `:` | `;` | `<` | `=` | `>` | `?` |
|------|-----|-----|-----|-----|-----|-----|-----|-----|-----|-----|-----|-----|-----|-----|-----|-----|
| Hz | 11025 | 22050 | 32000 | 44100 | 48000 | 8000 | 12000 | 16000 | 24000 | 96000 | 88200 | 176400 | 192000 | 352800 | 384000 | from header |

(`@`, `A`, `B`, `C` continue with 705600, 768000, 1411200, 1536000.)

Flags byte:

| Bit(s) | Meaning | Source |
|--------|---------|--------|
| 0x80 | loop the stream forever | **server**, ignored by squeezelite |
| 0x40 | don't restart the decoder | **server**, ignored by squeezelite |
| 0x20 | use TLS for the stream connection | `SL slimproto.c:376` |
| 0x0c | output channels: `>>2` = 1 left only, 2 right only (mono mix) | `SL slimproto.c:388`, `SL squeezelite.h:642-643` |
| 0x03 | invert polarity (squeezelite inverts only when both bits are set) | `SL slimproto.c:387` |

Autostart (`SL slimproto.c:355,664-682`, `SL slimproto.c:405-414`, `LP SlimProtoCoordinator.swift:1677-1685`):

| Byte | Start playback | Wait for `cont` before buffering |
|------|----------------|----------------------------------|
| `0` | no: buffer, send `STMl`, wait for `strm u` (sync groups, fade-in) | no |
| `1` | yes, as soon as the output threshold is met | no |
| `2` | no (like `0`) | yes |
| `3` | yes (like `1`) | yes |

`2`/`3` are used for **direct streams** (the player fetches a remote URL itself): the server first wants the response
headers (`RESP`) to parse them and answers with `cont` (which carries the ICY meta interval) (**server**;
client side `SL stream.c:637`, `SL slimproto.c:405-414`).

#### `strm s` handling (start a track)

In this order (`SL slimproto.c:344-391`):

1. Send `STAT STMf` (`SL slimproto.c:357`).
2. If the HTTP request exceeds 4095 bytes, stop here (`SL slimproto.c:358-361`).
3. Open the decoder for the format (`SL slimproto.c:362-370`). `?` is only allowed with autostart ≥ 2 (the server
   sends `codc` after it has seen the headers).
4. Connect to `ip:port` (IP 0 → slimproto server) and queue the HTTP request (`SL slimproto.c:376-378`,
   `SL stream.c:884-954`). squeezelite connects synchronously (10 s timeout, `SL stream.c:249`).
5. Send `STAT STMc` (`SL slimproto.c:380`), even if the connect failed; a failure shows up as `DSCO` reason 3 on the
   next status cycle (`SL stream.c:906-911`).
6. Reset the per-stream "sent once" flags for `STMu`, `STMo`, `STMl` (`SL slimproto.c:381`).
7. Store output threshold, **next** replay gain, fade mode/period, polarity and channel flags (`SL slimproto.c:382-390`).
   They take effect when this track's first sample reaches the output (gapless, §6).

Note: `strm s` does **not** flush the output buffer. The previous track keeps playing from the output buffer while the
new one streams and decodes behind it (that is how gapless works). A hard stop is `q`, a user skip arrives as `q` (or
`f`) followed by `s`.

#### Other `strm` commands

| Cmd | Action | Reply | Source |
|-----|--------|-------|--------|
| `t` | status request | `STAT STMt` echoing the 4 bytes at opcode offset 18 **unchanged** into `server_timestamp` | `SL slimproto.c:284-286,192` |
| `q` | stop: flush decoder and the whole output buffer, reset elapsed, close the stream, flush stream buffer | `STAT STMf` (always) | `SL slimproto.c:287-294` |
| `f` | flush: flush decoder; drop the output buffer from the start of the **not yet playing** track (the playing track continues); close the stream | `STAT STMf` only if a stream was open or a pending track was dropped | `SL slimproto.c:295-305`, `SL output.c:455-466` |
| `p` | pause. Interval 0: stop output now (keep buffers). Interval *n* > 0: play *n* ms of silence, then continue (sync adjustment) | `STAT STMp` only for interval 0 | `SL slimproto.c:306-321`, `SL output.c:85-96` |
| `u` | unpause. Jiffies 0: resume now. Jiffies *j*: output silence until local clock reaches *j*, then play (ignored if *j* is more than 10 s ahead) | `STAT STMr` | `SL slimproto.c:332-343`, `SL output.c:98-109` |
| `a` | skip ahead: drop *n* ms of decoded audio from the output buffer without playing it (sync catch-up) | none | `SL slimproto.c:322-331`, `SL output.c:69-83` |

R2 difference: its `f` flushes the whole output buffer like `q` (`R2 slimproto.c:270-278`). LyrPlay additionally
answers `STAT STMt` to `audg`, `aude`, `vers`, `grfe`, `grfb` and `STMp`/`STMt` to `vfdc` (`LP
SlimProtoCommandHandler.swift:48-69`); squeezelite sends nothing for these. We follow squeezelite.

### `audg`: volume

Struct `SL slimproto.h:137-147` (server packs it as `NNCCNN` plus optional sequence data, `LP
SlimProtoCommandHandler.swift:625-631`).

| Opcode off. | Body off. | Size | Field |
|------|------|---|-------|
| 4  | 0  | 4 | old gain left, u32 BE: legacy 0-128 scale; ignore |
| 8  | 4  | 4 | old gain right; ignore |
| 12 | 8  | 1 | adjust (digital volume control): 1 = apply the gains, 0 = fixed volume |
| 13 | 9  | 1 | preamp; ignore |
| 14 | 10 | 4 | new gain left, u32 BE, **16.16 fixed point** (0x10000 = unity = 0 dB) |
| 18 | 14 | 4 | new gain right, 16.16 |
| 22 | 18 | … | sequence id etc.; ignore |

Use the **new** gains. If `adjust == 0`, play at unity (`FIXED_ONE`) (`SL slimproto.c:440-448`, `SL squeezelite.h:448`).
Volume 0 arrives as gain 0 (mute). Left and right differ when balance is set (`Balance=1`). squeezelite either scales
samples (`gain * sample >> 16` with clipping, `SL output_pack.c:33-38`) or, with a hardware mixer, converts to dB:
`20·log10(gain/65536)` (`SL output_alsa.c:229-251`). The gain curve itself (volume % → dB) is computed by the server.

### `aude`: enable outputs

| Opcode off. | Body off. | Size | Field |
|---|---|---|---|
| 4 | 0 | 1 | enable S/PDIF |
| 5 | 1 | 1 | enable DAC |

squeezelite treats `enable_spdif == 0` as "player off": output state OFF (device may close); non-zero powers the output
back on (`SL slimproto.c:424-438`). The server sends it on power on/off (**server**). For us: power off → stop output,
keep the connection.

### `setd`: player settings

| Opcode off. | Body off. | Size | Field |
|---|---|---|---|
| 4 | 0 | 1 | setting id |
| 5 | 1 | … | data (absent = query) |

Id `0` = player name (`SL slimproto.c:450-478`, `LP SlimProtoCommandHandler.swift:74-110`):

- **Query** (no data, frame length 5): reply `SETD` id 0 with our name, if we have one (`SL slimproto.c:455-458`).
- **Set** (data = NUL-terminated name): store it (persist it, squeezelite writes `-N` file), then confirm by sending
  the same `SETD` back (`SL slimproto.c:459-476`).

Other ids (digital output, disable DAC, … for hardware models) are ignored by squeezelite.

### `serv`: switch server

| Opcode off. | Body off. | Size | Field |
|---|---|---|---|
| 4 | 0 | 4  | new server IPv4, BE; `0.0.0.1` = mysqueezebox.com (defunct) |
| 8 | 4 | 10 | optional: sync group id, 10 ASCII digits |

Close the connection and connect to the new server; if the 10-byte group id is present, add
`,SyncgroupID=<id>` to the next HELO's capabilities (`SL slimproto.c:483-510,909-915,957-962`; layout `pack 'NA10'`
per `LP SlimProtoCoordinator.swift:2040-2047`).

### `cont`: continue a direct stream

| Opcode off. | Body off. | Size | Field |
|---|---|---|---|
| 4 | 0 | 4 | ICY meta interval (bytes of audio between metadata blocks), u32 BE; 0 = none |
| 8 | 4 | 1 | loop |
| 9 | 5 | … | GUIDs (WMA); ignore |

Only acted on while autostart ≥ 2: subtracts 2 from autostart, moves the stream from "waiting" to "buffering" and arms
ICY parsing (`SL slimproto.c:399-415`).

### `codc`: codec decided after headers (squeezelite extension)

Body: format, sample size, rate, channels, endianness (one byte each, same encoding as `strm`) (`SL slimproto.h:171-179`,
`SL slimproto.c:417-422`). Used after `strm s` with format `?`.

### Ignorable server messages

`vers` (server version string), `vfdc` (display), `grfe`/`grfb`/`grfd`/`grfs` (graphics, brightness, scrolling),
`visu` (visualizer), `ledc`, `i2cc`, `bdac`, `body`, `knob`, `rstx`, `upgf`/`updn` (firmware) are for players with
displays or hardware (**server** list); squeezelite handles none of them (`SL slimproto.c:517-526`). Ignore silently.

## 5. Client → server messages

### `STAT`: status

Struct `SL slimproto.h:44-65`; payload is **53 bytes**, `length = 53` (`SL slimproto.c:158-204`).

| Offset (payload) | Size | Field | squeezelite value |
|------|---|-------|-------|
| 0  | 4 | event code, ASCII (`STMt`, …) | (`SL slimproto.c:178`) |
| 4  | 1 | num CRLF | 0 |
| 5  | 1 | MAS initialized | 0 (LyrPlay: `m`, `LP SlimProtoClient.swift:403`) |
| 6  | 1 | MAS mode | 0 (LyrPlay: `p` on `STMp`, `LP …:405-410`) |
| 7  | 4 | stream buffer size, BE | compressed-data buffer capacity, bytes |
| 11 | 4 | stream buffer fullness, BE | bytes buffered, not yet decoded |
| 15 | 4 | bytes received, high 32, BE | bytes of the **current stream** (reset per `strm s`) |
| 19 | 4 | bytes received, low 32, BE | |
| 23 | 2 | signal strength | `0xFFFF` = wired (`SL slimproto.c:185`) |
| 25 | 4 | jiffies, BE | local monotonic clock in ms, wraps at 2³² (`SL slimproto.c:186`, `SL utils.c:103-121`) |
| 29 | 4 | output buffer size, BE | decoded PCM buffer capacity, bytes |
| 33 | 4 | output buffer fullness, BE | decoded bytes not yet played |
| 37 | 4 | elapsed seconds, BE | `elapsed_ms / 1000` |
| 41 | 2 | voltage | 0 |
| 43 | 4 | elapsed milliseconds, BE | see §6 |
| 47 | 4 | server timestamp | the 4 bytes from `strm t`, copied **without byte swapping**; 0 otherwise (`SL slimproto.c:192`) |
| 51 | 2 | error code | 0 |

Event codes:

| Code | Meaning | When squeezelite sends it |
|------|---------|---------------------------|
| `STMa` | autostart / track started (old firmware) | never (**server** accepts it) |
| `STMc` | connected: stream request accepted | right after starting the HTTP connect on `strm s` (`SL slimproto.c:380`) |
| `STMd` | decoder done: this track is fully decoded, **send the next one** | decode complete (`SL slimproto.c:683-686,756`) |
| `STMe` | stream connection established | never (**server** accepts it) |
| `STMf` | flushed / stopped | on `strm s` (first), `q`, `f` (`SL slimproto.c:292,301,357`) |
| `STMh` | end of HTTP headers | never; headers go as `RESP` instead |
| `STMl` | buffer threshold reached (loaded), waiting for `strm u` | autostart 0 only, once per track (`SL slimproto.c:670-677,758`) |
| `STMn` | not supported / decode error: server skips the track | decoder error (`SL slimproto.c:685,761`); LyrPlay also for an unknown format on `strm s` (`LP SlimProtoCommandHandler.swift:207-211`) |
| `STMo` | output underrun while still streaming (rebuffering) | once per track (`SL slimproto.c:725-729,760`) |
| `STMp` | paused | `strm p` with interval 0 (`SL slimproto.c:318`) |
| `STMr` | resumed | `strm u` (`SL slimproto.c:341`) |
| `STMs` | track started: the first sample of a new track is being played | (`SL output.c:153-156`, `SL slimproto.c:702-706,755`) |
| `STMt` | timer / heartbeat | reply to `strm t`, and every ≥ 1 s while output runs (`SL slimproto.c:285,734-737,757`) |
| `STMu` | underrun at end: output drained, nothing streaming or decoding: **playback finished** | once per track (`SL slimproto.c:716-724,759`) |

### `RESP`: HTTP response headers

Payload: the raw response header block of the audio stream, status line included, up to and including the empty line
(`SL slimproto.c:219-230`). Sent once per stream as soon as the headers are complete
(`SL slimproto.c:654-660,762`, `SL stream.c:586-640`). The server uses it for direct streams (status, content type,
`icy-metaint`, redirects) and replies with `cont` (autostart ≥ 2).

### `META`: stream metadata

Payload: raw metadata text (`SL slimproto.c:232-243,661-666,763`):
- ICY: the metadata block as received, e.g. `StreamTitle='Artist - Title';StreamUrl='';` (NUL padding included)
  (`SL stream.c:648-696`).
- Ogg/Opus: `Ogg` followed by entries of u16 BE length + `TITLE=`/`ARTIST=`/`ALBUM=` comment (`SL stream.c:394-424`).

### `DSCO`: stream disconnected

Payload: 1 byte reason (`SL slimproto.h:67-72`, `SL slimproto.c:206-217`, codes `SL squeezelite.h:547`):

| Reason | Meaning | Example |
|--------|---------|---------|
| 0 | connection closed normally (end of stream) | `SL stream.c:709-711` |
| 1 | local disconnect / error | header too long, send failed (`SL stream.c:196,629`) |
| 2 | remote end reset the connection | read error (`SL stream.c:713-717`) |
| 3 | unreachable | connect failed (`SL stream.c:906-911`) |
| 4 | timeout | (defined, not used by squeezelite) |

Sent when the stream reached the `DISCONNECT` state (`SL slimproto.c:649-653,754`). A stream we close ourselves
(`q`, `f`, decode done) is not reported.

### `SETD`: setting value

Payload: id (1 byte) + data. Id 0 = player name, NUL terminated (`SL slimproto.c:245-258`). Sent as reply to a name
query or to confirm a rename.

### `IR  `: infrared code (optional)

Payload 10 bytes: jiffies u32 BE, format u8, bits u8, IR code u32 BE (`SL slimproto.h:96-104`, `SL slimproto.c:261-274`).
LMS maps codes to remote buttons. Could let dock keys act as a "remote", but the CLI/JSON-RPC (port 9000) is the
better control path, which `streamdockd`'s Lyrion module already uses.

### `BYE!` and `SHUT`

`BYE!` (payload 1 byte, 0) tells the server the player is going away (**server**; squeezelite never sends it, it just
closes the socket). LyrPlay sends `SHUT` with empty payload before disconnecting so LMS saves the playback position
(`LP SlimProtoClient.swift:164-189`); **open** whether LMS 9 acts on it. For us: on daemon shutdown send `BYE!`, then
close.

## 6. Streaming, playback model and timing

### Pipeline (squeezelite)

Three stages with two buffers (`SL squeezelite.h:265-267`):

```
HTTP socket ─▶ stream buffer (compressed, 2 MiB) ─▶ decoder ─▶ output buffer (PCM, ~10 s) ─▶ audio device
```

### The HTTP fetch

- Target: `strm` server IP:port; IP 0 means the slimproto server (`SL slimproto.c:348-350`); for local LMS tracks
  this is the LMS HTTP port (9000).
- Request: the bytes after the strm struct, sent **verbatim** (`SL stream.c:174-207,925-927`). LMS builds a full
  request, typically `GET /stream.mp3?player=<mac> HTTP/1.0` + headers + empty line (**server**). Send exactly what was
  given; do not use an HTTP client that rewrites it.
- Response: read until the header block ends (CR/LF counting, `SL stream.c:589-640`). squeezelite does not look at the
  status code; it forwards the headers via `RESP` and lets the server decide.
- TLS: if flag 0x20 is set **or** the port is 443, connect with TLS and SNI taken from the request's `Host:` header
  (`SL stream.c:893-901,255-288`). If TLS on 443 fails, retry plain (`SL stream.c:904`); if a plain connection closes
  before any header byte, retry once with TLS (`SL stream.c:600-617`). Only advertise `CanHTTPS=1` if we do this.
- Body: append to the stream buffer, counting `bytes received`; strip ICY metadata every `metaint` bytes after a
  `cont` (`SL stream.c:648-737`).
- Threshold: once more than `threshold` KiB arrived, the stream state becomes "streaming", which lets the decoder start
  (`SL stream.c:733-736`).
- EOF → `DSCO 0`; errors → `DSCO 1/2/3` (§5).

### Track state machine (one track, autostart `1`)

| Step | Trigger | Client sends |
|------|---------|--------------|
| 1 | `strm s` received | `STMf`, then `STMc` |
| 2 | HTTP headers complete | `RESP` |
| 3 | buffer > threshold, decoder starts; output state "buffer" | (autostart `0`: `STMl`, then wait for `strm u` → `STMr`) |
| 4 | output buffer > output threshold | (output starts) |
| 5 | first sample of this track played | `STMs` |
| 6 | while playing, every ≥ 1 s | `STMt` (timestamp 0) |
| 7 | HTTP EOF | `DSCO 0` |
| 8 | decoder consumed everything | `STMd` → server sends the **next** `strm s` (step 1 for track n+1 while n still plays) |
| 9 | output drained, nothing streaming or decoding | `STMu` → server ends playback (or treats it as end of queue) |

Plus at any time: `strm t` → `STMt` with echo; underrun mid-stream → `STMo`; decoder error → `STMn`.
Sources: `SL slimproto.c:344-391,643-763`, `SL output.c:62-67,126-165`, `SL stream.c:586-740`.

### Gapless and transitions

- The next track's `strm s` arrives after `STMd`, while the current track still plays from the output buffer.
  squeezelite marks the write position where the new track's first PCM lands (`track_start`) (`SL pcm.c:213-217`,
  `SL flac.c:174-178`, `SL mad.c:245-252`).
- When playback reaches that mark: switch sample rate if needed, reset the frame counter, apply the new replay gain,
  and raise "track started" → `STMs` (`SL output.c:126-165`).
- `strm f` drops only the not-yet-started part (`SL output.c:455-466`): used when the user skips while the next track
  is already buffered.
- Fades/crossfade (transition type and period) are applied at track boundaries (`SL output.c:291-352`). Optional for
  us: the server falls back to no transition when the player ignores it.

### Elapsed time

Per STAT, squeezelite computes the time played **of the current track** (`SL slimproto.c:160-173`):

```
elapsed_ms = (frames_played_at_last_measurement − frames_still_in_device_buffer) · 1000 / sample_rate
             + (now − time_of_that_measurement)
```

- `frames_played` resets to 0 when the track starts (`SL output.c:154`); `device_frames` is the hardware/driver delay
  (`SL output_alsa.c:820-840`), so the value is what is **audible**, not what was written.
- Fallback without a delay measurement: `now − track start time` (`SL slimproto.c:167-169`).
- 0 before the track started.

The server's song position is its own start offset (after a seek LMS streams from an offset) plus `elapsed_ms`; with
`AccuratePlayPoints=1` it trusts the player's value; otherwise it estimates from buffer fullness and bytes (**server**).
The `jiffies` field lets the server map our clock to its own: from it and the round-trip of the `strm t` timestamp
echo it estimates latency and our clock offset, which it needs for `strm u` start times (**server**). Use a monotonic
clock (`CLOCK_MONOTONIC` ms, `SL utils.c:103-121`), wrapping u32; never wall-clock time (`LP SlimProtoClient.swift:455-459`).

### Pause, resume, skip

- `strm p` (0): stop feeding the device, keep both buffers, send `STMp`. Elapsed stays frozen (no frames played).
- `strm u` (0): resume, `STMr`. With jiffies: start at that local time (sync start).
- `strm p` (*n* ms) / `strm a` (*n* ms): insert silence / drop audio to correct drift in sync groups.
- autostart `0` + `strm u`: the server's way to start several players at the same instant, and to start a fade-in.

### Volume and replay gain

Effective sample gain = `audg` gain × replay gain of the **playing** track (both 16.16; replay gain 0 = none)
(`SL output.c:54-57`). Apply the replay gain from the track-start point, not when `strm s` arrives. Digital volume:
multiply samples (with clipping) in the output path; a real mixer is optional.

## 7. Formats

Codec ids (strm format byte) and the capability tokens that advertise them:

| Id | Codec | Capability tokens | squeezelite decoder |
|----|-------|-------------------|---------------------|
| `p` | PCM (raw; WAV/AIFF header parsed when present) | `pcm`, `aif`, `wav` | `SL pcm.c:459-477` |
| `f` | FLAC (`ogf` = FLAC in Ogg, pcm size byte `o`) | `flc`, `ogf` | `SL flac.c:387-390`, `SL slimproto.c:377` |
| `m` | MP3 | `mp3` | `SL mad.c:402-403`, `SL mpg.c:251-252` |
| `a` | AAC (ADTS / MP4) | `aac` | `SL faad.c:641-642` |
| `o` | Ogg Vorbis | `ogg` | `SL vorbis.c:357-358` |
| `u` | Opus (in Ogg) | `ops` | `SL opus.c:302-303` |
| `l` | ALAC | `alc` | `SL alac.c:537-538` |
| `w` | WMA / WMA Pro / WMA Lossless (ffmpeg) | `wma`, `wmap`, `wmal` | `SL ffmpeg.c:776-777` |
| `d` | DSD (DSF/DFF; DoP or native) | `dsf`, `dff` | `SL dsd.c:890-891` |

(LyrPlay maps `A` to ALAC and `w` to WAV, `LP SlimProtoCommandHandler.swift:149-161`; squeezelite does not, we follow
squeezelite.)

For PCM, the pcm bytes describe the raw samples: size, rate, channels, endianness (`SL pcm.c:440-448`). squeezelite
only parses a WAV/AIFF header for local files or with `-W` (`SL pcm.c:192-194`, `SL main.c:538-539`); otherwise the
server's values are trusted. LMS's PCM from its transcoders is usually 16 bit little-endian with the track's rate
(**server**). Handle `?` (from header) anyway.

### How LMS chooses the format (server)

For each track LMS looks for the first rule in its transcoding table (`convert.conf`, Settings → Advanced → File
Types) whose output format is in the player's codec list: native passthrough (`flc flc`, `mp3 mp3`) when the player
lists the source format, otherwise a transcode (`flc pcm`, `* mp3` with LAME, …). The **order** of the codec list is
the preference order. If the player lists only `pcm`, every compressed source needs a working transcoder to PCM on
the server (e.g. `flac`, `faad`, `sox`, `lame` binaries).

**Observed on our server (H15-47):** when the needed transcoder is missing, LMS cannot build a stream for the track
and stops it (no `strm s` with audio arrives; the track is skipped or playback stops). Consequence for us: decode
natively what LMS stores (at least FLAC and MP3, plus PCM; AAC/Ogg/Opus when a pure-Rust decoder is cheap) and
advertise those, so LMS can pass files through unchanged.

## 8. Multi-room sync (optional)

To be a good sync-group member (`SL slimproto.c:306-343`, `SL output.c:69-109`):

1. Honour autostart `0`: buffer, send `STMl` once, don't play until `strm u`.
2. `strm u` with jiffies: start exactly at that local monotonic time (output silence until then).
3. `strm p` *n* / `strm a` *n*: insert *n* ms silence / skip *n* ms of buffered audio (server's drift correction).
4. Report **accurate** `elapsed_ms` including the device/driver latency, and real `jiffies` (monotonic ms).
5. Answer `strm t` immediately with the timestamp echo (latency measurement).
6. Keep `STMs` exact (sent when the first sample is audible, not when decoded).
7. Persist the sync group handed over by `serv` (`SyncgroupID`, §4).

Not required for the MVP: a single player works without any of the timing precision.

## 9. Implementation checklist (Rust, `streamdockd`)

### MVP

- [ ] **Config**: `[modules.lyrion] player = { enabled, name, server? (host[:port]), mac? }`; persist name changes and
      the generated MAC (state file).
- [ ] **Discovery**: UDP broadcast `e` + `NAME\0JSON\0` (+ `IPAD\0`, `VERS\0`), parse `E` TLVs, take the sender IP;
      retry every 5 s; skip when a server is configured.
- [ ] **Connection task** (tokio): TCP 3483, framing both ways, 35 s silence timeout, reconnect after 100 ms (then 5 s
      backoff), reconnect flag + bytes received in HELO, rediscover after 5 failures.
- [ ] **HELO**: device id 12, revision 0, MAC, zero UUID, caps
      `Model=squeezelite,ModelName=StreamDock,AccuratePlayPoints=1,HasDigitalOut=1,Firmware=<version>,MaxSampleRate=<n>,flc,mp3,pcm`
      (`CanHTTPS=1` only once TLS streaming works; `Model=squeezelite` keeps LMS's squeezelite feature set).
- [ ] **setd**: name query → `SETD 0 <name>\0`; set → store + echo.
- [ ] **strm**: `s` (STMf, open decoder, connect, STMc), `t` (STMt + echo), `q` (STMf), `f` (STMf if anything was
      flushed), `p` (0 → STMp; *n* → silence), `u` (0 → STMr; jiffies → start at), `a` (skip). Ignore unknown commands.
- [ ] **audg** (16.16 new gains, `adjust` flag), **aude** (power), **serv** (switch), **cont** (only for autostart ≥ 2);
      ignore everything else.
- [ ] **HTTP stream**: raw TCP, send the request verbatim, read headers → `RESP`, threshold → start decoding, EOF →
      `DSCO 0`, errors → `DSCO 1/2/3`.
- [ ] **Decode**: PCM (pcm byte parameters), FLAC, MP3 (candidate: `symphonia`; needs an ADR-001 entry).
- [ ] **Output**: PCM sink with a measurable delay (ALSA / PipeWire; **open** which on the Luckfox), digital volume ×
      replay gain, sample-rate switch at track boundaries.
- [ ] **Status loop** every 100 ms: `STMs`, `STMd`, `STMu`, `STMo`, `STMn`, `STMl`, `DSCO`, `RESP`, `META`, plus `STMt`
      every 1 s while playing; `elapsed_ms` from frames played minus device delay.
- [ ] **Gapless**: keep playing the output buffer when a new `strm s` arrives; mark the track boundary; `STMs` there.
- [ ] **Tests**: encode/decode every packet against the byte tables above (golden bytes), a fake LMS over a local
      socket that drives `strm s → STMd → strm s → STMu`, without real audio hardware (fake sink).

### Later

- ICY `META` and `cont` for direct radio streams, HTTPS (`rustls`), AAC/Ogg/Opus, fades/crossfade, sync (§8), `IR`.

### Open questions

1. **Audio output**: which device on the Luckfox (USB DAC, I2S, Bluetooth A2DP via BlueZ?) and on the desktop
   (PipeWire)? Determines the crate (`cpal`, `alsa`, `pipewire`) and how we read the device delay.
2. **Codec set**: is FLAC + MP3 + PCM enough on our LMS (what formats is the library in, and which transcoders exist
   on the server)? Capture the server's choice with only `pcm` vs. with `flc,mp3,pcm`.
3. **Player identity**: one player per `streamdockd` instance; MAC derived from machine id OK? Name default
   (`StreamDock <host>`)?
4. **Model string**: `Model=squeezelite` (full feature set in LMS) vs. our own model name (LMS may then treat it as a
   generic player). Verify in the LMS UI.
5. **`BYE!` / `SHUT`**: does LMS 9 save the position on either? Capture.
6. **Server-side details** marked **server** above (discovery TLVs `IPAD`/`UUID`/`VERS`, `strm t` cadence, spdif byte,
   flags 0x80/0x40, `STMa`/`STMe`/`STMh`, transcoding behaviour): confirm with a packet capture against our LMS before
   relying on them.
7. **Integration with the existing Lyrion module**: the module controls players over JSON-RPC; the built-in player
   should appear there as a normal player (by MAC) so the dock's music tab can select it.
