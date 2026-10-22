//! Generated protobuf types and Connect service stubs for the `streamdock.v1` API.
//!
//! The schema lives in `proto/streamdock/v1/`; `build.rs` generates buffa message types and
//! connectrpc service traits and clients from it (ADR-001).

#![allow(clippy::all, clippy::pedantic, missing_debug_implementations)]
// The workspace's complexity limit names this lint itself, so `clippy::all` doesn't cover it; a
// message with many fields (BluetoothDeviceDetail, H15-53) generates long encode functions.
#![allow(clippy::cognitive_complexity)]

connectrpc::include_generated!();
