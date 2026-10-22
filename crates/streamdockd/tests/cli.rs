//! The binary's arguments (H15-99): `--version` / `--help` / unknown ones exit before anything
//! starts. The config named here would bind a port and open docks if the daemon ran.

use std::net::TcpListener;
use std::process::Command;
use std::time::{Duration, Instant};

fn streamdockd(args: &[&str], config: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_streamdockd"))
        .args(args)
        .env("STREAMDOCKD_CONFIG", config)
        .env("RUST_LOG", "debug")
        .output()
        .expect("runs")
}

/// A config that would listen on `port` and look for real docks: never used by these tests.
fn trap_config(name: &str, port: u16) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("streamdockd-cli-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    std::fs::write(&path, format!("listen = \"127.0.0.1:{port}\"\nhardware = true\n")).unwrap();
    path
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn version_prints_and_exits_without_starting() {
    let port = free_port();
    let config = trap_config("version", port);
    let started = Instant::now();
    let out = streamdockd(&["--version"], &config);
    assert!(started.elapsed() < Duration::from_secs(5), "returned at once");
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("streamdockd "), "{stdout}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("settings loaded") && !stderr.contains("listening"),
        "no daemon output: {stderr}"
    );
    // Nothing kept or took the port.
    TcpListener::bind(("127.0.0.1", port)).expect("the listener was never bound");
}

#[test]
fn help_and_unknown_arguments() {
    let config = trap_config("help", free_port());
    let help = streamdockd(&["--help"], &config);
    assert_eq!(help.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&help.stdout).contains("STREAMDOCKD_HARDWARE"));
    let bad = streamdockd(&["--verbose"], &config);
    assert_eq!(bad.status.code(), Some(2));
    assert_eq!(String::from_utf8_lossy(&bad.stdout), "");
    let stderr = String::from_utf8_lossy(&bad.stderr);
    assert!(
        stderr.contains("Nothing was started") && !stderr.contains("settings loaded"),
        "{stderr}"
    );
}
