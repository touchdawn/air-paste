mod client;
mod clipboard;
mod config;
mod discovery;
mod hotkey;
mod identity;
mod paste;
mod peer;
mod relay;
mod state_file;

use crate::{
    client::ServerClient,
    clipboard::Clipboard,
    discovery::PeerDirectory,
    hotkey::{spawn_hotkey_listener, HotkeyAction, REMOTE_PASTE_HOTKEY_PASTES_AFTER_APPLY},
    identity::DeviceIdentity,
    paste::PasteSimulator,
    peer::{run_peer_server, PeerFileRegistry},
};
pub use crate::config::{app_support_dir, Args, ClipboardMode, DEFAULT_SERVER_URL};
pub use crate::state_file::{AgentState, StateFile};
use airpaste_core::{
    BlobRef, ClipId, ClipKind, DeviceId, EncryptionInfo, FileClip, FileEntry, FileEntryKind,
    TextClip, TransferToken,
};
use airpaste_crypto::EncryptionIdentity;
use airpaste_protocol::{CreateClipResponse, CreateRelaySessionRequest, ServerEvent};
use anyhow::{bail, Context};
use chrono::Duration as ChronoDuration;
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, Mutex},
};
use tokio_tungstenite::tungstenite::Message;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Timeout for establishing a websocket connection (control or relay) so a hung connect
/// during a network change recovers quickly instead of blocking on the OS TCP timeout.
const WS_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Base and max delay between control-websocket reconnect attempts (exponential backoff).
const WS_RECONNECT_BASE: Duration = Duration::from_secs(2);
const WS_RECONNECT_MAX: Duration = Duration::from_secs(30);

/// Isolated-mode timing for the synthetic paste dance: settle the clipboard after writing
/// our text, paste, then let the target app consume it before restoring the user's clipboard.
const CLIPBOARD_SETTLE: Duration = Duration::from_millis(80);
const PASTE_CONSUME: Duration = Duration::from_millis(150);

/// Display name of the isolated-mode hotkey modifier (Alt on Windows, the same physical key is
/// Option on macOS), used in user-facing strings. The chords are `<mod>+C` / `<mod>+V`.
pub const HOTKEY_MOD_NAME: &str = if cfg!(target_os = "macos") { "Option" } else { "Alt" };

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

/// Monotonic arrival counter so the isolated-mode `Alt+V` can paste whichever channel
/// (text inbox vs pending files) arrived most recently.
static CLIP_ARRIVAL_SEQ: AtomicU64 = AtomicU64::new(0);
fn next_arrival_seq() -> u64 {
    CLIP_ARRIVAL_SEQ.fetch_add(1, Ordering::Relaxed) + 1
}

/// Live progress of the current file download, for the UI. Process-global because an embedded
/// agent applies one file clip at a time; concurrent transfers would simply share the latest.
#[derive(Clone, Debug)]
pub struct TransferProgress {
    pub done: usize,
    pub total: usize,
    pub current: String,
}

static TRANSFER_PROGRESS: std::sync::Mutex<Option<TransferProgress>> =
    std::sync::Mutex::new(None);

pub(crate) fn set_transfer_progress(progress: Option<TransferProgress>) {
    if let Ok(mut guard) = TRANSFER_PROGRESS.lock() {
        *guard = progress;
    }
}

/// Clears the transfer progress whenever an `apply` returns (success, bail, or error).
struct TransferProgressGuard;
impl Drop for TransferProgressGuard {
    fn drop(&mut self) {
        set_transfer_progress(None);
    }
}

/// Clipboard integration mode plus the in-app inbox (recent remote texts and file clips held
/// in-app, newest first) and arrival markers used to pick text-vs-files by recency.
#[derive(Clone)]
struct ClipboardCtx {
    // Shared so the tray UI can flip the mode at runtime; read live on every poll/apply.
    isolated: Arc<AtomicBool>,
    inbox: Arc<Mutex<VecDeque<InboxItem>>>,
    // Arrival sequence of the latest isolated inbox text and the latest pending file clip.
    inbox_seq: Arc<AtomicU64>,
    file_seq: Arc<AtomicU64>,
}

/// One entry in the in-app inbox: a remote text, or a remote file clip the UI can download
/// on demand (the 下载 button).
struct InboxItem {
    /// Arrival sequence of this entry; also the id the UI passes back to act on it.
    id: u64,
    kind: InboxItemKind,
}

enum InboxItemKind {
    Text(String),
    Files {
        clip: PendingFileClip,
        state: FileDownloadState,
    },
}

/// Download lifecycle of a file entry in the inbox.
#[derive(Clone, Debug)]
pub enum FileDownloadState {
    Idle,
    Downloading,
    /// Downloaded into the cache (paths held here); the references were also written to the
    /// system clipboard.
    Done(Vec<PathBuf>),
    Failed(String),
}

/// A UI view of one inbox entry (newest first).
#[derive(Clone, Debug)]
pub enum InboxEntry {
    Text(String),
    Files {
        /// Pass to `AgentHandle::download_inbox_files` / `copy_inbox_files`.
        id: u64,
        count: usize,
        total_size: u64,
        names: Vec<String>,
        state: FileDownloadState,
    },
}

impl ClipboardCtx {
    fn is_isolated(&self) -> bool {
        self.isolated.load(Ordering::Relaxed)
    }
}

/// How many recent isolated-inbox texts to retain for the UI history.
const INBOX_HISTORY_MAX: usize = 20;

/// A summary of remote files waiting to be applied (downloaded on `Alt+V`), for the UI.
#[derive(Clone, Debug)]
pub struct PendingFiles {
    pub count: usize,
    pub total_size: u64,
    pub names: Vec<String>,
}

/// A device from the server's registry, for the "connected devices" view (mainly useful when this
/// host runs the embedded server). Built from a periodic `GET /v1/devices` snapshot.
#[derive(Clone, Debug)]
pub struct DeviceInfo {
    pub device_id: String,
    pub name: String,
    pub trusted: bool,
    /// Recent enough last-seen to count as currently connected.
    pub online: bool,
    /// Whether this row is the local host device.
    pub is_self: bool,
    /// Seconds since the server last saw the device; `None` if it never connected. May be slightly
    /// negative under clock skew — callers clamp to 0 for display.
    pub last_seen_secs: Option<i64>,
}

/// A device whose last-seen is within this window counts as online. The server refreshes last-seen
/// on WebSocket connect and every 30s heartbeat, so 90s tolerates a missed beat.
const PRESENCE_WINDOW_SECS: i64 = 90;

/// Outcome of a UI-initiated send (`AgentHandle::send_text` / `send_files`).
#[derive(Clone, Debug)]
pub enum SendStatus {
    Sending,
    Sent,
    Failed(String),
}

/// Everything a UI-initiated file publish needs from the running agent: the peer-file grant
/// registry (shared with the peer server), the advertised peer URL, and the transfer limits.
#[derive(Clone)]
struct FilePublishCtx {
    registry: PeerFileRegistry,
    peer_public_url: String,
    policy: FileTransferPolicy,
}

/// Everything a UI-initiated file download needs from the running agent — the same context
/// the hotkey apply path uses (`apply_file_clip`).
#[derive(Clone)]
struct FileApplyCtx {
    clipboard: Arc<Clipboard>,
    last_local_file_write: Arc<Mutex<Option<String>>>,
    identity: Arc<DeviceIdentity>,
    encryption: Arc<EncryptionIdentity>,
    policy: FileTransferPolicy,
    peer_directory: PeerDirectory,
    prefer_relay: bool,
    cache_dir: PathBuf,
}

/// Shared, observable agent state for embedders (the tray UI). Updated by the running agent.
pub struct AgentShared {
    inbox: Arc<Mutex<VecDeque<InboxItem>>>,
    connected: AtomicBool,
    device_name: std::sync::Mutex<String>,
    device_id: std::sync::Mutex<Option<String>>,
    isolated: Arc<AtomicBool>,
    // Last fatal error from the embedded agent (e.g. registration failed), surfaced in the UI.
    last_error: std::sync::Mutex<Option<String>>,
    // The latest remote file clip waiting to be applied (shared with the running agent so the
    // UI observes it for free). Cleared once the files are applied.
    pending_files: Arc<Mutex<Option<PendingFileClip>>>,
    // Arrival sequence of the latest inbox text / pending files (for recency-based paste).
    inbox_seq: Arc<AtomicU64>,
    file_seq: Arc<AtomicU64>,
    // The Tokio runtime the agent runs on, so the UI can launch async actions (pair code).
    runtime: tokio::runtime::Handle,
    // The connected client + this device id, published once registration succeeds, so the UI
    // can mint a pairing code without the CLI.
    client: std::sync::Mutex<Option<(ServerClient, DeviceId)>>,
    // Latest UI-requested pair code: Ok(code) / Ok("生成中…") / Err(reason).
    pair_code: std::sync::Mutex<Option<Result<String, String>>>,
    // Snapshot of the server's device registry for the UI, refreshed by a background poll while
    // connected. Empty until this device is trusted enough to list devices.
    devices: std::sync::Mutex<Vec<DeviceInfo>>,
    // Status of the latest UI-initiated text / file send (see `AgentHandle::send_text`,
    // `AgentHandle::send_files`).
    send_text: std::sync::Mutex<Option<SendStatus>>,
    send_files: std::sync::Mutex<Option<SendStatus>>,
    // Set by the running agent once its peer file server context exists, so the UI can
    // publish file manifests (drag-and-drop send).
    file_publish: std::sync::Mutex<Option<FilePublishCtx>>,
    // Set by the running agent once its download context exists, so the UI can pull inbox
    // file entries on demand (the 下载 button).
    file_apply: std::sync::Mutex<Option<FileApplyCtx>>,
    // Text-clip TTL from the launch args, so UI sends expire like hotkey/poll publishes do.
    text_clip_ttl_secs: u64,
}

