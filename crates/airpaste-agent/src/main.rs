mod client;
mod clipboard;
mod config;
mod hotkey;
mod identity;
mod paste;
mod peer;
mod state_file;

use crate::{
    client::ServerClient,
    clipboard::Clipboard,
    config::Args,
    hotkey::{spawn_remote_paste_listener, REMOTE_PASTE_HOTKEY_PASTES_AFTER_APPLY},
    identity::DeviceIdentity,
    paste::PasteSimulator,
    peer::{run_peer_server, PeerFileRegistry},
    state_file::{AgentState, StateFile},
};
use airpaste_core::{
    BlobRef, ClipId, ClipKind, DeviceId, EncryptionInfo, FileClip, FileEntry, FileEntryKind,
    TextClip, TransferToken,
};
use airpaste_protocol::{CreateRelaySessionRequest, ServerEvent};
use anyhow::{bail, Context};
use chrono::Duration as ChronoDuration;
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, Mutex},
};
use tokio_tungstenite::tungstenite::Message;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Clone)]
struct FileTransferPolicy {
    max_file_count: usize,
    max_total_file_bytes: u64,
    max_single_file_bytes: u64,
    transfer_token_ttl: Duration,
    transfer_token_ttl_secs: u64,
}

#[derive(Clone)]
struct PendingFileClip {
    clip_id: ClipId,
    source_device_id: DeviceId,
    file_clip: FileClip,
}

#[derive(Clone)]
struct TextPublishPolicy {
    filter_sensitive_text: bool,
    max_text_clip_bytes: usize,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "airpaste_agent=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let args = Args::parse();
    let state_path = args.state_path();
    let device_name = args.device_name();
    let cache_dir = args.cache_dir();
    let state_file = StateFile::new(state_path);
    let mut state = state_file.load()?;
    let identity = Arc::new(ensure_identity(&state_file, &mut state)?);
    let auth_token = args.auth_token.clone().filter(|token| !token.is_empty());
    let client = ServerClient::new(args.server_url.clone(), auth_token)?;
    let auto_apply_files = args.auto_apply_files;
    let auto_paste_files = args.auto_paste_files;
    let text_clip_ttl_secs = args.text_clip_ttl_secs;
    let text_publish_policy = TextPublishPolicy {
        filter_sensitive_text: args.filter_sensitive_text,
        max_text_clip_bytes: args.max_text_clip_bytes,
    };
    let transfer_token_ttl_secs = args.transfer_token_ttl_secs.max(1);
    let file_policy = FileTransferPolicy {
        max_file_count: args.max_file_count,
        max_total_file_bytes: args.max_total_file_bytes,
        max_single_file_bytes: args.max_single_file_bytes,
        transfer_token_ttl: Duration::from_secs(transfer_token_ttl_secs),
        transfer_token_ttl_secs,
    };

    let device_id = ensure_device(
        &client,
        &state_file,
        &mut state,
        &device_name,
        identity.public_key_base64(),
    )
    .await?;
    client
        .set_request_identity(device_id.clone(), identity.clone())
        .await;
    if let Some(pair_code) = args
        .pair_code
        .clone()
        .filter(|pair_code| !pair_code.trim().is_empty())
    {
        let device = client
            .confirm_pairing(pair_code, device_id.clone())
            .await
            .context("failed to confirm pairing")?;
        tracing::info!(trusted = device.trusted, "pairing confirmed");
    }
    if args.create_pair_code {
        let response = client
            .start_pairing(device_id.clone(), args.pair_ttl_seconds)
            .await
            .context("failed to start pairing")?;
        println!("{}", serde_json::to_string(&response)?);
        return Ok(());
    }
    if args.print_latest_clip {
        let clip = client
            .latest_clip()
            .await
            .context("failed to get latest clip")?;
        println!("{}", serde_json::to_string(&clip)?);
        return Ok(());
    }
    if args.apply_latest_files_once {
        let clipboard = Clipboard::new();
        let last_local_file_write = Mutex::new(None::<String>);
        let downloaded_files = apply_latest_files_once(
            &client,
            &clipboard,
            &device_id,
            &last_local_file_write,
            &identity,
            &file_policy,
            &cache_dir,
        )
        .await
        .context("failed to apply latest file clip")?;
        println!("{}", serde_json::to_string(&downloaded_files)?);
        return Ok(());
    }
    if args.replay_latest_clip_signature {
        client
            .replay_latest_clip_signature()
            .await
            .context("failed to verify latest clip replay rejection")?;
        println!("{}", serde_json::json!({"replay_rejected": true}));
        return Ok(());
    }
    if let Some(text) = args.publish_text_once.clone() {
        let utf8_len = text.len() as u64;
        let response = client
            .create_clip(
                device_id.clone(),
                ClipKind::Text(TextClip {
                    utf8_len,
                    preview: None,
                    encrypted_body_ref: BlobRef {
                        id: format!("inline:{utf8_len}"),
                        byte_len: utf8_len,
                    },
                    encrypted_inline_body: Some(text),
                }),
                EncryptionInfo {
                    scheme: "mvp-inline-placeholder".to_string(),
                    key_wrapped_for: vec![device_id.clone()],
                },
                text_clip_expires_at(text_clip_ttl_secs),
            )
            .await
            .context("failed to publish text clip")?;
        println!("{}", serde_json::to_string(&response)?);
        return Ok(());
    }
    if let Some(clip_id) = args.create_relay_for_clip.clone() {
        let recipient_device_id = args
            .relay_recipient_device_id
            .clone()
            .map(DeviceId::from)
            .unwrap_or_else(|| device_id.clone());
        let response = client
            .create_relay_session(CreateRelaySessionRequest {
                clip_id: ClipId::from(clip_id),
                source_device_id: device_id.clone(),
                recipient_device_id,
                max_bytes: args.relay_max_bytes,
                ttl_seconds: args.relay_ttl_seconds,
            })
            .await
            .context("failed to create relay session")?;
        println!("{}", serde_json::to_string(&response)?);
        return Ok(());
    }
    tracing::info!(%device_id, server = %args.server_url, "agent started");

