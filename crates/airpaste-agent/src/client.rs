use crate::identity::{DeviceIdentity, PEER_FILE_SIGNATURE_ALG};
use airpaste_core::{
    ClipId, ClipKind, ClipRecord, Device, DeviceId, EncryptionInfo, PairingCode, TransferToken,
};
use airpaste_protocol::{
    rest_body_sha256_base64url, ConfirmPairingRequest, ConfirmPairingResponse, CreateClipRequest,
    CreateClipResponse, CreateRelaySessionRequest, CreateRelaySessionResponse,
    RegisterDeviceRequest, RegisterDeviceResponse, StartPairingRequest, StartPairingResponse,
    AIRPASTE_BODY_SHA256_HEADER, AIRPASTE_DEVICE_ID_HEADER, AIRPASTE_NONCE_HEADER,
    AIRPASTE_REST_SIGNATURE_ALG, AIRPASTE_SIGNATURE_ALG_HEADER, AIRPASTE_SIGNATURE_HEADER,
    AIRPASTE_TIMESTAMP_HEADER,
};
use anyhow::Context;
use chrono::Utc;
use rand_core::{OsRng, RngCore};
use serde::Serialize;
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
    request_identity: Arc<RwLock<Option<RequestIdentity>>>,
    http: reqwest::Client,
}

#[derive(Clone)]
struct RequestIdentity {
    device_id: DeviceId,
    identity: Arc<DeviceIdentity>,
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
            request_identity: Arc::new(RwLock::new(None)),
            http: reqwest::Client::new(),
        })
    }

    pub async fn set_request_identity(&self, device_id: DeviceId, identity: Arc<DeviceIdentity>) {
        *self.request_identity.write().await = Some(RequestIdentity {
            device_id,
            identity,
        });
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
        let Some(request_identity) = self.request_identity.read().await.clone() else {
            anyhow::bail!("websocket request requires registered device identity");
        };
        let path_and_query = "/v1/ws";
        let signature_headers = rest_signature_headers(
            "GET",
            path_and_query,
            &request_identity.device_id,
            &request_identity.identity,
            "",
        );
        insert_header(
            request.headers_mut(),
            AIRPASTE_DEVICE_ID_HEADER,
            request_identity.device_id.as_str(),
        )?;
        for (name, value) in signature_headers {
            insert_header(request.headers_mut(), name, &value)?;
        }
        Ok(request)
    }

    pub async fn register_device(
        &self,
        name: String,
        public_key: String,
    ) -> anyhow::Result<Device> {
        let request = RegisterDeviceRequest { name, public_key };
        let response = self
            .authorized(self.http.post(format!("{}/v1/devices", self.base_url)))
            .json(&request)
            .send()
            .await?
            .error_for_status()?
            .json::<RegisterDeviceResponse>()
            .await?;
        Ok(response.device)
    }

    pub async fn list_devices(&self) -> anyhow::Result<Vec<Device>> {
        self.signed_get("/v1/devices")
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
        let request = ConfirmPairingRequest {
            code: PairingCode(code),
            device_id,
        };
        let response = self
            .authorized(self.http.post(format!("{}/v1/pair/confirm", self.base_url)))
            .json(&request)
            .send()
            .await?
            .error_for_status()?
            .json::<ConfirmPairingResponse>()
            .await
            .context("failed to decode confirm pairing response")?;
        Ok(response.device)
    }

    pub async fn start_pairing(
        &self,
        created_by: DeviceId,
        ttl_seconds: Option<i64>,
    ) -> anyhow::Result<StartPairingResponse> {
        let request = StartPairingRequest {
            created_by: Some(created_by),
            ttl_seconds,
        };
        self.signed_json("POST", "/v1/pair/start", &request)
            .await?
            .send()
            .await?
            .error_for_status()?
            .json::<StartPairingResponse>()
            .await
            .context("failed to decode start pairing response")
    }

    pub async fn create_clip(
        &self,
        source_device_id: DeviceId,
        kind: ClipKind,
        encryption: EncryptionInfo,
    ) -> anyhow::Result<CreateClipResponse> {
        let request = CreateClipRequest {
            source_device_id,
            expires_at: None,
            kind,
            encryption,
        };
        self.signed_json("POST", "/v1/clips", &request)
            .await?
            .send()
            .await?
            .error_for_status()?
            .json::<CreateClipResponse>()
            .await
            .context("failed to decode create clip response")
    }

    pub async fn get_clip(&self, clip_id: ClipId) -> anyhow::Result<ClipRecord> {
        self.signed_get(&format!("/v1/clips/{}", clip_id.as_str()))
            .await?
            .send()
            .await?
            .error_for_status()?
            .json::<ClipRecord>()
            .await
            .context("failed to decode clip")
    }

    pub async fn latest_clip(&self) -> anyhow::Result<Option<ClipRecord>> {
        self.signed_get("/v1/clips/latest")
            .await?
            .send()
            .await?
            .error_for_status()?
            .json::<Option<ClipRecord>>()
            .await
            .context("failed to decode latest clip")
    }

    pub async fn replay_latest_clip_signature(&self) -> anyhow::Result<()> {
        let Some(request_identity) = self.request_identity.read().await.clone() else {
            anyhow::bail!("server request requires registered device identity");
        };
        let path_and_query = "/v1/clips/latest";
        let signature_headers = rest_signature_headers(
            "GET",
            path_and_query,
            &request_identity.device_id,
            &request_identity.identity,
            "",
        );

        for attempt in 0..2 {
            let mut request = self
                .authorized(
                    self.http
                        .get(format!("{}{}", self.base_url, path_and_query)),
                )
                .header(
                    AIRPASTE_DEVICE_ID_HEADER,
                    request_identity.device_id.as_str(),
                );
            for (name, value) in &signature_headers {
                request = request.header(*name, value);
            }
            let response = request.send().await?;
            if attempt == 0 {
                response.error_for_status()?;
            } else if response.status() != reqwest::StatusCode::UNAUTHORIZED {
                anyhow::bail!(
                    "expected replayed request to return 401, got {}",
                    response.status()
                );
            }
        }
        Ok(())
    }

    pub async fn create_relay_session(
        &self,
        request: CreateRelaySessionRequest,
    ) -> anyhow::Result<CreateRelaySessionResponse> {
        self.signed_json("POST", "/v1/relay/sessions", &request)
            .await?
            .send()
            .await?
            .error_for_status()?
            .json::<CreateRelaySessionResponse>()
            .await
            .context("failed to decode relay session response")
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

    async fn signed_get(&self, path_and_query: &str) -> anyhow::Result<reqwest::RequestBuilder> {
        self.signed_request("GET", path_and_query, Vec::new()).await
    }

    async fn signed_json<T: Serialize>(
        &self,
        method: &str,
        path_and_query: &str,
        request: &T,
    ) -> anyhow::Result<reqwest::RequestBuilder> {
        let body = serde_json::to_vec(request).context("failed to encode request body")?;
        self.signed_request(method, path_and_query, body).await
    }

    async fn signed_request(
        &self,
        method: &str,
        path_and_query: &str,
        body: Vec<u8>,
    ) -> anyhow::Result<reqwest::RequestBuilder> {
        let Some(request_identity) = self.request_identity.read().await.clone() else {
            anyhow::bail!("server request requires registered device identity");
        };
        let signature_headers = rest_signature_headers(
            method,
            path_and_query,
            &request_identity.device_id,
            &request_identity.identity,
            &body,
        );
        let url = format!("{}{}", self.base_url, path_and_query);
        let mut request = match method {
            "GET" => self.http.get(url),
            "POST" => self.http.post(url),
            "DELETE" => self.http.delete(url),
            _ => anyhow::bail!("unsupported signed HTTP method {method}"),
        };
        request = self.authorized(request).header(
            AIRPASTE_DEVICE_ID_HEADER,
            request_identity.device_id.as_str(),
        );
        for (name, value) in signature_headers {
            request = request.header(name, value);
        }
        if body.is_empty() {
            Ok(request)
        } else {
            Ok(request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body))
        }
    }
}

