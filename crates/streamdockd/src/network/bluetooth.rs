//! Bluetooth through BlueZ's D-Bus API (H15-38): power, scan, pair + trust + connect,
//! disconnect, remove. Audio routing is the audio server's job (PipeWire on the board); this
//! manages the devices and reads the codec the audio streams use (H15-49).
//!
//! Pairing needs an agent. Ours has no input or output ("just works") and only accepts the device
//! the user is pairing from the settings page; anything else that asks is rejected, so nobody can
//! pair with the board unasked.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, bail};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

const BLUEZ: &str = "org.bluez";
const AGENT_PATH: &str = "/org/streamdockd/agent";
/// How long a scan runs in the background (H15-45): a classic inquiry takes ~10 s, and names
/// arrive afterwards through separate name requests, so the page polls while it runs.
pub const SCAN_TIME: Duration = Duration::from_secs(30);
/// UUIDs of audio profiles: A2DP sink, headset, hands-free, A2DP source.
const AUDIO_UUIDS: [&str; 4] = [
    "0000110b-0000-1000-8000-00805f9b34fb",
    "00001108-0000-1000-8000-00805f9b34fb",
    "0000111e-0000-1000-8000-00805f9b34fb",
    "0000110a-0000-1000-8000-00805f9b34fb",
];

#[zbus::proxy(interface = "org.bluez.Adapter1", default_service = "org.bluez")]
trait Adapter {
    fn start_discovery(&self) -> zbus::Result<()>;
    fn stop_discovery(&self) -> zbus::Result<()>;
    fn remove_device(&self, device: &zbus::zvariant::ObjectPath<'_>) -> zbus::Result<()>;
    #[zbus(property)]
    fn powered(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn set_powered(&self, value: bool) -> zbus::Result<()>;
    #[zbus(property)]
    fn name(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn discovering(&self) -> zbus::Result<bool>;
}

#[zbus::proxy(interface = "org.bluez.Device1", default_service = "org.bluez")]
trait Device {
    fn pair(&self) -> zbus::Result<()>;
    fn connect(&self) -> zbus::Result<()>;
    fn disconnect(&self) -> zbus::Result<()>;
    #[zbus(property)]
    fn set_trusted(&self, value: bool) -> zbus::Result<()>;
    #[zbus(property)]
    fn paired(&self) -> zbus::Result<bool>;
}

#[zbus::proxy(
    interface = "org.bluez.AgentManager1",
    default_service = "org.bluez",
    default_path = "/org/bluez"
)]
trait AgentManager {
    fn register_agent(&self, agent: &zbus::zvariant::ObjectPath<'_>, capability: &str) -> zbus::Result<()>;
    fn unregister_agent(&self, agent: &zbus::zvariant::ObjectPath<'_>) -> zbus::Result<()>;
}

/// A device BlueZ knows: in range from a scan, or paired before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub address: String,
    pub name: String,
    pub paired: bool,
    pub connected: bool,
    /// Offers an audio profile (speaker, headset).
    pub audio: bool,
    /// BlueZ's icon name, e.g. "audio-headset".
    pub icon: String,
    /// The codec of its audio stream, e.g. "LDAC 48 kHz"; empty without one.
    pub codec: String,
    /// Battery level in percent, where the device reports one (H15-91).
    pub battery: Option<u8>,
}

/// Everything BlueZ says about a device (H15-49).
#[allow(clippy::struct_excessive_bools, reason = "BlueZ's flags, shown as they are")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detail {
    pub device: Device,
    pub alias: String,
    pub address_type: String,
    pub class: Option<u32>,
    pub trusted: bool,
    pub blocked: bool,
    /// `None` where BlueZ doesn't report it (before 5.69).
    pub bonded: Option<bool>,
    pub legacy_pairing: bool,
    pub rssi: Option<i16>,
    pub battery: Option<u8>,
    /// Where the battery level comes from, e.g. "HFP" (BlueZ 5.66+); empty where not said.
    pub battery_source: String,
    pub modalias: String,
    /// Advertised transmit power in dBm (from a scan).
    pub tx_power: Option<i16>,
    pub services_resolved: bool,
    pub wake_allowed: bool,
    /// GAP appearance (LE devices).
    pub appearance: Option<u16>,
    /// Advertised manufacturer data: company id and payload.
    pub manufacturer_data: Vec<(u16, Vec<u8>)>,
    /// The profiles' UUIDs.
    pub uuids: Vec<String>,
    pub transports: Vec<Transport>,
    /// "hci0", its address and bus ("UART", "USB" …).
    pub adapter: String,
    pub adapter_address: String,
    pub adapter_bus: String,
    pub controller: Controller,
    /// The live link of a connected device (H15-53), or why it could not be read; `None` while
    /// not connected.
    pub link: Option<Result<Link, String>>,
}

/// A connection's live state from the kernel (`btmgmt conn-info`, MGMT "Get Connection
/// Information"); BlueZ's D-Bus API has no RSSI while connected. Needs `CAP_NET_ADMIN`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Link {
    /// dBm; `None` where the controller doesn't know (127).
    pub rssi: Option<i8>,
    pub tx_power: Option<i8>,
    pub max_tx_power: Option<i8>,
}

