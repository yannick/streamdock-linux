//! H15-58: loopback Lyrion protocol and recording output, with no audio/hardware side effects.
#[path = "scenarios/output.rs"]
mod output;
#[path = "scenarios/protocol.rs"]
mod protocol;
mod support;

#[path = "scenarios/real_player.rs"]
mod real_player;
