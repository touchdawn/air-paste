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
    /// Run an embedded control-plane server on this machine (for other devices to connect to).
    #[serde(default)]
    pub run_server: bool,
    /// Token for the embedded server's `/v1/simple/*` endpoints (simple devices like iPhone
    /// Shortcuts). Empty/absent keeps those endpoints disabled.
    #[serde(default)]
    pub simple_token: Option<String>,
    /// Mirror explicitly sent text (Alt+C / the send button) as plaintext to the server's
    /// simple-device inbox so simple devices can read it.
    #[serde(default)]
    pub simple_mirror: bool,
    /// Custom name for this device in every device list; absent uses the agent's platform
    /// default ("Mac Agent" / "Windows Agent").
    #[serde(default)]
    pub device_name: Option<String>,
    /// Chord for "send to AirPaste" (e.g. "ctrl+shift+c"); absent uses the agent default Alt+C.
    #[serde(default)]
    pub hotkey_copy: Option<String>,
    /// Chord for "paste from AirPaste"; absent uses the agent default Alt+V.
    #[serde(default)]
    pub hotkey_paste: Option<String>,
}

/// `<app-support>/AirPaste/tray-config.json`.
pub fn config_path() -> PathBuf {
    airpaste_agent::app_support_dir().join("tray-config.json")
}

impl TrayConfig {
    /// Load the config, or a default. A missing file yields defaults silently; a present but
    /// malformed file logs and yields defaults (so the problem is at least visible).
    pub fn load() -> Self {
        let path = config_path();
        let Ok(body) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match Self::parse(&body) {
            Ok(config) => config,
            Err(error) => {
                eprintln!(
                    "airpaste-tray: ignoring malformed {}: {error}",
                    path.display()
                );
                Self::default()
            }
        }
    }

    /// Parse config JSON, tolerating a leading UTF-8 BOM (editors/tools on Windows often write
    /// one, and `serde_json` rejects it — which previously made a BOM'd config silently ignored).
    fn parse(body: &str) -> serde_json::Result<Self> {
        serde_json::from_str(body.trim_start_matches('\u{feff}'))
    }

    /// Persist the config (creating the directory if needed). Best-effort; errors are returned
    /// for the caller to log.
    pub fn save(&self) -> std::io::Result<()> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string());
        std::fs::write(&path, body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_with_and_without_bom() {
        let json = r#"{"server_url":"http://host:18092","pair_code":"ABC123"}"#;

        let plain = TrayConfig::parse(json).expect("plain JSON parses");
        assert_eq!(plain.server_url.as_deref(), Some("http://host:18092"));
        assert_eq!(plain.pair_code.as_deref(), Some("ABC123"));

        // A UTF-8 BOM (what Windows PowerShell 5's Set-Content -Encoding UTF8 writes) must not
        // make the config silently ignored.
        let with_bom = format!("\u{feff}{json}");
        let bom = TrayConfig::parse(&with_bom).expect("BOM-prefixed JSON parses");
        assert_eq!(bom.server_url.as_deref(), Some("http://host:18092"));
        assert_eq!(bom.pair_code.as_deref(), Some("ABC123"));
    }
}
