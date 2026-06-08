//! Agent side of the server-mediated relay data path.
//!
//! The relay reuses the existing signed peer-file authorization: the recipient sends the
//! same Ed25519-signed request (clip/source/requester/token/index) over the relay pipe,
//! the source claims it from its [`PeerFileRegistry`], and the file bytes are end-to-end
//! encrypted for the recipient before traversing the server, which only forwards opaque
//! frames.

use crate::{
    client::ServerClient,
    identity::{DeviceIdentity, PEER_FILE_SIGNATURE_ALG},
    peer::PeerFileRegistry,
};
use airpaste_core::{ClipId, DeviceId, FileClip, SessionId, WrappedKey};
use airpaste_crypto::{open_bytes, seal_bytes, EncryptionIdentity, Recipient};
use airpaste_protocol::CreateRelaySessionRequest;
use anyhow::{bail, Context};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use futures_util::{stream::SplitStream, SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path, path::PathBuf, time::Duration};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    tungstenite::Message, MaybeTlsStream, WebSocketStream,
};

const RELAY_CHUNK_BYTES: usize = 256 * 1024;
const RELAY_RECV_TIMEOUT: Duration = Duration::from_secs(30);
const RELAY_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// If the source receives no request for this long, end the serve task so it does not
/// linger when the recipient never connects or vanishes.
const RELAY_SERVE_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

async fn connect_relay(
    request: tokio_tungstenite::tungstenite::handshake::client::Request,
) -> anyhow::Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
    let (ws, _) = tokio::time::timeout(RELAY_CONNECT_TIMEOUT, tokio_tungstenite::connect_async(request))
        .await
        .map_err(|_| anyhow::anyhow!("relay websocket connect timed out"))??;
    Ok(ws)
}

type RelayReader = SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>;

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RelayControl {
    Request {
        clip_id: String,
        source_device_id: String,
        requester_device_id: String,
        signature_alg: String,
        signature: String,
        token: String,
        index: usize,
    },
    FileHeader {
        index: usize,
        size: u64,
        body_nonce: String,
        wrapped_keys: Vec<WrappedKey>,
    },
    FileEnd {
        index: usize,
    },
    Error {
        message: String,
    },
}

/// Source side: connect to the relay session and serve files requested by the recipient.
/// Triggered by a `TransferRelayReady` event addressed to this (source) device.
pub async fn serve_relay_session(
    client: ServerClient,
    registry: PeerFileRegistry,
    session_id: SessionId,
    recipient_device_id: DeviceId,
) -> anyhow::Result<()> {
    let recipient = Recipient {
        public_key_base64: recipient_encryption_key(&client, &recipient_device_id).await?,
        device_id: recipient_device_id,
    };

    let request = client.relay_ws_request(session_id.as_str()).await?;
    let ws = connect_relay(request)
        .await
        .context("failed to connect relay websocket as source")?;
    let (mut writer, mut reader) = ws.split();
    tracing::info!(%session_id, "relay source connected");

    loop {
        let message = match tokio::time::timeout(RELAY_SERVE_IDLE_TIMEOUT, reader.next()).await {
            Ok(Some(Ok(message))) => message,
            Ok(Some(Err(error))) => {
                tracing::warn!(%session_id, %error, "relay source receive failed");
                break;
            }
            Ok(None) => break,
            Err(_) => {
                tracing::info!(%session_id, "relay source idle; closing");
                break;
            }
        };
        let Message::Text(text) = message else {
            continue;
        };
        let Ok(RelayControl::Request {
            clip_id,
            source_device_id,
            requester_device_id,
            signature_alg,
            signature,
            token,
            index,
        }) = serde_json::from_str::<RelayControl>(&text)
        else {
            continue;
        };

        match serve_one_file(
            &registry,
            &recipient,
            &token,
            index,
            &clip_id,
            &source_device_id,
            &requester_device_id,
            &signature_alg,
            &signature,
        )
        .await
        {
            Ok((header, ciphertext)) => {
                let send_result: anyhow::Result<()> = async {
                    writer
                        .send(Message::Text(serde_json::to_string(&header)?))
                        .await?;
                    for chunk in ciphertext.chunks(RELAY_CHUNK_BYTES) {
                        writer.send(Message::Binary(chunk.to_vec())).await?;
                    }
                    writer
                        .send(Message::Text(serde_json::to_string(&RelayControl::FileEnd {
                            index,
                        })?))
                        .await?;
                    Ok(())
                }
                .await;
                match send_result {
                    Ok(()) => {
                        // Bytes delivered: consume the one-time grant for this index.
                        registry.commit_served(&token, index);
                        tracing::info!(%session_id, index, "served relay file");
                    }
                    Err(error) => {
                        // Delivery failed mid-stream: release so a retry can re-claim.
                        registry.release(&token, index);
                        return Err(error);
                    }
                }
            }
            Err(reason) => {
                tracing::warn!(%session_id, index, %reason, "relay file request rejected");
                writer
                    .send(Message::Text(serde_json::to_string(&RelayControl::Error {
                        message: reason,
                    })?))
                    .await?;
            }
        }
    }

    tracing::info!(%session_id, "relay source disconnected");
    Ok(())
}