    let clipboard = Arc::new(Clipboard::new());
    let paste = Arc::new(PasteSimulator::new());
    let last_local_write = Arc::new(Mutex::new(None::<String>));
    let last_local_file_write = Arc::new(Mutex::new(None::<String>));
    let pending_file_clip = Arc::new(Mutex::new(None::<PendingFileClip>));
    let peer_registry = PeerFileRegistry::default();
    let peer_public_url = args
        .peer_public_url
        .clone()
        .unwrap_or_else(|| format!("http://{}", args.peer_bind));
    let peer_task = tokio::spawn(run_peer_server(args.peer_bind, peer_registry.clone()));

    let poll_task = if args.publish_clipboard {
        tokio::spawn(poll_clipboard(
            client.clone(),
            clipboard.clone(),
            device_id.clone(),
            last_local_write.clone(),
            last_local_file_write.clone(),
            peer_registry,
            peer_public_url,
            file_policy.clone(),
            text_clip_ttl_secs,
            text_publish_policy,
            Duration::from_millis(args.poll_ms),
        ))
    } else {
        tokio::spawn(std::future::pending())
    };

    let ws_task = tokio::spawn(run_ws(
        client,
        clipboard,
        device_id,
        last_local_write,
        last_local_file_write,
        pending_file_clip,
        args.apply_remote,
        paste,
        identity,
        args.remote_paste_hotkey,
        file_policy,
        auto_apply_files,
        auto_paste_files,
        cache_dir,
    ));

    tokio::select! {
        result = peer_task => result??,
        result = poll_task => result??,
        result = ws_task => result??,
        _ = shutdown_signal() => {
            tracing::info!("shutdown requested");
        }
    }

    Ok(())
}

fn ensure_identity(
    state_file: &StateFile,
    state: &mut AgentState,
) -> anyhow::Result<DeviceIdentity> {
    if let Some(private_key) = &state.device_private_key {
        return DeviceIdentity::from_private_key_base64(private_key);
    }

    let identity = DeviceIdentity::generate();
    state.device_private_key = Some(identity.private_key_base64());
    state.device_id = None;
    state_file.save(state)?;
    Ok(identity)
}

async fn ensure_device(
    client: &ServerClient,
    state_file: &StateFile,
    state: &mut AgentState,
    name: &str,
    public_key: String,
) -> anyhow::Result<DeviceId> {
    if let Some(device_id) = &state.device_id {
        return Ok(device_id.clone());
    }

    let device = client
        .register_device(name.to_string(), public_key)
        .await
        .context("failed to register device")?;
    state.device_id = Some(device.device_id.clone());
    state_file.save(state)?;
    Ok(device.device_id)
}

