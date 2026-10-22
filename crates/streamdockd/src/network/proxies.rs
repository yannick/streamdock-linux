//! Retain property subscriptions across network reads (H15-67).
use std::{collections::HashMap, sync::Arc, time::Instant};
use tokio::sync::Mutex;
use zbus::zvariant::OwnedObjectPath;

/// Bound retained objects even if a service fails to announce vanished scan objects.
const MAX_ENTRIES: usize = 128;
#[derive(Debug)]
struct Entry {
    proxy: zbus::Proxy<'static>,
    used: Instant,
}
type Key = (&'static str, OwnedObjectPath, &'static str);
#[derive(Debug)]
pub(super) struct Proxies {
    bus: zbus::Connection,
    entries: Mutex<HashMap<Key, Entry>>,
}
impl Proxies {
    pub fn new(bus: zbus::Connection) -> Arc<Self> {
        Arc::new(Self {
            bus,
            entries: Mutex::new(HashMap::new()),
        })
    }
    pub fn bus(&self) -> &zbus::Connection {
        &self.bus
    }
    pub async fn get<P: From<zbus::Proxy<'static>>>(
        &self,
        service: &'static str,
        path: OwnedObjectPath,
        interface: &'static str,
    ) -> zbus::Result<P> {
        let mut entries = self.entries.lock().await;
        let key = (service, path, interface);
        if let Some(entry) = entries.get_mut(&key) {
            entry.used = Instant::now();
            return Ok(entry.proxy.clone().into());
        }
        let proxy = zbus::Proxy::new_owned(self.bus.clone(), service, key.1.clone(), interface).await?;
        if entries.len() >= MAX_ENTRIES {
            let oldest = entries
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                entries.remove(&oldest);
            }
        }
        entries.insert(
            key,
            Entry {
                proxy: proxy.clone(),
                used: Instant::now(),
            },
        );
        Ok(proxy.into())
    }
    pub async fn remove_path(&self, service: &str, path: &str) {
        self.entries
            .lock()
            .await
            .retain(|(s, p, _), _| *s != service || p.as_str() != path);
    }
    pub async fn remove_service(&self, service: &str) {
        self.entries.lock().await.retain(|(s, _, _), _| *s != service);
    }
}

#[cfg(test)]
mod tests;
