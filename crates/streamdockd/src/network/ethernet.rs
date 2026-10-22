//! Ethernet through NetworkManager (H15-42): status (link, IPv4 address, gateway, DNS, DHCP or
//! static) and switching between DHCP and a static address.
//!
//! The board is usually reached over this very link, so a wrong address would lock everyone out.
//! A change is therefore only tried: it is applied to the running device (`Reapply`, the saved
//! profile is untouched) and reverted after [`TRIAL`] unless [`Ethernet::confirm`] makes it
//! permanent (written to the saved profile). The settings page confirms from the new address.

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use anyhow::{Context as _, bail};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

/// How long a change is tried before it is reverted.
pub const TRIAL: Duration = Duration::from_secs(60);
/// `NM_DEVICE_TYPE_ETHERNET`.
const DEVICE_TYPE_ETHERNET: u32 = 1;
/// `NM_DEVICE_STATE_ACTIVATED`.
const STATE_ACTIVATED: u32 = 100;

type Settings = HashMap<String, HashMap<String, OwnedValue>>;
/// One section of a connection's settings, e.g. `ipv4`.
pub type Section = HashMap<String, OwnedValue>;
/// The IP sections a trial changes: `ipv4` and `ipv6`.
pub type Sections = HashMap<String, Section>;
/// The sections a change may touch.
const IP_SECTIONS: [&str; 2] = ["ipv4", "ipv6"];

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait NetworkManager {
    #[zbus(property)]
    fn devices(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Device",
    default_service = "org.freedesktop.NetworkManager"
)]
trait Device {
    fn get_applied_connection(&self, flags: u32) -> zbus::Result<(Settings, u64)>;
    fn reapply(&self, connection: Settings, version_id: u64, flags: u32) -> zbus::Result<()>;
    #[zbus(property)]
    fn device_type(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn interface(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn state(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn ip4_config(&self) -> zbus::Result<OwnedObjectPath>;
    #[zbus(property)]
    fn active_connection(&self) -> zbus::Result<OwnedObjectPath>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Device.Wired",
    default_service = "org.freedesktop.NetworkManager"
)]
trait Wired {
    #[zbus(property)]
    fn carrier(&self) -> zbus::Result<bool>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.IP4Config",
    default_service = "org.freedesktop.NetworkManager"
)]
trait Ip4Config {
    #[zbus(property)]
    fn address_data(&self) -> zbus::Result<Vec<HashMap<String, OwnedValue>>>;
    #[zbus(property)]
    fn gateway(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn nameserver_data(&self) -> zbus::Result<Vec<HashMap<String, OwnedValue>>>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Connection.Active",
    default_service = "org.freedesktop.NetworkManager"
)]
trait ActiveConnection {
    #[zbus(property)]
    fn connection(&self) -> zbus::Result<OwnedObjectPath>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Settings.Connection",
    default_service = "org.freedesktop.NetworkManager"
)]
trait SavedConnection {
    fn get_settings(&self) -> zbus::Result<Settings>;
    fn update(&self, properties: Settings) -> zbus::Result<()>;
}

/// IPv4 configuration: DHCP, or a static address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ipv4 {
    Dhcp,
    Static {
        address: Ipv4Addr,
        prefix: u8,
        gateway: Option<Ipv4Addr>,
        dns: Vec<Ipv4Addr>,
    },
}

/// IPv6 configuration (H15-44).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ipv6 {
    /// Router advertisements (SLAAC), and `DHCPv6` if the router says so.
    Auto,
    Dhcp,
    Static {
        address: Ipv6Addr,
        prefix: u8,
        gateway: Option<Ipv6Addr>,
        dns: Vec<Ipv6Addr>,
    },
    Disabled,
}

/// Values configured in the applied profile, to populate the IPv6 form without guessing
/// from temporary addresses or router-advertised DNS.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfiguredIpv6 {
    pub method: String,
    pub address: String,
    pub prefix: u32,
    pub gateway: String,
    pub dns: Vec<String>,
}

