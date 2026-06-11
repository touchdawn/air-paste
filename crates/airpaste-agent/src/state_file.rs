use airpaste_core::DeviceId;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AgentState {
    pub device_id: Option<DeviceId>,
    pub device_private_key: Option<String>,
    #[serde(default)]
    pub device_encryption_private_key: Option<String>,
    /// Device name last synced to the server; a differing configured name triggers a rename on
    /// startup (the server only learns the name at registration otherwise).
    #[serde(default)]
    pub device_name: Option<String>,
}

#[derive(Clone, Debug)]
pub struct StateFile {
    path: PathBuf,
}

impl StateFile {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn load(&self) -> anyhow::Result<AgentState> {
        if !self.path.exists() {
            return Ok(AgentState::default());
        }
        let body = fs::read_to_string(&self.path)
            .with_context(|| format!("failed to read {}", self.path.display()))?;
        serde_json::from_str(&body)
            .with_context(|| format!("failed to parse {}", self.path.display()))
    }

    pub fn save(&self, state: &AgentState) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
        }
        let body = serde_json::to_string_pretty(state)?;
        fs::write(&self.path, body)
            .with_context(|| format!("failed to write {}", self.path.display()))
    }
}
