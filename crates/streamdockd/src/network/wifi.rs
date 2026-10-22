//! Wi-Fi through NetworkManager's D-Bus API (H15-38): list and scan networks, connect (with a
//! saved connection or a new one), disconnect, forget. Passwords go straight to NetworkManager and
//! are never logged or returned.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context as _, bail};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

/// `NM_DEVICE_TYPE_WIFI`.
const DEVICE_TYPE_WIFI: u32 = 2;
/// How long a scan may take before we list what NetworkManager has.
const SCAN_WAIT: Duration = Duration::from_secs(4);

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait NetworkManager {
    #[zbus(property)]
    fn devices(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
    fn activate_connection(
        &self,
        connection: &zbus::zvariant::ObjectPath<'_>,
        device: &zbus::zvariant::ObjectPath<'_>,
        specific_object: &zbus::zvariant::ObjectPath<'_>,
    ) -> zbus::Result<OwnedObjectPath>;
    fn add_and_activate_connection(
        &self,
        connection: HashMap<&str, HashMap<&str, Value<'_>>>,
        device: &zbus::zvariant::ObjectPath<'_>,
        specific_object: &zbus::zvariant::ObjectPath<'_>,
    ) -> zbus::Result<(OwnedObjectPath, OwnedObjectPath)>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Device",
    default_service = "org.freedesktop.NetworkManager"
)]
trait Device {
    #[zbus(property)]
    fn device_type(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn interface(&self) -> zbus::Result<String>;
    fn disconnect(&self) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Device.Wireless",
    default_service = "org.freedesktop.NetworkManager"
)]
trait Wireless {
    fn request_scan(&self, options: HashMap<&str, Value<'_>>) -> zbus::Result<()>;
    fn get_all_access_points(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
    #[zbus(property)]
    fn active_access_point(&self) -> zbus::Result<OwnedObjectPath>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.AccessPoint",
    default_service = "org.freedesktop.NetworkManager"
)]
trait AccessPoint {
    #[zbus(property)]
    fn ssid(&self) -> zbus::Result<Vec<u8>>;
    #[zbus(property)]
    fn strength(&self) -> zbus::Result<u8>;
    #[zbus(property)]
    fn frequency(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn flags(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn wpa_flags(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn rsn_flags(&self) -> zbus::Result<u32>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Settings",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager/Settings"
)]
trait Settings {
    fn list_connections(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Settings.Connection",
    default_service = "org.freedesktop.NetworkManager"
)]
trait SavedConnection {
    fn get_settings(&self) -> zbus::Result<HashMap<String, HashMap<String, OwnedValue>>>;
    fn delete(&self) -> zbus::Result<()>;
}

/// A Wi-Fi network in range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Network {
    pub ssid: String,
    /// 0-100.
    pub signal: u8,
    pub secure: bool,
    pub frequency_mhz: u32,
    pub in_use: bool,
    /// NetworkManager has a saved connection for it (no password needed).
    pub saved: bool,
}

/// The Wi-Fi interface and what it is connected to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub interface: String,
    /// Empty when not connected.
    pub connected_ssid: String,
    /// Every IPv4 and IPv6 address on the Wi-Fi link (H15-44).
    pub ip: super::ip::Addresses,
}

/// NetworkManager's Wi-Fi device.
#[derive(Debug, Clone)]
pub struct Wifi {
    proxies: std::sync::Arc<super::proxies::Proxies>,
    device: OwnedObjectPath,
    interface: String,
}

impl Wifi {
    /// Finds the first Wi-Fi device NetworkManager manages.
    pub(super) async fn find(proxies: std::sync::Arc<super::proxies::Proxies>) -> anyhow::Result<Self> {
        let nm: NetworkManagerProxy<'static> = proxies
            .get(
                "org.freedesktop.NetworkManager",
                "/org/freedesktop/NetworkManager".try_into()?,
                "org.freedesktop.NetworkManager",
            )
            .await
            .context("NetworkManager is not running")?;
        for path in nm.devices().await.context("NetworkManager: listing devices")? {
            let device: DeviceProxy<'static> = proxies
                .get(
                    "org.freedesktop.NetworkManager",
                    path.clone(),
                    "org.freedesktop.NetworkManager.Device",
                )
                .await?;
            if device.device_type().await? == DEVICE_TYPE_WIFI {
                return Ok(Self {
                    proxies: std::sync::Arc::clone(&proxies),
                    interface: device.interface().await?,
                    device: path,
                });
            }
        }
        bail!("NetworkManager has no Wi-Fi device")
    }

