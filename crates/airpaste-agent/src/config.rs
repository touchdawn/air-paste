use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "airpaste-agent")]
#[command(about = "Air Paste desktop agent MVP")]
pub struct Args {
    #[arg(long, env = "AIRPASTE_SERVER", default_value = "http://127.0.0.1:8080")]
    pub server_url: String,

    #[arg(long, env = "AIRPASTE_DEVICE_NAME", default_value = "Windows Agent")]
    pub device_name: String,

    #[arg(long, env = "AIRPASTE_STATE", default_value = ".airpaste-agent.json")]
    pub state_path: PathBuf,

    #[arg(long, env = "AIRPASTE_POLL_MS", default_value_t = 750)]
    pub poll_ms: u64,

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