async fn poll_clipboard(
    client: ServerClient,
    clipboard: Arc<Clipboard>,
    device_id: DeviceId,
    last_local_write: Arc<Mutex<Option<String>>>,
    last_local_file_write: Arc<Mutex<Option<String>>>,
    peer_registry: PeerFileRegistry,
    peer_public_url: String,
    file_policy: FileTransferPolicy,
    text_clip_ttl_secs: u64,
    text_publish_policy: TextPublishPolicy,
    interval: Duration,
) -> anyhow::Result<()> {
    let mut last_seen = clipboard.get_text().unwrap_or_default();
    let mut last_seen_files = clipboard_signature(&clipboard.get_files()?.unwrap_or_default());
    let mut ticker = tokio::time::interval(interval);

    loop {
        ticker.tick().await;
        if let Some(files) = clipboard.get_files()? {
            let signature = clipboard_signature(&files);
            if !files.is_empty() && signature != last_seen_files {
                let ignored = {
                    let mut guard = last_local_file_write.lock().await;
                    if guard.as_ref() == signature.as_ref() {
                        *guard = None;
                        true
                    } else {
                        false
                    }
                };
                last_seen_files = signature;
                if ignored {
                    continue;
                }
                if let Err(error) = publish_file_manifest(
                    &client,
                    &device_id,
                    &peer_registry,
                    &peer_public_url,
                    &file_policy,
                    files,
                )
                .await
                {
                    tracing::warn!(%error, "ignored file clipboard");
                }
                continue;
            }
        }

        let Some(text) = clipboard.get_text()? else {
            continue;
        };
        if text.is_empty() || Some(text.clone()) == last_seen {
            continue;
        }

        let ignored = {
            let mut guard = last_local_write.lock().await;
            if guard.as_ref() == Some(&text) {
                *guard = None;
                true
            } else {
                false
            }
        };
        last_seen = Some(text.clone());
        if ignored {
            continue;
        }
        if let Some(reason) = text_publish_skip_reason(&text, &text_publish_policy) {
            tracing::warn!(
                reason,
                byte_len = text.len(),
                "skipped text clipboard publish"
            );
            continue;
        }

        let utf8_len = text.len() as u64;
        let clip = ClipKind::Text(TextClip {
            utf8_len,
            preview: None,
            encrypted_body_ref: BlobRef {
                id: format!("inline:{utf8_len}"),
                byte_len: utf8_len,
            },
            encrypted_inline_body: Some(text),
        });
        let response = client
            .create_clip(
                device_id.clone(),
                clip,
                EncryptionInfo {
                    scheme: "mvp-inline-placeholder".to_string(),
                    key_wrapped_for: vec![device_id.clone()],
                },
                text_clip_expires_at(text_clip_ttl_secs),
            )
            .await?;
        tracing::info!(clip_id = %response.clip_id, "published text clip");
    }
}

async fn publish_file_manifest(
    client: &ServerClient,
    device_id: &DeviceId,
    peer_registry: &PeerFileRegistry,
    peer_public_url: &str,
    file_policy: &FileTransferPolicy,
    paths: Vec<std::path::PathBuf>,
) -> anyhow::Result<()> {
    if paths.len() > file_policy.max_file_count {
        bail!(
            "file clipboard contains {} items, above configured limit {}",
            paths.len(),
            file_policy.max_file_count
        );
    }

    let mut files = Vec::with_capacity(paths.len());
    let mut total_size = 0u64;
    let transfer_token = TransferToken::new();

    for path in &paths {
        let metadata = std::fs::metadata(&path).ok();
        let is_file = metadata.as_ref().is_some_and(|metadata| metadata.is_file());
        let size = metadata
            .as_ref()
            .filter(|_| is_file)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if is_file && size > file_policy.max_single_file_bytes {
            bail!(
                "file {} is {} bytes, above configured single-file limit {}",
                path.display(),
                size,
                file_policy.max_single_file_bytes
            );
        }
        total_size = total_size.saturating_add(size);
        if total_size > file_policy.max_total_file_bytes {
            bail!(
                "file clipboard is {} bytes, above configured limit {}",
                total_size,
                file_policy.max_total_file_bytes
            );
        }

        let display_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unnamed")
            .to_string();
        let kind = if metadata.as_ref().is_some_and(|metadata| metadata.is_dir()) {
            FileEntryKind::Directory
        } else {
            FileEntryKind::File
        };
        let sha256 = if is_file {
            Some(
                hash_file_sha256(path)
                    .await
                    .with_context(|| format!("failed to hash file {}", path.display()))?,
            )
        } else {
            None
        };

        files.push(FileEntry {
            relative_path: display_name.clone(),
            display_name,
            size,
            modified_at: None,
            sha256,
            kind,
        });
    }

    let transfer_expires_at = airpaste_core::now()
        + ChronoDuration::seconds(file_policy.transfer_token_ttl_secs.min(i64::MAX as u64) as i64);
    let file_count = files.len();
    peer_registry.register(
        &transfer_token,
        None,
        device_id.clone(),
        trusted_device_public_keys(client).await?,
        paths.clone(),
        file_policy.transfer_token_ttl,
    )?;
    let response = client
        .create_clip(
            device_id.clone(),
            ClipKind::Files(FileClip {
                files,
                total_size,
                transfer_token: transfer_token.clone(),
                source_peer_url: Some(peer_public_url.trim_end_matches('/').to_string()),
                transfer_expires_at: Some(transfer_expires_at),
            }),
            EncryptionInfo {
                scheme: "mvp-manifest-placeholder".to_string(),
                key_wrapped_for: vec![device_id.clone()],
            },
            None,
        )
        .await?;
    tracing::info!(
        clip_id = %response.clip_id,
        file_count,
        total_size,
        "published file manifest"
    );
    peer_registry.bind_clip_id(&transfer_token, response.clip_id)?;

    Ok(())
}

