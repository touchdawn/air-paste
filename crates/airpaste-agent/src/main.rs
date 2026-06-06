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
    hotkey::spawn_remote_paste_listener,
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
    let state_file = StateFile::new(args.state_path.clone());
    let mut state = state_file.load()?;
    let identity = Arc::new(ensure_identity(&state_file, &mut state)?);
    let auth_token = args.auth_token.clone().filter(|token| !token.is_empty());
    let client = ServerClient::new(args.server_url.clone(), auth_token)?;
    let auto_apply_files = args.auto_apply_files;
    let auto_paste_files = args.auto_paste_files;
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
        &args.device_name,
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
    let cache_dir = args.cache_dir.clone();

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
                            true,
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
) -> anyhow::Result<()> {
    let Some(pending) = pending_file_clip.lock().await.clone() else {
        tracing::info!("remote paste requested with no pending file clip");
        return Ok(());
    };
    let file_clip = &pending.file_clip;
    validate_file_clip(&file_clip, file_policy)?;

    let downloaded_files =
        download_remote_files(client, cache_dir, requester_device_id, identity, &pending).await?;
    if downloaded_files.is_empty() {
        bail!("remote file clip did not contain downloadable files");
    }

    clipboard.set_files(&downloaded_files)?;
    *last_local_file_write.lock().await = clipboard_signature(&downloaded_files);
    *pending_file_clip.lock().await = None;
    tracing::info!(
        file_count = downloaded_files.len(),
        "applied downloaded files to local clipboard"
    );
    if paste_after_apply {
        tokio::time::sleep(Duration::from_millis(120)).await;
        paste.paste()?;
        tracing::info!("sent paste hotkey for downloaded files");
    }

    Ok(())
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