impl AgentShared {
    fn new(args: &Args) -> Self {
        Self {
            inbox: Arc::new(Mutex::new(VecDeque::new())),
            connected: AtomicBool::new(false),
            device_name: std::sync::Mutex::new(args.device_name()),
            device_id: std::sync::Mutex::new(None),
            isolated: Arc::new(AtomicBool::new(
                args.clipboard_mode == ClipboardMode::Isolated,
            )),
            last_error: std::sync::Mutex::new(None),
            pending_files: Arc::new(Mutex::new(None)),
            inbox_seq: Arc::new(AtomicU64::new(0)),
            file_seq: Arc::new(AtomicU64::new(0)),
            // Both callers (spawn_embedded / run_cli) construct this from within a Tokio runtime.
            runtime: tokio::runtime::Handle::current(),
            client: std::sync::Mutex::new(None),
            pair_code: std::sync::Mutex::new(None),
            devices: std::sync::Mutex::new(Vec::new()),
            send_text: std::sync::Mutex::new(None),
            send_files: std::sync::Mutex::new(None),
            file_publish: std::sync::Mutex::new(None),
            file_apply: std::sync::Mutex::new(None),
            text_clip_ttl_secs: args.text_clip_ttl_secs,
        }
    }
}

/// A handle the tray UI uses to observe the embedded agent.
#[derive(Clone)]
pub struct AgentHandle {
    shared: Arc<AgentShared>,
}

impl AgentHandle {
    /// Whether the control WebSocket is currently connected.
    pub fn connected(&self) -> bool {
        self.shared.connected.load(Ordering::Relaxed)
    }

    pub fn device_name(&self) -> String {
        self.shared.device_name.lock().unwrap().clone()
    }

    pub fn device_id(&self) -> Option<String> {
        self.shared.device_id.lock().unwrap().clone()
    }

    pub fn isolated(&self) -> bool {
        self.shared.isolated.load(Ordering::Relaxed)
    }

    /// Toggle isolated mode at runtime. Note: this changes the inbound/outbound text behaviour
    /// live, but the `Alt+C` global hotkey is only registered if the agent *started* in
    /// isolated mode (hotkeys cannot be re-registered after launch).
    pub fn set_isolated(&self, value: bool) {
        self.shared.isolated.store(value, Ordering::Relaxed);
    }

    /// The most recent remote text held in the isolated-mode inbox, if any.
    pub fn latest_inbox(&self) -> Option<String> {
        self.shared.inbox.try_lock().ok().and_then(|guard| {
            guard.iter().find_map(|item| match &item.kind {
                InboxItemKind::Text(text) => Some(text.clone()),
                InboxItemKind::Files { .. } => None,
            })
        })
    }

    /// Recent inbox entries (texts and file clips), newest first (up to `INBOX_HISTORY_MAX`).
    pub fn inbox_history(&self) -> Vec<InboxEntry> {
        self.shared
            .inbox
            .try_lock()
            .map(|guard| guard.iter().map(inbox_entry_view).collect())
            .unwrap_or_default()
    }

    /// The last fatal error from the embedded agent (e.g. failed to register), if any.
    pub fn last_error(&self) -> Option<String> {
        self.shared.last_error.lock().unwrap().clone()
    }

    /// Remote files waiting to be applied (via `Alt+V`), if any. Present in both clipboard
    /// modes; in isolated mode they coexist with the text inbox and the more recent one wins.
    pub fn pending_files(&self) -> Option<PendingFiles> {
        let guard = self.shared.pending_files.try_lock().ok()?;
        let pending = guard.as_ref()?;
        let names = pending
            .file_clip
            .files
            .iter()
            .map(|entry| entry.relative_path.clone())
            .collect();
        Some(PendingFiles {
            count: pending.file_clip.files.len(),
            total_size: pending.file_clip.total_size,
            names,
        })
    }

    /// Live progress of the current file download, if one is running.
    pub fn transfer_progress(&self) -> Option<TransferProgress> {
        TRANSFER_PROGRESS.lock().ok().and_then(|guard| guard.clone())
    }

    /// Snapshot of devices known to the server (for the "connected devices" view), refreshed in
    /// the background while connected.
    pub fn devices(&self) -> Vec<DeviceInfo> {
        self.shared.devices.lock().unwrap().clone()
    }

    /// Mint a pairing code for another device (only this device must be trusted). Non-blocking:
    /// the result lands in `pair_code()`. Requires an active connection.
    pub fn generate_pair_code(&self) {
        let snapshot = self.shared.client.lock().unwrap().clone();
        let Some((client, device_id)) = snapshot else {
            *self.shared.pair_code.lock().unwrap() = Some(Err("尚未连接".to_string()));
            return;
        };
        *self.shared.pair_code.lock().unwrap() = Some(Ok("生成中…".to_string()));
        let shared = self.shared.clone();
        self.shared.runtime.spawn(async move {
            let entry = match client.start_pairing(device_id, Some(600)).await {
                Ok(response) => Ok(response.code.0),
                Err(error) => Err(format!("{error:#}")),
            };
            *shared.pair_code.lock().unwrap() = Some(entry);
        });
    }

    /// The latest pair code the UI requested: `Ok(code)` (or "生成中…") / `Err(reason)`.
    pub fn pair_code(&self) -> Option<Result<String, String>> {
        self.shared.pair_code.lock().unwrap().clone()
    }

    /// Dismiss the displayed pair code.
    pub fn clear_pair_code(&self) {
        *self.shared.pair_code.lock().unwrap() = None;
    }

    /// Publish `text` to all trusted devices, end-to-end encrypted — the UI equivalent of
    /// `Alt+C`. Non-blocking: the outcome lands in `send_text_status()`. Requires an active
    /// connection. Like `Alt+C` (and unlike clipboard polling), this skips the sensitive-text
    /// filter: an explicit send is taken as user intent.
    pub fn send_text(&self, text: String) {
        let snapshot = self.shared.client.lock().unwrap().clone();
        let Some((client, device_id)) = snapshot else {
            *self.shared.send_text.lock().unwrap() =
                Some(SendStatus::Failed("尚未连接".to_string()));
            return;
        };
        *self.shared.send_text.lock().unwrap() = Some(SendStatus::Sending);
        let shared = self.shared.clone();
        let ttl_secs = self.shared.text_clip_ttl_secs;
        self.shared.runtime.spawn(async move {
            let status = match publish_text_clip(&client, &device_id, text, ttl_secs).await {
                Ok(response) => {
                    tracing::info!(clip_id = %response.clip_id, "published text clip from UI");
                    SendStatus::Sent
                }
                Err(error) => SendStatus::Failed(format!("{error:#}")),
            };
            *shared.send_text.lock().unwrap() = Some(status);
        });
    }

    /// Status of the most recent `send_text`, if any.
    pub fn send_text_status(&self) -> Option<SendStatus> {
        self.shared.send_text.lock().unwrap().clone()
    }

    /// Publish a file manifest for `paths` (e.g. files dragged onto the window) — the UI
    /// equivalent of copying files: recipients see a pending file clip and pull it with
    /// `Alt+V`. Hashing and publishing run on the agent runtime; the outcome lands in
    /// `send_files_status()`. Requires an active connection.
    pub fn send_files(&self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let client_snapshot = self.shared.client.lock().unwrap().clone();
        let publish_snapshot = self.shared.file_publish.lock().unwrap().clone();
        let (Some((client, device_id)), Some(publish)) = (client_snapshot, publish_snapshot)
        else {
            *self.shared.send_files.lock().unwrap() =
                Some(SendStatus::Failed("尚未连接".to_string()));
            return;
        };
        *self.shared.send_files.lock().unwrap() = Some(SendStatus::Sending);
        let shared = self.shared.clone();
        self.shared.runtime.spawn(async move {
            let status = match publish_file_manifest(
                &client,
                &device_id,
                &publish.registry,
                &publish.peer_public_url,
                &publish.policy,
                paths,
            )
            .await
            {
                Ok(()) => SendStatus::Sent,
                Err(error) => SendStatus::Failed(format!("{error:#}")),
            };
            *shared.send_files.lock().unwrap() = Some(status);
        });
    }