fn configured_ipv6(section: Option<&Section>) -> ConfiguredIpv6 {
    let Some(section) = section else {
        return ConfiguredIpv6 {
            method: "auto".into(),
            ..Default::default()
        };
    };
    let addresses = section
        .get("address-data")
        .and_then(|v| Vec::<HashMap<String, OwnedValue>>::try_from(v.try_clone().ok()?).ok())
        .unwrap_or_default();
    let address = addresses.first();
    let dns = section
        .get("dns")
        .and_then(|v| Vec::<Vec<u8>>::try_from(v.try_clone().ok()?).ok())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|b| <[u8; 16]>::try_from(b).ok().map(Ipv6Addr::from))
        .map(|ip| ip.to_string())
        .collect();
    ConfiguredIpv6 {
        method: field_text(section, "method"),
        address: address.map(|a| field_text(a, "address")).unwrap_or_default(),
        prefix: address
            .and_then(|a| a.get("prefix"))
            .and_then(|p| u32::try_from(p).ok())
            .unwrap_or(0),
        gateway: field_text(section, "gateway"),
        dns,
    }
}

/// What the Ethernet link looks like now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub interface: String,
    /// A cable with a live link is plugged in.
    pub carrier: bool,
    /// NetworkManager has the link up and configured.
    pub connected: bool,
    /// The configured method: DHCP (`true`) or static.
    pub dhcp: bool,
    /// The current address (from DHCP or static), e.g. "192.168.1.141".
    pub address: String,
    pub prefix: u32,
    pub gateway: String,
    pub dns: Vec<String>,
    /// NetworkManager's IPv6 method: "auto", "dhcp", "manual", "disabled", "ignore", …
    pub ipv6_method: String,
    pub ipv6: ConfiguredIpv6,
    /// Every IPv4 and IPv6 address, gateways and DNS (H15-44).
    pub ip: super::ip::Addresses,
}

/// NetworkManager's Ethernet device.
#[derive(Debug, Clone)]
pub struct Ethernet {
    proxies: std::sync::Arc<super::proxies::Proxies>,
    device: OwnedObjectPath,
}

