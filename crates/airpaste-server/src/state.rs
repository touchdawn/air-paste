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
    /// Bearer token for the `/v1/simple/*` endpoints (simple devices without device signing or
    /// E2E crypto, e.g. iPhone Shortcuts). `None` disables those endpoints entirely.
    pub simple_token: Option<String>,
    /// Latest plaintext text visible to simple devices. In-memory on purpose: the mirrored copy
    /// of a desktop send never touches the database, and a restart simply empties the slot.
    /// (Uploads FROM simple devices do persist briefly, as regular plaintext clips with a TTL,
    /// so desktop agents receive them through the normal notify/apply path.)
    simple_inbox: Arc<RwLock<Option<SimpleInboxEntry>>>,
    nonce_cache: NonceCache,
}

#[derive(Clone)]
pub struct SimpleInboxEntry {
    pub text: String,
    pub source: String,
    pub created_at: airpaste_core::Timestamp,
    pub expires_at: Option<airpaste_core::Timestamp>,
}

impl AppState {
    pub fn new(store: Store, auth_token: Option<String>, simple_token: Option<String>) -> Self {
        Self {
            store,
            hub: EventHub::new(),
            relay_hub: RelayHub::new(),
            auth_token,
            simple_token,
            simple_inbox: Arc::new(RwLock::new(None)),
            nonce_cache: NonceCache::new(Duration::from_secs(300)),
        }
    }

    pub async fn record_nonce(&self, device_id: &DeviceId, nonce: &str) -> bool {
        self.nonce_cache.record(device_id, nonce).await
    }

    pub async fn set_simple_inbox(&self, entry: SimpleInboxEntry) {
        *self.simple_inbox.write().await = Some(entry);
    }

    /// The current simple-inbox entry, dropping it lazily once expired.
    pub async fn simple_inbox(&self) -> Option<SimpleInboxEntry> {
        let entry = self.simple_inbox.read().await.clone()?;
        if let Some(expires_at) = entry.expires_at {
            if expires_at <= airpaste_core::now() {
                *self.simple_inbox.write().await = None;
                return None;
            }
        }
        Some(entry)
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