    /// Status of the most recent `send_files`, if any.
    pub fn send_files_status(&self) -> Option<SendStatus> {
        self.shared.send_files.lock().unwrap().clone()
    }

    /// Download an inbox file entry (the UI 下载 button): pull the files (direct → relay
    /// fallback) into the cache and write the references to the system clipboard, exactly
    /// like the `Alt+V` apply. Non-blocking; live progress is visible via
    /// `transfer_progress()` and the outcome lands in the entry's `FileDownloadState`.
    pub fn download_inbox_files(&self, id: u64) {
        let client_snapshot = self.shared.client.lock().unwrap().clone();
        let apply_snapshot = self.shared.file_apply.lock().unwrap().clone();
        let shared = self.shared.clone();
        self.shared.runtime.spawn(async move {
            let (Some((client, device_id)), Some(ctx)) = (client_snapshot, apply_snapshot)
            else {
                set_inbox_file_state(
                    &shared.inbox,
                    id,
                    FileDownloadState::Failed("尚未连接".to_string()),
                )
                .await;
                return;
            };
            // Claim the entry (Idle/Failed -> Downloading); a vanished, in-flight, or already
            // downloaded entry is a no-op.
            let Some(clip) = claim_inbox_download(&shared.inbox, id).await else {
                return;
            };
            let result = apply_file_clip(
                &client,
                &ctx.clipboard,
                &device_id,
                &ctx.last_local_file_write,
                &clip,
                &ctx.identity,
                &ctx.encryption,
                &ctx.policy,
                &ctx.peer_directory,
                ctx.prefer_relay,
                &ctx.cache_dir,
            )
            .await;
            let state = match result {
                Ok(paths) => {
                    tracing::info!(file_count = paths.len(), "downloaded inbox file entry");
                    FileDownloadState::Done(paths)
                }
                Err(error) => FileDownloadState::Failed(format!("{error:#}")),
            };
            set_inbox_file_state(&shared.inbox, id, state).await;
        });
    }

    /// Re-copy an already-downloaded inbox file entry's local paths to the system clipboard
    /// (the entry's button after a successful download).
    pub fn copy_inbox_files(&self, id: u64) {
        let apply_snapshot = self.shared.file_apply.lock().unwrap().clone();
        let shared = self.shared.clone();
        self.shared.runtime.spawn(async move {
            let Some(ctx) = apply_snapshot else {
                return;
            };
            let paths = shared.inbox.lock().await.iter().find_map(|item| {
                match &item.kind {
                    InboxItemKind::Files {
                        state: FileDownloadState::Done(paths),
                        ..
                    } if item.id == id => Some(paths.clone()),
                    _ => None,
                }
            });
            let Some(paths) = paths else {
                return;
            };
            match ctx.clipboard.set_files(&paths) {
                Ok(()) => {
                    *ctx.last_local_file_write.lock().await = clipboard_signature(&paths);
                    tracing::info!(file_count = paths.len(), "copied inbox files to clipboard");
                }
                Err(error) => {
                    tracing::warn!(%error, "failed to copy inbox files to clipboard");
                }
            }
        });
    }
}

/// Project an internal inbox item into its UI view.
fn inbox_entry_view(item: &InboxItem) -> InboxEntry {
    match &item.kind {
        InboxItemKind::Text(text) => InboxEntry::Text(text.clone()),
        InboxItemKind::Files { clip, state } => InboxEntry::Files {
            id: item.id,
            count: clip.file_clip.files.len(),
            total_size: clip.file_clip.total_size,
            names: clip
                .file_clip
                .files
                .iter()
                .map(|entry| entry.relative_path.clone())
                .collect(),
            state: state.clone(),
        },
    }
}

/// Move an Idle/Failed file entry to Downloading and return its clip. `None` when the entry
/// is gone, not a file entry, already downloading, or already done.
async fn claim_inbox_download(
    inbox: &Mutex<VecDeque<InboxItem>>,
    id: u64,
) -> Option<PendingFileClip> {
    let mut guard = inbox.lock().await;
    let item = guard.iter_mut().find(|item| item.id == id)?;
    let InboxItemKind::Files { clip, state } = &mut item.kind else {
        return None;
    };
    match state {
        FileDownloadState::Downloading | FileDownloadState::Done(_) => None,
        FileDownloadState::Idle | FileDownloadState::Failed(_) => {
            *state = FileDownloadState::Downloading;
            Some(clip.clone())
        }
    }
}

/// Set the download state of an inbox file entry by id. Best-effort; a vanished entry is a
/// no-op.
async fn set_inbox_file_state(
    inbox: &Mutex<VecDeque<InboxItem>>,
    id: u64,
    state: FileDownloadState,
) {
    if let Some(item) = inbox.lock().await.iter_mut().find(|item| item.id == id) {
        if let InboxItemKind::Files { state: slot, .. } = &mut item.kind {
            *slot = state;
        }
    }
}

/// Set the download state of the inbox file entry for `clip_id`, so an `Alt+V` apply is
/// reflected on the matching inbox row (its one-time grant is consumed either way).
async fn set_inbox_file_state_by_clip(
    inbox: &Mutex<VecDeque<InboxItem>>,
    clip_id: &ClipId,
    state: FileDownloadState,
) {
    if let Some(item) = inbox.lock().await.iter_mut().find(
        |item| matches!(&item.kind, InboxItemKind::Files { clip, .. } if &clip.clip_id == clip_id),
    ) {
        if let InboxItemKind::Files { state: slot, .. } = &mut item.kind {
            *slot = state;
        }
    }
}

/// Initialize tracing to stderr. Safe to call once per process; a second call is ignored.
pub fn init_tracing() {
    let _ = tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "airpaste_agent=debug".into()),
        )
        // Log to stderr (unbuffered): keeps stdout for data output (e.g. --print-latest-clip
        // JSON) and ensures a long-running agent's logs flush promptly when redirected to a
        // file, which block-buffered stdout does not do on Windows.
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .try_init();
}

/// Parse agent arguments from the process command line (so embedders need no `clap` dep).
pub fn parse_args() -> Args {
    Args::parse()
}

/// CLI entry point: initialize tracing, parse args, and run to completion.
pub async fn run_cli() -> anyhow::Result<()> {
    init_tracing();
    let args = Args::parse();
    let shared = Arc::new(AgentShared::new(&args));
    run(args, shared).await
}

/// Start the agent in the background for an embedder (the tray UI), returning a handle to
/// observe it. Tracing must already be initialized by the embedder. Must be called from
/// within a Tokio runtime.
pub fn spawn_embedded(args: Args) -> AgentHandle {
    let shared = Arc::new(AgentShared::new(&args));
    let handle = AgentHandle {
        shared: shared.clone(),
    };
    tokio::spawn(async move {
        let result = run(args, shared.clone()).await;
        // `run` aborts every agent task before returning, so nothing can flip this back to
        // true: from here on the UI truthfully shows the agent as stopped.
        shared.connected.store(false, Ordering::Relaxed);
        let message = match result {
            Ok(()) => {
                tracing::info!("embedded agent exited");
                "agent exited".to_string()
            }
            Err(error) => {
                tracing::error!(%error, "embedded agent stopped");
                format!("{error:#}")
            }
        };
        *shared.last_error.lock().unwrap() = Some(message);
    });
    handle
}