impl Ethernet {
    /// Finds the first Ethernet device NetworkManager manages.
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
            if device.device_type().await? == DEVICE_TYPE_ETHERNET {
                return Ok(Self {
                    proxies: std::sync::Arc::clone(&proxies),
                    device: path,
                });
            }
        }
        bail!("NetworkManager has no Ethernet device")
    }

    async fn proxy(&self) -> zbus::Result<DeviceProxy<'static>> {
        self.proxies
            .get::<DeviceProxy<'static>>(
                "org.freedesktop.NetworkManager",
                self.device.clone(),
                "org.freedesktop.NetworkManager.Device",
            )
            .await
    }

    /// Radio fallback only needs cached link properties, not IP/profile state.
    pub async fn link_up(&self) -> zbus::Result<bool> {
        let device = self.proxy().await?;
        let wired: WiredProxy<'static> = self
            .proxies
            .get(
                "org.freedesktop.NetworkManager",
                self.device.clone(),
                "org.freedesktop.NetworkManager.Device.Wired",
            )
            .await?;
        Ok(device.state().await? == STATE_ACTIVATED && wired.carrier().await?)
    }

    pub async fn status(&self) -> anyhow::Result<Status> {
        let device = self.proxy().await?;
        let wired = self
            .proxies
            .get::<WiredProxy<'static>>(
                "org.freedesktop.NetworkManager",
                self.device.clone(),
                "org.freedesktop.NetworkManager.Device.Wired",
            )
            .await?;
        let mut status = Status {
            interface: device.interface().await?,
            carrier: wired.carrier().await.unwrap_or(false),
            connected: device.state().await? == STATE_ACTIVATED,
            ..Status::default()
        };
        if let Ok((applied, _)) = device.get_applied_connection(0).await {
            let method = |section: &str| {
                applied
                    .get(section)
                    .and_then(|v| v.get("method"))
                    .and_then(|m| String::try_from(m.try_clone().ok()?).ok())
            };
            status.dhcp = method("ipv4").is_none_or(|m| m == "auto");
            status.ipv6 = configured_ipv6(applied.get("ipv6"));
            status.ipv6_method = status.ipv6.method.clone();
        }
        status.ip = super::ip::addresses(&self.proxies, &self.device)
            .await
            .unwrap_or_default();
        let config = device.ip4_config().await?;
        if config.as_str() != "/" {
            let ip4 = self
                .proxies
                .get::<Ip4ConfigProxy<'static>>(
                    "org.freedesktop.NetworkManager",
                    config,
                    "org.freedesktop.NetworkManager.IP4Config",
                )
                .await?;
            if let Some(first) = ip4.address_data().await?.first() {
                status.address = field_text(first, "address");
                status.prefix = first
                    .get("prefix")
                    .and_then(|v| u32::try_from(v).ok())
                    .unwrap_or(0);
            }
            status.gateway = ip4.gateway().await.unwrap_or_default();
            status.dns = ip4
                .nameserver_data()
                .await
                .unwrap_or_default()
                .iter()
                .map(|d| field_text(d, "address"))
                .collect();
        }
        Ok(status)
    }

    /// The IP sections now applied to the device, to restore after a trial.
    pub async fn applied_sections(&self) -> anyhow::Result<Sections> {
        let (applied, _) = self
            .proxy()
            .await?
            .get_applied_connection(0)
            .await
            .context("NetworkManager: the Ethernet link has no active connection")?;
        IP_SECTIONS
            .iter()
            .map(|name| {
                let section = applied.get(*name).map(clone_map).transpose()?.unwrap_or_default();
                Ok(((*name).to_owned(), section))
            })
            .collect()
    }

    /// Applies `sections` to the running device only (the saved profile keeps the old ones).
    pub async fn reapply(&self, sections: Sections) -> anyhow::Result<()> {
        let device = self.proxy().await?;
        let (applied, version) = device
            .get_applied_connection(0)
            .await
            .context("NetworkManager: the Ethernet link has no active connection")?;
        let applied = merge_sections(applied, sections);
        device
            .reapply(applied, version, 0)
            .await
            .map_err(|e| anyhow::anyhow!("NetworkManager refused the Ethernet change: {}", nm_error(&e)))
    }

    /// Writes `sections` into the saved profile of the active connection, so they survive a reboot.
    pub async fn persist(&self, sections: Sections) -> anyhow::Result<()> {
        let active = self.proxy().await?.active_connection().await?;
        anyhow::ensure!(
            active.as_str() != "/",
            "the Ethernet link has no active connection"
        );
        let active = self
            .proxies
            .get::<ActiveConnectionProxy<'static>>(
                "org.freedesktop.NetworkManager",
                active,
                "org.freedesktop.NetworkManager.Connection.Active",
            )
            .await?;
        let saved = self
            .proxies
            .get::<SavedConnectionProxy<'static>>(
                "org.freedesktop.NetworkManager",
                active.connection().await?,
                "org.freedesktop.NetworkManager.Settings.Connection",
            )
            .await?;
        let settings = merge_sections(saved.get_settings().await?, sections);
        saved.update(settings).await.map_err(|e| {
            anyhow::anyhow!(
                "NetworkManager could not save the Ethernet setting: {}",
                nm_error(&e)
            )
        })
    }
}

/// Builds only the IP sections explicitly requested by a trial.
pub fn changes(ipv4: Option<&Ipv4>, ipv6: Option<&Ipv6>) -> anyhow::Result<Sections> {
    let mut sections = Sections::new();
    if let Some(ipv4) = ipv4 {
        sections.insert("ipv4".into(), ipv4_settings(ipv4)?);
    }
    if let Some(ipv6) = ipv6 {
        sections.insert("ipv6".into(), ipv6_settings(ipv6)?);
    }
    anyhow::ensure!(!sections.is_empty(), "no Ethernet IP change was requested");
    Ok(sections)
}

/// NetworkManager requires a full connection; overlay only the requested sections.
fn merge_sections(mut profile: Settings, changes: Sections) -> Settings {
    profile.extend(changes);
    profile
}

