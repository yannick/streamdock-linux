//! This machine's Wi-Fi and Bluetooth (H15-38), for the settings page: NetworkManager and BlueZ
//! over the system D-Bus (`zbus`). Only active with `[network] manage = true`; elsewhere every call
//! says so and nothing is touched.
//!
//! The daemon needs permission for both: on the Luckfox, a polkit rule for NetworkManager and the
//! `bluetooth` group for BlueZ (`packaging/luckfox/`).

pub mod bluetooth;
pub mod codec;
pub mod ethernet;
pub mod ip;
pub mod names;
mod proxies;
mod trial;
pub mod watch;
pub mod wifi;

use std::sync::{Arc, Mutex};

use anyhow::bail;
use streamdock_proto::streamdock::v1 as pb;
use tokio::sync::OnceCell;

/// The Wi-Fi and Bluetooth managers, connected on first use.
#[derive(Debug)]
pub struct Network {
    manage: bool,
    bus: OnceCell<zbus::Connection>,
    bluetooth: OnceCell<bluetooth::Bluetooth>,
    proxies: OnceCell<Arc<proxies::Proxies>>,
    /// An Ethernet change being tried; reverted at its deadline unless confirmed.
    trial: Arc<trial::Trials>,
    /// When the Wi-Fi radio is on (H15-44).
    wifi_mode: Mutex<WifiMode>,
    /// Woken after a change made through the API, so the state is pushed at once (H15-49).
    changed: Arc<tokio::sync::Notify>,
}

/// When the Wi-Fi radio is on (`[network] wifi`, H15-44).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WifiMode {
    /// Radio off, so Bluetooth has the combo chip to itself.
    Off,
    /// Wi-Fi only while Ethernet is down.
    #[default]
    Fallback,
    /// Both links up.
    Both,
}

impl WifiMode {
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        Ok(match text {
            "off" => Self::Off,
            "fallback" => Self::Fallback,
            "both" => Self::Both,
            other => bail!("[network] wifi must be \"off\", \"fallback\" or \"both\", got {other:?}"),
        })
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Fallback => "fallback",
            Self::Both => "both",
        }
    }

    /// Whether the radio should be on, given whether Ethernet is up.
    #[must_use]
    pub fn radio_on(self, ethernet_up: bool) -> bool {
        match self {
            Self::Off => false,
            Self::Fallback => !ethernet_up,
            Self::Both => true,
        }
    }
}

