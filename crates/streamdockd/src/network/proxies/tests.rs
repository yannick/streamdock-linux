use super::*;
use std::{
    io::BufRead as _,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU32, Ordering},
};
struct Bus(Child);
impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Counter {
    reads: Arc<AtomicU32>,
    value: u32,
}
#[zbus::interface(name = "org.streamdock.Test")]
impl Counter {
    #[zbus(property)]
    fn value(&self) -> u32 {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.value
    }
    #[zbus(property)]
    fn set_value(&mut self, value: u32) {
        self.value = value;
    }
}
const SERVICE: &str = "org.freedesktop.NetworkManager";
async fn cached(cache: &Proxies) -> zbus::Proxy<'static> {
    cache
        .get(SERVICE, "/test".try_into().unwrap(), "org.streamdock.Test")
        .await
        .unwrap()
}
async fn server(address: &str, reads: &Arc<AtomicU32>, value: u32) -> zbus::Connection {
    zbus::connection::Builder::address(address)
        .unwrap()
        .name(SERVICE)
        .unwrap()
        .serve_at(
            "/test",
            Counter {
                reads: Arc::clone(reads),
                value,
            },
        )
        .unwrap()
        .build()
        .await
        .unwrap()
}
#[tokio::test]
#[ignore = "H15-67: explicit private-bus integration test requires dbus-daemon"]
async fn reuses_properties_and_handles_updates_removals_and_service_restart() {
    use futures_util::StreamExt as _;
    let mut child = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut address = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut address)
        .unwrap();
    let _bus = Bus(child);
    let reads = Arc::new(AtomicU32::new(0));
    let service = server(address.trim(), &reads, 7).await;
    let client = zbus::connection::Builder::address(address.trim())
        .unwrap()
        .build()
        .await
        .unwrap();
    let network = super::super::Network::new(true, super::super::WifiMode::Both);
    network.bus.set(client).unwrap();
    let cache = Arc::clone(network.proxies().await.unwrap());
    let mut signals = network.signals().await.unwrap();
    for _ in 0..5 {
        assert_eq!(
            cached(&cache).await.get_property::<u32>("Value").await.unwrap(),
            7
        );
    }
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    let proxy = cached(&cache).await;
    proxy.set_property("Value", &9_u32).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while proxy.get_property::<u32>("Value").await.unwrap() != 9 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let before = reads.load(Ordering::SeqCst);
    for _ in 0..5 {
        assert_eq!(
            cached(&cache).await.get_property::<u32>("Value").await.unwrap(),
            9
        );
    }
    assert_eq!(reads.load(Ordering::SeqCst), before);
    service
        .emit_signal(
            None::<&str>,
            "/org/freedesktop/NetworkManager",
            "org.freedesktop.NetworkManager",
            "DeviceRemoved",
            &(OwnedObjectPath::try_from("/test").unwrap(),),
        )
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), signals.next())
        .await
        .unwrap()
        .unwrap();
    assert!(cache.entries.lock().await.is_empty());
    assert_eq!(
        cached(&cache).await.get_property::<u32>("Value").await.unwrap(),
        9
    );
    assert_eq!(reads.load(Ordering::SeqCst), before + 1);
    service.close().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), signals.next())
        .await
        .unwrap()
        .unwrap();
    assert!(cache.entries.lock().await.is_empty());
    let _replacement = server(address.trim(), &reads, 11).await;
    tokio::time::timeout(std::time::Duration::from_secs(1), signals.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        cached(&cache).await.get_property::<u32>("Value").await.unwrap(),
        11
    );
}

