use airpaste_core::{ClipId, ClipKind, ClipRecord, Device, DeviceId, EncryptionInfo};
use airpaste_protocol::{
    CreateClipRequest, CreateClipResponse, RegisterDeviceRequest, RegisterDeviceResponse,
};
use anyhow::Context;

#[derive(Clone)]
pub struct ServerClient {
    base_url: String,
    ws_url: String,
    http: reqwest::Client,
}

impl ServerClient {
    pub fn new(base_url: String) -> anyhow::Result<Self> {
        let base_url = base_url.trim_end_matches('/').to_string();
        let ws_url = if let Some(rest) = base_url.strip_prefix("https://") {
            format!("wss://{rest}/v1/ws")
        } else if let Some(rest) = base_url.strip_prefix("http://") {
            format!("ws://{rest}/v1/ws")
        } else {
            anyhow::bail!("server URL must start with http:// or https://");
        };

        Ok(Self {
            base_url,
            ws_url,
            http: reqwest::Client::new(),
        })
    }

    pub fn ws_url(&self) -> &str {
        &self.ws_url
    }

    pub async fn register_device(
        &self,
        name: String,
        public_key: String,
    ) -> anyhow::Result<Device> {
        let response = self
            .http
            .post(format!("{}/v1/devices", self.base_url))
            .json(&RegisterDeviceRequest { name, public_key })
            .send()
            .await?
            .error_for_status()?
            .json::<RegisterDeviceResponse>()
            .await?;
        Ok(response.device)
    }

    pub async fn create_clip(
        &self,
        source_device_id: DeviceId,
        kind: ClipKind,
        encryption: EncryptionInfo,
    ) -> anyhow::Result<CreateClipResponse> {
        self.http
            .post(format!("{}/v1/clips", self.base_url))
            .json(&CreateClipRequest {
                source_device_id,
                expires_at: None,
                kind,
                encryption,
            })
            .send()
            .await?
            .error_for_status()?
            .json::<CreateClipResponse>()
            .await
            .context("failed to decode create clip response")
    }

    pub async fn get_clip(&self, clip_id: ClipId) -> anyhow::Result<ClipRecord> {
        self.http
            .get(format!("{}/v1/clips/{}", self.base_url, clip_id.as_str()))
            .send()
            .await?
            .error_for_status()?
            .json::<ClipRecord>()
            .await
            .context("failed to decode clip")
    }
}