/// More about the controller (H15-53), from BlueZ's `Adapter1`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Controller {
    /// Its name as others see it, e.g. "luckfox".
    pub name: String,
    /// e.g. "usb:v1D6Bp0246d0540" (BlueZ's own device ID).
    pub modalias: String,
    /// "central", "peripheral" (BlueZ 5.56+).
    pub roles: Vec<String>,
    pub discoverable: bool,
    pub pairable: bool,
}

/// An audio stream of a device (`MediaTransport1`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transport {
    pub uuid: String,
    pub state: String,
    pub codec: super::codec::Codec,
    pub configuration: Vec<u8>,
    pub volume: Option<u16>,
    /// The delay the device reports, in 1/10 ms (A2DP delay reporting).
    pub delay: Option<u16>,
}

/// The adapter's state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub name: String,
    pub powered: bool,
}

/// The device being paired from the settings page; the agent accepts only it.
type PairingTarget = Arc<Mutex<Option<OwnedObjectPath>>>;

/// BlueZ's first adapter.
#[derive(Debug, Clone)]
pub struct Bluetooth {
    bus: zbus::Connection,
    proxies: Arc<super::proxies::Proxies>,
    adapter: OwnedObjectPath,
    pairing: PairingTarget,
    /// Counts scans; a scan's timer only stops discovery if no newer scan started since.
    scans: Arc<AtomicU64>,
}

type Objects =
    HashMap<OwnedObjectPath, HashMap<zbus::names::OwnedInterfaceName, HashMap<String, OwnedValue>>>;

impl Bluetooth {
    /// Finds the first adapter. Our pairing agent is only registered while pairing.
    pub(super) async fn find(proxies: Arc<super::proxies::Proxies>) -> anyhow::Result<Self> {
        let bus = proxies.bus();
        let objects = managed_objects(&proxies).await?;
        let adapter = objects
            .iter()
            .find(|(_, ifaces)| ifaces.keys().any(|i| i.as_str() == "org.bluez.Adapter1"))
            .map(|(path, _)| path.clone())
            .context("BlueZ has no Bluetooth adapter")?;
        let pairing = PairingTarget::default();
        Ok(Self {
            bus: bus.clone(),
            proxies: Arc::clone(&proxies),
            adapter,
            pairing,
            scans: Arc::default(),
        })
    }