fn text_clip_expires_at(ttl_secs: u64) -> Option<airpaste_core::Timestamp> {
    if ttl_secs == 0 {
        None
    } else {
        Some(airpaste_core::now() + ChronoDuration::seconds(ttl_secs.min(i64::MAX as u64) as i64))
    }
}

fn text_publish_skip_reason(text: &str, policy: &TextPublishPolicy) -> Option<&'static str> {
    if policy.max_text_clip_bytes > 0 && text.len() > policy.max_text_clip_bytes {
        return Some("text too large");
    }
    if !policy.filter_sensitive_text {
        return None;
    }

    let trimmed = text.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("-----begin ") && lower.contains("private key-----") {
        return Some("private key");
    }
    if looks_like_jwt(trimmed) {
        return Some("jwt");
    }
    if contains_bearer_token(trimmed) {
        return Some("bearer token");
    }
    if contains_secret_assignment(trimmed) {
        return Some("secret assignment");
    }
    if looks_like_one_time_code(trimmed) {
        return Some("one-time code");
    }
    if contains_credit_card_like_number(trimmed) {
        return Some("credit-card-like number");
    }

    None
}

fn looks_like_jwt(text: &str) -> bool {
    let token = text.trim();
    let mut parts = token.split('.');
    let Some(header) = parts.next() else {
        return false;
    };
    let Some(payload) = parts.next() else {
        return false;
    };
    let Some(signature) = parts.next() else {
        return false;
    };
    parts.next().is_none()
        && header.len() >= 8
        && payload.len() >= 8
        && signature.len() >= 16
        && [header, payload, signature]
            .iter()
            .all(|part| part.bytes().all(is_base64url_byte))
}

fn contains_bearer_token(text: &str) -> bool {
    text.split_whitespace().any(|word| {
        let Some(token) = word.strip_prefix("Bearer ") else {
            return false;
        };
        token.len() >= 20 && token.bytes().all(is_token_byte)
    }) || text
        .split_ascii_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .any(|pair| {
            pair[0].eq_ignore_ascii_case("bearer")
                && pair[1].len() >= 20
                && pair[1].bytes().all(is_token_byte)
        })
}

fn contains_secret_assignment(text: &str) -> bool {
    const SECRET_KEYS: &[&str] = &[
        "api_key",
        "apikey",
        "access_key",
        "access_token",
        "auth_token",
        "client_secret",
        "password",
        "passwd",
        "private_key",
        "secret",
        "token",
    ];

    text.lines().any(|line| {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            return false;
        }
        let Some(separator_index) = line.find(['=', ':']) else {
            return false;
        };
        let key = line[..separator_index]
            .trim()
            .trim_matches(['"', '\'', '`'])
            .to_ascii_lowercase();
        let value = line[separator_index + 1..]
            .trim()
            .trim_matches(['"', '\'', '`', ',', ';']);
        value.len() >= 8
            && SECRET_KEYS
                .iter()
                .any(|secret_key| key.contains(secret_key))
    })
}

fn looks_like_one_time_code(text: &str) -> bool {
    let code = text.trim();
    (4..=8).contains(&code.len()) && code.bytes().all(|byte| byte.is_ascii_digit())
}

fn contains_credit_card_like_number(text: &str) -> bool {
    let mut digits = String::new();
    for ch in text.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_digit() {
            digits.push(ch);
            continue;
        }
        if ch == ' ' || ch == '-' {
            continue;
        }
        if credit_card_digits_match(&digits) {
            return true;
        }
        digits.clear();
    }
    credit_card_digits_match(&digits)
}