    async fn wireless(&self) -> zbus::Result<WirelessProxy<'static>> {
        self.proxies
            .get::<WirelessProxy<'static>>(
                "org.freedesktop.NetworkManager",
                self.device.clone(),
                "org.freedesktop.NetworkManager.Device.Wireless",
            )
            .await
    }

    pub async fn status(&self) -> anyhow::Result<Status> {
        let active = self.wireless().await?.active_access_point().await?;
        let connected_ssid = if active.as_str() == "/" {
            String::new()
        } else {
            ssid_text(&self.access_point(&active).await?.ssid().await?)
        };
        Ok(Status {
            interface: self.interface.clone(),
            connected_ssid,
            ip: super::ip::addresses(&self.proxies, &self.device)
                .await
                .unwrap_or_default(),
        })
    }

    async fn access_point(&self, path: &OwnedObjectPath) -> zbus::Result<AccessPointProxy<'static>> {
        self.proxies
            .get::<AccessPointProxy<'static>>(
                "org.freedesktop.NetworkManager",
                path.clone(),
                "org.freedesktop.NetworkManager.AccessPoint",
            )
            .await
    }

    /// Asks for a fresh scan (when `rescan`), then lists the networks in range: one entry per
    /// SSID (the strongest access point), strongest first, hidden networks left out.
    pub async fn networks(&self, rescan: bool) -> anyhow::Result<Vec<Network>> {
        let wireless = self.wireless().await?;
        if rescan {
            // NetworkManager refuses scans in quick succession; the last results are fine then.
            if let Err(e) = wireless.request_scan(HashMap::new()).await {
                tracing::debug!(error = %e, "Wi-Fi scan not started");
            } else {
                tokio::time::sleep(SCAN_WAIT).await;
            }
        }
        let active = wireless.active_access_point().await?;
        let saved = self.saved_ssids().await.unwrap_or_default();
        let mut found = Vec::new();
        for path in wireless.get_all_access_points().await? {
            let ap = self.access_point(&path).await?;
            let ssid = ssid_text(&ap.ssid().await?);
            if ssid.is_empty() {
                continue;
            }
            found.push(Network {
                secure: ap.flags().await? != 0 || ap.wpa_flags().await? != 0 || ap.rsn_flags().await? != 0,
                signal: ap.strength().await?,
                frequency_mhz: ap.frequency().await?,
                in_use: path == active,
                saved: saved.contains_key(&ssid),
                ssid,
            });
        }
        Ok(strongest_per_ssid(found))
    }

    /// Saved Wi-Fi connections by SSID.
    async fn saved_ssids(&self) -> anyhow::Result<HashMap<String, OwnedObjectPath>> {
        let settings: SettingsProxy<'static> = self
            .proxies
            .get(
                "org.freedesktop.NetworkManager",
                "/org/freedesktop/NetworkManager/Settings".try_into()?,
                "org.freedesktop.NetworkManager.Settings",
            )
            .await?;
        let mut saved = HashMap::new();
        for path in settings.list_connections().await? {
            let conn = self
                .proxies
                .get::<SavedConnectionProxy<'static>>(
                    "org.freedesktop.NetworkManager",
                    path.clone(),
                    "org.freedesktop.NetworkManager.Settings.Connection",
                )
                .await?;
            let Ok(values) = conn.get_settings().await else {
                continue;
            };
            if let Some(ssid) = values
                .get("802-11-wireless")
                .and_then(|w| w.get("ssid"))
                .and_then(|v| Vec::<u8>::try_from(v.clone()).ok())
            {
                saved.insert(ssid_text(&ssid), path);
            }
        }
        Ok(saved)
    }

    /// Connects to `ssid`: with its saved connection, else a new one with `password` (empty for
    /// open networks).
    pub async fn connect(&self, ssid: &str, password: &str) -> anyhow::Result<()> {
        let nm: NetworkManagerProxy<'static> = self
            .proxies
            .get(
                "org.freedesktop.NetworkManager",
                "/org/freedesktop/NetworkManager".try_into()?,
                "org.freedesktop.NetworkManager",
            )
            .await?;
        let root = zbus::zvariant::ObjectPath::try_from("/")?;
        if let Some(saved) = self.saved_ssids().await?.get(ssid) {
            nm.activate_connection(saved, &self.device, &root)
                .await
                .with_context(|| format!("NetworkManager could not connect to {ssid:?}"))?;
            return Ok(());
        }
        let mut wireless: HashMap<&str, Value<'_>> = HashMap::new();
        wireless.insert("ssid", Value::from(ssid.as_bytes().to_vec()));
        wireless.insert("mode", Value::from("infrastructure"));
        let mut connection: HashMap<&str, HashMap<&str, Value<'_>>> = HashMap::new();
        connection.insert("802-11-wireless", wireless);
        if !password.is_empty() {
            let mut security: HashMap<&str, Value<'_>> = HashMap::new();
            security.insert("key-mgmt", Value::from("wpa-psk"));
            security.insert("psk", Value::from(password));
            connection.insert("802-11-wireless-security", security);
        }
        nm.add_and_activate_connection(connection, &self.device, &root)
            .await
            .map_err(|e| anyhow::anyhow!("NetworkManager could not connect to {ssid:?}: {}", nm_error(&e)))?;
        Ok(())
    }

    pub async fn disconnect(&self) -> anyhow::Result<()> {
        let device = self
            .proxies
            .get::<DeviceProxy<'static>>(
                "org.freedesktop.NetworkManager",
                self.device.clone(),
                "org.freedesktop.NetworkManager.Device",
            )
            .await?;
        device
            .disconnect()
            .await
            .context("NetworkManager could not disconnect")
    }

    /// Deletes the saved connection for `ssid`; `Ok(false)` if there was none.
    pub async fn forget(&self, ssid: &str) -> anyhow::Result<bool> {
        let Some(path) = self.saved_ssids().await?.remove(ssid) else {
            return Ok(false);
        };
        let conn = self
            .proxies
            .get::<SavedConnectionProxy<'static>>(
                "org.freedesktop.NetworkManager",
                path,
                "org.freedesktop.NetworkManager.Settings.Connection",
            )
            .await?;
        conn.delete()
            .await
            .with_context(|| format!("NetworkManager could not forget {ssid:?}"))?;
        Ok(true)
    }
}