fn rest_signature_headers(
    method: &str,
    path_and_query: &str,
    device_id: &DeviceId,
    identity: &DeviceIdentity,
    body: impl AsRef<[u8]>,
) -> Vec<(&'static str, String)> {
    let body = body.as_ref();
    let timestamp = Utc::now().to_rfc3339();
    let nonce = random_nonce();
    let body_sha256 = rest_body_sha256_base64url(body);
    let signature = identity.sign_rest_request(
        method,
        path_and_query,
        device_id,
        &timestamp,
        &nonce,
        &body_sha256,
    );
    vec![
        (
            AIRPASTE_SIGNATURE_ALG_HEADER,
            AIRPASTE_REST_SIGNATURE_ALG.to_string(),
        ),
        (AIRPASTE_TIMESTAMP_HEADER, timestamp),
        (AIRPASTE_NONCE_HEADER, nonce),
        (AIRPASTE_BODY_SHA256_HEADER, body_sha256),
        (AIRPASTE_SIGNATURE_HEADER, signature),
    ]
}

fn random_nonce() -> String {
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        value.push(HEX[(byte >> 4) as usize] as char);
        value.push(HEX[(byte & 0x0f) as usize] as char);
    }
    value
}

fn insert_header(
    headers: &mut tokio_tungstenite::tungstenite::http::HeaderMap,
    name: &'static str,
    value: &str,
) -> anyhow::Result<()> {
    headers.insert(
        HeaderName::from_static(name),
        HeaderValue::from_str(value).context("invalid header value")?,
    );
    Ok(())
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
