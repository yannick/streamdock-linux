//! Lyrion Music Server's player protocol, Slimproto, from the player's side (H15-55, spec in
//! `docs/slimproto.md`). This crate speaks to the server: discovery, the connection, the commands
//! both ways and the HTTP fetch of the audio stream. Decoding and output are the [`Playback`]
//! implementation's job (in `streamdockd`, H15-47).

pub mod discovery;
pub mod messages;
pub mod playback;
pub mod player;
pub mod silent;
pub mod stream;

pub use playback::{Format, Playback, PlaybackEvent, PlaybackStatus, Track, Transition, TransitionKind};
pub use player::{Codec, Config, PlayerState, mac_from_seed, run, run_named};
pub use silent::Silent;
