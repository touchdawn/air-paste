use crate::{relay::RelayHub, store::Store};
use airpaste_core::DeviceId;
use airpaste_protocol::ServerEvent;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{broadcast, RwLock};

#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub hub: EventHub,
    pub relay_hub: RelayHub,
    pub auth_token: Option<String>,
    nonce_cache: NonceCache,
}

impl AppState {
    pub fn new(store: Store, auth_token: Option<String>) -> Self {
        Self {
            store,
            hub: EventHub::new(),
            relay_hub: RelayHub::new(),
            auth_token,
            nonce_cache: NonceCache::new(Duration::from_secs(300)),
        }
    }

    pub async fn record_nonce(&self, device_id: &DeviceId, nonce: &str) -> bool {
        self.nonce_cache.record(device_id, nonce).await
    }
}

#[derive(Clone)]
pub struct EventHub {
    global_tx: broadcast::Sender<ServerEvent>,
    device_txs: Arc<RwLock<HashMap<DeviceId, broadcast::Sender<ServerEvent>>>>,
}

impl EventHub {
    pub fn new() -> Self {
        let (global_tx, _) = broadcast::channel(512);
        Self {
            global_tx,
            device_txs: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn subscribe_global(&self) -> broadcast::Receiver<ServerEvent> {
        self.global_tx.subscribe()
    }

    pub async fn subscribe_device(&self, device_id: &DeviceId) -> broadcast::Receiver<ServerEvent> {
        let mut txs = self.device_txs.write().await;
        txs.entry(device_id.clone())
            .or_insert_with(|| {
                let (tx, _) = broadcast::channel(256);
                tx
            })
            .subscribe()
    }

    pub fn broadcast(&self, event: ServerEvent) {
        let _ = self.global_tx.send(event);
    }

    pub async fn send_to(&self, device_id: &DeviceId, event: ServerEvent) {
        let tx = {
            let txs = self.device_txs.read().await;
            txs.get(device_id).cloned()
        };

        if let Some(tx) = tx {
            let _ = tx.send(event);
        }
    }
}

#[derive(Clone)]
struct NonceCache {
    ttl: Duration,
    entries: Arc<RwLock<HashMap<(DeviceId, String), Instant>>>,
}

impl NonceCache {
    fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn record(&self, device_id: &DeviceId, nonce: &str) -> bool {
        let now = Instant::now();
        let mut entries = self.entries.write().await;
        entries.retain(|_, expires_at| *expires_at > now);
        let key = (device_id.clone(), nonce.to_string());
        if entries.contains_key(&key) {
            return false;
        }
        entries.insert(key, now + self.ttl);
        true
    }
}