async fn run(args: Args, shared: Arc<AgentShared>) -> anyhow::Result<()> {
    let state_path = args.state_path();
    if let Some(legacy) = args.legacy_state_path_hint() {
        tracing::warn!(
            legacy = %legacy.display(),
            state = %state_path.display(),
            "found a legacy state file in the working directory but the default state path is \
             now per-user; a fresh device identity will be created. To keep the old identity, \
             move the legacy file to the new path or pass --state-path / AIRPASTE_STATE"
        );
    }
    let device_name = args.device_name();
    let cache_dir = args.cache_dir();
    let state_file = StateFile::new(state_path);
    let mut state = state_file.load()?;
    let identity = Arc::new(ensure_identity(&state_file, &mut state)?);
    let encryption = Arc::new(ensure_encryption_identity(&state_file, &mut state)?);
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
        encryption.public_key_base64(),
    )
    .await?;
    *shared.device_id.lock().unwrap() = Some(device_id.as_str().to_string());
    client
        .set_request_identity(device_id.clone(), identity.clone())
        .await;
    // Publish the signed client so the UI can mint a pairing code (this device is trusted once
    // registered/paired; the server enforces trust on start_pairing).
    *shared.client.lock().unwrap() = Some((client.clone(), device_id.clone()));
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
            &encryption,
            &file_policy,
            &PeerDirectory::default(),
            args.prefer_relay,
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
        let response = publish_text_clip(&client, &device_id, text, text_clip_ttl_secs)
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
    let clip_ctx = ClipboardCtx {
        isolated: shared.isolated.clone(),
        inbox: shared.inbox.clone(),
        inbox_seq: shared.inbox_seq.clone(),
        file_seq: shared.file_seq.clone(),
    };
    if clip_ctx.is_isolated() {
        tracing::info!(
            "clipboard isolated mode: {HOTKEY_MOD_NAME}+C publishes the current clipboard, {HOTKEY_MOD_NAME}+V pastes from AirPaste"
        );
        if !paste.accessibility_trusted() {
            tracing::warn!(
                "isolated mode's {HOTKEY_MOD_NAME}+V needs Accessibility permission to paste into \
                 other apps; grant it in System Settings -> Privacy & Security -> Accessibility, then restart"
            );
        }
    }
    let last_local_write = Arc::new(Mutex::new(None::<String>));
    let last_local_file_write = Arc::new(Mutex::new(None::<String>));
    // Shared with the UI so it can show files waiting to be applied.
    let pending_file_clip = shared.pending_files.clone();
    let peer_registry = PeerFileRegistry::default();
    let peer_public_url = args
        .peer_public_url
        .clone()
        .unwrap_or_else(|| format!("http://{}", args.peer_bind));
    // Let the UI publish file manifests (drag-and-drop send) through the same grant registry
    // the peer server serves from.
    *shared.file_publish.lock().unwrap() = Some(FilePublishCtx {
        registry: peer_registry.clone(),
        peer_public_url: peer_public_url.clone(),
        policy: file_policy.clone(),
    });
    let mut peer_task = tokio::spawn(run_peer_server(args.peer_bind, peer_registry.clone()));

    let (_mdns_daemon, peer_directory) =
        match discovery::start(&device_id, &device_name, args.peer_bind.port()) {
            Ok((daemon, directory)) => (Some(daemon), directory),
            Err(error) => {
                tracing::warn!(%error, "mDNS discovery disabled; falling back to source_peer_url");
                (None, PeerDirectory::default())
            }
        };

    let ws_peer_registry = peer_registry.clone();
    let prefer_relay = args.prefer_relay;

    // Let the UI download inbox file entries on demand (the 下载 button), with the same
    // context the hotkey apply path uses.
    *shared.file_apply.lock().unwrap() = Some(FileApplyCtx {
        clipboard: clipboard.clone(),
        last_local_file_write: last_local_file_write.clone(),
        identity: identity.clone(),
        encryption: encryption.clone(),
        policy: file_policy.clone(),
        peer_directory: peer_directory.clone(),
        prefer_relay,
        cache_dir: cache_dir.clone(),
    });

    let mut poll_task = if args.publish_clipboard {
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
            clip_ctx.clone(),
            Duration::from_millis(args.poll_ms),
        ))
    } else {
        tokio::spawn(std::future::pending())
    };

    // Keep a fresh snapshot of the server's device registry for the UI. Not select!-ed on
    // below: failures (e.g. before this device is trusted) must not take down the agent — but
    // it is aborted with the rest once the agent stops. Clone client/device_id since `run_ws`
    // below takes ownership of both.
    let refresh_task = tokio::spawn(refresh_devices_loop(
        client.clone(),
        device_id.clone(),
        shared.clone(),
    ));

    let mut ws_task = tokio::spawn(run_ws(
        client,
        clipboard,
        device_id,
        encryption,
        peer_directory,
        ws_peer_registry,
        last_local_write,
        last_local_file_write,
        pending_file_clip,
        args.apply_remote,
        prefer_relay,
        paste,
        identity,
        args.remote_paste_hotkey,
        file_policy,
        auto_apply_files,
        auto_paste_files,
        clip_ctx,
        text_clip_ttl_secs,
        cache_dir,
        shared.clone(),
    ));

    let result = tokio::select! {
        result = &mut peer_task => flatten_task(result),
        result = &mut poll_task => flatten_task(result),
        result = &mut ws_task => flatten_task(result),
        _ = shutdown_signal() => {
            tracing::info!("shutdown requested");
            Ok(())
        }
    };

    // The core tasks live and die together: when one of them exits (e.g. the peer file server
    // could not bind its port), the agent as a whole stops. Merely dropping the JoinHandles
    // would detach the survivors, leaving a half-alive agent that keeps reconnecting and
    // setting `connected` behind the UI's back.
    peer_task.abort();
    poll_task.abort();
    ws_task.abort();
    refresh_task.abort();

    result
}

/// Unwrap a core task's join result, surfacing a panic or cancellation as an error.
fn flatten_task(result: Result<anyhow::Result<()>, tokio::task::JoinError>) -> anyhow::Result<()> {
    result.map_err(anyhow::Error::from)?
}

/// Periodically refresh the UI's device-registry snapshot while connected. Errors (this device
/// not trusted yet, server briefly down) are logged at debug and retried on the next tick; the
/// loop only ends when `run` aborts it on agent shutdown.
async fn refresh_devices_loop(
    client: ServerClient,
    self_device_id: DeviceId,
    shared: Arc<AgentShared>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    loop {
        interval.tick().await;
        if !shared.connected.load(Ordering::Relaxed) {
            continue;
        }
        match client.list_devices().await {
            Ok(devices) => {
                let now = chrono::Utc::now();
                let infos = devices
                    .into_iter()
                    .map(|device| {
                        let last_seen_secs = device.last_seen_at.map(|ts| (now - ts).num_seconds());
                        DeviceInfo {
                            online: last_seen_secs.map_or(false, |secs| secs < PRESENCE_WINDOW_SECS),
                            is_self: device.device_id == self_device_id,
                            device_id: device.device_id.0,
                            name: device.name,
                            trusted: device.trusted,
                            last_seen_secs,
                        }
                    })
                    .collect();
                *shared.devices.lock().unwrap() = infos;
            }
            Err(error) => tracing::debug!(%error, "device list refresh failed"),
        }
    }
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

fn ensure_encryption_identity(
    state_file: &StateFile,
    state: &mut AgentState,
) -> anyhow::Result<EncryptionIdentity> {
    if let Some(private_key) = &state.device_encryption_private_key {
        return Ok(EncryptionIdentity::from_private_key_base64(private_key)?);
    }

    let identity = EncryptionIdentity::generate();
    state.device_encryption_private_key = Some(identity.private_key_base64());
    // Force re-registration so the server learns this device's encryption public key.
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
    encryption_public_key: String,
) -> anyhow::Result<DeviceId> {
    if let Some(device_id) = &state.device_id {
        return Ok(device_id.clone());
    }

    let device = client
        .register_device(name.to_string(), public_key, encryption_public_key)
        .await
        .context("failed to register device")?;
    state.device_id = Some(device.device_id.clone());
    state_file.save(state)?;
    Ok(device.device_id)
}

#[allow(clippy::too_many_arguments)]
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
    clip_ctx: ClipboardCtx,
    interval: Duration,
) -> anyhow::Result<()> {
    let mut last_seen = clipboard.get_text().unwrap_or_default();
    let mut last_seen_files =
        clipboard_signature(&clipboard.get_files().ok().flatten().unwrap_or_default());
    let mut ticker = tokio::time::interval(interval);

    loop {
        ticker.tick().await;
        // Transient local clipboard read failures are logged and skipped, never fatal.
        match clipboard.get_files() {
            Ok(Some(files)) => {
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
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, "failed to read file clipboard"),
        }

        // In isolated mode, text is only published on demand via Alt+C, never by
        // watching the system clipboard.
        if clip_ctx.is_isolated() {
            continue;
        }

        let text = match clipboard.get_text() {
            Ok(Some(text)) => text,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(%error, "failed to read text clipboard");
                continue;
            }
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

        match publish_text_clip(&client, &device_id, text, text_clip_ttl_secs).await {
            Ok(response) => tracing::info!(clip_id = %response.clip_id, "published text clip"),
            Err(error) => tracing::warn!(%error, "failed to publish text clip"),
        }
    }
}

/// Encrypt `text` for every trusted device (including ourselves) and publish it as a clip.
///
/// Sealing only needs the recipients' public keys, so the local encryption identity is
/// not required here; the sender is included as a recipient via the trusted device list.
async fn publish_text_clip(
    client: &ServerClient,
    device_id: &DeviceId,
    text: String,
    ttl_secs: u64,
) -> anyhow::Result<CreateClipResponse> {
    let recipients = trusted_encryption_recipients(client).await?;
    if recipients.is_empty() {
        bail!("no trusted device has an encryption public key; cannot encrypt text");
    }

    let sealed = airpaste_crypto::seal_text(&text, &recipients)
        .map_err(|error| anyhow::anyhow!("failed to encrypt text clip: {error}"))?;
    let key_wrapped_for = sealed
        .wrapped_keys
        .iter()
        .map(|wrapped| wrapped.device_id.clone())
        .collect();

    let clip = ClipKind::Text(TextClip {
        utf8_len: text.len() as u64,
        preview: None,
        encrypted_body_ref: BlobRef {
            id: airpaste_crypto::TEXT_ENCRYPTION_SCHEME.to_string(),
            byte_len: sealed.body_ciphertext_base64.len() as u64,
        },
        encrypted_inline_body: Some(sealed.body_ciphertext_base64),
    });
    client
        .create_clip(
            device_id.clone(),
            clip,
            EncryptionInfo {
                scheme: airpaste_crypto::TEXT_ENCRYPTION_SCHEME.to_string(),
                key_wrapped_for,
                wrapped_keys: sealed.wrapped_keys,
                body_nonce: Some(sealed.body_nonce_base64),
            },
            text_clip_expires_at(ttl_secs),
        )
        .await
}

