use airpaste_core::{
    ClipId, ClipKind, Device, DeviceId, EncryptionInfo, PairingCode, RelaySession, Timestamp,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const AIRPASTE_DEVICE_ID_HEADER: &str = "x-airpaste-device-id";
pub const AIRPASTE_SIGNATURE_ALG_HEADER: &str = "x-airpaste-signature-alg";
pub const AIRPASTE_SIGNATURE_HEADER: &str = "x-airpaste-signature";
pub const AIRPASTE_TIMESTAMP_HEADER: &str = "x-airpaste-timestamp";
pub const AIRPASTE_NONCE_HEADER: &str = "x-airpaste-nonce";
pub const AIRPASTE_BODY_SHA256_HEADER: &str = "x-airpaste-body-sha256";
pub const AIRPASTE_REST_SIGNATURE_ALG: &str = "ed25519-rest-v1";
pub const AIRPASTE_EMPTY_BODY_SHA256: &str = "47DEQpj8HBSa-_TImW-5JCeuQeRkm5NMpJWZG3hSuFU";

pub fn rest_body_sha256_base64url(body: &[u8]) -> String {
    base64url_no_pad(&Sha256::digest(body))
}

pub fn rest_signing_message(
    method: &str,
    path_and_query: &str,
    device_id: &DeviceId,
    timestamp: &str,
    nonce: &str,
    body_sha256: &str,
) -> String {
    format!(
        "airpaste-rest-v1\nmethod:{}\npath:{}\ndevice_id:{}\ntimestamp:{}\nnonce:{}\nbody_sha256:{}\n",
        method.to_ascii_uppercase(),
        path_and_query,
        device_id.as_str(),
        timestamp,
        nonce,
        body_sha256,
    )
}

fn base64url_no_pad(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut output = String::with_capacity((bytes.len() * 4).div_ceil(3));
    let mut chunks = bytes.chunks_exact(3);
    for chunk in &mut chunks {
        let value = ((chunk[0] as u32) << 16) | ((chunk[1] as u32) << 8) | (chunk[2] as u32);
        output.push(ALPHABET[((value >> 18) & 0x3f) as usize] as char);
        output.push(ALPHABET[((value >> 12) & 0x3f) as usize] as char);
        output.push(ALPHABET[((value >> 6) & 0x3f) as usize] as char);
        output.push(ALPHABET[(value & 0x3f) as usize] as char);
    }

    let remainder = chunks.remainder();
    if remainder.len() == 1 {
        let value = (remainder[0] as u32) << 16;
        output.push(ALPHABET[((value >> 18) & 0x3f) as usize] as char);
        output.push(ALPHABET[((value >> 12) & 0x3f) as usize] as char);
    } else if remainder.len() == 2 {
        let value = ((remainder[0] as u32) << 16) | ((remainder[1] as u32) << 8);
        output.push(ALPHABET[((value >> 18) & 0x3f) as usize] as char);
        output.push(ALPHABET[((value >> 12) & 0x3f) as usize] as char);
        output.push(ALPHABET[((value >> 6) & 0x3f) as usize] as char);
    }

    output
}

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