/// NetworkManager's `ipv6` settings for `ipv6`.
pub fn ipv6_settings(ipv6: &Ipv6) -> anyhow::Result<Section> {
    let mut map = HashMap::new();
    let method = match ipv6 {
        Ipv6::Auto => "auto",
        Ipv6::Dhcp => "dhcp",
        Ipv6::Disabled => "disabled",
        Ipv6::Static {
            address,
            prefix,
            gateway,
            dns,
        } => {
            let mut addr: HashMap<String, Value<'_>> = HashMap::new();
            addr.insert("address".to_owned(), Value::from(address.to_string()));
            addr.insert("prefix".to_owned(), Value::from(u32::from(*prefix)));
            map.insert("address-data".to_owned(), owned(Value::from(vec![addr]))?);
            if let Some(gateway) = gateway {
                map.insert("gateway".to_owned(), owned(Value::from(gateway.to_string()))?);
            }
            if !dns.is_empty() {
                let dns: Vec<Vec<u8>> = dns.iter().map(|ip| ip.octets().to_vec()).collect();
                map.insert("dns".to_owned(), owned(Value::from(dns))?);
            }
            "manual"
        }
    };
    map.insert("method".to_owned(), owned(Value::from(method))?);
    Ok(map)
}

/// Checks and parses an IPv6 method as chosen on the settings page.
pub fn parse_ipv6(
    method: &str,
    address: &str,
    prefix: u32,
    gateway: &str,
    dns: &[String],
) -> anyhow::Result<Ipv6> {
    Ok(match method {
        "auto" => Ipv6::Auto,
        "dhcp" => Ipv6::Dhcp,
        "disabled" => Ipv6::Disabled,
        "static" => {
            let address: Ipv6Addr = address
                .trim()
                .parse()
                .with_context(|| format!("{address:?} is not an IPv6 address like 2001:db8::50"))?;
            anyhow::ensure!(
                (8..=128).contains(&prefix),
                "IPv6 prefix /{prefix} must be 8-128, usually 64"
            );
            let gateway = match gateway.trim() {
                "" => None,
                g => Some(
                    g.parse::<Ipv6Addr>()
                        .with_context(|| format!("IPv6 gateway {g:?} is not an IPv6 address"))?,
                ),
            };
            Ipv6::Static {
                address,
                prefix: u8::try_from(prefix).expect("checked above"),
                gateway,
                dns: dns
                    .iter()
                    .map(|d| d.trim())
                    .filter(|d| !d.is_empty())
                    .map(|d| {
                        d.parse::<Ipv6Addr>()
                            .with_context(|| format!("DNS server {d:?} is not an IPv6 address"))
                    })
                    .collect::<anyhow::Result<_>>()?,
            }
        }
        other => bail!("unknown IPv6 method {other:?} (auto, dhcp, static, disabled)"),
    })
}

/// NetworkManager's `ipv4` settings for `ipv4`.
pub fn ipv4_settings(ipv4: &Ipv4) -> anyhow::Result<Section> {
    let mut map = HashMap::new();
    match ipv4 {
        Ipv4::Dhcp => {
            map.insert("method".to_owned(), owned(Value::from("auto"))?);
        }
        Ipv4::Static {
            address,
            prefix,
            gateway,
            dns,
        } => {
            map.insert("method".to_owned(), owned(Value::from("manual"))?);
            let mut addr: HashMap<String, Value<'_>> = HashMap::new();
            addr.insert("address".to_owned(), Value::from(address.to_string()));
            addr.insert("prefix".to_owned(), Value::from(u32::from(*prefix)));
            map.insert("address-data".to_owned(), owned(Value::from(vec![addr]))?);
            if let Some(gateway) = gateway {
                map.insert("gateway".to_owned(), owned(Value::from(gateway.to_string()))?);
            }
            if !dns.is_empty() {
                // `ipv4.dns`: addresses as u32 in network byte order.
                let dns: Vec<u32> = dns.iter().map(|a| u32::from_ne_bytes(a.octets())).collect();
                map.insert("dns".to_owned(), owned(Value::from(dns))?);
            }
        }
    }
    Ok(map)
}

