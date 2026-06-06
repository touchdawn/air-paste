use airpaste_core::{ClipId, DeviceId, TransferToken};
use airpaste_protocol::rest_signing_message;
use anyhow::Context;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand_core::OsRng;

pub const PEER_FILE_SIGNATURE_ALG: &str = "ed25519-v1";

#[derive(Clone)]
pub struct DeviceIdentity {
    signing_key: SigningKey,
}

impl DeviceIdentity {
    pub fn generate() -> Self {
        Self {
            signing_key: SigningKey::generate(&mut OsRng),
        }
    }

    pub fn from_private_key_base64(value: &str) -> anyhow::Result<Self> {
        let bytes = STANDARD
            .decode(value)
            .context("failed to decode device private key")?;
        let key_bytes: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("device private key must be 32 bytes"))?;
        Ok(Self {
            signing_key: SigningKey::from_bytes(&key_bytes),
        })
    }

    pub fn private_key_base64(&self) -> String {
        STANDARD.encode(self.signing_key.to_bytes())
    }

    pub fn public_key_base64(&self) -> String {
        STANDARD.encode(self.signing_key.verifying_key().to_bytes())
    }

    pub fn sign_peer_file_request(
        &self,
        clip_id: &ClipId,
        source_device_id: &DeviceId,
        requester_device_id: &DeviceId,
        transfer_token: &TransferToken,
        index: usize,
    ) -> String {
        let message = peer_file_signing_message(
            clip_id.as_str(),
            source_device_id.as_str(),
            requester_device_id.as_str(),
            transfer_token.as_str(),
            index,
        );
        STANDARD.encode(self.signing_key.sign(message.as_bytes()).to_bytes())
    }

    pub fn sign_rest_request(
        &self,
        method: &str,
        path_and_query: &str,
        device_id: &DeviceId,
        timestamp: &str,
        nonce: &str,
        body_sha256: &str,
    ) -> String {
        let message = rest_signing_message(
            method,
            path_and_query,
            device_id,
            timestamp,
            nonce,
            body_sha256,
        );
        STANDARD.encode(self.signing_key.sign(message.as_bytes()).to_bytes())
    }
}

pub fn verify_peer_file_request(
    public_key_base64: &str,
    signature_base64: &str,
    clip_id: &str,
    source_device_id: &str,
    requester_device_id: &str,
    transfer_token: &str,
    index: usize,
) -> anyhow::Result<()> {
    let public_key = STANDARD
        .decode(public_key_base64)
        .context("failed to decode peer public key")?;
    let public_key: [u8; 32] = public_key
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("peer public key must be 32 bytes"))?;
    let verifying_key = VerifyingKey::from_bytes(&public_key)?;

    let signature = STANDARD
        .decode(signature_base64)
        .context("failed to decode peer signature")?;
    let signature: [u8; 64] = signature
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("peer signature must be 64 bytes"))?;
    let signature = Signature::from_bytes(&signature);

    let message = peer_file_signing_message(
        clip_id,
        source_device_id,
        requester_device_id,
        transfer_token,
        index,
    );
    verifying_key.verify(message.as_bytes(), &signature)?;
    Ok(())
}

fn peer_file_signing_message(
    clip_id: &str,
    source_device_id: &str,
    requester_device_id: &str,
    transfer_token: &str,
    index: usize,
) -> String {
    format!(
        "airpaste-peer-file-v1\nclip_id:{clip_id}\nsource_device_id:{source_device_id}\nrequester_device_id:{requester_device_id}\ntransfer_token:{transfer_token}\nindex:{index}\n"
    )
}