const DEVICE: &str = "/org/freedesktop/NetworkManager/Devices/1";
struct Radio(Arc<std::sync::atomic::AtomicBool>);
#[allow(
    clippy::unused_self,
    reason = "H15-67: D-Bus fixture property getters require an instance"
)]
#[zbus::interface(name = "org.freedesktop.NetworkManager")]
impl Radio {
    #[zbus(property)]
    fn devices(&self) -> Vec<OwnedObjectPath> {
        vec![DEVICE.try_into().unwrap()]
    }
    #[zbus(property)]
    fn wireless_enabled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    #[zbus(property)]
    fn set_wireless_enabled(&mut self, enabled: bool) {
        self.0.store(enabled, Ordering::SeqCst);
    }
}
struct DeviceState {
    state: u32,
}
#[allow(
    clippy::unused_self,
    reason = "H15-67: D-Bus fixture property getters require an instance"
)]
#[zbus::interface(name = "org.freedesktop.NetworkManager.Device")]
impl DeviceState {
    #[zbus(property)]
    fn device_type(&self) -> u32 {
        1
    }
    #[zbus(property)]
    fn interface(&self) -> &'static str {
        "eth-test"
    }
    #[zbus(property)]
    fn state(&self) -> u32 {
        self.state
    }
    #[zbus(property)]
    fn set_state(&mut self, state: u32) {
        self.state = state;
    }
}
struct Link {
    carrier: bool,
}
#[zbus::interface(name = "org.freedesktop.NetworkManager.Device.Wired")]
impl Link {
    #[zbus(property)]
    fn carrier(&self) -> bool {
        self.carrier
    }
    #[zbus(property)]
    fn set_carrier(&mut self, carrier: bool) {
        self.carrier = carrier;
    }
}
async fn wait_radio(radio: &std::sync::atomic::AtomicBool, expected: bool) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while radio.load(Ordering::SeqCst) != expected {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
#[ignore = "H15-67: explicit private-bus integration test requires dbus-daemon"]
async fn wifi_fallback_follows_cached_link_signals_without_waiting_for_poll() {
    let mut child = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut address = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut address)
        .unwrap();
    let _bus = Bus(child);
    let radio = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _server = zbus::connection::Builder::address(address.trim())
        .unwrap()
        .name(SERVICE)
        .unwrap()
        .serve_at("/org/freedesktop/NetworkManager", Radio(Arc::clone(&radio)))
        .unwrap()
        .serve_at(DEVICE, DeviceState { state: 0 })
        .unwrap()
        .serve_at(DEVICE, Link { carrier: false })
        .unwrap()
        .build()
        .await
        .unwrap();
    let client = zbus::connection::Builder::address(address.trim())
        .unwrap()
        .build()
        .await
        .unwrap();
    let network = Arc::new(super::super::Network::new(true, super::super::WifiMode::Fallback));
    network.bus.set(client).unwrap();
    let watcher = tokio::spawn(Arc::clone(&network).watch(crate::hub::Hub::new()));
    wait_radio(&radio, true).await;
    let cache = network.proxies().await.unwrap();
    let device: zbus::Proxy<'static> = cache
        .get(
            SERVICE,
            DEVICE.try_into().unwrap(),
            "org.freedesktop.NetworkManager.Device",
        )
        .await
        .unwrap();
    let link: zbus::Proxy<'static> = cache
        .get(
            SERVICE,
            DEVICE.try_into().unwrap(),
            "org.freedesktop.NetworkManager.Device.Wired",
        )
        .await
        .unwrap();
    link.set_property("Carrier", &true).await.unwrap();
    device.set_property("State", &100_u32).await.unwrap();
    wait_radio(&radio, false).await;
    link.set_property("Carrier", &false).await.unwrap();
    wait_radio(&radio, true).await;
    watcher.abort();
    let _ = watcher.await;
}

#[tokio::test]
#[ignore = "H15-67: explicit private-bus integration test requires dbus-daemon"]
async fn bluetooth_scan_removes_fifty_proxies_and_cache_is_bounded() {
    use futures_util::StreamExt as _;
    let mut child = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut address = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut address)
        .unwrap();
    let _bus = Bus(child);
    let service = zbus::connection::Builder::address(address.trim())
        .unwrap()
        .name("org.bluez")
        .unwrap()
        .build()
        .await
        .unwrap();
    let client = zbus::connection::Builder::address(address.trim())
        .unwrap()
        .build()
        .await
        .unwrap();
    let network = super::super::Network::new(true, super::super::WifiMode::Both);
    network.bus.set(client).unwrap();
    let cache = network.proxies().await.unwrap();
    let mut signals = network.signals().await.unwrap();
    for index in 0..50 {
        let path: OwnedObjectPath = format!("/org/bluez/hci0/dev_{index}").try_into().unwrap();
        let _: zbus::Proxy<'static> = cache.get("org.bluez", path, "org.bluez.Device1").await.unwrap();
    }
    assert_eq!(cache.entries.lock().await.len(), 50);
    for index in 0..50 {
        let path: OwnedObjectPath = format!("/org/bluez/hci0/dev_{index}").try_into().unwrap();
        service
            .emit_signal(
                None::<&str>,
                "/",
                "org.freedesktop.DBus.ObjectManager",
                "InterfacesRemoved",
                &(path, vec!["org.bluez.Device1"]),
            )
            .await
            .unwrap();
    }
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !cache.entries.lock().await.is_empty() {
            signals.next().await.unwrap();
        }
    })
    .await
    .unwrap();
    for index in 0..MAX_ENTRIES + 50 {
        let path: OwnedObjectPath = format!("/org/bluez/hci0/dev_{index}").try_into().unwrap();
        let _: zbus::Proxy<'static> = cache.get("org.bluez", path, "org.bluez.Device1").await.unwrap();
    }
    assert_eq!(cache.entries.lock().await.len(), MAX_ENTRIES);
}