    async fn adapter(&self) -> zbus::Result<AdapterProxy<'static>> {
        self.proxies
            .get::<AdapterProxy<'static>>(BLUEZ, self.adapter.clone(), "org.bluez.Adapter1")
            .await
    }

    async fn device(&self, address: &str) -> anyhow::Result<(OwnedObjectPath, DeviceProxy<'static>)> {
        let objects = managed_objects(&self.proxies).await?;
        let path = objects
            .iter()
            .find(|(_, ifaces)| {
                device_props(ifaces).is_some_and(|p| text(p, "Address").eq_ignore_ascii_case(address))
            })
            .map(|(path, _)| path.clone())
            .with_context(|| format!("no Bluetooth device {address}"))?;
        let proxy = self
            .proxies
            .get::<DeviceProxy<'static>>(BLUEZ, path.clone(), "org.bluez.Device1")
            .await?;
        Ok((path, proxy))
    }

    pub async fn status(&self) -> anyhow::Result<Status> {
        let adapter = self.adapter().await?;
        Ok(Status {
            name: adapter.name().await.unwrap_or_default(),
            powered: adapter.powered().await?,
        })
    }

    pub async fn set_powered(&self, on: bool) -> anyhow::Result<()> {
        self.adapter()
            .await?
            .set_powered(on)
            .await
            .context("BlueZ could not switch the adapter (rfkill blocked?)")
    }

    /// Starts (or extends) a scan of [`SCAN_TIME`] in the background, powering the adapter on;
    /// returns at once. Devices appear in [`Bluetooth::devices`] while it runs.
    pub async fn scan(&self) -> anyhow::Result<()> {
        let adapter = self.adapter().await?;
        if !adapter.powered().await? {
            self.set_powered(true).await?;
        }
        if !adapter.discovering().await.unwrap_or(false) {
            adapter
                .start_discovery()
                .await
                .context("BlueZ could not start a scan")?;
        }
        let scan = self.scans.fetch_add(1, Ordering::SeqCst) + 1;
        let scans = Arc::clone(&self.scans);
        tokio::spawn(async move {
            tokio::time::sleep(SCAN_TIME).await;
            if scans.load(Ordering::SeqCst) == scan {
                let _ = adapter.stop_discovery().await;
            }
        });
        Ok(())
    }

    /// Whether a scan is running.
    pub async fn scanning(&self) -> bool {
        match self.adapter().await {
            Ok(a) => a.discovering().await.unwrap_or(false),
            Err(_) => false,
        }
    }

    /// The devices BlueZ knows, connected and paired first, then by name.
    pub async fn devices(&self) -> anyhow::Result<Vec<Device>> {
        let objects = managed_objects(&self.proxies).await?;
        let transports = transports(&objects);
        let mut list: Vec<Device> = objects
            .iter()
            .filter_map(|(path, ifaces)| {
                let mut device = device_from(device_props(ifaces)?);
                device.codec = codec_label(transports.get(path.as_str()).map(Vec::as_slice));
                device.battery = battery(ifaces);
                Some(device)
            })
            .collect();
        // Connected, paired, audio and named devices first; unnamed ones (still resolving) last.
        list.sort_by(|a, b| {
            (b.connected, b.paired, b.audio, !b.name.is_empty())
                .cmp(&(a.connected, a.paired, a.audio, !a.name.is_empty()))
                .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(list)
    }

    /// Everything about `address`: flags, profiles, battery, transports and the adapter.
    pub async fn detail(&self, address: &str) -> anyhow::Result<Detail> {
        let objects = managed_objects(&self.proxies).await?;
        let (path, ifaces) = objects
            .iter()
            .find(|(_, ifaces)| {
                device_props(ifaces).is_some_and(|p| text(p, "Address").eq_ignore_ascii_case(address))
            })
            .with_context(|| format!("no Bluetooth device {address}"))?;
        let props = device_props(ifaces).expect("found by its Device1 properties");
        let transports = transports(&objects).remove(path.as_str()).unwrap_or_default();
        let mut device = device_from(props);
        device.codec = codec_label(Some(&transports));
        let adapter_path = props
            .get("Adapter")
            .and_then(|v| OwnedObjectPath::try_from(v.try_clone().ok()?).ok())
            .unwrap_or_else(|| self.adapter.clone());
        let adapter_props = objects
            .get(&adapter_path)
            .and_then(|i| interface(i, "org.bluez.Adapter1"));
        let adapter = adapter_path
            .as_str()
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_owned();
        let link = if device.connected {
            Some(conn_info(&adapter, address, &text(props, "AddressType")).await)
        } else {
            None
        };
        Ok(Detail {
            link,
            alias: text(props, "Alias"),
            address_type: text(props, "AddressType"),
            class: props.get("Class").and_then(|v| u32::try_from(v).ok()),
            trusted: flag(props, "Trusted"),
            blocked: flag(props, "Blocked"),
            bonded: props.get("Bonded").and_then(|v| bool::try_from(v).ok()),
            legacy_pairing: flag(props, "LegacyPairing"),
            rssi: props.get("RSSI").and_then(|v| i16::try_from(v).ok()),
            battery: battery(ifaces),
            battery_source: interface(ifaces, "org.bluez.Battery1")
                .map(|b| text(b, "Source"))
                .unwrap_or_default(),
            modalias: text(props, "Modalias"),
            tx_power: props.get("TxPower").and_then(|v| i16::try_from(v).ok()),
            services_resolved: flag(props, "ServicesResolved"),
            wake_allowed: flag(props, "WakeAllowed"),
            appearance: props.get("Appearance").and_then(|v| u16::try_from(v).ok()),
            manufacturer_data: manufacturer_data(props),
            controller: adapter_props.map(controller).unwrap_or_default(),
            uuids: uuids(props),
            transports,
            adapter_address: adapter_props.map(|p| text(p, "Address")).unwrap_or_default(),
            adapter_bus: adapter_bus(&adapter),
            adapter,
            device,
        })
    }

    /// Pairs (if needed), trusts and connects `address`.
    pub async fn connect(&self, address: &str) -> anyhow::Result<()> {
        let (path, device) = self.device(address).await?;
        if !device.paired().await? {
            // Our agent exists only while we pair: BlueZ asks the agent of the app that calls
            // Pair(), so we never take over the machine's default agent (e.g. a desktop's).
            register_agent(&self.bus, &self.pairing).await?;
            *self.pairing.lock().expect("pairing lock") = Some(path.clone());
            let paired = device.pair().await;
            *self.pairing.lock().expect("pairing lock") = None;
            unregister_agent(&self.bus).await;
            paired.map_err(|e| anyhow::anyhow!("pairing with {address} failed: {}", bluez_error(&e)))?;
        }
        device
            .set_trusted(true)
            .await
            .context("BlueZ could not trust the device")?;
        device
            .connect()
            .await
            .map_err(|e| anyhow::anyhow!("connecting {address} failed: {}", bluez_error(&e)))
    }

    pub async fn disconnect(&self, address: &str) -> anyhow::Result<()> {
        let (_, device) = self.device(address).await?;
        device
            .disconnect()
            .await
            .map_err(|e| anyhow::anyhow!("disconnecting {address} failed: {}", bluez_error(&e)))
    }

    /// Unpairs and forgets `address`.
    pub async fn remove(&self, address: &str) -> anyhow::Result<()> {
        let (path, _) = self.device(address).await?;
        self.adapter()
            .await?
            .remove_device(&path)
            .await
            .map_err(|e| anyhow::anyhow!("removing {address} failed: {}", bluez_error(&e)))
    }
}

async fn managed_objects(proxies: &super::proxies::Proxies) -> anyhow::Result<Objects> {
    let manager: zbus::fdo::ObjectManagerProxy<'static> = proxies
        .get(BLUEZ, "/".try_into()?, "org.freedesktop.DBus.ObjectManager")
        .await?;
    manager
        .get_managed_objects()
        .await
        .context("BlueZ is not running (no Bluetooth stack?)")
}

type Interfaces = HashMap<zbus::names::OwnedInterfaceName, HashMap<String, OwnedValue>>;

fn interface<'a>(ifaces: &'a Interfaces, name: &str) -> Option<&'a HashMap<String, OwnedValue>> {
    ifaces.iter().find(|(i, _)| i.as_str() == name).map(|(_, p)| p)
}

