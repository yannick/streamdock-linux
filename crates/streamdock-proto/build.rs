//! Generates the buffa message types and connectrpc service stubs for `proto/streamdock/v1`.
//! Needs `protoc` on PATH (or `PROTOC`).

fn main() {
    connectrpc_build::Config::new()
        .files(&["../../proto/streamdock/v1/device.proto"])
        .includes(&["../../proto"])
        .include_file("_connectrpc.rs")
        .compile()
        .expect("generating streamdock.v1 failed");
}
