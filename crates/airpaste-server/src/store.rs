use airpaste_core::{
    now, ClipId, ClipRecord, Device, DeviceId, PairingCode, PairingSession, RelaySession, SessionId,
};
use chrono::Duration;
use redb::{Database, ReadableTable, TableDefinition};
use serde::{de::DeserializeOwned, Serialize};
use std::{path::Path, sync::Arc};
use thiserror::Error;

const DEVICES: TableDefinition<&str, &[u8]> = TableDefinition::new("devices");
const PAIRINGS: TableDefinition<&str, &[u8]> = TableDefinition::new("pairing_sessions");
const CLIPS: TableDefinition<&str, &[u8]> = TableDefinition::new("clip_records");
const RELAYS: TableDefinition<&str, &[u8]> = TableDefinition::new("relay_sessions");

#[derive(Clone)]
pub struct Store {
    db: Arc<Database>,
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Redb(#[from] redb::Error),
    #[error("storage transaction error: {0}")]
    Transaction(#[from] redb::TransactionError),
    #[error("storage table error: {0}")]
    Table(#[from] redb::TableError),
    #[error("storage commit error: {0}")]
    Commit(#[from] redb::CommitError),
    #[error("storage error: {0}")]
    Storage(#[from] redb::StorageError),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("not found")]
    NotFound,
}

pub type StoreResult<T> = Result<T, StoreError>;

impl Store {
    pub fn open(path: &Path) -> StoreResult<Self> {
        let db = Database::create(path)?;
        let store = Self { db: Arc::new(db) };
        store.init()?;
        Ok(store)
    }

    fn init(&self) -> StoreResult<()> {
        let write_txn = self.db.begin_write()?;
        {
            write_txn.open_table(DEVICES)?;
            write_txn.open_table(PAIRINGS)?;
            write_txn.open_table(CLIPS)?;
            write_txn.open_table(RELAYS)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    pub fn register_device(
        &self,
        name: String,
        public_key: String,
        encryption_public_key: String,
    ) -> StoreResult<Device> {
        let trusted = self.list::<Device>(DEVICES)?.is_empty();
        let device = Device {
            device_id: DeviceId::new(),
            name,
            public_key,
            encryption_public_key,
            trusted,
            created_at: now(),
            last_seen_at: None,
        };
        self.put(DEVICES, device.device_id.as_str(), &device)?;
        Ok(device)
    }

    pub fn trust_device(&self, device_id: &DeviceId) -> StoreResult<Device> {
        let mut device: Device = self
            .get(DEVICES, device_id.as_str())?
            .ok_or(StoreError::NotFound)?;
        device.trusted = true;
        self.put(DEVICES, device.device_id.as_str(), &device)?;
        Ok(device)
    }

    pub fn get_device(&self, device_id: &DeviceId) -> StoreResult<Option<Device>> {
        self.get(DEVICES, device_id.as_str())
    }

    pub fn touch_device(&self, device_id: &DeviceId) -> StoreResult<Option<Device>> {
        let Some(mut device) = self.get::<Device>(DEVICES, device_id.as_str())? else {
            return Ok(None);
        };
        device.last_seen_at = Some(now());
        self.put(DEVICES, device.device_id.as_str(), &device)?;
        Ok(Some(device))
    }

    pub fn list_devices(&self) -> StoreResult<Vec<Device>> {
        self.list(DEVICES)
    }

    pub fn start_pairing(
        &self,
        created_by: Option<DeviceId>,
        ttl_seconds: Option<i64>,
    ) -> StoreResult<PairingSession> {
        let ttl = ttl_seconds.unwrap_or(600).clamp(60, 3600);
        let session = PairingSession {
            code: PairingCode::new(),
            created_by,
            candidate_device_id: None,
            expires_at: now() + Duration::seconds(ttl),
            confirmed: false,
        };
        self.put(PAIRINGS, &session.code.0, &session)?;
        Ok(session)
    }

    pub fn confirm_pairing(&self, code: &PairingCode, device_id: &DeviceId) -> StoreResult<Device> {
        let mut session: PairingSession =
            self.get(PAIRINGS, &code.0)?.ok_or(StoreError::NotFound)?;
        if session.expires_at < now() {
            return Err(StoreError::NotFound);
        }
        session.confirmed = true;
        session.candidate_device_id = Some(device_id.clone());
        self.put(PAIRINGS, &session.code.0, &session)?;
        self.trust_device(device_id)
    }

    pub fn create_clip(&self, mut clip: ClipRecord) -> StoreResult<ClipRecord> {
        if clip.clip_id.as_str().is_empty() {
            clip.clip_id = ClipId::new();
        }
        self.cleanup_expired_clips()?;
        self.put(CLIPS, clip.clip_id.as_str(), &clip)?;
        Ok(clip)
    }

    pub fn get_clip(&self, clip_id: &ClipId) -> StoreResult<Option<ClipRecord>> {
        let Some(clip) = self.get::<ClipRecord>(CLIPS, clip_id.as_str())? else {
            return Ok(None);
        };
        if clip_is_expired(&clip) {
            self.delete_clip(clip_id)?;
            return Ok(None);
        }
        Ok(Some(clip))
    }

    pub fn latest_clip(&self) -> StoreResult<Option<ClipRecord>> {
        self.cleanup_expired_clips()?;
        let clips = self.list::<ClipRecord>(CLIPS)?;
        Ok(clips.into_iter().max_by_key(|clip| clip.created_at))
    }

    pub fn clip_history(&self, limit: usize) -> StoreResult<Vec<ClipRecord>> {
        self.cleanup_expired_clips()?;
        let mut clips = self.list::<ClipRecord>(CLIPS)?;
        clips.sort_by_key(|clip| std::cmp::Reverse(clip.created_at));
        clips.truncate(limit);
        Ok(clips)
    }

    pub fn delete_clip(&self, clip_id: &ClipId) -> StoreResult<bool> {
        let write_txn = self.db.begin_write()?;
        let removed = {
            let mut table = write_txn.open_table(CLIPS)?;
            let removed = table.remove(clip_id.as_str())?.is_some();
            removed
        };
        write_txn.commit()?;
        Ok(removed)
    }

    pub fn create_relay_session(
        &self,
        clip_id: ClipId,
        source_device_id: DeviceId,
        recipient_device_id: DeviceId,
        max_bytes: Option<u64>,
        ttl_seconds: Option<i64>,
    ) -> StoreResult<RelaySession> {
        let ttl = ttl_seconds.unwrap_or(1800).clamp(60, 3600);
        let relay = RelaySession {
            session_id: SessionId::new(),
            clip_id,
            source_device_id,
            recipient_device_id,
            max_bytes: max_bytes.unwrap_or(2 * 1024 * 1024 * 1024),
            expires_at: now() + Duration::seconds(ttl),
            created_at: now(),
        };
        self.put(RELAYS, relay.session_id.as_str(), &relay)?;
        Ok(relay)
    }

    pub fn get_relay_session(&self, session_id: &SessionId) -> StoreResult<Option<RelaySession>> {
        self.get(RELAYS, session_id.as_str())
    }

    fn put<T: Serialize>(
        &self,
        definition: TableDefinition<&str, &[u8]>,
        key: &str,
        value: &T,
    ) -> StoreResult<()> {
        let bytes = serde_json::to_vec(value)?;
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(definition)?;
            table.insert(key, bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    fn get<T: DeserializeOwned>(
        &self,
        definition: TableDefinition<&str, &[u8]>,
        key: &str,
    ) -> StoreResult<Option<T>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(definition)?;
        let Some(value) = table.get(key)? else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_slice(value.value())?))
    }

    fn list<T: DeserializeOwned>(
        &self,
        definition: TableDefinition<&str, &[u8]>,
    ) -> StoreResult<Vec<T>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(definition)?;
        let mut values = Vec::new();
        for item in table.iter()? {
            let (_, value) = item?;
            values.push(serde_json::from_slice(value.value())?);
        }
        Ok(values)
    }

    fn cleanup_expired_clips(&self) -> StoreResult<()> {
        let now = now();
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(CLIPS)?;
            let mut expired = Vec::new();
            for item in table.iter()? {
                let (key, value) = item?;
                let clip: ClipRecord = serde_json::from_slice(value.value())?;
                if clip
                    .expires_at
                    .as_ref()
                    .is_some_and(|expires_at| expires_at <= &now)
                {
                    expired.push(key.value().to_string());
                }
            }
            for key in expired {
                table.remove(key.as_str())?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }
}

impl From<redb::DatabaseError> for StoreError {
    fn from(value: redb::DatabaseError) -> Self {
        StoreError::Redb(value.into())
    }
}

fn clip_is_expired(clip: &ClipRecord) -> bool {
    clip.expires_at
        .as_ref()
        .is_some_and(|expires_at| expires_at <= &now())
}
