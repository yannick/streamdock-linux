//! Pushes the network and Bluetooth state to the UI as it changes (H15-49): listens to BlueZ's
//! and NetworkManager's D-Bus signals (objects added and removed, properties changed), waits until
//! a burst is over and publishes the new [`pb::NetworkState`] through the hub, which only announces
//! it when something differs. A slow tick catches what no signal reports.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{FutureExt as _, StreamExt as _};
use zbus::message::Type;

use super::Network;
use crate::hub::Hub;

/// How long a burst of signals may last before the state is read (a connect sends dozens).
const SETTLE: Duration = Duration::from_millis(400);
/// Reads the state anyway this often.
const TICK: Duration = Duration::from_secs(30);

type Signals = futures_util::stream::BoxStream<'static, ()>;

impl Network {
    /// Publishes the state now and after every change, while the daemon runs.
    pub async fn watch_state(self: Arc<Self>, hub: Arc<Hub>) {
        let mut signals = match self.signals().await {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "no D-Bus signals for network changes; reading the state every 30 s");
                None
            }
        };
        let mut ticker = tokio::time::interval(TICK);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await;
        loop {
            hub.set_network_state(self.state().await);
            self.wait_for_change(&mut signals, &mut ticker).await;
        }
    }

    /// Waits for a signal, an API change or the tick, then for the burst to settle.
    async fn wait_for_change(&self, signals: &mut Option<Signals>, ticker: &mut tokio::time::Interval) {
        tokio::select! {
            more = next_signal(signals) => if !more {
                tracing::warn!("D-Bus signal stream ended; reading the network state every 30 s");
                *signals = None;
            },
            () = self.changed.notified() => {}
            _ = ticker.tick() => {}
        }
        tokio::time::sleep(SETTLE).await;
        if let Some(stream) = signals.as_mut() {
            while let Some(Some(())) = stream.next().now_or_never() {}
        }
    }

    /// The signals of BlueZ and NetworkManager that may change what the settings show.
    pub(super) async fn signals(&self) -> anyhow::Result<Signals> {
        let bus = self.bus().await?;
        let mut streams = Vec::new();
        let proxies = Arc::clone(self.proxies().await?);
        for rule in [
            zbus::MatchRule::builder()
                .msg_type(Type::Signal)
                .sender("org.freedesktop.DBus")?
                .interface("org.freedesktop.DBus")?
                .member("NameOwnerChanged")?
                .add_arg("org.freedesktop.NetworkManager")?
                .build(),
            zbus::MatchRule::builder()
                .msg_type(Type::Signal)
                .sender("org.freedesktop.DBus")?
                .interface("org.freedesktop.DBus")?
                .member("NameOwnerChanged")?
                .add_arg("org.bluez")?
                .build(),
            // Devices, adapters, transports and batteries: PropertiesChanged.
            zbus::MatchRule::builder()
                .msg_type(Type::Signal)
                .path_namespace("/org/bluez")?
                .build(),
            // Devices found or removed: BlueZ's ObjectManager lives on "/".
            zbus::MatchRule::builder()
                .msg_type(Type::Signal)
                .sender("org.bluez")?
                .interface("org.freedesktop.DBus.ObjectManager")?
                .build(),
            zbus::MatchRule::builder()
                .msg_type(Type::Signal)
                .path_namespace("/org/freedesktop/NetworkManager")?
                .build(),
        ] {
            streams.push(zbus::MessageStream::for_match_rule(rule, bus, Some(256)).await?);
        }
        let merged = futures_util::stream::select_all(streams).filter_map(move |msg| {
            let proxies = Arc::clone(&proxies);
            async move {
                let msg = msg.ok()?;
                let header = msg.header();
                let member = header
                    .member()
                    .map(zbus::names::MemberName::as_str)
                    .unwrap_or_default();
                if member == "NameOwnerChanged" {
                    let (name, _, _): (String, String, String) = msg.body().deserialize().ok()?;
                    if !matches!(name.as_str(), "org.bluez" | "org.freedesktop.NetworkManager") {
                        return None;
                    }
                    proxies.remove_service(&name).await;
                    return Some(());
                }
                let path = header.path().map(ToString::to_string).unwrap_or_default();
                let service = if path.starts_with("/org/freedesktop/NetworkManager") {
                    "org.freedesktop.NetworkManager"
                } else {
                    "org.bluez"
                };
                if member == "InterfacesRemoved" {
                    let (removed, _): (zbus::zvariant::OwnedObjectPath, Vec<String>) =
                        msg.body().deserialize().ok()?;
                    proxies.remove_path(service, removed.as_str()).await;
                } else if matches!(member, "DeviceRemoved" | "AccessPointRemoved") {
                    let removed: zbus::zvariant::OwnedObjectPath = msg.body().deserialize().ok()?;
                    proxies.remove_path(service, removed.as_str()).await;
                }
                // AP signal strength is not part of the pushed state.
                (!path.contains("/AccessPoint/")).then_some(())
            }
        });
        Ok(merged.boxed())
    }
}

/// Waits for the next signal; `false` when the stream ended. Without a stream, waits forever.
async fn next_signal(signals: &mut Option<Signals>) -> bool {
    match signals {
        Some(stream) => stream.next().await.is_some(),
        None => std::future::pending().await,
    }
}
