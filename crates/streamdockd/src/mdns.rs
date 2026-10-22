//! mDNS / DNS-SD announcement (H15-35): `_streamdockd._tcp.local.` with the web UI port, so tools
//! (`just discover`, `just deploy-luckfox`) and other clients find every daemon on the LAN.
//!
//! Only daemons that listen beyond loopback announce themselves (dev instances on 127.0.0.1 stay
//! silent). TXT records: `version` (crate version), `arch` (e.g. `x86_64`, `arm`), `os`.

use std::net::SocketAddr;

use mdns_sd::{ServiceDaemon, ServiceInfo};

/// DNS-SD service type.
pub const SERVICE: &str = "_streamdockd._tcp.local.";

/// Keeps the announcement alive; dropping it unregisters the service.
pub struct Announcement {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Drop for Announcement {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

/// The machine's host name, for the instance name and the `.local` host.
fn host_name() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|h| h.trim().to_owned())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "streamdockd".to_owned())
}

/// Announces the daemon listening on `listen`; `None` for loopback addresses or if mDNS fails
/// (it is a convenience, the daemon runs without it).
pub fn announce(listen: SocketAddr) -> Option<Announcement> {
    if listen.ip().is_loopback() {
        return None;
    }
    let host = host_name();
    let properties = [
        ("version", env!("CARGO_PKG_VERSION")),
        ("arch", std::env::consts::ARCH),
        ("os", std::env::consts::OS),
    ];
    let result = (|| -> Result<Announcement, mdns_sd::Error> {
        let daemon = ServiceDaemon::new()?;
        let info = ServiceInfo::new(
            SERVICE,
            &host,
            &format!("{host}.local."),
            "",
            listen.port(),
            &properties[..],
        )?
        .enable_addr_auto();
        let fullname = info.get_fullname().to_owned();
        daemon.register(info)?;
        Ok(Announcement { daemon, fullname })
    })();
    match result {
        Ok(a) => {
            tracing::info!(service = SERVICE, instance = %host, port = listen.port(), "announced via mDNS");
            Some(a)
        }
        Err(e) => {
            tracing::warn!(error = %e, "mDNS announcement failed; the daemon runs without it");
            None
        }
    }
}
