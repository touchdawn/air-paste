mod client;
mod clipboard;
mod config;
mod state_file;

use crate::{
    client::ServerClient,
    clipboard::Clipboard,
    config::Args,
    state_file::{AgentState, StateFile},
};
use airpaste_core::{
    BlobRef, ClipKind, DeviceId, EncryptionInfo, FileClip, FileEntry, FileEntryKind, TextClip,
    TransferToken,
};
use airpaste_protocol::ServerEvent;
use anyhow::Context;
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use std::{sync::Arc, time::Duration};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

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
    let client = ServerClient::new(args.server_url.clone())?;

    let device_id = ensure_device(&client, &state_file, &mut state, &args.device_name).await?;
    tracing::info!(%device_id, server = %args.server_url, "agent started");

    let clipboard = Arc::new(Clipboard::new());
    let last_local_write = Arc::new(Mutex::new(None::<String>));

    let poll_task = if args.publish_clipboard {
        tokio::spawn(poll_clipboard(
            client.clone(),
            clipboard.clone(),
            device_id.clone(),
            last_local_write.clone(),
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
        args.apply_remote,
    ));

    tokio::select! {
        result = poll_task => result??,
        result = ws_task => result??,
        _ = shutdown_signal() => {
            tracing::info!("shutdown requested");
        }
    }

    Ok(())
}

async fn ensure_device(
    client: &ServerClient,
    state_file: &StateFile,
    state: &mut AgentState,
    name: &str,
) -> anyhow::Result<DeviceId> {
    if let Some(device_id) = &state.device_id {
        return Ok(device_id.clone());
    }

    let device = client
        .register_device(name.to_string(), "mvp-agent-public-key".to_string())
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
                last_seen_files = signature;
                publish_file_manifest(&client, &device_id, files).await?;
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
    paths: Vec<std::path::PathBuf>,
) -> anyhow::Result<()> {
    let mut files = Vec::with_capacity(paths.len());
    let mut total_size = 0u64;

    for path in paths {
        let metadata = std::fs::metadata(&path).ok();
        let size = metadata
            .as_ref()
            .filter(|metadata| metadata.is_file())
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        total_size = total_size.saturating_add(size);

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

        files.push(FileEntry {
            relative_path: display_name.clone(),
            display_name,
            size,
            modified_at: None,
            sha256: None,
            kind,
        });
    }

    let file_count = files.len();
    let response = client
        .create_clip(
            device_id.clone(),
            ClipKind::Files(FileClip {
                files,
                total_size,
                transfer_token: TransferToken::new(),
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

    Ok(())
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
    apply_remote: bool,
) -> anyhow::Result<()> {
    loop {
        match run_ws_once(
            &client,
            &clipboard,
            &device_id,
            &last_local_write,
            apply_remote,
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
    apply_remote: bool,
) -> anyhow::Result<()> {
    let (ws, _) = tokio_tungstenite::connect_async(client.ws_url()).await?;
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
            event,
            apply_remote,
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
    event: ServerEvent,
    apply_remote: bool,
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
            let ClipKind::Files(file_clip) = clip.kind else {
                return Ok(());
            };
            tracing::info!(
                source_device_id = %source_device_id,
                file_count = file_clip.files.len(),
                total_size = file_clip.total_size,
                "remote file clipboard available"
            );
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

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install Ctrl+C handler");
}
