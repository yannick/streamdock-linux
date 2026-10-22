# Third-party material

streamdockd itself is licensed under MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`). The repository
also contains, or the built binary embeds, the following works under their own licences.

| What | Where | Licence |
|---|---|---|
| Fira Sans Condensed (fonts) | `crates/streamdockd/assets/fonts/`, `web/public/design/fonts/` | SIL Open Font License 1.1 (`OFL.txt` next to the fonts) |
| bs2b crossfeed coefficient equations, Boris Mikhaylov | `crates/player/src/dsp/` | MIT (`crates/player/src/dsp/LICENSE-BS2B.txt`) |
| microWakeWord models by Kevin Ahrendt / ESPHome (`.tflite` + manifests), unmodified | `crates/wakeword/models/` | Apache-2.0 (see `crates/wakeword/models/README.md`) |
| React and React DOM (bundled into the web UI by Vite) | `web/` dependencies, embedded in the binary | MIT |
| Linux kernel patches for the Luckfox Pico board | `packaging/luckfox/kernel/*.patch` | GPL-2.0, like the kernel they patch |
| Synthetic speech fixtures, Piper voice `de_DE-thorsten-high` (Thorsten-Voice) | `crates/streamdockd/src/assist/fixtures/` | CC0 |
| Synthetic speech fixtures, Piper voice `en_US-ljspeech-medium` (trained from scratch on the LJ Speech dataset) | `crates/wakeword/tests/fixtures/*.wav` | Public domain (LJ Speech) |

Rust crates and npm packages pulled in at build time keep their own licences; `cargo about` or
`cargo license` and `pnpm licenses list` list them.