/// Claim and prepare one file for relay delivery. On success the index is *reserved* in the
/// registry (held until the caller commits or releases it after the bytes are sent). A
/// failure after a successful claim releases the reservation so the recipient can retry.
#[allow(clippy::too_many_arguments)]
async fn serve_one_file(
    registry: &PeerFileRegistry,
    recipient: &Recipient,
    token: &str,
    index: usize,
    clip_id: &str,
    source_device_id: &str,
    requester_device_id: &str,
    signature_alg: &str,
    signature: &str,
) -> Result<(RelayControl, Vec<u8>), String> {
    let path = match registry.claim_relay_file(
        token,
        index,
        clip_id,
        source_device_id,
        requester_device_id,
        signature_alg,
        signature,
    ) {
        Ok(Ok(path)) => path,
        Ok(Err(reason)) => return Err(reason.to_string()),
        Err(error) => return Err(format!("registry error: {error}")),
    };

    // Index is reserved now; release it if reading or sealing fails.
    match prepare_relay_file(recipient, &path, index).await {
        Ok(prepared) => Ok(prepared),
        Err(reason) => {
            registry.release(token, index);
            Err(reason)
        }
    }
}

async fn prepare_relay_file(
    recipient: &Recipient,
    path: &Path,
    index: usize,
) -> Result<(RelayControl, Vec<u8>), String> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|error| format!("failed to read file: {error}"))?;
    let size = bytes.len() as u64;
    let sealed = seal_bytes(&bytes, std::slice::from_ref(recipient))
        .map_err(|error| format!("failed to encrypt file: {error}"))?;

    let header = RelayControl::FileHeader {
        index,
        size,
        body_nonce: STANDARD.encode(sealed.body_nonce),
        wrapped_keys: sealed.wrapped_keys,
    };
    Ok((header, sealed.body_ciphertext))
}

