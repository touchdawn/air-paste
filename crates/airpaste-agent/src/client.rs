use crate::identity::{DeviceIdentity, PEER_FILE_SIGNATURE_ALG};
use airpaste_core::{
    ClipId, ClipKind, ClipRecord, Device, DeviceId, EncryptionInfo, PairingCode, TransferToken,
};
use airpaste_protocol::{
    ConfirmPairingRequest, ConfirmPairingResponse, CreateClipRequest, CreateClipResponse,
    RegisterDeviceRequest, RegisterDeviceResponse, AIRPASTE_DEVICE_ID_HEADER,
};
use anyhow::Context;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest,
    handshake::client::Request,
    http::header::{HeaderName, HeaderValue, AUTHORIZATION},
};

#[derive(Clone)]
pub struct ServerClient {
    base_url: String,
    ws_url: String,
    auth_token: Option<String>,
    request_device_id: Arc<RwLock<Option<DeviceId>>>,
    http: reqwest::Client,
}

impl ServerClient {
    pub fn new(base_url: String, auth_token: Option<String>) -> anyhow::Result<Self> {
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
            auth_token,
            request_device_id: Arc::new(RwLock::new(None)),
            http: reqwest::Client::new(),
        })
    }

    pub async fn set_request_device_id(&self, device_id: DeviceId) {
        *self.request_device_id.write().await = Some(device_id);
    }

    pub async fn ws_request(&self) -> anyhow::Result<Request> {
        let mut request = self.ws_url.as_str().into_client_request()?;
        if let Some(token) = &self.auth_token {
            request.headers_mut().insert(
                AUTHORIZATION,
                format!("Bearer {token}")
                    .parse()
                    .context("invalid auth token header value")?,
            );
        }
        if let Some(device_id) = self.request_device_id.read().await.as_ref() {
            request.headers_mut().insert(
                HeaderName::from_static(AIRPASTE_DEVICE_ID_HEADER),
                HeaderValue::from_str(device_id.as_str())
                    .context("invalid device id header value")?,
            );
        }
        Ok(request)
    }

    pub async fn register_device(
        &self,
        name: String,
        public_key: String,
    ) -> anyhow::Result<Device> {
        let response = self
            .authorized(self.http.post(format!("{}/v1/devices", self.base_url)))
            .json(&RegisterDeviceRequest { name, public_key })
            .send()
            .await?
            .error_for_status()?
            .json::<RegisterDeviceResponse>()
            .await?;
        Ok(response.device)
    }

    pub async fn list_devices(&self) -> anyhow::Result<Vec<Device>> {
        self.authenticated_device(self.http.get(format!("{}/v1/devices", self.base_url)))
            .await?
            .send()
            .await?
            .error_for_status()?
            .json::<Vec<Device>>()
            .await
            .context("failed to decode device list")
    }

    pub async fn confirm_pairing(
        &self,
        code: String,
        device_id: DeviceId,
    ) -> anyhow::Result<Device> {
        let response = self
            .authorized(self.http.post(format!("{}/v1/pair/confirm", self.base_url)))
            .json(&ConfirmPairingRequest {
                code: PairingCode(code),
                device_id,
            })
            .send()
            .await?
            .error_for_status()?
            .json::<ConfirmPairingResponse>()
            .await
            .context("failed to decode confirm pairing response")?;
        Ok(response.device)
    }

    pub async fn create_clip(
        &self,
        source_device_id: DeviceId,
        kind: ClipKind,
        encryption: EncryptionInfo,
    ) -> anyhow::Result<CreateClipResponse> {
        self.authenticated_device(self.http.post(format!("{}/v1/clips", self.base_url)))
            .await?
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
        self.authenticated_device(self.http.get(format!(
            "{}/v1/clips/{}",
            self.base_url,
            clip_id.as_str()
        )))
        .await?
        .send()
        .await?
        .error_for_status()?
        .json::<ClipRecord>()
        .await
        .context("failed to decode clip")
    }

    pub async fn download_peer_file(
        &self,
        url: &str,
        clip_id: &ClipId,
        source_device_id: &DeviceId,
        requester_device_id: &DeviceId,
        identity: &DeviceIdentity,
    ) -> anyhow::Result<bytes::Bytes> {
        let (transfer_token, index) = peer_file_url_parts(url)?;
        let signature = identity.sign_peer_file_request(
            clip_id,
            source_device_id,
            requester_device_id,
            &transfer_token,
            index,
        );

        self.http
            .get(url)
            .header("x-airpaste-clip-id", clip_id.as_str())
            .header("x-airpaste-source-device-id", source_device_id.as_str())
            .header(
                "x-airpaste-requester-device-id",
                requester_device_id.as_str(),
            )
            .header("x-airpaste-signature-alg", PEER_FILE_SIGNATURE_ALG)
            .header("x-airpaste-signature", signature)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await
            .context("failed to download bytes")
    }

    fn authorized(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(token) = &self.auth_token {
            request.bearer_auth(token)
        } else {
            request
        }
    }

    async fn authenticated_device(
        &self,
        request: reqwest::RequestBuilder,
    ) -> anyhow::Result<reqwest::RequestBuilder> {
        let Some(device_id) = self.request_device_id.read().await.clone() else {
            anyhow::bail!("server request requires registered device id");
        };
        Ok(self
            .authorized(request)
            .header(AIRPASTE_DEVICE_ID_HEADER, device_id.as_str()))
    }
}

fn peer_file_url_parts(url: &str) -> anyhow::Result<(TransferToken, usize)> {
    let mut parts = url.trim_end_matches('/').rsplit('/');
    let index = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("peer file URL missing index"))?
        .parse::<usize>()
        .context("peer file URL index must be a number")?;
    let transfer_token = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("peer file URL missing transfer token"))?;
    Ok((TransferToken::from(transfer_token.to_string()), index))
}
