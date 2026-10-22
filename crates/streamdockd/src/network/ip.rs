//! The IP side of a NetworkManager device (H15-44): every IPv4 and IPv6 address, gateways and DNS,
//! read from its `IP4Config` / `IP6Config` objects.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr};

use streamdock_proto::streamdock::v1 as pb;

use zbus::zvariant::{OwnedObjectPath, OwnedValue};

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Device",
    default_service = "org.freedesktop.NetworkManager"
)]
trait Device {
    #[zbus(property)]
    fn interface(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn ip4_config(&self) -> zbus::Result<OwnedObjectPath>;
    #[zbus(property)]
    fn ip6_config(&self) -> zbus::Result<OwnedObjectPath>;
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
    interface = "org.freedesktop.NetworkManager.IP6Config",
    default_service = "org.freedesktop.NetworkManager"
)]
trait Ip6Config {
    #[zbus(property)]
    fn address_data(&self) -> zbus::Result<Vec<HashMap<String, OwnedValue>>>;
    #[zbus(property)]
    fn gateway(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn nameservers(&self) -> zbus::Result<Vec<Vec<u8>>>;
}

/// The addresses of one device, as text: `"192.168.1.145/24"`, `"fe80::2a3d:e8ff:fe83:5930/64"`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Addresses {
    pub ipv4: Vec<String>,
    pub ipv6: Vec<String>,
    pub gateway4: String,
    pub gateway6: String,
    pub dns4: Vec<String>,
    pub dns6: Vec<String>,
    pub details: Vec<Detail>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detail {
    pub cidr: String,
    pub scope: String,
    pub temporary: bool,
    pub stable_privacy: bool,
}

impl Addresses {
    pub fn dns(&self) -> Vec<String> {
        self.dns4.iter().chain(&self.dns6).cloned().collect()
    }

    pub fn messages(&self) -> Vec<pb::NetworkAddress> {
        self.details
            .iter()
            .map(|d| pb::NetworkAddress {
                cidr: d.cidr.clone(),
                scope: d.scope.clone(),
                temporary: d.temporary,
                stable_privacy: d.stable_privacy,
                ..Default::default()
            })
            .collect()
    }

    /// Every address, IPv4 first.
    #[must_use]
    pub fn all(&self) -> Vec<String> {
        self.ipv4.iter().chain(&self.ipv6).cloned().collect()
    }
}

/// Reads `device`'s current IP configuration; a device without one gives empty lists.
pub(super) async fn addresses(
    proxies: &super::proxies::Proxies,
    device: &OwnedObjectPath,
) -> zbus::Result<Addresses> {
    let dev = proxies
        .get::<DeviceProxy<'static>>(
            "org.freedesktop.NetworkManager",
            device.clone(),
            "org.freedesktop.NetworkManager.Device",
        )
        .await?;
    let mut out = Addresses::default();
    let ip4 = dev.ip4_config().await?;
    if ip4.as_str() != "/" {
        let cfg = proxies
            .get::<Ip4ConfigProxy<'static>>(
                "org.freedesktop.NetworkManager",
                ip4,
                "org.freedesktop.NetworkManager.IP4Config",
            )
            .await?;
        out.ipv4 = cfg
            .address_data()
            .await
            .unwrap_or_default()
            .iter()
            .map(with_prefix)
            .collect();
        out.gateway4 = cfg.gateway().await.unwrap_or_default();
        out.dns4.extend(
            cfg.nameserver_data()
                .await
                .unwrap_or_default()
                .iter()
                .map(|d| text(d, "address")),
        );
    }
    let ip6 = dev.ip6_config().await?;
    if ip6.as_str() != "/" {
        let cfg = proxies
            .get::<Ip6ConfigProxy<'static>>(
                "org.freedesktop.NetworkManager",
                ip6,
                "org.freedesktop.NetworkManager.IP6Config",
            )
            .await?;
        out.ipv6 = cfg
            .address_data()
            .await
            .unwrap_or_default()
            .iter()
            .map(with_prefix)
            .collect();
        out.gateway6 = cfg.gateway().await.unwrap_or_default();
        out.dns6.extend(
            cfg.nameservers()
                .await
                .unwrap_or_default()
                .iter()
                .filter_map(|b| ipv6_text(b)),
        );
    }
    let interface = dev.interface().await?;
    let kernel = std::fs::read_to_string("/proc/net/if_inet6").unwrap_or_default();
    out.details = out
        .all()
        .iter()
        .filter_map(|a| detail(a, &interface, &kernel))
        .collect();
    Ok(out)
}