/// Checks and parses a static configuration as typed on the settings page.
pub fn parse_static(address: &str, prefix: u32, gateway: &str, dns: &[String]) -> anyhow::Result<Ipv4> {
    let address: Ipv4Addr = address
        .trim()
        .parse()
        .with_context(|| format!("{address:?} is not an IPv4 address like 192.168.1.50"))?;
    anyhow::ensure!(
        (8..=30).contains(&prefix),
        "prefix /{prefix} doesn't fit a home network; use e.g. 24 (= 255.255.255.0)"
    );
    let prefix = u8::try_from(prefix).expect("checked above");
    let gateway = match gateway.trim() {
        "" => None,
        g => Some(
            g.parse::<Ipv4Addr>()
                .with_context(|| format!("gateway {g:?} is not an IPv4 address"))?,
        ),
    };
    if let Some(gw) = gateway {
        anyhow::ensure!(
            same_subnet(address, gw, prefix),
            "gateway {gw} is not in {address}/{prefix}; the board couldn't reach it"
        );
    }
    let dns = dns
        .iter()
        .map(|d| d.trim())
        .filter(|d| !d.is_empty())
        .map(|d| {
            d.parse::<Ipv4Addr>()
                .with_context(|| format!("DNS server {d:?} is not an IPv4 address"))
        })
        .collect::<anyhow::Result<_>>()?;
    Ok(Ipv4::Static {
        address,
        prefix,
        gateway,
        dns,
    })
}

fn same_subnet(a: Ipv4Addr, b: Ipv4Addr, prefix: u8) -> bool {
    let mask = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
    (u32::from(a) & mask) == (u32::from(b) & mask)
}

fn owned(value: Value<'_>) -> anyhow::Result<OwnedValue> {
    OwnedValue::try_from(value).context("D-Bus value")
}

fn clone_map(map: &HashMap<String, OwnedValue>) -> anyhow::Result<HashMap<String, OwnedValue>> {
    map.iter()
        .map(|(k, v)| Ok((k.clone(), v.try_clone().context("D-Bus value")?)))
        .collect()
}

fn field_text(map: &HashMap<String, OwnedValue>, key: &str) -> String {
    map.get(key)
        .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
        .unwrap_or_default()
}