fn device_props(ifaces: &Interfaces) -> Option<&HashMap<String, OwnedValue>> {
    interface(ifaces, "org.bluez.Device1")
}

/// The audio transports by device path.
fn transports(objects: &Objects) -> HashMap<String, Vec<Transport>> {
    let mut out: HashMap<String, Vec<Transport>> = HashMap::new();
    for props in objects
        .values()
        .filter_map(|i| interface(i, "org.bluez.MediaTransport1"))
    {
        let device = props
            .get("Device")
            .and_then(|v| OwnedObjectPath::try_from(v.try_clone().ok()?).ok());
        let Some(device) = device else { continue };
        let uuid = text(props, "UUID");
        let configuration: Vec<u8> = props
            .get("Configuration")
            .and_then(|v| Vec::<u8>::try_from(v.try_clone().ok()?).ok())
            .unwrap_or_default();
        let codec = props.get("Codec").and_then(|v| u8::try_from(v).ok()).unwrap_or(0);
        out.entry(device.as_str().to_owned())
            .or_default()
            .push(Transport {
                codec: super::codec::decode(codec, &configuration, &uuid),
                state: text(props, "State"),
                volume: props.get("Volume").and_then(|v| u16::try_from(v).ok()),
                delay: props.get("Delay").and_then(|v| u16::try_from(v).ok()),
                configuration,
                uuid,
            });
    }
    out
}