fn detail(cidr: &str, interface: &str, kernel: &str) -> Option<Detail> {
    let (address, prefix) = cidr.split_once('/')?;
    let ip: IpAddr = address.parse().ok()?;
    let prefix: u32 = prefix.parse().ok()?;
    if prefix > if ip.is_ipv4() { 32 } else { 128 } {
        return None;
    }
    let scope = match ip {
        _ if ip.is_loopback() => "loopback",
        IpAddr::V4(ip) if ip.is_link_local() => "link-local",
        IpAddr::V4(ip) if ip.is_private() => "private",
        IpAddr::V6(ip) if ip.is_unicast_link_local() => "link-local",
        IpAddr::V6(ip) if ip.is_unique_local() => "unique-local",
        _ => "global",
    };
    let flags = match ip {
        IpAddr::V6(ip) => kernel_flags(ip, interface, kernel),
        IpAddr::V4(_) => 0,
    };
    Some(Detail {
        cidr: cidr.to_owned(),
        scope: scope.to_owned(),
        temporary: flags & 0x01 != 0,
        stable_privacy: flags & 0x800 != 0,
    })
}

/// Linux `IFA_F_TEMPORARY` / `IFA_F_STABLE_PRIVACY` are per-address, not guesses from its text.
fn kernel_flags(address: Ipv6Addr, interface: &str, kernel: &str) -> u32 {
    kernel
        .lines()
        .find_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() != 6 || fields[5] != interface {
                return None;
            }
            let parsed = Ipv6Addr::from(u128::from_str_radix(fields[0], 16).ok()?);
            (parsed == address)
                .then(|| u32::from_str_radix(fields[4], 16).ok())
                .flatten()
        })
        .unwrap_or(0)
}

fn text(map: &HashMap<String, OwnedValue>, key: &str) -> String {
    map.get(key)
        .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
        .unwrap_or_default()
}

/// `"address/prefix"` from NetworkManager's `address-data` entry.
fn with_prefix(map: &HashMap<String, OwnedValue>) -> String {
    let prefix = map.get("prefix").and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
    format!("{}/{prefix}", text(map, "address"))
}

/// An IPv6 address from its 16 bytes (`IP6Config.Nameservers`).
fn ipv6_text(bytes: &[u8]) -> Option<String> {
    let octets: [u8; 16] = bytes.try_into().ok()?;
    Some(std::net::Ipv6Addr::from(octets).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_temporary_global_and_link_local_addresses_from_kernel_flags() {
        let kernel = "20010db8000000000000000000000001 02 40 00 01 eth0\nfe800000000000000000000000000002 02 40 20 80 eth0\n20010db8000000000000000000000003 03 40 00 01 wlan0";
        let temporary = detail("2001:db8::1/64", "eth0", kernel).unwrap();
        assert_eq!(temporary.scope, "global");
        assert!(temporary.temporary);
        assert!(!detail("2001:db8::3/64", "eth0", kernel).unwrap().temporary);
        let local = detail("fe80::2/64", "eth0", kernel).unwrap();
        assert_eq!(local.scope, "link-local");
        assert!(!local.temporary);
        assert_eq!(detail("fd12::1/64", "eth0", "").unwrap().scope, "unique-local");
        assert_eq!(detail("192.168.1.2/24", "eth0", "").unwrap().scope, "private");
        assert!(detail("bad/64", "eth0", kernel).is_none());
    }

    #[test]
    fn ipv6_from_bytes() {
        let mut b = [0u8; 16];
        b[15] = 1;
        assert_eq!(ipv6_text(&b).as_deref(), Some("::1"));
        assert_eq!(ipv6_text(&[1, 2, 3]), None);
    }
}