/// Safety refresh; NetworkManager signals normally enforce the radio immediately.
const WATCH_EVERY: std::time::Duration = std::time::Duration::from_secs(30);

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait NetworkManagerRadio {
    #[zbus(property)]
    fn wireless_enabled(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn set_wireless_enabled(&self, value: bool) -> zbus::Result<()>;
}

impl Network {
    #[must_use]
    pub fn new(manage: bool, wifi_mode: WifiMode) -> Self {
        Self {
            wifi_mode: Mutex::new(wifi_mode),
            manage,
            bus: OnceCell::new(),
            bluetooth: OnceCell::new(),
            proxies: OnceCell::new(),
            trial: Arc::default(),
            changed: Arc::default(),
        }
    }

    /// Something changed (through the API, or a trial ran out): push the state now.
    pub fn changed(&self) {
        self.changed.notify_one();
    }

    /// Whether this daemon manages the machine's network (`[network] manage`).
    #[must_use]
    pub fn managed(&self) -> bool {
        self.manage
    }

    async fn bus(&self) -> anyhow::Result<&zbus::Connection> {
        if !self.manage {
            bail!("this streamdockd doesn't manage the network; set [network] manage = true");
        }
        self.bus
            .get_or_try_init(|| async {
                zbus::Connection::system()
                    .await
                    .map_err(|e| anyhow::anyhow!("no system D-Bus: {e}"))
            })
            .await
    }

    async fn proxies(&self) -> anyhow::Result<&Arc<proxies::Proxies>> {
        self.proxies
            .get_or_try_init(|| async { Ok(proxies::Proxies::new(self.bus().await?.clone())) })
            .await
    }

    /// The Wi-Fi device (looked up each time: NetworkManager may restart).
    pub async fn wifi(&self) -> anyhow::Result<wifi::Wifi> {
        wifi::Wifi::find(Arc::clone(self.proxies().await?)).await
    }

    pub async fn ethernet(&self) -> anyhow::Result<ethernet::Ethernet> {
        ethernet::Ethernet::find(Arc::clone(self.proxies().await?)).await
    }

    /// Seconds left before a tried Ethernet change is reverted; 0 when none is on trial.
    #[must_use]
    pub fn ethernet_trial_left(&self) -> u32 {
        self.trial.left()
    }

    /// Applies only explicitly requested IP families, with a 60-second rollback.
    pub async fn try_ethernet(
        &self,
        ipv4: Option<&ethernet::Ipv4>,
        ipv6: Option<&ethernet::Ipv6>,
    ) -> anyhow::Result<u32> {
        self.trial
            .apply(
                self.ethernet().await?,
                ethernet::changes(ipv4, ipv6)?,
                Arc::clone(&self.changed),
            )
            .await
    }

    /// Keeps the pending trial only after its saved-profile update succeeds.
    pub async fn confirm_ethernet(&self) -> anyhow::Result<()> {
        self.trial.confirm(&self.ethernet().await?).await
    }

    #[must_use]
    pub fn wifi_mode(&self) -> WifiMode {
        *self.wifi_mode.lock().expect("wifi mode lock")
    }

    /// Changes the Wi-Fi mode and applies it now.
    pub async fn set_wifi_mode(&self, mode: WifiMode) -> anyhow::Result<()> {
        *self.wifi_mode.lock().expect("wifi mode lock") = mode;
        self.enforce_wifi_mode().await.map(drop)
    }

    /// Switches the Wi-Fi radio as the mode says, given the Ethernet link. Returns the change
    /// made (`Some(on)`), or `None` when the radio was already right.
    pub async fn enforce_wifi_mode(&self) -> anyhow::Result<Option<bool>> {
        let ethernet_up = match self.ethernet().await {
            Ok(eth) => eth.link_up().await.unwrap_or(false),
            Err(_) => false,
        };
        let want = self.wifi_mode().radio_on(ethernet_up);
        let nm: NetworkManagerRadioProxy<'static> = self
            .proxies()
            .await?
            .get(
                "org.freedesktop.NetworkManager",
                "/org/freedesktop/NetworkManager".try_into()?,
                "org.freedesktop.NetworkManager",
            )
            .await?;
        if nm.wireless_enabled().await? == want {
            return Ok(None);
        }
        nm.set_wireless_enabled(want)
            .await
            .map_err(|e| anyhow::anyhow!("NetworkManager could not switch the Wi-Fi radio: {e}"))?;
        tracing::info!(
            mode = self.wifi_mode().as_str(),
            ethernet_up,
            radio = want,
            "Wi-Fi radio switched"
        );
        Ok(Some(want))
    }

    /// Whether the Wi-Fi radio is on.
    pub async fn radio_on(&self) -> anyhow::Result<bool> {
        let nm: NetworkManagerRadioProxy<'static> = self
            .proxies()
            .await?
            .get(
                "org.freedesktop.NetworkManager",
                "/org/freedesktop/NetworkManager".try_into()?,
                "org.freedesktop.NetworkManager",
            )
            .await?;
        Ok(nm.wireless_enabled().await?)
    }

    /// Keeps the Wi-Fi mode applied while the daemon runs (Ethernet plugged or unplugged),
    /// reporting radio changes to the activity feed.
    pub async fn watch(self: Arc<Self>, hub: Arc<crate::hub::Hub>) {
        let mut ticker = tokio::time::interval(WATCH_EVERY);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut warned = false;
        let mut signals = self.signals().await.ok();
        ticker.tick().await;
        loop {
            match self.enforce_wifi_mode().await {
                Ok(Some(on)) => {
                    warned = false;
                    let text = match (self.wifi_mode(), on) {
                        (WifiMode::Fallback, true) => "Ethernet is down: Wi-Fi on (fallback)",
                        (WifiMode::Fallback, false) => "Ethernet is back: Wi-Fi off (fallback)",
                        (_, true) => "Wi-Fi on",
                        (_, false) => "Wi-Fi off",
                    };
                    hub.report(
                        crate::hub::DAEMON,
                        pb::ActivityLevel::ACTIVITY_LEVEL_INFO,
                        text.to_owned(),
                    );
                }
                Ok(None) => warned = false,
                Err(e) if !warned => {
                    warned = true;
                    tracing::warn!(error = %format!("{e:#}"), "cannot apply the Wi-Fi mode; will keep trying");
                }
                Err(_) => {}
            }
            if let Some(stream) = &mut signals {
                use futures_util::{FutureExt as _, StreamExt as _};
                tokio::select! {
                    message = stream.next() => if message.is_none() { signals = None; } else {
                        // Let zbus populate property caches after this signal burst.
                        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                        while stream.next().now_or_never().flatten().is_some() {}
                    },
                    _ = ticker.tick() => {}
                }
            } else {
                // Failed subscriptions retain the old <=5 s fallback reaction and retry.
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                signals = self.signals().await.ok();
            }
        }
    }

    /// The Bluetooth adapter.
    pub async fn bluetooth(&self) -> anyhow::Result<&bluetooth::Bluetooth> {
        let proxies = Arc::clone(self.proxies().await?);
        self.bluetooth
            .get_or_try_init(|| bluetooth::Bluetooth::find(proxies))
            .await
    }
}

/// The API's view (H15-38): status, networks and devices as messages.
impl Network {
    /// Wi-Fi and Bluetooth status; problems (no NetworkManager, no Bluetooth stack) are reported,
    /// not returned as errors, so the settings page can show what is missing.
    pub async fn status(&self) -> pb::GetNetworkResponse {
        if !self.manage {
            return pb::GetNetworkResponse::default();
        }
        let wifi = match self.wifi().await {
            Ok(w) => match w.status().await {
                Ok(s) => pb::WifiStatus {
                    available: true,
                    interface: s.interface,
                    connected_ssid: s.connected_ssid,
                    gateway: s.ip.gateway4.clone(),
                    gateway6: s.ip.gateway6.clone(),
                    dns: s.ip.dns(),
                    addresses: s.ip.all(),
                    address_details: s.ip.messages(),
                    radio_on: self.radio_on().await.unwrap_or(false),
                    ..Default::default()
                },
                Err(e) => problem_wifi(&e),
            },
            Err(e) => problem_wifi(&e),
        };
        let ethernet = match self.ethernet().await {
            Ok(e) => match e.status().await {
                Ok(s) => pb::EthernetStatus {
                    available: true,
                    interface: s.interface,
                    carrier: s.carrier,
                    connected: s.connected,
                    dhcp: s.dhcp,
                    address: s.address,
                    prefix: s.prefix,
                    gateway: s.gateway,
                    dns: s.ip.dns(),
                    addresses: s.ip.all(),
                    address_details: s.ip.messages(),
                    gateway6: s.ip.gateway6.clone(),
                    ipv6_method: s.ipv6_method,
                    ipv4_dns: s.ip.dns4.clone(),
                    ipv6_config: buffa::MessageField::some(pb::EthernetIpv6Settings {
                        method: s.ipv6.method,
                        address: s.ipv6.address,
                        prefix: s.ipv6.prefix,
                        gateway: s.ipv6.gateway,
                        dns: s.ipv6.dns,
                        ..Default::default()
                    }),
                    revert_in_seconds: self.ethernet_trial_left(),
                    ..Default::default()
                },
                Err(e) => problem_ethernet(&e),
            },
            Err(e) => problem_ethernet(&e),
        };
        let bluetooth = match self.bluetooth().await {
            Ok(b) => match b.status().await {
                Ok(s) => pb::BluetoothStatus {
                    available: true,
                    powered: s.powered,
                    name: s.name,
                    ..Default::default()
                },
                Err(e) => problem_bluetooth(&e),
            },
            Err(e) => problem_bluetooth(&e),
        };
        pb::GetNetworkResponse {
            managed: true,
            wifi_mode: self.wifi_mode().as_str().to_owned(),
            wifi: buffa::MessageField::some(wifi),
            ethernet: buffa::MessageField::some(ethernet),
            bluetooth: buffa::MessageField::some(bluetooth),
            ..Default::default()
        }
    }

    pub async fn wifi_networks(&self, rescan: bool) -> anyhow::Result<Vec<pb::WifiNetwork>> {
        let list = self.wifi().await?.networks(rescan).await?;
        Ok(list
            .into_iter()
            .map(|n| pb::WifiNetwork {
                ssid: n.ssid,
                signal: u32::from(n.signal),
                secure: n.secure,
                frequency_mhz: n.frequency_mhz,
                in_use: n.in_use,
                saved: n.saved,
                ..Default::default()
            })
            .collect())
    }

    /// The devices BlueZ knows, and whether a scan runs; `scan` starts or extends one (H15-45).
    pub async fn bluetooth_devices(&self, scan: bool) -> anyhow::Result<(Vec<pb::BluetoothDevice>, bool)> {
        let bt = self.bluetooth().await?;
        if scan {
            bt.scan().await?;
        }
        let devices = bt.devices().await?.into_iter().map(device_message).collect();
        Ok((devices, bt.scanning().await))
    }

    /// Status and Bluetooth devices, as pushed to the UI (H15-49).
    pub async fn state(&self) -> pb::NetworkState {
        let status = self.status().await;
        let bluetooth = status.bluetooth.as_option().is_some_and(|b| b.available);
        let (devices, scanning) = if bluetooth {
            self.bluetooth_devices(false).await.unwrap_or_default()
        } else {
            (Vec::new(), false)
        };
        pb::NetworkState {
            status: buffa::MessageField::some(status),
            bluetooth_devices: devices,
            bluetooth_scanning: scanning,
            ..Default::default()
        }
    }

    pub async fn bluetooth_detail(&self, address: &str) -> anyhow::Result<pb::BluetoothDeviceDetail> {
        let d = self.bluetooth().await?.detail(address).await?;
        Ok(pb::BluetoothDeviceDetail {
            alias: d.alias,
            address_type: d.address_type,
            class: d.class.map(|c| format!("{c:#08x}")).unwrap_or_default(),
            class_text: d.class.map(names::class).unwrap_or_default(),
            trusted: d.trusted,
            blocked: d.blocked,
            bonded: d.bonded,
            legacy_pairing: d.legacy_pairing,
            rssi: d.rssi.map(i32::from),
            battery: d.battery.map(u32::from),
            battery_source: d.battery_source,
            tx_power: d.tx_power.map(i32::from),
            services_resolved: d.services_resolved,
            wake_allowed: d.wake_allowed,
            appearance: d.appearance.map(names::appearance).unwrap_or_default(),
            manufacturer_data: d
                .manufacturer_data
                .iter()
                .map(|(id, data)| pb::BluetoothManufacturerData {
                    company_id: u32::from(*id),
                    company: names::company(*id).to_owned(),
                    data_hex: codec::hex(data),
                    ..Default::default()
                })
                .collect(),
            device_id: names::device_id(&d.modalias),
            adapter_name: d.controller.name,
            adapter_device_id: names::device_id(&d.controller.modalias),
            adapter_roles: d.controller.roles,
            adapter_discoverable: d.controller.discoverable,
            adapter_pairable: d.controller.pairable,
            link: d.link.map(link_message).into(),
            modalias: d.modalias,
            profiles: d
                .uuids
                .iter()
                .map(|u| pb::BluetoothProfile {
                    name: names::profile(u).to_owned(),
                    uuid: u.clone(),
                    ..Default::default()
                })
                .collect(),
            transports: d.transports.iter().map(transport_message).collect(),
            adapter: d.adapter,
            adapter_address: d.adapter_address,
            adapter_bus: d.adapter_bus,
            device: buffa::MessageField::some(device_message(d.device)),
            ..Default::default()
        })
    }
}

fn device_message(d: bluetooth::Device) -> pb::BluetoothDevice {
    pb::BluetoothDevice {
        address: d.address,
        name: d.name,
        paired: d.paired,
        connected: d.connected,
        audio: d.audio,
        icon: d.icon,
        codec: d.codec,
        ..Default::default()
    }
}

fn link_message(link: Result<bluetooth::Link, String>) -> pb::BluetoothLink {
    match link {
        Ok(l) => pb::BluetoothLink {
            rssi: l.rssi.map(i32::from),
            tx_power: l.tx_power.map(i32::from),
            max_tx_power: l.max_tx_power.map(i32::from),
            ..Default::default()
        },
        Err(problem) => pb::BluetoothLink {
            problem,
            ..Default::default()
        },
    }
}

fn transport_message(t: &bluetooth::Transport) -> pb::BluetoothTransport {
    let profile = names::profile(&t.uuid);
    pb::BluetoothTransport {
        profile: if profile.is_empty() {
            t.uuid.clone()
        } else {
            profile.to_owned()
        },
        state: t.state.clone(),
        codec: t.codec.label(),
        configuration: t.codec.configuration(),
        configuration_hex: codec::hex(&t.configuration),
        volume: t.volume.map(u32::from),
        bitrate: t.codec.bitrate.clone(),
        delay_ms: t.delay.map(|d| f64::from(d) / 10.0),
        ..Default::default()
    }
}

fn problem_wifi(e: &anyhow::Error) -> pb::WifiStatus {
    pb::WifiStatus {
        problem: format!("{e:#}"),
        ..Default::default()
    }
}

fn problem_ethernet(e: &anyhow::Error) -> pb::EthernetStatus {
    pb::EthernetStatus {
        problem: format!("{e:#}"),
        ..Default::default()
    }
}

fn problem_bluetooth(e: &anyhow::Error) -> pb::BluetoothStatus {
    pb::BluetoothStatus {
        problem: format!("{e:#}"),
        ..Default::default()
    }
}

/// An id that tells one trial from the next.
fn rand_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}