/// The codec to show for a device: its active stream's, else any stream's.
fn codec_label(transports: Option<&[Transport]>) -> String {
    let transports = transports.unwrap_or_default();
    transports
        .iter()
        .find(|t| t.state == "active")
        .or_else(|| transports.first())
        .map(|t| t.codec.label())
        .unwrap_or_default()
}

/// How long `btmgmt` may take; it answers in milliseconds.
const BTMGMT_TIMEOUT: Duration = Duration::from_secs(3);

/// The live link of a connected device via `btmgmt conn-info` (H15-53). BR/EDR first (audio
/// devices); a device with an LE public address is tried as LE when that finds no connection.
async fn conn_info(adapter: &str, address: &str, address_type: &str) -> Result<Link, String> {
    let index = adapter.trim_start_matches("hci");
    let types: &[&str] = match address_type {
        "random" => &["2"],
        _ => &["0", "1"],
    };
    let mut problem = String::new();
    for kind in types {
        let args = ["--index", index, "conn-info", "-t", kind, address];
        let run = tokio::process::Command::new("btmgmt")
            .args(args)
            .kill_on_drop(true)
            .output();
        let out = match tokio::time::timeout(BTMGMT_TIMEOUT, run).await {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => return Err(format!("cannot run btmgmt: {e}")),
            Err(_) => return Err("btmgmt did not answer".to_owned()),
        };
        // btmgmt exits 0 also when the request failed; the text says which.
        let text = String::from_utf8_lossy(&out.stdout) + String::from_utf8_lossy(&out.stderr);
        match parse_conn_info(&text) {
            Ok(link) => return Ok(link),
            Err(e) => problem = e,
        }
        if !problem.contains("Not Connected") {
            break;
        }
    }
    Err(problem)
}

/// Parses `btmgmt conn-info`: "\tRSSI -58\tTX power 4\tmaximum TX power 10", or the failure
/// ("… failed. status 0x14 (Permission Denied)").
fn parse_conn_info(text: &str) -> Result<Link, String> {
    let value = |field: &str| -> Option<i8> {
        let v: i32 = text
            .split('\t')
            .find_map(|part| part.trim().strip_prefix(field)?.trim().parse().ok())?;
        // 127 is HCI's "not available".
        i8::try_from(v).ok().filter(|&v| v != 127)
    };
    if text.contains("RSSI ") && !text.contains("failed") {
        return Ok(Link {
            rssi: value("RSSI "),
            tx_power: value("TX power "),
            max_tx_power: value("maximum TX power "),
        });
    }
    let reason = text
        .split_once("status ")
        .and_then(|(_, rest)| rest.split_once('(')?.1.split_once(')'))
        .map_or_else(|| strip_ansi(text).trim().to_owned(), |(r, _)| r.to_owned());
    if reason.contains("Permission Denied") {
        return Err("needs CAP_NET_ADMIN (granted to the service on the Luckfox)".to_owned());
    }
    Err(reason)
}

/// Drops terminal colour codes (`\x1b[0;91m`).
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            chars.by_ref().find(|&c| c == 'm');
        } else {
            out.push(c);
        }
    }
    out
}

