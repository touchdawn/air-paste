use airpaste_core::{
    ClipId, ClipKind, Device, DeviceId, EncryptionInfo, PairingCode, RelaySession, Timestamp,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub now: Timestamp,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegisterDeviceRequest {
    pub name: String,
    pub public_key: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegisterDeviceResponse {
    pub device: Device,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StartPairingRequest {
    pub created_by: Option<DeviceId>,
    pub ttl_seconds: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StartPairingResponse {
    pub code: PairingCode,
    pub expires_at: Timestamp,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConfirmPairingRequest {
    pub code: PairingCode,
    pub device_id: DeviceId,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConfirmPairingResponse {
    pub device: Device,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateClipRequest {
    pub source_device_id: DeviceId,
    pub expires_at: Option<Timestamp>,
    pub kind: ClipKind,
    pub encryption: EncryptionInfo,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateClipResponse {
    pub clip_id: ClipId,
    pub created_at: Timestamp,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClipSummary {
    pub clip_id: ClipId,
    pub source_device_id: DeviceId,
    pub created_at: Timestamp,
    pub expires_at: Option<Timestamp>,
    pub kind: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateRelaySessionRequest {
    pub clip_id: ClipId,
    pub source_device_id: DeviceId,
    pub recipient_device_id: DeviceId,
    pub max_bytes: Option<u64>,
    pub ttl_seconds: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateRelaySessionResponse {
    pub relay: RelaySession,
}
