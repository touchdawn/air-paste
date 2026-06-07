use clap::Parser;
use std::{net::SocketAddr, path::PathBuf};

#[derive(Debug, Parser)]
#[command(name = "airpaste-agent")]
#[command(about = "Air Paste desktop agent MVP")]
pub struct Args {
    #[arg(long, env = "AIRPASTE_SERVER", default_value = "http://127.0.0.1:8080")]
    pub server_url: String,

    #[arg(long, env = "AIRPASTE_AUTH_TOKEN")]
    pub auth_token: Option<String>,

    #[arg(long, env = "AIRPASTE_PAIR_CODE")]
    pub pair_code: Option<String>,

    #[arg(long, env = "AIRPASTE_CREATE_PAIR_CODE", default_value_t = false)]
    pub create_pair_code: bool,

    #[arg(long, env = "AIRPASTE_PAIR_TTL_SECONDS")]
    pub pair_ttl_seconds: Option<i64>,

    #[arg(long, env = "AIRPASTE_PRINT_LATEST_CLIP", default_value_t = false)]
    pub print_latest_clip: bool,

    #[arg(
        long,
        env = "AIRPASTE_APPLY_LATEST_FILES_ONCE",
        default_value_t = false
    )]
    pub apply_latest_files_once: bool,

    #[arg(
        long,
        env = "AIRPASTE_REPLAY_LATEST_CLIP_SIGNATURE",
        default_value_t = false
    )]
    pub replay_latest_clip_signature: bool,

    #[arg(long, env = "AIRPASTE_PUBLISH_TEXT_ONCE")]
    pub publish_text_once: Option<String>,

    #[arg(long, env = "AIRPASTE_CREATE_RELAY_FOR_CLIP")]
    pub create_relay_for_clip: Option<String>,

    #[arg(long, env = "AIRPASTE_RELAY_RECIPIENT_DEVICE_ID")]
    pub relay_recipient_device_id: Option<String>,

    #[arg(long, env = "AIRPASTE_RELAY_MAX_BYTES")]
    pub relay_max_bytes: Option<u64>,

    #[arg(long, env = "AIRPASTE_RELAY_TTL_SECONDS")]
    pub relay_ttl_seconds: Option<i64>,

    #[arg(long, env = "AIRPASTE_DEVICE_NAME")]
    pub device_name: Option<String>,

    #[arg(long, env = "AIRPASTE_STATE")]
    pub state_path: Option<PathBuf>,

    #[arg(long, env = "AIRPASTE_POLL_MS", default_value_t = 750)]
    pub poll_ms: u64,

    #[arg(long, env = "AIRPASTE_TEXT_CLIP_TTL_SECS", default_value_t = 600)]
    pub text_clip_ttl_secs: u64,

    #[arg(
        long,
        env = "AIRPASTE_FILTER_SENSITIVE_TEXT",
        default_value_t = true,
        action = clap::ArgAction::Set
    )]
    pub filter_sensitive_text: bool,

    #[arg(long, env = "AIRPASTE_MAX_TEXT_CLIP_BYTES", default_value_t = 128 * 1024)]
    pub max_text_clip_bytes: usize,

    #[arg(long, env = "AIRPASTE_PEER_BIND", default_value = "127.0.0.1:17390")]
    pub peer_bind: SocketAddr,

    #[arg(long, env = "AIRPASTE_PEER_PUBLIC_URL")]
    pub peer_public_url: Option<String>,

    #[arg(long, env = "AIRPASTE_CACHE_DIR")]
    pub cache_dir: Option<PathBuf>,

    #[arg(long, env = "AIRPASTE_MAX_FILE_COUNT", default_value_t = 1000)]
    pub max_file_count: usize,

    #[arg(
        long,
        env = "AIRPASTE_MAX_TOTAL_FILE_BYTES",
        default_value_t = 10 * 1024 * 1024 * 1024
    )]
    pub max_total_file_bytes: u64,

    #[arg(
        long,
        env = "AIRPASTE_MAX_SINGLE_FILE_BYTES",
        default_value_t = 10 * 1024 * 1024 * 1024
    )]
    pub max_single_file_bytes: u64,

    #[arg(long, env = "AIRPASTE_TRANSFER_TOKEN_TTL_SECS", default_value_t = 600)]
    pub transfer_token_ttl_secs: u64,

    #[arg(
        long,
        env = "AIRPASTE_AUTO_PASTE_FILES",
        default_value_t = false,
        action = clap::ArgAction::Set
    )]
    pub auto_paste_files: bool,

    #[arg(
        long,
        env = "AIRPASTE_AUTO_APPLY_FILES",
        default_value_t = false,
        action = clap::ArgAction::Set
    )]
    pub auto_apply_files: bool,

    #[arg(
        long,
        env = "AIRPASTE_REMOTE_PASTE_HOTKEY",
        default_value_t = true,
        action = clap::ArgAction::Set
    )]
    pub remote_paste_hotkey: bool,

    #[arg(
        long,
        env = "AIRPASTE_PUBLISH_CLIPBOARD",
        default_value_t = true,
        action = clap::ArgAction::Set
    )]
    pub publish_clipboard: bool,

    #[arg(
        long,
        env = "AIRPASTE_APPLY_REMOTE",
        default_value_t = true,
        action = clap::ArgAction::Set
    )]
    pub apply_remote: bool,
}

impl Args {
    pub fn device_name(&self) -> String {
        self.device_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(default_device_name)
    }

    pub fn state_path(&self) -> PathBuf {
        self.state_path.clone().unwrap_or_else(default_state_path)
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.cache_dir.clone().unwrap_or_else(default_cache_dir)
    }
}

fn default_device_name() -> String {
    #[cfg(windows)]
    {
        "Windows Agent".to_string()
    }

    #[cfg(target_os = "macos")]
    {
        "Mac Agent".to_string()
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    {
        "Air Paste Agent".to_string()
    }
}

fn default_state_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = home_dir() {
            return home
                .join("Library")
                .join("Application Support")
                .join("AirPaste")
                .join("agent.json");
        }
    }

    PathBuf::from(".airpaste-agent.json")
}

fn default_cache_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = home_dir() {
            return home.join("Library").join("Caches").join("AirPaste");
        }
    }

    PathBuf::from(".airpaste-cache")
}

#[cfg(target_os = "macos")]
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}
