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

    #[arg(long, env = "AIRPASTE_DEVICE_NAME", default_value = "Windows Agent")]
    pub device_name: String,

    #[arg(long, env = "AIRPASTE_STATE", default_value = ".airpaste-agent.json")]
    pub state_path: PathBuf,

    #[arg(long, env = "AIRPASTE_POLL_MS", default_value_t = 750)]
    pub poll_ms: u64,

    #[arg(long, env = "AIRPASTE_PEER_BIND", default_value = "127.0.0.1:17390")]
    pub peer_bind: SocketAddr,

    #[arg(long, env = "AIRPASTE_PEER_PUBLIC_URL")]
    pub peer_public_url: Option<String>,

    #[arg(long, env = "AIRPASTE_CACHE_DIR", default_value = ".airpaste-cache")]
    pub cache_dir: PathBuf,

    #[arg(long, env = "AIRPASTE_MAX_FILE_COUNT", default_value_t = 1000)]
    pub max_file_count: usize,

    #[arg(
        long,
        env = "AIRPASTE_MAX_TOTAL_FILE_BYTES",
        default_value_t = 10 * 1024 * 1024 * 1024
    )]
    pub max_total_file_bytes: u64,

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