/// Trusted devices that advertise an X25519 encryption key, including ourselves.
async fn trusted_encryption_recipients(
    client: &ServerClient,
) -> anyhow::Result<Vec<airpaste_crypto::Recipient>> {
    Ok(client
        .list_devices()
        .await?
        .into_iter()
        .filter(|device| device.trusted && !device.encryption_public_key.trim().is_empty())
        .map(|device| airpaste_crypto::Recipient {
            device_id: device.device_id,
            public_key_base64: device.encryption_public_key,
        })
        .collect())
}

/// Decrypt a remote text clip. Returns the plaintext, or `None` if there is nothing to apply.
fn decrypt_remote_text(
    text_clip: &TextClip,
    encryption_info: &EncryptionInfo,
    device_id: &DeviceId,
    encryption: &EncryptionIdentity,
) -> anyhow::Result<Option<String>> {
    if encryption_info.scheme == airpaste_crypto::TEXT_ENCRYPTION_SCHEME {
        let body = text_clip
            .encrypted_inline_body
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("encrypted text clip is missing its body"))?;
        let nonce = encryption_info
            .body_nonce
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("encrypted text clip is missing its body nonce"))?;
        let text = airpaste_crypto::open_text(
            body,
            nonce,
            &encryption_info.wrapped_keys,
            device_id,
            encryption,
        )
        .map_err(|error| anyhow::anyhow!("failed to decrypt text clip: {error}"))?;
        Ok(Some(text))
    } else if let Some(text) = text_clip.encrypted_inline_body.as_ref() {
        tracing::warn!(
            scheme = %encryption_info.scheme,
            "applying legacy plaintext text clip"
        );
        Ok(Some(text.clone()))
    } else {
        Ok(None)
    }
}

/// Flatten a dropped selection into the regular files to transfer, walking directories
/// recursively. Each tuple is `(relative_path_within_the_clip, absolute_source_path)`, kept in
/// the order the peer will serve them by index. The top-level base name is included so the
/// receiver recreates a copied folder by name. Symlinks and special files are skipped (avoids
/// cycles and selecting outside the chosen items).
fn collect_publish_files(inputs: &[PathBuf]) -> anyhow::Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    for input in inputs {
        let base = input
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "item".to_string());
        let metadata = std::fs::symlink_metadata(input)
            .with_context(|| format!("failed to stat {}", input.display()))?;
        if metadata.is_file() {
            out.push((base, input.clone()));
        } else if metadata.is_dir() {
            walk_publish_dir(input, &base, &mut out)?;
        }
        // symlinks / sockets / fifos are skipped.
    }
    Ok(out)
}

fn walk_publish_dir(
    dir: &Path,
    rel_prefix: &str,
    out: &mut Vec<(String, PathBuf)>,
) -> anyhow::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("failed to read directory {}", dir.display()))?
        .filter_map(Result::ok)
        .collect();
    // Deterministic order so the manifest and served paths are stable.
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let child_rel = format!("{rel_prefix}/{name}");
        let metadata = std::fs::symlink_metadata(&path)
            .with_context(|| format!("failed to stat {}", path.display()))?;
        if metadata.is_file() {
            out.push((child_rel, path));
        } else if metadata.is_dir() {
            walk_publish_dir(&path, &child_rel, out)?;
        }
    }
    Ok(())
}

