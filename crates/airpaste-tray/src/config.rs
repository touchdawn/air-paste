//! Persistent tray connection config, so the app can be configured from its window instead of
//! only via CLI flags. Stored as JSON next to the agent state (see `airpaste_agent::app_support_dir`).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// User-editable connection settings the tray persists and restores across launches. All fields
/// are optional so an absent/partial file degrades to "use the agent defaults".
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TrayConfig {
    #[serde(default)]
    pub server_url: Option<String>,
    #[serde(default)]
    pub auth_token: Option<String>,
    /// One-shot pairing code; cleared once the device is trusted (a consumed code is a hard
    /// error on the next connect).
    #[serde(default)]
    pub pair_code: Option<String>,
    /// The server we last connected to; used to detect a server change (which invalidates the
    /// cached device id registered on the old server).
    #[serde(default)]
    pub last_server_url: Option<String>,
}

/// `<app-support>/AirPaste/tray-config.json`.
pub fn config_path() -> PathBuf {
    airpaste_agent::app_support_dir().join("tray-config.json")
}

impl TrayConfig {
    /// Load the config, or a default (best-effort: a missing or malformed file yields defaults).
    pub fn load() -> Self {
        let path = config_path();
        if !path.exists() {
            return Self::default();
        }
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|body| serde_json::from_str(&body).ok())
            .unwrap_or_default()
    }

    /// Persist the config (creating the directory if needed). Best-effort; errors are returned
    /// for the caller to log.
    pub fn save(&self) -> std::io::Result<()> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = serde_json::to_string_pretty(self)
            .unwrap_or_else(|_| "{}".to_string());
        std::fs::write(&path, body)
    }
}
