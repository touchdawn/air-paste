use crate::{ClipId, DeviceId, PairingCode, SessionId, Timestamp, TransferToken};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Device {
    pub device_id: DeviceId,
    pub name: String,
    pub public_key: String,
    pub trusted: bool,
    pub created_at: Timestamp,
    pub last_seen_at: Option<Timestamp>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PairingSession {
    pub code: PairingCode,
    pub created_by: Option<DeviceId>,
    pub candidate_device_id: Option<DeviceId>,
    pub expires_at: Timestamp,
    pub confirmed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RelaySession {
    pub session_id: SessionId,
    pub clip_id: ClipId,
    pub source_device_id: DeviceId,
    pub recipient_device_id: DeviceId,
    pub max_bytes: u64,
    pub expires_at: Timestamp,
    pub created_at: Timestamp,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClipRecord {
    pub clip_id: ClipId,
    pub source_device_id: DeviceId,
    pub created_at: Timestamp,
    pub expires_at: Option<Timestamp>,
    pub kind: ClipKind,
    pub encryption: EncryptionInfo,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipKind {
    Text(TextClip),
    Image(ImageClip),
    Files(FileClip),
}

impl ClipKind {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Text(_) => "text",
            Self::Image(_) => "image",
            Self::Files(_) => "files",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EncryptionInfo {
    pub scheme: String,
    pub key_wrapped_for: Vec<DeviceId>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TextClip {
    pub utf8_len: u64,
    pub preview: Option<String>,
    pub encrypted_body_ref: BlobRef,
    pub encrypted_inline_body: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageClip {
    pub width: u32,
    pub height: u32,
    pub mime: String,
    pub byte_len: u64,
    pub payload_ref: PayloadRef,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileClip {
    pub files: Vec<FileEntry>,
    pub total_size: u64,
    pub transfer_token: TransferToken,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileEntry {
    pub relative_path: String,
    pub display_name: String,
    pub size: u64,
    pub modified_at: Option<Timestamp>,
    pub sha256: Option<String>,
    pub kind: FileEntryKind,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileEntryKind {
    File,
    Directory,
    Symlink,
    MacAppBundle,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlobRef {
    pub id: String,
    pub byte_len: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayloadRef {
    ServerBlob(BlobRef),
    RemoteManifest,
}
