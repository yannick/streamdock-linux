//! A silent player for testing against a real Lyrion server (H15-55): it registers, takes commands
//! and drains the audio stream without decoding it. `RUST_LOG=slimproto=debug cargo run -p
//! slimproto --example null_player -- [name] [server-ip]`.

use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Arc;

use slimproto::{Config, PlayerState, Silent};
use tokio::sync::watch;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let mut args = std::env::args().skip(1);
    let name = args.next().unwrap_or_else(|| "streamdock-test".into());
    let server = args
        .next()
        .map(|ip| SocketAddrV4::new(ip.parse::<Ipv4Addr>().expect("an IPv4 address"), 3483));
    let config = Config {
        mac: slimproto::mac_from_seed(name.as_bytes()),
        name,
        server,
        max_sample_rate: 192_000,
        firmware: env!("CARGO_PKG_VERSION").into(),
        codecs: Vec::new(),
    };
    let (state, mut rx) = watch::channel(PlayerState::default());
    tokio::spawn(async move {
        while rx.changed().await.is_ok() {
            tracing::info!(state = ?*rx.borrow(), "player state");
        }
    });
    slimproto::run(config, Arc::new(Silent::default()), state).await;
}