fn credit_card_digits_match(digits: &str) -> bool {
    (13..=19).contains(&digits.len()) && luhn_valid(digits)
}

fn luhn_valid(digits: &str) -> bool {
    let mut sum = 0u32;
    let mut double = false;
    for byte in digits.bytes().rev() {
        let mut digit = (byte - b'0') as u32;
        if double {
            digit *= 2;
            if digit > 9 {
                digit -= 9;
            }
        }
        sum += digit;
        double = !double;
    }
    sum % 10 == 0
}

fn is_base64url_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'~' | b'+' | b'/' | b'=')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_policy() -> TextPublishPolicy {
        TextPublishPolicy {
            filter_sensitive_text: true,
            max_text_clip_bytes: 128 * 1024,
        }
    }

    #[test]
    fn skips_obvious_sensitive_text() {
        let policy = default_policy();
        assert_eq!(
            text_publish_skip_reason(
                "-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n-----END OPENSSH PRIVATE KEY-----",
                &policy
            ),
            Some("private key")
        );
        assert_eq!(
            text_publish_skip_reason(
                "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c",
                &policy
            ),
            Some("jwt")
        );
        assert_eq!(
            text_publish_skip_reason("Authorization: Bearer abcdefghijklmnopqrstuvwx", &policy),
            Some("bearer token")
        );
        assert_eq!(
            text_publish_skip_reason("DATABASE_PASSWORD=correct-horse", &policy),
            Some("secret assignment")
        );
        assert_eq!(
            text_publish_skip_reason("123456", &policy),
            Some("one-time code")
        );
        assert_eq!(
            text_publish_skip_reason("4111 1111 1111 1111", &policy),
            Some("credit-card-like number")
        );
    }

    #[test]
    fn allows_normal_clipboard_text() {
        assert_eq!(
            text_publish_skip_reason("airpaste publish smoke text", &default_policy()),
            None
        );
    }

    #[test]
    fn sensitive_filter_can_be_disabled_without_disabling_size_guard() {
        let policy = TextPublishPolicy {
            filter_sensitive_text: false,
            max_text_clip_bytes: 16,
        };
        assert_eq!(
            text_publish_skip_reason("DATABASE_PASSWORD=correct-horse", &policy),
            Some("text too large")
        );

        let policy = TextPublishPolicy {
            filter_sensitive_text: false,
            max_text_clip_bytes: 0,
        };
        assert_eq!(
            text_publish_skip_reason("DATABASE_PASSWORD=correct-horse", &policy),
            None
        );
    }
}

async fn trusted_device_public_keys(
    client: &ServerClient,
) -> anyhow::Result<HashMap<DeviceId, String>> {
    Ok(client
        .list_devices()
        .await?
        .into_iter()
        .filter(|device| device.trusted && !device.public_key.trim().is_empty())
        .map(|device| (device.device_id, device.public_key))
        .collect())
}

