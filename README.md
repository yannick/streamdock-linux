# streamdockd

Linux software for the **StreamDock 293s / N3** (FHOOU / Mirabox, USB `6603:1003`): 6 LCD keys, 3 plain
buttons and 3 knobs. `streamdockd` is one Rust daemon. It drives the dock, connects it to
**Home Assistant**, **Lyrion Music Server**, Bluetooth audio and your own actions, and serves a web
UI that shows the dock live and edits everything.

It runs on a normal Linux PC or on a **Luckfox Pico Ultra W** (a small ARM board with Ethernet, PoE,
Wi-Fi and Bluetooth). There it is also the music player and the audio output for headphones and
speakers.

![The dashboard: the dock's light screen with a live mirror of the keys, activity on the right](docs/screenshots/dashboard-lights.png)

- [What it does](#what-it-does)
- [The web UI](#the-web-ui)
- [Hardware](#hardware)
- [Install](#install)
- [Configuration](#configuration)
- [Development](#development)
- [Documentation](#documentation)
- [Contributing and licence](#contributing-and-licence)

## What it does

**Knob 3 switches between screens** ("modes"). Each screen is a tab on the dashboard. You can have
as many as you like, and **profiles** limit which ones knob 3 turns through (for example "Kids"
for a child's room and "Office" for the office).

### Lights and blinds (Home Assistant)

- One key per light or blind, either from a Home Assistant area or pinned per key.
- The keys show the live state: on/off with icons, brightness, and the blind's position and movement.
- Blinds get separate **up / stop / down** keys. Pressing a direction while it moves stops it.
- The **big knob dims** and the small knob sets the **colour temperature**. Turning the dimmer while
  the lights are off switches them on from 1 %.
- Optional **level bars** on keys show the brightness and colour temperature while you turn.

| A Home Assistant area, with level bars | Pinned keys: a light plus blind up / stop / down |
|---|---|
| ![Living room lights](docs/screenshots/dashboard-lights.png) | ![Kids' room lights and blind](docs/screenshots/dashboard-kids-lights.png) |

### Music (Lyrion Music Server)

- **Now playing**: title and artist run across the top keys, and the same component is on the web page.
- Play / pause / previous / next on keys. The **big knob is the volume**, with a bar and % on a key.
- A **playlists** screen shows your Lyrion playlists as keys, filtered by a name prefix. A press
  plays it and jumps back to the music screen.
- Each music tab can control a **different Lyrion player** (chosen from Lyrion's player list and
  stored by MAC address).
- With a Sony speaker, knob 1 can set its **bass**.

| Music screen | Playlists | Kids' playlists (prefix `_Kids_`) |
|---|---|---|
| ![Music](docs/screenshots/dashboard-music.png) | ![Playlists](docs/screenshots/dashboard-playlists.png) | ![Kids' playlists](docs/screenshots/dashboard-kids-music.png) |

### Built-in music player and audio (Luckfox)

- On the Luckfox, `streamdockd` is itself a **Lyrion player** (slimproto). It decodes FLAC/MP3/PCM,
  resamples in the player and plays through PipeWire.
- **Native DSP** for every output:
  - tone (bass/mid/treble, balance) and a "sound character" pad (warm ↔ bright, relaxed ↔ energetic);
  - correction EQ presets from AutoEq;
  - crossfeed for headphones;
  - a limiter so boosts never clip.
- **Bluetooth audio** with codec choice per device (LDAC, AAC, SBC …) and **output priority**: the
  highest present output plays, and a headset that comes back takes over again.
- **Headset mode**: one key moves the running playlist (queue, track and position) between a
  speaker and the headset. `[player] max_volume` caps the volume, for example for a child.


### Settings screen on the dock

The paired Bluetooth headphones and speakers sit on the keys, with their state and battery. A press
connects or disconnects one. Key 5 is the headset-mode switch.

![Settings screen on the dock](docs/screenshots/dashboard-settings-screen.png)

### Custom keys

- Pages of keys with your own actions: Home Assistant services, HTTP webhooks, Lyrion, audio volume,
  brightness, switching modes or profiles, sequences (`multi`), on/off **toggles** that follow an
  entity, and optional shell commands.
- Edit them in the browser (`/keys`), with built-in icons or your own uploaded images.

| Custom keys on the dock | The keys editor |
|---|---|
| ![Custom keys](docs/screenshots/dashboard-keys.png) | ![Keys editor](docs/screenshots/keys-editor-action.png) |

### Voice

- **Push-to-talk or tap-to-talk** with Home Assistant Assist: speech to text, the answer as speech
  through the same audio output.
- An optional **wake word** ("hey dock") runs on the board itself: our own pure-Rust microWakeWord
  engine, about 2 % CPU on the Luckfox.
- Wake words can be trained on a Mac with the companion trainer app and uploaded.

![Voice settings](docs/screenshots/settings-voice.png)

### Dock behaviour

- Screen off after idle minutes; any key, button or knob wakes it (that first input only wakes it).
- A night window with its own brightness.
- Brightness per dock, kept across restarts.
- Several docks at once, hot-plug and reconnect.

## The web UI

The daemon serves the UI itself, at `http://<host>:7315/` (on the Luckfox also on port 80). Every
change on the dock shows up in every open browser at once (a server stream over connectRPC). Clicks
on the mirrored keys and knobs act exactly like the real dock. It works on a phone too.

| Phone | Settings: integrations |
|---|---|
| ![Phone](docs/screenshots/phone-dashboard.png) | ![Integrations](docs/screenshots/settings-integrations.png) |

**Settings** (`/settings`), one section each:

- **Integrations:** Home Assistant (URL, token, area), Lyrion, custom keys, settings screen.
- **Network** (Luckfox): Ethernet with a 60 s safety revert, Wi-Fi as a fallback, IPv4/IPv6.
- **Bluetooth** (Luckfox): pairing, codec, bitrate and signal of a connected device.
- **Audio** (Luckfox): outputs, priority, tone, EQ, crossfeed, limiter.
- **Lyrion player:** name, server, formats, replay gain, buffer, resampling quality.
- **Voice:** the Assist pipeline, microphone, VAD and the wake word.
- **Dock & daemon:** brightness, sleep and the night window, version.
- **Key images:** upload your own pictures for keys.
- **Backup:** export and import the whole configuration as one file.

![Lyrion player](docs/screenshots/settings-player.png)

| Dock & daemon | Key images | Backup |
|---|---|---|
| ![Dock](docs/screenshots/settings-dock.png) | ![Images](docs/screenshots/settings-images.png) | ![Backup](docs/screenshots/settings-backup.png) |

**Design system** (`/design`): all colours and components of the UI, live. **Changelog**
(`/changelog`): what changed, newest first.

| Design system | Changelog |
|---|---|
| ![Design](docs/screenshots/design.png) | ![Changelog](docs/screenshots/changelog.png) |

## Hardware

- **StreamDock 293s / N3** (USB `6603:1003`, the Ajazz AKP03 family). The protocol was reverse
  engineered from the vendor software (static analysis only) and is described in
  [docs/protocol.md](docs/protocol.md) and, standalone for other hosts,
  [docs/streamdock-n3-protocol.md](docs/streamdock-n3-protocol.md).
- **Any Linux PC** with hidraw access to the dock (`udev/70-streamdock.rules`).
- **Luckfox Pico Ultra W** (RV1106) with Ubuntu and our own kernel
  ([ADR-004](docs/adr/0004-luckfox-board-kernel.md)). Plug the dock straight into the USB-A port.
  The kernel raises the USB signal level so this works without a hub.

## Install

You need Rust (the version is pinned in `rust-toolchain.toml`), Node with pnpm, `buf` and `just`.

**On this PC**, as a systemd user service:

```sh
just udev-install       # access to the dock without root (sudo)
just install-service    # release build, streamdockd.service, http://localhost:7315
just logs               # follow the log
```

**On a Luckfox** (static ARMv7 binary, system service):

```sh
just deploy-luckfox                    # finds the board via mDNS
just deploy-luckfox root@192.168.1.50  # or give it directly
```

`streamdockd --version` prints the version; `just discover` lists every `streamdockd` on the LAN.

## Configuration

One TOML file: `~/.config/streamdockd/config.toml` (or `STREAMDOCKD_CONFIG`). Start from
[packaging/config.example.toml](packaging/config.example.toml). Almost everything can be edited in
the web UI instead. Changes to the file are picked up live, except daemon-level settings, which need
a restart.

```toml
brightness = 80
sleep_after_minutes = 5

[modules.homeassistant]
enabled = true
url = "https://ha.example.org"   # the token goes into homeassistant.token next to this file
area = "Office"

[[modules.lyrion.tab]]
id = "music"
title = "Music"
player = "aa:bb:cc:dd:ee:ff"     # the Lyrion player this tab controls
layout = "controls"

[[profiles]]
name = "Office"
modes = ["homeassistant", "music", "keys"]
```

Secrets (the Home Assistant token) are separate files with mode 600 and never go into git.

## Development

```sh
just dev      # safe dev instance on :7397: a simulated dock with demo modes only
just demo     # :7399, the simulated dock against a fake Home Assistant and Lyrion (neutral demo data)
just test     # all tests
just check    # lint + all tests, what CI runs
```

- **The real dock belongs to one daemon.** Dev instances use the simulated dock (`hardware = false`,
  `fake_device = true`).
- **Key presses act on the real world.** Don't give a dev config real integrations you don't want
  to trigger: a key press, including one from the simulated dock, sends real calls.
- Architecture and libraries are fixed in [ADR-001](docs/adr/0001-architecture-and-libraries.md):
  - one binary on tokio and axum;
  - the API as protobuf over connectRPC (`proto/`);
  - a React + TypeScript web UI compiled into the binary;
  - the built-in player in `crates/slimproto` and `crates/player`.

## Documentation

- [docs/adr/](docs/adr/): architecture decisions (libraries, the Luckfox kernel)
- [docs/protocol.md](docs/protocol.md) and [docs/streamdock-n3-protocol.md](docs/streamdock-n3-protocol.md): the dock's USB protocol
- [docs/slimproto.md](docs/slimproto.md): the built-in Lyrion player
- [docs/wake-word.md](docs/wake-word.md): the wake word engine
- [docs/design-system.md](docs/design-system.md): the UI's tokens and components
- [CHANGELOG.md](CHANGELOG.md): what changed

Screenshots: `docs/screenshots/`, taken with `just demo`: the simulated dock against a fake Home
Assistant and Lyrion with neutral names (`crates/streamdockd/examples/demo_backend.rs`,
`packaging/demo-config.toml`). The Luckfox-only sections (network, Bluetooth, audio) aren't shown,
since they would show a real network.

## Contributing and licence

See [CONTRIBUTING.md](CONTRIBUTING.md); it also explains the `H15-…` ticket ids in comments and the
changelog. The protocol was worked out from the vendor's public SDKs, the
[`mirajazz`](https://github.com/4ndv/mirajazz) crate and static analysis of the vendor app; no vendor
code is included.

streamdockd is licensed under MIT OR Apache-2.0, at your option ([LICENSE-MIT](LICENSE-MIT),
[LICENSE-APACHE](LICENSE-APACHE)). Bundled fonts, models and other third-party material keep their
own licences: see [THIRD-PARTY.md](THIRD-PARTY.md).