fn nm_error(e: &zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(_, Some(message), _) => message.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv6_only_change_preserves_the_entire_ipv4_profile() {
        let mut original = Section::new();
        original.insert("method".into(), owned(Value::from("auto")).unwrap());
        original.insert("route-metric".into(), owned(Value::from(123u32)).unwrap());
        original.insert(
            "dns-search".into(),
            owned(Value::from(vec!["home.test"])).unwrap(),
        );
        original.insert("ignore-auto-dns".into(), owned(Value::from(true)).unwrap());
        let mut profile = Settings::new();
        profile.insert("ipv4".into(), clone_map(&original).unwrap());
        let next = changes(None, Some(&Ipv6::Disabled)).unwrap();
        assert!(!next.contains_key("ipv4"));
        let changed = merge_sections(profile, next);
        assert_eq!(changed["ipv4"], original);
        assert_eq!(
            String::try_from(changed["ipv6"]["method"].try_clone().unwrap()).unwrap(),
            "disabled"
        );
    }

    #[test]
    fn parses_a_static_configuration() {
        let ip = parse_static(
            " 192.168.1.150 ",
            24,
            "192.168.1.1",
            &["192.168.1.1".into(), " ".into()],
        )
        .unwrap();
        assert_eq!(
            ip,
            Ipv4::Static {
                address: Ipv4Addr::new(192, 168, 1, 150),
                prefix: 24,
                gateway: Some(Ipv4Addr::new(192, 168, 1, 1)),
                dns: vec![Ipv4Addr::new(192, 168, 1, 1)],
            }
        );
        assert!(
            parse_static("192.168.1.150", 24, "", &[]).is_ok(),
            "the gateway is optional"
        );
    }

    #[test]
    fn rejects_settings_that_would_lock_the_board_out() {
        for (address, prefix, gateway, why) in [
            ("192.168.1", 24, "", "not an IPv4 address"),
            ("192.168.1.150", 4, "", "doesn't fit"),
            ("192.168.1.150", 32, "", "doesn't fit"),
            ("192.168.1.150", 24, "10.0.0.1", "not in 192.168.1.150/24"),
            ("192.168.1.150", 24, "router", "gateway"),
        ] {
            let err = format!("{:#}", parse_static(address, prefix, gateway, &[]).unwrap_err());
            assert!(err.contains(why), "{address}/{prefix} via {gateway:?}: {err}");
        }
    }

    #[test]
    fn builds_networkmanager_settings() {
        let dhcp = ipv4_settings(&Ipv4::Dhcp).unwrap();
        assert_eq!(
            String::try_from(dhcp["method"].try_clone().unwrap()).unwrap(),
            "auto"
        );
        let fixed =
            ipv4_settings(&parse_static("192.168.1.150", 24, "192.168.1.1", &["1.1.1.1".into()]).unwrap())
                .unwrap();
        assert_eq!(
            String::try_from(fixed["method"].try_clone().unwrap()).unwrap(),
            "manual"
        );
        assert_eq!(
            String::try_from(fixed["gateway"].try_clone().unwrap()).unwrap(),
            "192.168.1.1"
        );
        let dns = Vec::<u32>::try_from(fixed["dns"].try_clone().unwrap()).unwrap();
        assert_eq!(dns, vec![u32::from_ne_bytes([1, 1, 1, 1])]);
        assert!(fixed.contains_key("address-data"));
    }

    #[test]
    fn static_ipv6_dns_and_form_values_round_trip() {
        let parsed = parse_ipv6(
            "static",
            "2001:db8::145",
            64,
            "fe80::1",
            &["2001:db8::53".into(), " fe80::53 ".into()],
        )
        .unwrap();
        let settings = ipv6_settings(&parsed).unwrap();
        let config = configured_ipv6(Some(&settings));
        assert_eq!(config.method, "manual");
        assert_eq!(config.address, "2001:db8::145");
        assert_eq!(config.prefix, 64);
        assert_eq!(config.gateway, "fe80::1");
        assert_eq!(config.dns, vec!["2001:db8::53", "fe80::53"]);
        assert!(parse_ipv6("static", "2001:db8::145", 64, "", &["1.1.1.1".into()]).is_err());
    }

    #[test]
    fn parses_ipv6_choices() {
        assert_eq!(parse_ipv6("auto", "", 0, "", &[]).unwrap(), Ipv6::Auto);
        assert_eq!(parse_ipv6("disabled", "", 0, "", &[]).unwrap(), Ipv6::Disabled);
        assert_eq!(
            parse_ipv6("static", "2a02:168:e678::145", 64, "fe80::1", &[]).unwrap(),
            Ipv6::Static {
                address: "2a02:168:e678::145".parse().unwrap(),
                prefix: 64,
                gateway: Some("fe80::1".parse().unwrap()),
                dns: vec![],
            }
        );
        for (method, address, prefix, why) in [
            ("static", "2a02::zz", 64, "not an IPv6 address"),
            ("static", "2a02::1", 129, "8-128"),
            ("slaac", "", 0, "unknown IPv6 method"),
        ] {
            let err = format!("{:#}", parse_ipv6(method, address, prefix, "", &[]).unwrap_err());
            assert!(err.contains(why), "{method} {address}/{prefix}: {err}");
        }
        let fixed = ipv6_settings(&parse_ipv6("static", "2a02::1", 64, "", &[]).unwrap()).unwrap();
        assert_eq!(
            String::try_from(fixed["method"].try_clone().unwrap()).unwrap(),
            "manual"
        );
        assert!(!fixed.contains_key("gateway"));
    }

    #[test]
    fn subnets() {
        assert!(same_subnet(
            Ipv4Addr::new(10, 1, 2, 3),
            Ipv4Addr::new(10, 1, 2, 254),
            24
        ));
        assert!(!same_subnet(
            Ipv4Addr::new(10, 1, 2, 3),
            Ipv4Addr::new(10, 1, 3, 1),
            24
        ));
        assert!(same_subnet(
            Ipv4Addr::new(10, 1, 2, 3),
            Ipv4Addr::new(10, 1, 3, 1),
            16
        ));
    }
}