/// NetworkManager's message without the D-Bus error name, which never holds the password.
fn nm_error(e: &zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(_, Some(message), _) => message.clone(),
        other => other.to_string(),
    }
}

fn ssid_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim_end_matches('\0').to_owned()
}

/// One entry per SSID (its strongest access point; "in use" if any of them is), strongest first.
fn strongest_per_ssid(found: Vec<Network>) -> Vec<Network> {
    let mut by_ssid: HashMap<String, Network> = HashMap::new();
    for n in found {
        match by_ssid.get_mut(&n.ssid) {
            Some(best) => {
                let in_use = best.in_use || n.in_use;
                if n.signal > best.signal {
                    *best = n;
                }
                best.in_use = in_use;
            }
            None => {
                by_ssid.insert(n.ssid.clone(), n);
            }
        }
    }
    let mut list: Vec<Network> = by_ssid.into_values().collect();
    list.sort_by(|a, b| {
        b.in_use
            .cmp(&a.in_use)
            .then(b.signal.cmp(&a.signal))
            .then(a.ssid.cmp(&b.ssid))
    });
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(ssid: &str, signal: u8, in_use: bool) -> Network {
        Network {
            ssid: ssid.into(),
            signal,
            secure: true,
            frequency_mhz: 2412,
            in_use,
            saved: false,
        }
    }

    #[test]
    fn keeps_the_strongest_access_point_per_ssid() {
        let list = strongest_per_ssid(vec![
            net("H15", 29, false),
            net("1337", 32, false),
            net("H15", 32, false),
            net("IOS", 30, true),
            net("IOS", 40, false),
        ]);
        let got: Vec<_> = list
            .iter()
            .map(|n| (n.ssid.as_str(), n.signal, n.in_use))
            .collect();
        assert_eq!(
            got,
            vec![("IOS", 40, true), ("1337", 32, false), ("H15", 32, false)]
        );
    }

    #[test]
    fn ssids_are_text() {
        assert_eq!(ssid_text(b"H15\0"), "H15");
        assert_eq!(ssid_text(&[0x48, 0xff]), "H\u{fffd}");
    }
}