/// Recipient side: create a relay session and pull each missing file through the
/// server-mediated pipe, decrypting and verifying it before writing to the cache. Indexes
/// already present in `downloaded` (e.g. fetched directly before a fallback) are skipped, so
/// a partial direct transfer is completed rather than re-pulled from scratch.
#[allow(clippy::too_many_arguments)]
pub async fn download_via_relay(
    client: &ServerClient,
    identity: &DeviceIdentity,
    encryption: &EncryptionIdentity,
    requester_device_id: &DeviceId,
    clip_id: &ClipId,
    source_device_id: &DeviceId,
    file_clip: &FileClip,
    cache_dir: &Path,
    downloaded: &mut BTreeMap<usize, PathBuf>,
) -> anyhow::Result<()> {
    let missing = crate::missing_file_indexes(file_clip, downloaded);
    if missing.is_empty() {
        return Ok(());
    }

    let response = client
        .create_relay_session(CreateRelaySessionRequest {
            clip_id: clip_id.clone(),
            source_device_id: source_device_id.clone(),
            recipient_device_id: requester_device_id.clone(),
            max_bytes: None,
            ttl_seconds: None,
        })
        .await
        .context("failed to create relay session")?;
    let session_id = response.relay.session_id;
    tracing::info!(%session_id, missing = missing.len(), "relay recipient created session");

    let request = client.relay_ws_request(session_id.as_str()).await?;
    let ws = connect_relay(request)
        .await
        .context("failed to connect relay websocket as recipient")?;
    let (mut writer, mut reader) = ws.split();

    let clip_cache_dir = cache_dir.join(file_clip.transfer_token.as_str());
    tokio::fs::create_dir_all(&clip_cache_dir).await?;

    for index in missing {
        let entry = &file_clip.files[index];

        let signature = identity.sign_peer_file_request(
            clip_id,
            source_device_id,
            requester_device_id,
            &file_clip.transfer_token,
            index,
        );
        let request = RelayControl::Request {
            clip_id: clip_id.as_str().to_string(),
            source_device_id: source_device_id.as_str().to_string(),
            requester_device_id: requester_device_id.as_str().to_string(),
            signature_alg: PEER_FILE_SIGNATURE_ALG.to_string(),
            signature,
            token: file_clip.transfer_token.as_str().to_string(),
            index,
        };
        writer
            .send(Message::Text(serde_json::to_string(&request)?))
            .await?;

        let (body_nonce, wrapped_keys) = recv_file_header(&mut reader, index).await?;
        let ciphertext = recv_file_body(&mut reader).await?;

        let nonce = STANDARD.decode(&body_nonce)?;
        let plaintext = open_bytes(
            &ciphertext,
            &nonce,
            &wrapped_keys,
            requester_device_id,
            encryption,
        )
        .map_err(|error| anyhow::anyhow!("failed to decrypt relayed file: {error}"))?;

        if plaintext.len() as u64 != entry.size {
            bail!(
                "relayed file {} size mismatch: got {}, expected {}",
                entry.relative_path,
                plaintext.len(),
                entry.size
            );
        }
        if let Some(expected) = &entry.sha256 {
            let actual = crate::hex_lower(&Sha256::digest(&plaintext));
            if &actual != expected {
                bail!("relayed file {} failed sha256 verification", entry.relative_path);
            }
        }

        let destination =
            crate::safe_cache_path(&clip_cache_dir, &entry.relative_path, &entry.display_name);
        // Recreate the entry's subdirectories (a copied folder's structure) before writing.
        if let Some(parent) = destination.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&destination, &plaintext).await?;
        tracing::info!(path = %destination.display(), "downloaded remote file via relay");
        downloaded.insert(index, destination);
    }

    Ok(())
}

async fn recv_file_header(
    reader: &mut RelayReader,
    index: usize,
) -> anyhow::Result<(String, Vec<WrappedKey>)> {
    loop {
        match recv_with_timeout(reader).await? {
            Message::Text(text) => match serde_json::from_str::<RelayControl>(&text)? {
                RelayControl::FileHeader {
                    index: header_index,
                    body_nonce,
                    wrapped_keys,
                    ..
                } if header_index == index => return Ok((body_nonce, wrapped_keys)),
                RelayControl::Error { message } => bail!("relay source error: {message}"),
                _ => {}
            },
            Message::Close(_) => bail!("relay closed before file header"),
            _ => {}
        }
    }
}

async fn recv_file_body(reader: &mut RelayReader) -> anyhow::Result<Vec<u8>> {
    let mut ciphertext = Vec::new();
    loop {
        match recv_with_timeout(reader).await? {
            Message::Binary(bytes) => ciphertext.extend_from_slice(&bytes),
            Message::Text(text) => match serde_json::from_str::<RelayControl>(&text)? {
                RelayControl::FileEnd { .. } => return Ok(ciphertext),
                RelayControl::Error { message } => bail!("relay source error: {message}"),
                _ => {}
            },
            Message::Close(_) => bail!("relay closed before file end"),
            _ => {}
        }
    }
}

async fn recv_with_timeout(reader: &mut RelayReader) -> anyhow::Result<Message> {
    match tokio::time::timeout(RELAY_RECV_TIMEOUT, reader.next()).await {
        Ok(Some(Ok(message))) => Ok(message),
        Ok(Some(Err(error))) => Err(anyhow::anyhow!("relay receive failed: {error}")),
        Ok(None) => bail!("relay connection closed"),
        Err(_) => bail!("relay receive timed out"),
    }
}

async fn recipient_encryption_key(
    client: &ServerClient,
    device_id: &DeviceId,
) -> anyhow::Result<String> {
    client
        .list_devices()
        .await?
        .into_iter()
        .find(|device| {
            &device.device_id == device_id
                && device.trusted
                && !device.encryption_public_key.trim().is_empty()
        })
        .map(|device| device.encryption_public_key)
        .ok_or_else(|| anyhow::anyhow!("recipient {device_id} has no trusted encryption key"))
}