/// `ManufacturerData` (`a{qv}`, each value a byte array), by company id.
fn manufacturer_data(props: &HashMap<String, OwnedValue>) -> Vec<(u16, Vec<u8>)> {
    let Some(map) = props
        .get("ManufacturerData")
        .and_then(|v| HashMap::<u16, OwnedValue>::try_from(v.try_clone().ok()?).ok())
    else {
        return Vec::new();
    };
    let mut out: Vec<(u16, Vec<u8>)> = map.into_iter().map(|(id, v)| (id, bytes(&v))).collect();
    out.sort_unstable();
    out
}

/// A byte array, also when it comes wrapped in a variant.
fn bytes(value: &Value<'_>) -> Vec<u8> {
    let inner = match value {
        Value::Value(inner) => inner.as_ref(),
        v => v,
    };
    inner
        .try_clone()
        .ok()
        .and_then(|v| Vec::<u8>::try_from(v).ok())
        .unwrap_or_default()
}

fn controller(props: &HashMap<String, OwnedValue>) -> Controller {
    Controller {
        name: text(props, "Alias"),
        modalias: text(props, "Modalias"),
        roles: props
            .get("Roles")
            .and_then(|v| Vec::<String>::try_from(v.try_clone().ok()?).ok())
            .unwrap_or_default(),
        discoverable: flag(props, "Discoverable"),
        pairable: flag(props, "Pairable"),
    }
}

fn uuids(props: &HashMap<String, OwnedValue>) -> Vec<String> {
    props
        .get("UUIDs")
        .and_then(|v| Vec::<String>::try_from(v.try_clone().ok()?).ok())
        .unwrap_or_default()
}

/// How the controller is attached, from its sysfs device path.
fn adapter_bus(adapter: &str) -> String {
    let Ok(target) = std::fs::read_link(format!("/sys/class/bluetooth/{adapter}")) else {
        return String::new();
    };
    bus_of(&target.to_string_lossy()).to_owned()
}

fn bus_of(sysfs: &str) -> &'static str {
    if sysfs.contains("/usb") {
        "USB"
    } else if sysfs.contains("/mmc") || sysfs.contains("sdio") {
        "SDIO"
    } else if sysfs.contains("serial") || sysfs.contains("/tty") || sysfs.contains("uart") {
        "UART"
    } else if sysfs.contains("/pci") {
        "PCI"
    } else {
        "other"
    }
}

fn text(props: &HashMap<String, OwnedValue>, key: &str) -> String {
    props
        .get(key)
        .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
        .unwrap_or_default()
}

fn flag(props: &HashMap<String, OwnedValue>, key: &str) -> bool {
    props
        .get(key)
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(false)
}

fn device_from(props: &HashMap<String, OwnedValue>) -> Device {
    let uuids = uuids(props);
    let address = text(props, "Address");
    let name = device_name(&address, &text(props, "Name"), &text(props, "Alias"));
    Device {
        name,
        paired: flag(props, "Paired"),
        connected: flag(props, "Connected"),
        audio: is_audio(&uuids),
        icon: text(props, "Icon"),
        codec: String::new(),
        battery: None,
        address,
    }
}

/// `org.bluez.Battery1`'s level, where BlueZ has one for the device.
fn battery(ifaces: &Interfaces) -> Option<u8> {
    interface(ifaces, "org.bluez.Battery1")
        .and_then(|b| b.get("Percentage"))
        .and_then(|v| u8::try_from(v).ok())
}

/// The name to show: BlueZ's `Name` once resolved, else an `Alias` the user set; empty while
/// neither exists (BlueZ's default alias is the address, "02-00-00-00-00-38", which is no name).
fn device_name(address: &str, name: &str, alias: &str) -> String {
    if !name.is_empty() {
        return name.to_owned();
    }
    let plain = |s: &str| s.replace(['-', ':'], "").to_ascii_uppercase();
    if alias.is_empty() || plain(alias) == plain(address) {
        String::new()
    } else {
        alias.to_owned()
    }
}

fn is_audio(uuids: &[String]) -> bool {
    uuids
        .iter()
        .any(|u| AUDIO_UUIDS.contains(&u.to_ascii_lowercase().as_str()))
}