async fn publish_file_manifest(
    client: &ServerClient,
    device_id: &DeviceId,
    peer_registry: &PeerFileRegistry,
    peer_public_url: &str,
    file_policy: &FileTransferPolicy,
    paths: Vec<std::path::PathBuf>,
) -> anyhow::Result<()> {
    // Flatten the selection into regular files, walking directories recursively so a copied
    // folder transfers its contents (each file keeps a relative path under the folder name).
    let collected = collect_publish_files(&paths)?;
    if collected.is_empty() {
        tracing::warn!(
            "file selection had no transferable regular files (empty dirs / symlinks are skipped)"
        );
        return Ok(());
    }
    if collected.len() > file_policy.max_file_count {
        bail!(
            "file selection expands to {} files, above configured limit {}",
            collected.len(),
            file_policy.max_file_count
        );
    }

    let mut files = Vec::with_capacity(collected.len());
    let mut served_paths = Vec::with_capacity(collected.len());
    let mut total_size = 0u64;
    let transfer_token = TransferToken::new();

    for (relative_path, source_path) in collected {
        let size = std::fs::metadata(&source_path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if size > file_policy.max_single_file_bytes {
            bail!(
                "file {} is {} bytes, above configured single-file limit {}",
                source_path.display(),
                size,
                file_policy.max_single_file_bytes
            );
        }
        total_size = total_size.saturating_add(size);
        if total_size > file_policy.max_total_file_bytes {
            bail!(
                "file selection is {} bytes, above configured limit {}",
                total_size,
                file_policy.max_total_file_bytes
            );
        }

        let display_name = std::path::Path::new(&relative_path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unnamed")
            .to_string();
        let sha256 = Some(
            hash_file_sha256(&source_path)
                .await
                .with_context(|| format!("failed to hash file {}", source_path.display()))?,
        );

        files.push(FileEntry {
            relative_path,
            display_name,
            size,
            modified_at: None,
            sha256,
            kind: FileEntryKind::File,
        });
        served_paths.push(source_path);
    }

    let transfer_expires_at = airpaste_core::now()
        + ChronoDuration::seconds(file_policy.transfer_token_ttl_secs.min(i64::MAX as u64) as i64);
    let file_count = files.len();
    peer_registry.register(
        &transfer_token,
        None,
        device_id.clone(),
        trusted_device_public_keys(client).await?,
        served_paths,
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
                wrapped_keys: Vec::new(),
                body_nonce: None,
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
    if contains_provider_token(trimmed) {
        return Some("provider token");
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

fn contains_provider_token(text: &str) -> bool {
    text.split_whitespace().any(|word| {
        let token = word.trim_matches(['"', '\'', '`', ',', ';']);
        let lower = token.to_ascii_lowercase();
        lower.starts_with("github_pat_")
            || lower.starts_with("ghp_")
            || (lower.starts_with("sk-") && token.len() >= 32)
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
    fn safe_cache_path_recreates_subdirs_but_never_escapes() {
        let base = Path::new("/cache/tok");

        // Normal structured path: subdirectories are recreated under the base.
        assert_eq!(
            safe_cache_path(base, "docs/sub/file.txt", "file.txt"),
            Path::new("/cache/tok/docs/sub/file.txt")
        );

        // Plain file (relative_path == display_name): one component under the base.
        assert_eq!(
            safe_cache_path(base, "note.txt", "note.txt"),
            Path::new("/cache/tok/note.txt")
        );

        // Traversal must be neutralized: `..` components are dropped, so the result stays under
        // the base no matter what a (buggy/compromised) peer sends.
        for evil in [
            "../../etc/passwd",
            "..\\..\\Windows\\System32\\x",
            "/etc/passwd",
            "a/../../b",
            "....//x", // not a real parent ref; sanitized component, still contained
        ] {
            let got = safe_cache_path(base, evil, "fallback.bin");
            assert!(
                got.starts_with(base),
                "{evil:?} escaped the cache dir: {got:?}"
            );
            assert!(!got.to_string_lossy().contains(".."));
        }

        // All-unusable relative path falls back to the sanitized display name.
        assert_eq!(
            safe_cache_path(base, "../..", "fallback.bin"),
            Path::new("/cache/tok/fallback.bin")
        );
    }

    #[test]
    fn collect_publish_files_walks_directories() {
        let root = std::env::temp_dir().join(format!("airpaste-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("dir/sub")).unwrap();
        std::fs::write(root.join("top.txt"), b"a").unwrap();
        std::fs::write(root.join("dir/one.txt"), b"bb").unwrap();
        std::fs::write(root.join("dir/sub/two.txt"), b"ccc").unwrap();

        // Select a loose file plus a directory.
        let inputs = vec![root.join("top.txt"), root.join("dir")];
        let mut collected = collect_publish_files(&inputs).unwrap();
        collected.sort_by(|a, b| a.0.cmp(&b.0));

        let rels: Vec<&str> = collected.iter().map(|(rel, _)| rel.as_str()).collect();
        assert_eq!(rels, vec!["dir/one.txt", "dir/sub/two.txt", "top.txt"]);
        // The folder name is preserved as the top-level prefix.
        assert!(collected.iter().all(|(rel, path)| path.is_file() && !rel.contains("..")));

        let _ = std::fs::remove_dir_all(&root);
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
    fn skips_provider_tokens() {
        let policy = default_policy();
        assert_eq!(
            text_publish_skip_reason("ghp_0123456789abcdefghijklmnopqrstuvwx", &policy),
            Some("provider token")
        );
        assert_eq!(
            text_publish_skip_reason("github_pat_11ABCDEFG0abcdefghij_0123456789", &policy),
            Some("provider token")
        );
        assert_eq!(
            text_publish_skip_reason("sk-abcdefghijklmnopqrstuvwxyz0123456789", &policy),
            Some("provider token")
        );
        // Short "sk-" words must not trip the provider-token filter.
        assert_eq!(
            text_publish_skip_reason("sk-123 is a ski resort code", &policy),
            None
        );
    }

    #[test]
    fn allows_normal_clipboard_text() {
        assert_eq!(
            text_publish_skip_reason("airpaste publish smoke text", &default_policy()),
            None
        );
    }

    fn file_entry(name: &str, kind: FileEntryKind) -> FileEntry {
        FileEntry {
            relative_path: name.to_string(),
            display_name: name.to_string(),
            size: 1,
            modified_at: None,
            sha256: None,
            kind,
        }
    }

    fn file_clip_with(kinds: &[FileEntryKind]) -> FileClip {
        FileClip {
            files: kinds
                .iter()
                .enumerate()
                .map(|(i, kind)| file_entry(&format!("f{i}"), kind.clone()))
                .collect(),
            total_size: kinds.len() as u64,
            transfer_token: TransferToken::from("tt-test".to_string()),
            source_peer_url: None,
            transfer_expires_at: None,
        }
    }

    #[test]
    fn missing_file_indexes_skips_directories_and_done() {
        let clip = file_clip_with(&[
            FileEntryKind::File,
            FileEntryKind::Directory,
            FileEntryKind::File,
            FileEntryKind::File,
        ]);

        // Nothing downloaded yet: every regular file (0, 2, 3) is missing; the directory (1) is not.
        let empty = BTreeMap::new();
        assert_eq!(missing_file_indexes(&clip, &empty), vec![0, 2, 3]);

        // Index 0 already delivered directly: only 2 and 3 remain for the relay fallback.
        let mut partial = BTreeMap::new();
        partial.insert(0usize, PathBuf::from("/cache/f0"));
        assert_eq!(missing_file_indexes(&clip, &partial), vec![2, 3]);

        // All regular files done: nothing left to pull, so the relay creates no session.
        let mut done = partial;
        done.insert(2usize, PathBuf::from("/cache/f2"));
        done.insert(3usize, PathBuf::from("/cache/f3"));
        assert!(missing_file_indexes(&clip, &done).is_empty());
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

#[allow(clippy::too_many_arguments)]
async fn run_ws(
    client: ServerClient,
    clipboard: Arc<Clipboard>,
    device_id: DeviceId,
    encryption: Arc<EncryptionIdentity>,
    peer_directory: PeerDirectory,
    peer_registry: PeerFileRegistry,
    last_local_write: Arc<Mutex<Option<String>>>,
    last_local_file_write: Arc<Mutex<Option<String>>>,
    pending_file_clip: Arc<Mutex<Option<PendingFileClip>>>,
    apply_remote: bool,
    prefer_relay: bool,
    paste: Arc<PasteSimulator>,
    identity: Arc<DeviceIdentity>,
    remote_paste_hotkey: bool,
    file_policy: FileTransferPolicy,
    auto_apply_files: bool,
    auto_paste_files: bool,
    clip_ctx: ClipboardCtx,
    text_clip_ttl_secs: u64,
    cache_dir: PathBuf,
    shared: Arc<AgentShared>,
) -> anyhow::Result<()> {
    let (hotkey_tx, mut hotkey_rx) = mpsc::unbounded_channel::<HotkeyAction>();
    if remote_paste_hotkey && apply_remote {
        match spawn_hotkey_listener(hotkey_tx, clip_ctx.is_isolated()) {
            Ok(()) => {
                let hotkey_client = client.clone();
                let hotkey_clipboard = clipboard.clone();
                let hotkey_device_id = device_id.clone();
                let hotkey_last_local_file_write = last_local_file_write.clone();
                let hotkey_pending_file_clip = pending_file_clip.clone();
                let hotkey_paste = paste.clone();
                let hotkey_identity = identity.clone();
                let hotkey_encryption = encryption.clone();
                let hotkey_file_policy = file_policy.clone();
                let hotkey_peer_directory = peer_directory.clone();
                let hotkey_cache_dir = cache_dir.clone();
                let hotkey_clip_ctx = clip_ctx.clone();
                let paste_after_hotkey = REMOTE_PASTE_HOTKEY_PASTES_AFTER_APPLY;
                tokio::spawn(async move {
                    // De-dup state for Alt+C: skip re-publishing an unchanged clipboard (a stale
                    // clipboard the user didn't refresh, or an accidental double-press).
                    let last_pushed = Mutex::new(None::<String>);
                    while let Some(action) = hotkey_rx.recv().await {
                        let result = match action {
                            HotkeyAction::CopyToAirPaste => copy_selection_to_airpaste(
                                &hotkey_client,
                                &hotkey_device_id,
                                &hotkey_clipboard,
                                &last_pushed,
                                text_clip_ttl_secs,
                            )
                            .await
                            .with_context(|| format!("{HOTKEY_MOD_NAME}+C failed")),
                            HotkeyAction::PasteRemote => {
                                paste_remote_via_hotkey(
                                    &hotkey_clip_ctx,
                                    &hotkey_clipboard,
                                    &hotkey_paste,
                                    &hotkey_client,
                                    &hotkey_device_id,
                                    &hotkey_last_local_file_write,
                                    &hotkey_pending_file_clip,
                                    &hotkey_identity,
                                    &hotkey_encryption,
                                    &hotkey_file_policy,
                                    &hotkey_peer_directory,
                                    prefer_relay,
                                    paste_after_hotkey,
                                    &hotkey_cache_dir,
                                )
                                .await
                                .with_context(|| format!("{HOTKEY_MOD_NAME}+V failed"))
                            }
                        };
                        if let Err(error) = result {
                            tracing::warn!(%error, "hotkey action failed");
                        }
                    }
                    tracing::warn!("hotkey listener channel closed");
                });
            }
            Err(error) => tracing::warn!(%error, "hotkeys disabled"),
        }
    }

    let mut backoff = WS_RECONNECT_BASE;
    loop {
        let outcome = run_ws_once(
            &client,
            &clipboard,
            &device_id,
            &encryption,
            &peer_directory,
            &peer_registry,
            &last_local_write,
            &last_local_file_write,
            &pending_file_clip,
            apply_remote,
            prefer_relay,
            &paste,
            &identity,
            &file_policy,
            auto_apply_files,
            auto_paste_files,
            &clip_ctx,
            &cache_dir,
            &shared.connected,
        )
        .await;
        shared.connected.store(false, Ordering::Relaxed);
        match &outcome {
            Ok(()) => tracing::warn!("websocket disconnected"),
            Err(error) => tracing::warn!(%error, "websocket failed"),
        }
        // A clean session (we were connected) resets the backoff; repeated connect
        // failures back off exponentially so a network outage does not busy-reconnect.
        if outcome.is_ok() {
            backoff = WS_RECONNECT_BASE;
        }
        tokio::time::sleep(backoff).await;
        if outcome.is_err() {
            backoff = (backoff * 2).min(WS_RECONNECT_MAX);
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_ws_once(
    client: &ServerClient,
    clipboard: &Clipboard,
    device_id: &DeviceId,
    encryption: &EncryptionIdentity,
    peer_directory: &PeerDirectory,
    peer_registry: &PeerFileRegistry,
    last_local_write: &Mutex<Option<String>>,
    last_local_file_write: &Mutex<Option<String>>,
    pending_file_clip: &Mutex<Option<PendingFileClip>>,
    apply_remote: bool,
    prefer_relay: bool,
    paste: &PasteSimulator,
    identity: &DeviceIdentity,
    file_policy: &FileTransferPolicy,
    auto_apply_files: bool,
    auto_paste_files: bool,
    clip_ctx: &ClipboardCtx,
    cache_dir: &Path,
    connected: &AtomicBool,
) -> anyhow::Result<()> {
    let request = client.ws_request().await?;
    let (ws, _) = tokio::time::timeout(WS_CONNECT_TIMEOUT, tokio_tungstenite::connect_async(request))
        .await
        .map_err(|_| anyhow::anyhow!("websocket connect timed out"))??;
    let (mut writer, mut reader) = ws.split();
    writer
        .send(Message::Text(serde_json::to_string(
            &airpaste_protocol::ClientEvent::Hello {
                device_id: device_id.clone(),
            },
        )?))
        .await?;
    connected.store(true, Ordering::Relaxed);

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
            encryption,
            peer_directory,
            peer_registry,
            last_local_write,
            last_local_file_write,
            pending_file_clip,
            event,
            apply_remote,
            prefer_relay,
            paste,
            identity,
            file_policy,
            auto_apply_files,
            auto_paste_files,
            clip_ctx,
            cache_dir,
        )
        .await?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_server_event(
    client: &ServerClient,
    clipboard: &Clipboard,
    device_id: &DeviceId,
    encryption: &EncryptionIdentity,
    peer_directory: &PeerDirectory,
    peer_registry: &PeerFileRegistry,
    last_local_write: &Mutex<Option<String>>,
    last_local_file_write: &Mutex<Option<String>>,
    pending_file_clip: &Mutex<Option<PendingFileClip>>,
    event: ServerEvent,
    apply_remote: bool,
    prefer_relay: bool,
    paste: &PasteSimulator,
    identity: &DeviceIdentity,
    file_policy: &FileTransferPolicy,
    auto_apply_files: bool,
    auto_paste_files: bool,
    clip_ctx: &ClipboardCtx,
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
            match decrypt_remote_text(&text_clip, &clip.encryption, device_id, encryption) {
                Ok(Some(text)) => {
                    if clip_ctx.is_isolated() {
                        // Isolated mode: keep the text in the in-app inbox (newest first,
                        // bounded history); the system clipboard is left untouched until the
                        // user presses Alt+V.
                        let seq = next_arrival_seq();
                        {
                            let mut inbox = clip_ctx.inbox.lock().await;
                            inbox.push_front(InboxItem {
                                id: seq,
                                kind: InboxItemKind::Text(text),
                            });
                            inbox.truncate(INBOX_HISTORY_MAX);
                        }
                        clip_ctx.inbox_seq.store(seq, Ordering::Relaxed);
                        tracing::info!("stored remote text in isolated inbox");
                    } else {
                        clipboard.set_text(&text)?;
                        *last_local_write.lock().await = Some(text);
                        tracing::info!("applied remote text clip");
                    }
                }
                Ok(None) => {}
                Err(error) => tracing::warn!(%error, "failed to apply remote text clip"),
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
            let pending = PendingFileClip {
                clip_id: pending_clip_id,
                source_device_id: pending_source_device_id,
                file_clip,
            };
            *pending_file_clip.lock().await = Some(pending.clone());
            // Also record it in the inbox so the UI can download it on demand (下载 button),
            // independent of the single "latest pending" Alt+V slot.
            let seq = next_arrival_seq();
            {
                let mut inbox = clip_ctx.inbox.lock().await;
                inbox.push_front(InboxItem {
                    id: seq,
                    kind: InboxItemKind::Files {
                        clip: pending,
                        state: FileDownloadState::Idle,
                    },
                });
                inbox.truncate(INBOX_HISTORY_MAX);
            }
            clip_ctx.file_seq.store(seq, Ordering::Relaxed);
            if auto_apply_files {
                apply_pending_file_clip(
                    client,
                    clipboard,
                    device_id,
                    last_local_file_write,
                    pending_file_clip,
                    &clip_ctx.inbox,
                    paste,
                    identity,
                    encryption,
                    file_policy,
                    peer_directory,
                    prefer_relay,
                    auto_paste_files,
                    cache_dir,
                )
                .await?;
            }
        }
        ServerEvent::TransferRelayReady {
            session_id,
            source_device_id,
            recipient_device_id,
            ..
        } if source_device_id == *device_id => {
            // We are the source: connect to the relay and serve files to the recipient.
            let serve_client = client.clone();
            let serve_registry = peer_registry.clone();
            tokio::spawn(async move {
                if let Err(error) = relay::serve_relay_session(
                    serve_client,
                    serve_registry,
                    session_id,
                    recipient_device_id,
                )
                .await
                {
                    tracing::warn!(%error, "relay source session failed");
                }
            });
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

/// Alt+V dispatch. In isolated mode, paste whichever channel arrived most recently — the
/// inbox text (zero-touch) or the pending remote files — falling back to the other if the chosen
/// one is empty. In system mode, run the file-paste flow.
#[allow(clippy::too_many_arguments)]
async fn paste_remote_via_hotkey(
    clip_ctx: &ClipboardCtx,
    clipboard: &Clipboard,
    paste: &PasteSimulator,
    client: &ServerClient,
    device_id: &DeviceId,
    last_local_file_write: &Mutex<Option<String>>,
    pending_file_clip: &Mutex<Option<PendingFileClip>>,
    identity: &DeviceIdentity,
    encryption: &EncryptionIdentity,
    file_policy: &FileTransferPolicy,
    peer_directory: &PeerDirectory,
    prefer_relay: bool,
    paste_after_apply: bool,
    cache_dir: &Path,
) -> anyhow::Result<()> {
    if clip_ctx.is_isolated() {
        let files_newer = clip_ctx.file_seq.load(Ordering::Relaxed)
            > clip_ctx.inbox_seq.load(Ordering::Relaxed)
            && pending_file_clip.lock().await.is_some();
        // Unless files arrived more recently, try the text inbox first; an empty inbox returns
        // false so we fall through to the files below.
        if !files_newer && paste_inbox_text(&clip_ctx.inbox, clipboard, paste).await? {
            return Ok(());
        }
    }
    apply_pending_file_clip(
        client,
        clipboard,
        device_id,
        last_local_file_write,
        pending_file_clip,
        &clip_ctx.inbox,
        paste,
        identity,
        encryption,
        file_policy,
        peer_directory,
        prefer_relay,
        paste_after_apply,
        cache_dir,
    )
    .await?;
    Ok(())
}

/// Paste the latest inbox text into the focused app without leaving it on the system
/// clipboard: save the current clipboard, set ours, synthesize paste, then restore. Returns
/// `false` (no-op) when the inbox is empty so the caller can fall back to files.
async fn paste_inbox_text(
    inbox: &Mutex<VecDeque<InboxItem>>,
    clipboard: &Clipboard,
    paste: &PasteSimulator,
) -> anyhow::Result<bool> {
    // Paste the most recent text entry (the inbox also holds file entries; those are pulled
    // via the pending-clip path or the UI's 下载 button, not pasted from here).
    let text = inbox.lock().await.iter().find_map(|item| match &item.kind {
        InboxItemKind::Text(text) => Some(text.clone()),
        InboxItemKind::Files { .. } => None,
    });
    let Some(text) = text else {
        return Ok(false);
    };
    let saved = clipboard.get_text().ok().flatten();
    clipboard.set_text(&text)?;
    tokio::time::sleep(CLIPBOARD_SETTLE).await;
    paste.paste()?;
    tokio::time::sleep(PASTE_CONSUME).await;
    if let Some(previous) = saved {
        let _ = clipboard.set_text(&previous);
    }
    tracing::info!("pasted AirPaste inbox text");
    Ok(true)
}

/// Alt+C: publish the current system clipboard to the AirPaste channel. The user copies normally
/// (Ctrl/Cmd+C) first, then presses Alt+C, so we read whatever they just placed on the clipboard
/// instead of synthesizing a copy ourselves. This has no focus dependency, needs no Accessibility
/// permission, and never touches the user's clipboard. De-duplicates against the last push so a
/// stale clipboard or an accidental double-press does not republish.
async fn copy_selection_to_airpaste(
    client: &ServerClient,
    device_id: &DeviceId,
    clipboard: &Clipboard,
    last_pushed: &Mutex<Option<String>>,
    text_clip_ttl_secs: u64,
) -> anyhow::Result<()> {
    let Some(text) = clipboard.get_text().ok().flatten() else {
        tracing::warn!(
            "{HOTKEY_MOD_NAME}+C found no clipboard text (copy something with Ctrl/Cmd+C first; \
             over RDP the clipboard can lag several seconds)"
        );
        return Ok(());
    };
    if text.trim().is_empty() {
        return Ok(());
    }
    // Skip when the clipboard is unchanged since our last push: avoids re-sending a stale
    // clipboard the user didn't refresh, and collapses an accidental double Alt+C into one clip.
    if last_pushed.lock().await.as_deref() == Some(text.as_str()) {
        tracing::info!("{HOTKEY_MOD_NAME}+C: clipboard unchanged since last push, skipping");
        return Ok(());
    }
    let response = publish_text_clip(client, device_id, text.clone(), text_clip_ttl_secs).await?;
    *last_pushed.lock().await = Some(text);
    tracing::info!(clip_id = %response.clip_id, "pushed clipboard to AirPaste");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn apply_pending_file_clip(
    client: &ServerClient,
    clipboard: &Clipboard,
    requester_device_id: &DeviceId,
    last_local_file_write: &Mutex<Option<String>>,
    pending_file_clip: &Mutex<Option<PendingFileClip>>,
    inbox: &Mutex<VecDeque<InboxItem>>,
    paste: &PasteSimulator,
    identity: &DeviceIdentity,
    encryption: &EncryptionIdentity,
    file_policy: &FileTransferPolicy,
    peer_directory: &PeerDirectory,
    prefer_relay: bool,
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
        encryption,
        file_policy,
        peer_directory,
        prefer_relay,
        cache_dir,
    )
    .await?;
    *pending_file_clip.lock().await = None;
    // Reflect the apply on the matching inbox row: its one-time grants are consumed now, so
    // the UI's 下载 button must become the already-downloaded state instead of a doomed retry.
    set_inbox_file_state_by_clip(
        inbox,
        &pending.clip_id,
        FileDownloadState::Done(downloaded_files.clone()),
    )
    .await;
    if paste_after_apply {
        tokio::time::sleep(Duration::from_millis(120)).await;
        paste.paste()?;
        tracing::info!("sent paste hotkey for downloaded files");
    }

    Ok(downloaded_files)
}

#[allow(clippy::too_many_arguments)]
async fn apply_latest_files_once(
    client: &ServerClient,
    clipboard: &Clipboard,
    requester_device_id: &DeviceId,
    last_local_file_write: &Mutex<Option<String>>,
    identity: &DeviceIdentity,
    encryption: &EncryptionIdentity,
    file_policy: &FileTransferPolicy,
    peer_directory: &PeerDirectory,
    prefer_relay: bool,
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
        encryption,
        file_policy,
        peer_directory,
        prefer_relay,
        cache_dir,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn apply_file_clip(
    client: &ServerClient,
    clipboard: &Clipboard,
    requester_device_id: &DeviceId,
    last_local_file_write: &Mutex<Option<String>>,
    pending: &PendingFileClip,
    identity: &DeviceIdentity,
    encryption: &EncryptionIdentity,
    file_policy: &FileTransferPolicy,
    peer_directory: &PeerDirectory,
    prefer_relay: bool,
    cache_dir: &Path,
) -> anyhow::Result<Vec<PathBuf>> {
    validate_file_clip(&pending.file_clip, file_policy)?;
    // Surface per-file download progress to the UI; cleared on any return.
    let _progress = TransferProgressGuard;

    // Indexes are filled in by whichever path delivers them. On a direct->relay fallback the
    // map already holds the files fetched directly, so the relay only pulls the rest instead
    // of re-pulling the whole transfer (which would hit `already served` for done indexes).
    let mut downloaded: BTreeMap<usize, PathBuf> = BTreeMap::new();
    if prefer_relay {
        tracing::info!(
            source_device_id = %pending.source_device_id,
            "pulling remote files through the encrypted relay"
        );
        download_via_relay_for(
            client,
            identity,
            encryption,
            requester_device_id,
            pending,
            cache_dir,
            &mut downloaded,
        )
        .await?;
    } else if let Err(direct_error) = download_remote_files(
        client,
        cache_dir,
        requester_device_id,
        identity,
        peer_directory,
        pending,
        &mut downloaded,
    )
    .await
    {
        // Direct/LAN transfer failed or stalled partway. Fall back to the server-mediated
        // encrypted relay for whatever was not delivered directly.
        tracing::warn!(
            %direct_error,
            source_device_id = %pending.source_device_id,
            delivered_directly = downloaded.len(),
            "direct file download failed; falling back to encrypted relay"
        );
        download_via_relay_for(
            client,
            identity,
            encryption,
            requester_device_id,
            pending,
            cache_dir,
            &mut downloaded,
        )
        .await
        .context("relay fallback after direct download failure also failed")?;
    }

    let downloaded_files: Vec<PathBuf> = downloaded.into_values().collect();
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

/// Pull a pending file clip's still-missing files through the server-mediated encrypted relay.
#[allow(clippy::too_many_arguments)]
async fn download_via_relay_for(
    client: &ServerClient,
    identity: &DeviceIdentity,
    encryption: &EncryptionIdentity,
    requester_device_id: &DeviceId,
    pending: &PendingFileClip,
    cache_dir: &Path,
    downloaded: &mut BTreeMap<usize, PathBuf>,
) -> anyhow::Result<()> {
    relay::download_via_relay(
        client,
        identity,
        encryption,
        requester_device_id,
        &pending.clip_id,
        &pending.source_device_id,
        &pending.file_clip,
        cache_dir,
        downloaded,
    )
    .await
}

/// File indexes in a manifest that are regular files and not yet present in `downloaded`.
fn missing_file_indexes(
    file_clip: &FileClip,
    downloaded: &BTreeMap<usize, PathBuf>,
) -> Vec<usize> {
    file_clip
        .files
        .iter()
        .enumerate()
        .filter(|(index, entry)| {
            matches!(entry.kind, FileEntryKind::File) && !downloaded.contains_key(index)
        })
        .map(|(index, _)| index)
        .collect()
}

/// Resolve where to download a peer's files from: prefer the mDNS-discovered LAN address of
/// the source device, falling back to the `source_peer_url` advertised in the manifest.
async fn resolve_peer_base_url(
    peer_directory: &PeerDirectory,
    pending: &PendingFileClip,
) -> anyhow::Result<String> {
    if let Some(addr) = peer_directory.resolve(&pending.source_device_id).await {
        tracing::info!(
            source_device_id = %pending.source_device_id,
            %addr,
            "resolved source peer via mDNS"
        );
        return Ok(format!("http://{addr}"));
    }
    match &pending.file_clip.source_peer_url {
        Some(url) => Ok(url.trim_end_matches('/').to_string()),
        None => bail!(
            "source peer {} was not found via mDNS and the manifest has no source_peer_url",
            pending.source_device_id
        ),
    }
}

/// Download each still-missing regular file directly from the source peer, inserting
/// successes into `downloaded`. Returns on the first failure with the partial progress
/// preserved in `downloaded`, so the caller can fall back to the relay for the rest.
#[allow(clippy::too_many_arguments)]
async fn download_remote_files(
    client: &ServerClient,
    cache_dir: &Path,
    requester_device_id: &DeviceId,
    identity: &DeviceIdentity,
    peer_directory: &PeerDirectory,
    pending: &PendingFileClip,
    downloaded: &mut BTreeMap<usize, PathBuf>,
) -> anyhow::Result<()> {
    let file_clip = &pending.file_clip;
    let peer_base_url = resolve_peer_base_url(peer_directory, pending).await?;

    let clip_cache_dir = cache_dir.join(file_clip.transfer_token.as_str());
    tokio::fs::create_dir_all(&clip_cache_dir).await?;

    let total = total_files(file_clip);
    for index in missing_file_indexes(file_clip, downloaded) {
        let entry = &file_clip.files[index];
        set_transfer_progress(Some(TransferProgress {
            done: downloaded.len(),
            total,
            current: entry.relative_path.clone(),
        }));
        let url = format!(
            "{}/v1/files/{}/{}",
            peer_base_url.trim_end_matches('/'),
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
        let destination = safe_cache_path(&clip_cache_dir, &entry.relative_path, &entry.display_name);
        download_peer_file_to_cache(response, entry, &destination).await?;
        downloaded.insert(index, destination.clone());
        tracing::info!(path = %destination.display(), progress = format!("{}/{}", downloaded.len(), total), "downloaded remote file");
        set_transfer_progress(Some(TransferProgress {
            done: downloaded.len(),
            total,
            current: entry.relative_path.clone(),
        }));
    }

    Ok(())
}

/// Number of regular-file entries in a manifest (directories/symlinks are not transferred).
fn total_files(file_clip: &FileClip) -> usize {
    file_clip
        .files
        .iter()
        .filter(|entry| matches!(entry.kind, FileEntryKind::File))
        .count()
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
    // Recreate the entry's subdirectories (a copied folder's structure) before writing.
    if let Some(parent) = destination.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
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

/// Build a destination under `cache_dir` for a downloaded entry, recreating the subdirectories
/// encoded in `relative_path` (so a copied folder keeps its structure) while **never escaping
/// `cache_dir`**: each path component is sanitized and `.`/`..`/empty/root components are
/// dropped, so traversal is impossible by construction. Falls back to a flattened `display_name`
/// if `relative_path` yields no usable component.
pub(crate) fn safe_cache_path(cache_dir: &Path, relative_path: &str, display_name: &str) -> PathBuf {
    let mut path = cache_dir.to_path_buf();
    let mut pushed = false;
    for component in relative_path.split(['/', '\\']) {
        if let Some(name) = sanitize_component(component) {
            path.push(name);
            pushed = true;
        }
    }
    if !pushed {
        path.push(sanitize_component(display_name).unwrap_or_else(|| "download.bin".to_string()));
    }
    path
}

/// Sanitize one path component, or `None` if it must be dropped (empty, `.`, `..`, or
/// all-illegal). Never returns a separator or traversal sequence, so callers can safely
/// `push` the result under a base directory.
fn sanitize_component(component: &str) -> Option<String> {
    let trimmed = component.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return None;
    }
    let sanitized: String = trimmed
        .chars()
        .map(|ch| match ch {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' | '/' | '\\' | '\0' => '_',
            ch if ch.is_control() => '_',
            ch => ch,
        })
        .collect();
    // Trailing spaces/dots are invalid on Windows.
    let cleaned = sanitized.trim_end_matches([' ', '.']);
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.to_string())
    }
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