fn clipboard_signature(paths: &[std::path::PathBuf]) -> Option<String> {
    if paths.is_empty() {
        return None;
    }
    Some(
        paths
            .iter()
            .map(|path| path.to_string_lossy())
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

async fn run_ws(
    client: ServerClient,
    clipboard: Arc<Clipboard>,
    device_id: DeviceId,
    last_local_write: Arc<Mutex<Option<String>>>,
    last_local_file_write: Arc<Mutex<Option<String>>>,
    pending_file_clip: Arc<Mutex<Option<PendingFileClip>>>,
    apply_remote: bool,
    paste: Arc<PasteSimulator>,
    identity: Arc<DeviceIdentity>,
    remote_paste_hotkey: bool,
    file_policy: FileTransferPolicy,
    auto_apply_files: bool,
    auto_paste_files: bool,
    cache_dir: PathBuf,
) -> anyhow::Result<()> {
    let (hotkey_tx, mut hotkey_rx) = mpsc::unbounded_channel();
    if remote_paste_hotkey && apply_remote {
        match spawn_remote_paste_listener(hotkey_tx) {
            Ok(()) => {
                let hotkey_client = client.clone();
                let hotkey_clipboard = clipboard.clone();
                let hotkey_device_id = device_id.clone();
                let hotkey_last_local_file_write = last_local_file_write.clone();
                let hotkey_pending_file_clip = pending_file_clip.clone();
                let hotkey_paste = paste.clone();
                let hotkey_identity = identity.clone();
                let hotkey_file_policy = file_policy.clone();
                let hotkey_cache_dir = cache_dir.clone();
                let paste_after_hotkey = REMOTE_PASTE_HOTKEY_PASTES_AFTER_APPLY;
                tokio::spawn(async move {
                    while hotkey_rx.recv().await.is_some() {
                        if let Err(error) = apply_pending_file_clip(
                            &hotkey_client,
                            &hotkey_clipboard,
                            &hotkey_device_id,
                            &hotkey_last_local_file_write,
                            &hotkey_pending_file_clip,
                            &hotkey_paste,
                            &hotkey_identity,
                            &hotkey_file_policy,
                            paste_after_hotkey,
                            &hotkey_cache_dir,
                        )
                        .await
                        {
                            tracing::warn!(%error, "remote paste hotkey failed");
                        }
                    }
                    tracing::warn!("remote paste hotkey listener channel closed");
                });
            }
            Err(error) => tracing::warn!(%error, "remote paste hotkey disabled"),
        }
    }

    loop {
        match run_ws_once(
            &client,
            &clipboard,
            &device_id,
            &last_local_write,
            &last_local_file_write,
            &pending_file_clip,
            apply_remote,
            &paste,
            &identity,
            &file_policy,
            auto_apply_files,
            auto_paste_files,
            &cache_dir,
        )
        .await
        {
            Ok(()) => tracing::warn!("websocket disconnected"),
            Err(error) => tracing::warn!(%error, "websocket failed"),
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn run_ws_once(
    client: &ServerClient,
    clipboard: &Clipboard,
    device_id: &DeviceId,
    last_local_write: &Mutex<Option<String>>,
    last_local_file_write: &Mutex<Option<String>>,
    pending_file_clip: &Mutex<Option<PendingFileClip>>,
    apply_remote: bool,
    paste: &PasteSimulator,
    identity: &DeviceIdentity,
    file_policy: &FileTransferPolicy,
    auto_apply_files: bool,
    auto_paste_files: bool,
    cache_dir: &Path,
) -> anyhow::Result<()> {
    let (ws, _) = tokio_tungstenite::connect_async(client.ws_request().await?).await?;
    let (mut writer, mut reader) = ws.split();
    writer
        .send(Message::Text(serde_json::to_string(
            &airpaste_protocol::ClientEvent::Hello {
                device_id: device_id.clone(),
            },
        )?))
        .await?;

    while let Some(message) = reader.next().await {
        let message = message?;
        let Message::Text(text) = message else {
            continue;
        };
        let event: ServerEvent = serde_json::from_str(&text)?;
        handle_server_event(
            client,
            clipboard,
            device_id,
            last_local_write,
            last_local_file_write,
            pending_file_clip,
            event,
            apply_remote,
            paste,
            identity,
            file_policy,
            auto_apply_files,
            auto_paste_files,
            cache_dir,
        )
        .await?;
    }
    Ok(())
}

async fn handle_server_event(
    client: &ServerClient,
    clipboard: &Clipboard,
    device_id: &DeviceId,
    last_local_write: &Mutex<Option<String>>,
    last_local_file_write: &Mutex<Option<String>>,
    pending_file_clip: &Mutex<Option<PendingFileClip>>,
    event: ServerEvent,
    apply_remote: bool,
    paste: &PasteSimulator,
    identity: &DeviceIdentity,
    file_policy: &FileTransferPolicy,
    auto_apply_files: bool,
    auto_paste_files: bool,
    cache_dir: &Path,
) -> anyhow::Result<()> {
    match event {
        ServerEvent::ClipCreated {
            clip_id,
            source_device_id,
            kind,
        } if apply_remote && source_device_id != *device_id && kind == "text" => {
            let clip = client.get_clip(clip_id).await?;
            let ClipKind::Text(text_clip) = clip.kind else {
                return Ok(());
            };
            if let Some(text) = text_clip.encrypted_inline_body {
                clipboard.set_text(&text)?;
                *last_local_write.lock().await = Some(text);
                tracing::info!("applied remote text clip");
            }
        }
        ServerEvent::ClipCreated {
            clip_id,
            source_device_id,
            kind,
        } if apply_remote && source_device_id != *device_id && kind == "files" => {
            let clip = client.get_clip(clip_id).await?;
            let pending_clip_id = clip.clip_id.clone();
            let pending_source_device_id = clip.source_device_id.clone();
            let ClipKind::Files(file_clip) = clip.kind else {
                return Ok(());
            };
            tracing::info!(
                source_device_id = %source_device_id,
                file_count = file_clip.files.len(),
                total_size = file_clip.total_size,
                "remote file clipboard available"
            );
            *pending_file_clip.lock().await = Some(PendingFileClip {
                clip_id: pending_clip_id,
                source_device_id: pending_source_device_id,
                file_clip,
            });
            if auto_apply_files {
                apply_pending_file_clip(
                    client,
                    clipboard,
                    device_id,
                    last_local_file_write,
                    pending_file_clip,
                    paste,
                    identity,
                    file_policy,
                    auto_paste_files,
                    cache_dir,
                )
                .await?;
            }
        }
        ServerEvent::HelloAck { .. }
        | ServerEvent::DeviceOnline { .. }
        | ServerEvent::DeviceOffline { .. }
        | ServerEvent::ClipCreated { .. }
        | ServerEvent::TransferOffer { .. }
        | ServerEvent::TransferAnswer { .. }
        | ServerEvent::TransferCandidate { .. }
        | ServerEvent::TransferCancelled { .. }
        | ServerEvent::TransferRelayReady { .. } => {}
        ServerEvent::Error { message } => tracing::warn!(%message, "server event error"),
    }
    Ok(())
}

async fn apply_pending_file_clip(
    client: &ServerClient,
    clipboard: &Clipboard,
    requester_device_id: &DeviceId,
    last_local_file_write: &Mutex<Option<String>>,
    pending_file_clip: &Mutex<Option<PendingFileClip>>,
    paste: &PasteSimulator,
    identity: &DeviceIdentity,
    file_policy: &FileTransferPolicy,
    paste_after_apply: bool,
    cache_dir: &Path,
) -> anyhow::Result<Vec<PathBuf>> {
    let Some(pending) = pending_file_clip.lock().await.clone() else {
        tracing::info!("remote paste requested with no pending file clip");
        return Ok(Vec::new());
    };
    let downloaded_files = apply_file_clip(
        client,
        clipboard,
        requester_device_id,
        last_local_file_write,
        &pending,
        identity,
        file_policy,
        cache_dir,
    )
    .await?;
    *pending_file_clip.lock().await = None;
    if paste_after_apply {
        tokio::time::sleep(Duration::from_millis(120)).await;
        paste.paste()?;
        tracing::info!("sent paste hotkey for downloaded files");
    }

    Ok(downloaded_files)
}

async fn apply_latest_files_once(
    client: &ServerClient,
    clipboard: &Clipboard,
    requester_device_id: &DeviceId,
    last_local_file_write: &Mutex<Option<String>>,
    identity: &DeviceIdentity,
    file_policy: &FileTransferPolicy,
    cache_dir: &Path,
) -> anyhow::Result<Vec<PathBuf>> {
    let Some(clip) = client.latest_clip().await? else {
        bail!("no latest clip is available");
    };
    if clip.source_device_id == *requester_device_id {
        bail!("latest file clip was published by this device");
    }

    let clip_id = clip.clip_id.clone();
    let source_device_id = clip.source_device_id.clone();
    let ClipKind::Files(file_clip) = clip.kind else {
        bail!("latest clip is not a file clip");
    };

    let pending = PendingFileClip {
        clip_id,
        source_device_id,
        file_clip,
    };
    apply_file_clip(
        client,
        clipboard,
        requester_device_id,
        last_local_file_write,
        &pending,
        identity,
        file_policy,
        cache_dir,
    )
    .await
}

async fn apply_file_clip(
    client: &ServerClient,
    clipboard: &Clipboard,
    requester_device_id: &DeviceId,
    last_local_file_write: &Mutex<Option<String>>,
    pending: &PendingFileClip,
    identity: &DeviceIdentity,
    file_policy: &FileTransferPolicy,
    cache_dir: &Path,
) -> anyhow::Result<Vec<PathBuf>> {
    validate_file_clip(&pending.file_clip, file_policy)?;

    let downloaded_files =
        download_remote_files(client, cache_dir, requester_device_id, identity, pending).await?;
    if downloaded_files.is_empty() {
        bail!("remote file clip did not contain downloadable files");
    }

    clipboard.set_files(&downloaded_files)?;
    *last_local_file_write.lock().await = clipboard_signature(&downloaded_files);
    tracing::info!(
        file_count = downloaded_files.len(),
        "applied downloaded files to local clipboard"
    );

    Ok(downloaded_files)
}

fn validate_file_clip(
    file_clip: &FileClip,
    file_policy: &FileTransferPolicy,
) -> anyhow::Result<()> {
    if file_clip.files.len() > file_policy.max_file_count {
        bail!(
            "remote file clip contains {} items, above configured limit {}",
            file_clip.files.len(),
            file_policy.max_file_count
        );
    }
    if file_clip.total_size > file_policy.max_total_file_bytes {
        bail!(
            "remote file clip is {} bytes, above configured limit {}",
            file_clip.total_size,
            file_policy.max_total_file_bytes
        );
    }
    for entry in &file_clip.files {
        if matches!(entry.kind, FileEntryKind::File)
            && entry.size > file_policy.max_single_file_bytes
        {
            bail!(
                "remote file {} is {} bytes, above configured single-file limit {}",
                entry.relative_path,
                entry.size,
                file_policy.max_single_file_bytes
            );
        }
    }
    if let Some(expires_at) = &file_clip.transfer_expires_at {
        if expires_at < &airpaste_core::now() {
            bail!("remote file clip transfer token expired at {expires_at}");
        }
    }
    Ok(())
}

async fn download_remote_files(
    client: &ServerClient,
    cache_dir: &Path,
    requester_device_id: &DeviceId,
    identity: &DeviceIdentity,
    pending: &PendingFileClip,
) -> anyhow::Result<Vec<PathBuf>> {
    let file_clip = &pending.file_clip;
    let Some(source_peer_url) = &file_clip.source_peer_url else {
        bail!("remote file clip has no source_peer_url");
    };

    let clip_cache_dir = cache_dir.join(file_clip.transfer_token.as_str());
    tokio::fs::create_dir_all(&clip_cache_dir).await?;
    let mut downloaded_files = Vec::new();

    for (index, entry) in file_clip.files.iter().enumerate() {
        if !matches!(entry.kind, FileEntryKind::File) {
            tracing::warn!(
                relative_path = %entry.relative_path,
                "skipping non-file entry in MVP transfer"
            );
            continue;
        }

        let url = format!(
            "{}/v1/files/{}/{}",
            source_peer_url.trim_end_matches('/'),
            file_clip.transfer_token.as_str(),
            index
        );
        let response = client
            .open_peer_file_download(
                &url,
                &pending.clip_id,
                &pending.source_device_id,
                requester_device_id,
                identity,
            )
            .await?;
        let destination = safe_cache_path(&clip_cache_dir, &entry.display_name);
        download_peer_file_to_cache(response, entry, &destination).await?;
        tracing::info!(path = %destination.display(), "downloaded remote file");
        downloaded_files.push(destination);
    }

    Ok(downloaded_files)
}

async fn hash_file_sha256(path: &Path) -> anyhow::Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];

    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(hex_lower(&hasher.finalize()))
}

async fn download_peer_file_to_cache(
    mut response: reqwest::Response,
    entry: &FileEntry,
    destination: &Path,
) -> anyhow::Result<()> {
    let temporary = destination.with_extension("airpaste-download");
    let mut file = tokio::fs::File::create(&temporary).await?;
    let mut hasher = Sha256::new();
    let mut downloaded_size = 0u64;

    let result = async {
        while let Some(chunk) = response.chunk().await? {
            downloaded_size = downloaded_size.saturating_add(chunk.len() as u64);
            hasher.update(&chunk);
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        drop(file);

        if downloaded_size != entry.size {
            bail!(
                "remote file size mismatch for {}: manifest declared {} bytes, downloaded {} bytes",
                entry.relative_path,
                entry.size,
                downloaded_size
            );
        }

        if let Some(expected_sha256) = &entry.sha256 {
            let actual_sha256 = hex_lower(&hasher.finalize());
            if !expected_sha256.eq_ignore_ascii_case(&actual_sha256) {
                bail!(
                    "remote file SHA-256 mismatch for {}: manifest declared {}, downloaded {}",
                    entry.relative_path,
                    expected_sha256,
                    actual_sha256
                );
            }
        } else {
            tracing::warn!(
                relative_path = %entry.relative_path,
                "remote file manifest omitted SHA-256; verified size only"
            );
        }

        tokio::fs::rename(&temporary, destination).await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;

    if result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    result
}

fn safe_cache_path(cache_dir: &Path, display_name: &str) -> PathBuf {
    let sanitized = display_name
        .chars()
        .map(|ch| match ch {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' | '/' | '\\' => '_',
            ch if ch.is_control() => '_',
            ch => ch,
        })
        .collect::<String>();
    let file_name = if sanitized.trim().is_empty() {
        "download.bin"
    } else {
        sanitized.trim()
    };
    cache_dir.join(file_name)
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        value.push(HEX[(byte >> 4) as usize] as char);
        value.push(HEX[(byte & 0x0f) as usize] as char);
    }
    value
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install Ctrl+C handler");
}