/// BlueZ's message (e.g. "Authentication Failed") without the D-Bus error name.
fn bluez_error(e: &zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(name, Some(message), _) => format!(
            "{message} ({})",
            name.as_str().trim_start_matches("org.bluez.Error.")
        ),
        zbus::Error::MethodError(name, None, _) => {
            name.as_str().trim_start_matches("org.bluez.Error.").to_owned()
        }
        other => other.to_string(),
    }
}

/// The pairing agent: "just works", and only for the device being paired from the settings page.
struct Agent {
    pairing: PairingTarget,
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.bluez.Error")]
enum AgentError {
    Rejected(String),
}

impl Agent {
    fn check(&self, device: &zbus::zvariant::ObjectPath<'_>) -> Result<(), AgentError> {
        let expected = self.pairing.lock().expect("pairing lock").clone();
        if expected.as_ref().is_some_and(|p| p.as_str() == device.as_str()) {
            Ok(())
        } else {
            tracing::warn!(%device, "Bluetooth: rejected a pairing request nobody asked for");
            Err(AgentError::Rejected("not pairing with this device".into()))
        }
    }
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::unused_self,
    unused_variables,
    reason = "BlueZ's Agent1 interface fixes the signatures"
)]
#[zbus::interface(name = "org.bluez.Agent1")]
impl Agent {
    fn release(&self) {}

    fn request_pin_code(&self, device: zbus::zvariant::ObjectPath<'_>) -> Result<String, AgentError> {
        self.check(&device)?;
        Err(AgentError::Rejected("PIN codes are not supported".into()))
    }

    fn display_pin_code(
        &self,
        device: zbus::zvariant::ObjectPath<'_>,
        pincode: String,
    ) -> Result<(), AgentError> {
        self.check(&device)
    }

    fn request_passkey(&self, device: zbus::zvariant::ObjectPath<'_>) -> Result<u32, AgentError> {
        self.check(&device)?;
        Err(AgentError::Rejected("passkeys are not supported".into()))
    }

    fn display_passkey(
        &self,
        device: zbus::zvariant::ObjectPath<'_>,
        passkey: u32,
        entered: u16,
    ) -> Result<(), AgentError> {
        self.check(&device)
    }

    fn request_confirmation(
        &self,
        device: zbus::zvariant::ObjectPath<'_>,
        passkey: u32,
    ) -> Result<(), AgentError> {
        self.check(&device)
    }

    fn request_authorization(&self, device: zbus::zvariant::ObjectPath<'_>) -> Result<(), AgentError> {
        self.check(&device)
    }

    fn authorize_service(
        &self,
        device: zbus::zvariant::ObjectPath<'_>,
        uuid: String,
    ) -> Result<(), AgentError> {
        self.check(&device)
    }

    fn cancel(&self) {}
}

async fn register_agent(bus: &zbus::Connection, pairing: &PairingTarget) -> anyhow::Result<()> {
    let registered = bus
        .object_server()
        .at(
            AGENT_PATH,
            Agent {
                pairing: Arc::clone(pairing),
            },
        )
        .await?;
    if !registered {
        return Ok(()); // already registered on this connection
    }
    let manager = AgentManagerProxy::new(bus).await?;
    let path = zbus::zvariant::ObjectPath::try_from(AGENT_PATH)?;
    manager
        .register_agent(&path, "NoInputNoOutput")
        .await
        .context("BlueZ refused our pairing agent")?;
    Ok(())
}

async fn unregister_agent(bus: &zbus::Connection) {
    if let Ok(manager) = AgentManagerProxy::new(bus).await
        && let Ok(path) = zbus::zvariant::ObjectPath::try_from(AGENT_PATH)
        && let Err(e) = manager.unregister_agent(&path).await
    {
        tracing::debug!(error = %e, "Bluetooth: unregistering our agent failed");
    }
    let _ = bus.object_server().remove::<Agent, _>(AGENT_PATH).await;
}

/// Rejects an address that isn't `XX:XX:XX:XX:XX:XX`.
pub fn check_address(address: &str) -> anyhow::Result<()> {
    let ok = address.len() == 17
        && address
            .split(':')
            .all(|part| part.len() == 2 && part.chars().all(|c| c.is_ascii_hexdigit()));
    if !ok {
        bail!("{address:?} is not a Bluetooth address like 00:11:22:33:44:55");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// H15-53: `ManufacturerData` is `a{qv}`, each variant holding the payload bytes.
    #[test]
    fn reads_manufacturer_data() {
        let mut map: HashMap<u16, Value<'_>> = HashMap::new();
        map.insert(0x012d, Value::Value(Box::new(Value::from(vec![1u8, 2, 3]))));
        map.insert(0x004c, Value::from(vec![9u8]));
        let dict = zbus::zvariant::Dict::from(map);
        let mut props = HashMap::new();
        props.insert(
            "ManufacturerData".to_owned(),
            OwnedValue::try_from(Value::Dict(dict)).unwrap(),
        );
        assert_eq!(
            manufacturer_data(&props),
            vec![(0x004c, vec![9]), (0x012d, vec![1, 2, 3])]
        );
        assert_eq!(manufacturer_data(&HashMap::new()), Vec::new());
    }

    #[test]
    fn parses_conn_info() {
        let ok = "Connection Information for 02:00:00:00:00:B4 (BR/EDR)\n\tRSSI -58\tTX power 4\tmaximum TX power 127\n";
        assert_eq!(
            parse_conn_info(ok),
            Ok(Link {
                rssi: Some(-58),
                tx_power: Some(4),
                max_tx_power: None
            })
        );
        // Seen on the Luckfox (BlueZ 5.64) for a paired headset that is off.
        let off = "\x1b[0;91mGet Conn Info for 02:00:00:00:00:B4 (BR/EDR) failed. status 0x02 (Not Connected)\n\x1b[0m";
        assert_eq!(parse_conn_info(off), Err("Not Connected".to_owned()));
        let denied = "Get Conn Info for 02:00:00:00:00:B4 (BR/EDR) failed. status 0x14 (Permission Denied)\n";
        assert!(parse_conn_info(denied).unwrap_err().contains("CAP_NET_ADMIN"));
        assert_eq!(parse_conn_info("\x1b[0;91moops\x1b[0m\n"), Err("oops".to_owned()));
    }

    #[test]
    fn names_are_never_the_address() {
        let a = "02:00:00:00:00:38";
        assert_eq!(device_name(a, "WH-1000XM4", "02-00-00-00-00-38"), "WH-1000XM4");
        assert_eq!(
            device_name(a, "", "02-00-00-00-00-38"),
            "",
            "default alias = address"
        );
        assert_eq!(device_name(a, "", "Kitchen"), "Kitchen", "an alias the user set");
        assert_eq!(device_name(a, "", ""), "");
    }

    #[test]
    fn recognises_audio_devices() {
        assert!(is_audio(&["0000110B-0000-1000-8000-00805F9B34FB".into()]));
        assert!(!is_audio(&["00001124-0000-1000-8000-00805f9b34fb".into()]));
    }

    #[test]
    fn controller_bus() {
        assert_eq!(
            bus_of("../../devices/platform/ffb10000.serial/tty/ttyS1/hci0"),
            "UART"
        );
        assert_eq!(
            bus_of("../../devices/pci0000:00/0000:00:14.0/usb1/1-10/1-10:1.0/bluetooth/hci0"),
            "USB"
        );
    }

    #[test]
    fn checks_addresses() {
        assert!(check_address("00:1A:7d:da:71:13").is_ok());
        for bad in ["", "00:1A:7D:DA:71", "00-1A-7D-DA-71-13", "00:1A:7D:DA:71:1G"] {
            assert!(check_address(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_agent_accepts_only_the_device_being_paired() {
        let pairing = PairingTarget::default();
        let agent = Agent {
            pairing: Arc::clone(&pairing),
        };
        let speaker = zbus::zvariant::ObjectPath::try_from("/org/bluez/hci0/dev_00_11_22_33_44_55").unwrap();
        let stranger = zbus::zvariant::ObjectPath::try_from("/org/bluez/hci0/dev_66_77_88_99_AA_BB").unwrap();
        assert!(agent.check(&speaker).is_err(), "nothing is being paired");
        *pairing.lock().unwrap() = Some(speaker.clone().into());
        assert!(agent.check(&speaker).is_ok());
        assert!(agent.check(&stranger).is_err());
    }
}
