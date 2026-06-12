use crate::{
    relay::{relay_ws_handler, RelayRole},
    state::AppState,
    store::StoreError,
    ws::ws_handler,
};
use airpaste_core::{
    now, BlobRef, ClipId, ClipKind, ClipRecord, Device, DeviceId, EncryptionInfo, SessionId,
    TextClip,
};
use airpaste_protocol::{
    rest_body_sha256_base64url, rest_signing_message, ClipSummary, ConfirmPairingRequest,
    ConfirmPairingResponse, CreateClipRequest, CreateClipResponse, CreateRelaySessionRequest,
    CreateRelaySessionResponse, HealthResponse, RegisterDeviceRequest, RegisterDeviceResponse,
    RenameDeviceRequest, RenameDeviceResponse, ServerEvent, SimpleClipLatest, SimpleClipUpload,
    StartPairingRequest, StartPairingResponse, TrustDeviceResponse, AIRPASTE_BODY_SHA256_HEADER,
    AIRPASTE_DEVICE_ID_HEADER, AIRPASTE_NONCE_HEADER, AIRPASTE_REST_SIGNATURE_ALG,
    AIRPASTE_SIGNATURE_ALG_HEADER, AIRPASTE_SIGNATURE_HEADER, AIRPASTE_TIMESTAMP_HEADER,
};
use axum::{
    body::{to_bytes, Body},
    extract::{FromRequestParts, Path, Query, State, WebSocketUpgrade},
    http::{header::AUTHORIZATION, request::Parts, HeaderMap, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;
use std::{sync::Arc, time::Duration};

const MAX_SIGNED_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_REST_SIGNATURE_SKEW: Duration = Duration::from_secs(300);
/// Upper bound for text accepted from / mirrored to simple devices (matches the agent's default
/// `--max-text-clip-bytes`).
const MAX_SIMPLE_TEXT_BYTES: usize = 128 * 1024;
/// Server-side TTL for clips uploaded by simple devices (same as the agent's text default).
const SIMPLE_CLIP_TTL_SECS: i64 = 600;
/// Scheme marker for plaintext clips minted on behalf of simple devices; anything other than the
/// E2E scheme takes the agents' legacy-plaintext apply path.
const SIMPLE_PLAINTEXT_SCHEME: &str = "plaintext-simple-v1";
/// Upper bound for device display names (in characters, not bytes).
const MAX_DEVICE_NAME_CHARS: usize = 64;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/health", get(health))
        .route("/v1/devices", get(list_devices).post(register_device))
        .route("/v1/devices/:device_id/trust", post(trust_device))
        .route("/v1/devices/:device_id/rename", post(rename_device))
        .route("/v1/pair/start", post(start_pairing))
        .route("/v1/pair/confirm", post(confirm_pairing))
        .route("/v1/clips", post(create_clip))
        .route("/v1/clips/latest", get(latest_clip))
        .route("/v1/clips/history", get(clip_history))
        .route("/v1/clips/:clip_id", get(get_clip).delete(delete_clip))
        .route("/v1/relay/sessions", post(create_relay_session))
        .route("/v1/relay/:session_id/ws", get(relay_ws_upgrade))
        .route("/v1/ws", get(ws_upgrade))
        .route("/v1/simple/clips", post(simple_create_clip))
        .route("/v1/simple/clips/latest", get(simple_latest_clip))
        .layer(middleware::from_fn_with_state(state.clone(), require_auth))
        .with_state(state)
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        now: now(),
    })
}

async fn register_device(
    State(state): State<Arc<AppState>>,
    Json(request): Json<RegisterDeviceRequest>,
) -> ApiResult<Json<RegisterDeviceResponse>> {
    let device = state
        .store
        .register_device(
            request.name,
            request.public_key,
            request.encryption_public_key,
        )
        .map_err(ApiError::from)?;
    Ok(Json(RegisterDeviceResponse { device }))
}

async fn list_devices(
    State(state): State<Arc<AppState>>,
    TrustedDevice(_request_device): TrustedDevice,
) -> ApiResult<Json<Vec<airpaste_core::Device>>> {
    Ok(Json(state.store.list_devices().map_err(ApiError::from)?))
}

/// Trust a registered device directly, approved by an already-trusted device — the in-app
/// alternative to the pairing-code dance. The approver must be trusted (`TrustedDevice`), which
/// gives this the same authority as minting a pairing code.
async fn trust_device(
    State(state): State<Arc<AppState>>,
    TrustedDevice(_request_device): TrustedDevice,
    Path(device_id): Path<String>,
) -> ApiResult<Json<TrustDeviceResponse>> {
    let device_id = DeviceId::from(device_id);
    let device = state
        .store
        .trust_device(&device_id)
        .map_err(|error| match error {
            StoreError::NotFound => ApiError::not_found("device not found"),
            other => ApiError::from(other),
        })?;
    state.hub.broadcast(ServerEvent::DeviceOnline {
        device_id: device.device_id.clone(),
    });
    Ok(Json(TrustDeviceResponse { device }))
}

/// Rename a registered device. The requester must be trusted: agents rename themselves on
/// startup when their configured name changes, and a trusted device may also relabel another
/// one (the same authority as trusting it).
async fn rename_device(
    State(state): State<Arc<AppState>>,
    TrustedDevice(_request_device): TrustedDevice,
    Path(device_id): Path<String>,
    Json(request): Json<RenameDeviceRequest>,
) -> ApiResult<Json<RenameDeviceResponse>> {
    let name = request.name.trim().to_string();
    if name.is_empty() {
        return Err(ApiError::bad_request("device name must not be empty"));
    }
    if name.chars().count() > MAX_DEVICE_NAME_CHARS {
        return Err(ApiError::bad_request("device name too long"));
    }
    let device_id = DeviceId::from(device_id);
    let device = state
        .store
        .rename_device(&device_id, name)
        .map_err(|error| match error {
            StoreError::NotFound => ApiError::not_found("device not found"),
            other => ApiError::from(other),
        })?;
    Ok(Json(RenameDeviceResponse { device }))
}

async fn start_pairing(
    State(state): State<Arc<AppState>>,
    TrustedDevice(request_device): TrustedDevice,
    Json(request): Json<StartPairingRequest>,
) -> ApiResult<Json<StartPairingResponse>> {
    if let Some(created_by) = &request.created_by {
        if created_by != &request_device.device_id {
            return Err(ApiError::forbidden(
                "pairing creator must match request device",
            ));
        }
    }
    let session = state
        .store
        .start_pairing(Some(request_device.device_id), request.ttl_seconds)
        .map_err(ApiError::from)?;
    Ok(Json(StartPairingResponse {
        code: session.code,
        expires_at: session.expires_at,
    }))
}

async fn confirm_pairing(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ConfirmPairingRequest>,
) -> ApiResult<Json<ConfirmPairingResponse>> {
    let device = state
        .store
        .confirm_pairing(&request.code, &request.device_id)
        .map_err(ApiError::from)?;
    state.hub.broadcast(ServerEvent::DeviceOnline {
        device_id: device.device_id.clone(),
    });
    Ok(Json(ConfirmPairingResponse { device }))
}

async fn create_clip(
    State(state): State<Arc<AppState>>,
    TrustedDevice(request_device): TrustedDevice,
    Json(request): Json<CreateClipRequest>,
) -> ApiResult<Json<CreateClipResponse>> {
    if request.source_device_id != request_device.device_id {
        return Err(ApiError::forbidden(
            "clip source device must match request device",
        ));
    }
    let clip = ClipRecord {
        clip_id: ClipId::new(),
        source_device_id: request.source_device_id,
        created_at: now(),
        expires_at: request.expires_at,
        kind: request.kind,
        encryption: request.encryption,
    };
    let kind = clip.kind.name().to_string();
    let clip = state.store.create_clip(clip).map_err(ApiError::from)?;
    state.hub.broadcast(ServerEvent::ClipCreated {
        clip_id: clip.clip_id.clone(),
        source_device_id: clip.source_device_id.clone(),
        kind,
    });

    // An explicitly-sent text clip may carry a plaintext copy for simple devices. The plaintext
    // only lives in the in-memory simple inbox — never in the database — and is dropped
    // silently when simple access is disabled or the copy is oversized.
    if let Some(text) = request.simple_mirror_text {
        if state.simple_token.is_some() && text.len() <= MAX_SIMPLE_TEXT_BYTES {
            state
                .set_simple_inbox(crate::state::SimpleInboxEntry {
                    text,
                    source: request_device.name.clone(),
                    created_at: clip.created_at,
                    expires_at: clip.expires_at,
                })
                .await;
        }
    }

    Ok(Json(CreateClipResponse {
        clip_id: clip.clip_id,
        created_at: clip.created_at,
    }))
}

/// `POST /v1/simple/clips`: a plaintext text clip from a simple device (e.g. iPhone Shortcuts).
/// The server immediately seals the text end-to-end for every trusted device (it already holds
/// their X25519 public keys), so what gets stored and broadcast is a regular encrypted clip —
/// agents on plain-HTTP links never see simple uploads in cleartext. The plaintext survives
/// only in the in-memory simple inbox (for other simple devices) and falls back to a legacy
/// plaintext clip only when no trusted device has an encryption key yet.
async fn simple_create_clip(
    State(state): State<Arc<AppState>>,
    Json(request): Json<SimpleClipUpload>,
) -> ApiResult<Json<CreateClipResponse>> {
    let text = request.text;
    if text.trim().is_empty() {
        return Err(ApiError::bad_request("text must not be empty"));
    }
    if text.len() > MAX_SIMPLE_TEXT_BYTES {
        return Err(ApiError::bad_request("text too large"));
    }
    let source = request
        .device_name
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "simple-device".to_string());

    let recipients: Vec<airpaste_crypto::Recipient> = state
        .store
        .list_devices()
        .map_err(ApiError::from)?
        .into_iter()
        .filter(|device| device.trusted && !device.encryption_public_key.trim().is_empty())
        .map(|device| airpaste_crypto::Recipient {
            device_id: device.device_id,
            public_key_base64: device.encryption_public_key,
        })
        .collect();

    let (kind, encryption) = if recipients.is_empty() {
        tracing::warn!(
            "no trusted device has an encryption key; storing simple upload as plaintext"
        );
        (
            ClipKind::Text(TextClip {
                utf8_len: text.len() as u64,
                preview: None,
                encrypted_body_ref: BlobRef {
                    id: SIMPLE_PLAINTEXT_SCHEME.to_string(),
                    byte_len: text.len() as u64,
                },
                encrypted_inline_body: Some(text.clone()),
            }),
            EncryptionInfo {
                scheme: SIMPLE_PLAINTEXT_SCHEME.to_string(),
                key_wrapped_for: Vec::new(),
                wrapped_keys: Vec::new(),
                body_nonce: None,
            },
        )
    } else {
        let sealed = airpaste_crypto::seal_text(&text, &recipients).map_err(|error| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("failed to seal simple clip: {error}"),
        })?;
        let key_wrapped_for = sealed
            .wrapped_keys
            .iter()
            .map(|wrapped| wrapped.device_id.clone())
            .collect();
        (
            ClipKind::Text(TextClip {
                utf8_len: text.len() as u64,
                preview: None,
                encrypted_body_ref: BlobRef {
                    id: airpaste_crypto::TEXT_ENCRYPTION_SCHEME.to_string(),
                    byte_len: sealed.body_ciphertext_base64.len() as u64,
                },
                encrypted_inline_body: Some(sealed.body_ciphertext_base64),
            }),
            EncryptionInfo {
                scheme: airpaste_crypto::TEXT_ENCRYPTION_SCHEME.to_string(),
                key_wrapped_for,
                wrapped_keys: sealed.wrapped_keys,
                body_nonce: Some(sealed.body_nonce_base64),
            },
        )
    };

    let clip = ClipRecord {
        clip_id: ClipId::new(),
        source_device_id: DeviceId::from(format!("simple:{source}")),
        created_at: now(),
        expires_at: Some(now() + chrono::Duration::seconds(SIMPLE_CLIP_TTL_SECS)),
        kind,
        encryption,
    };
    let kind = clip.kind.name().to_string();
    let clip = state.store.create_clip(clip).map_err(ApiError::from)?;
    state.hub.broadcast(ServerEvent::ClipCreated {
        clip_id: clip.clip_id.clone(),
        source_device_id: clip.source_device_id.clone(),
        kind,
    });
    state
        .set_simple_inbox(crate::state::SimpleInboxEntry {
            text,
            source,
            created_at: clip.created_at,
            expires_at: clip.expires_at,
        })
        .await;
    Ok(Json(CreateClipResponse {
        clip_id: clip.clip_id,
        created_at: clip.created_at,
    }))
}

/// `GET /v1/simple/clips/latest`: the most recent text visible to simple devices — either
/// mirrored from an explicit desktop send or uploaded by another simple device. `null` when
/// nothing is available (or the entry expired / the server restarted).
async fn simple_latest_clip(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Option<SimpleClipLatest>>> {
    Ok(Json(state.simple_inbox().await.map(|entry| {
        SimpleClipLatest {
            text: entry.text,
            source: entry.source,
            created_at: entry.created_at,
        }
    })))
}

async fn get_clip(
    State(state): State<Arc<AppState>>,
    TrustedDevice(_request_device): TrustedDevice,
    Path(clip_id): Path<String>,
) -> ApiResult<Json<ClipRecord>> {
    let clip_id = ClipId::from(clip_id);
    state
        .store
        .get_clip(&clip_id)
        .map_err(ApiError::from)?
        .map(Json)
        .ok_or(ApiError::not_found("clip not found"))
}

async fn latest_clip(
    State(state): State<Arc<AppState>>,
    TrustedDevice(_request_device): TrustedDevice,
) -> ApiResult<Json<Option<ClipRecord>>> {
    Ok(Json(state.store.latest_clip().map_err(ApiError::from)?))
}

#[derive(Debug, Deserialize)]
struct HistoryQuery {
    limit: Option<usize>,
}

async fn clip_history(
    State(state): State<Arc<AppState>>,
    TrustedDevice(_request_device): TrustedDevice,
    Query(query): Query<HistoryQuery>,
) -> ApiResult<Json<Vec<ClipSummary>>> {
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let clips = state.store.clip_history(limit).map_err(ApiError::from)?;
    Ok(Json(clips.into_iter().map(clip_summary).collect()))
}

async fn delete_clip(
    State(state): State<Arc<AppState>>,
    TrustedDevice(_request_device): TrustedDevice,
    Path(clip_id): Path<String>,
) -> ApiResult<StatusCode> {
    let removed = state
        .store
        .delete_clip(&ClipId::from(clip_id))
        .map_err(ApiError::from)?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("clip not found"))
    }
}

async fn create_relay_session(
    State(state): State<Arc<AppState>>,
    TrustedDevice(request_device): TrustedDevice,
    Json(request): Json<CreateRelaySessionRequest>,
) -> ApiResult<Json<CreateRelaySessionResponse>> {
    if request.source_device_id != request_device.device_id
        && request.recipient_device_id != request_device.device_id
    {
        return Err(ApiError::forbidden(
            "relay requester must be the source or recipient device",
        ));
    }
    ensure_trusted_device(&state, &request.source_device_id)?;
    ensure_trusted_device(&state, &request.recipient_device_id)?;

    let relay = state
        .store
        .create_relay_session(
            request.clip_id,
            request.source_device_id,
            request.recipient_device_id,
            request.max_bytes,
            request.ttl_seconds,
        )
        .map_err(ApiError::from)?;

    let event = ServerEvent::TransferRelayReady {
        session_id: relay.session_id.clone(),
        clip_id: relay.clip_id.clone(),
        source_device_id: relay.source_device_id.clone(),
        recipient_device_id: relay.recipient_device_id.clone(),
    };
    state
        .hub
        .send_to(&relay.source_device_id, event.clone())
        .await;
    state.hub.send_to(&relay.recipient_device_id, event).await;

    Ok(Json(CreateRelaySessionResponse { relay }))
}

async fn ws_upgrade(
    State(state): State<Arc<AppState>>,
    TrustedDevice(request_device): TrustedDevice,
    ws: WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket| ws_handler(socket, state, request_device.device_id))
}

async fn relay_ws_upgrade(
    State(state): State<Arc<AppState>>,
    TrustedDevice(request_device): TrustedDevice,
    Path(session_id): Path<String>,
    ws: WebSocketUpgrade,
) -> Response {
    let session_id = SessionId::from(session_id);
    let session = match state.store.get_relay_session(&session_id) {
        Ok(Some(session)) => session,
        Ok(None) => return (StatusCode::NOT_FOUND, "relay session not found").into_response(),
        Err(error) => return ApiError::from(error).into_response(),
    };

    let now_ts = now();
    if session.expires_at <= now_ts {
        return (StatusCode::GONE, "relay session expired").into_response();
    }

    let role = if request_device.device_id == session.source_device_id {
        RelayRole::Source
    } else if request_device.device_id == session.recipient_device_id {
        RelayRole::Recipient
    } else {
        return (
            StatusCode::FORBIDDEN,
            "device is not a party to this relay session",
        )
            .into_response();
    };

    let hub = state.relay_hub.clone();
    let max_bytes = session.max_bytes;
    // Tear the relay down at the session deadline even if it stays connected.
    let ttl = Duration::from_secs((session.expires_at - now_ts).num_seconds().max(0) as u64);
    ws.on_upgrade(move |socket| relay_ws_handler(socket, hub, session_id, role, max_bytes, ttl))
}

async fn require_auth(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    request: Request<Body>,
    next: Next,
) -> Response {
    if request.uri().path() == "/health" || request.uri().path() == "/v1/health" {
        return next.run(request).await;
    }

    // Simple-device endpoints authenticate with their own dedicated bearer token and nothing
    // else: the main auth token must not open them, and the simple token must not open anything
    // beyond `/v1/simple/`. Disabled (404) unless the server was started with a simple token.
    if request.uri().path().starts_with("/v1/simple/") {
        let Some(expected) = state.simple_token.as_deref() else {
            return ApiError::not_found("simple access is not enabled").into_response();
        };
        let authorized = headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .is_some_and(|actual| constant_time_eq(actual.as_bytes(), expected.as_bytes()));
        if !authorized {
            return ApiError::unauthorized("missing or invalid simple token").into_response();
        }
        return next.run(request).await;
    }

    if let Some(expected) = state.auth_token.as_deref() {
        let authorized = headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .is_some_and(|actual| constant_time_eq(actual.as_bytes(), expected.as_bytes()));

        if !authorized {
            return ApiError::unauthorized("missing or invalid bearer token").into_response();
        }
    }

    if !requires_device_signature(request.method().as_str(), request.uri().path()) {
        return next.run(request).await;
    }

    match verify_signed_request(&state, &headers, request).await {
        Ok(request) => next.run(request).await,
        Err(error) => error.into_response(),
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (left, right) in left.iter().zip(right) {
        diff |= left ^ right;
    }
    diff == 0
}

fn clip_summary(value: ClipRecord) -> ClipSummary {
    ClipSummary {
        clip_id: value.clip_id,
        source_device_id: value.source_device_id,
        created_at: value.created_at,
        expires_at: value.expires_at,
        kind: value.kind.name().to_string(),
    }
}

type ApiResult<T> = Result<T, ApiError>;

#[derive(Clone)]
struct VerifiedDevice(Device);

struct TrustedDevice(Device);

#[axum::async_trait]
impl FromRequestParts<Arc<AppState>> for TrustedDevice {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        if let Some(verified) = parts.extensions.get::<VerifiedDevice>() {
            return Ok(Self(verified.0.clone()));
        }

        Err(ApiError::unauthorized("missing verified device signature"))
    }
}

fn ensure_trusted_device(state: &AppState, device_id: &DeviceId) -> ApiResult<Device> {
    let device = state
        .store
        .get_device(device_id)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found("device not found"))?;
    if !device.trusted {
        return Err(ApiError::forbidden("device is not trusted"));
    }
    Ok(device)
}

fn requires_device_signature(method: &str, path: &str) -> bool {
    !(method == "POST" && (path == "/v1/devices" || path == "/v1/pair/confirm"))
}

async fn verify_signed_request(
    state: &AppState,
    headers: &HeaderMap,
    request: Request<Body>,
) -> ApiResult<Request<Body>> {
    let (mut parts, body) = request.into_parts();
    let body = to_bytes(body, MAX_SIGNED_BODY_BYTES)
        .await
        .map_err(|_| ApiError::bad_request("failed to read request body"))?;
    let signed_headers = SignedRequestHeaders::from_headers(headers)?;
    let body_sha256 = rest_body_sha256_base64url(&body);
    if body_sha256 != signed_headers.body_sha256 {
        return Err(ApiError::unauthorized("request body hash mismatch"));
    }

    let device = validate_trusted_device(state, &signed_headers.device_id)?;
    validate_timestamp(&signed_headers.timestamp)?;
    verify_rest_signature(
        &device,
        parts.method.as_str(),
        parts
            .uri
            .path_and_query()
            .map(|value| value.as_str())
            .unwrap_or(parts.uri.path()),
        &signed_headers,
    )?;
    if !state
        .record_nonce(&signed_headers.device_id, &signed_headers.nonce)
        .await
    {
        return Err(ApiError::unauthorized("request nonce was already used"));
    }

    parts.extensions.insert(VerifiedDevice(device));
    Ok(Request::from_parts(parts, Body::from(body)))
}

fn validate_trusted_device(state: &AppState, device_id: &DeviceId) -> ApiResult<Device> {
    let device = state
        .store
        .get_device(device_id)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::unauthorized("device is not registered"))?;
    if !device.trusted {
        return Err(ApiError::forbidden("device is not trusted"));
    }
    if device.public_key.trim().is_empty() {
        return Err(ApiError::unauthorized("device public key is missing"));
    }
    Ok(device)
}

fn verify_rest_signature(
    device: &Device,
    method: &str,
    path_and_query: &str,
    headers: &SignedRequestHeaders,
) -> ApiResult<()> {
    let public_key = STANDARD
        .decode(device.public_key.trim())
        .map_err(|_| ApiError::unauthorized("invalid device public key"))?;
    let public_key: [u8; 32] = public_key
        .as_slice()
        .try_into()
        .map_err(|_| ApiError::unauthorized("invalid device public key"))?;
    let verifying_key = VerifyingKey::from_bytes(&public_key)
        .map_err(|_| ApiError::unauthorized("invalid device public key"))?;

    let signature = STANDARD
        .decode(&headers.signature)
        .map_err(|_| ApiError::unauthorized("invalid request signature"))?;
    let signature: [u8; 64] = signature
        .as_slice()
        .try_into()
        .map_err(|_| ApiError::unauthorized("invalid request signature"))?;
    let signature = Signature::from_bytes(&signature);

    let message = rest_signing_message(
        method,
        path_and_query,
        &headers.device_id,
        &headers.timestamp,
        &headers.nonce,
        &headers.body_sha256,
    );
    verifying_key
        .verify(message.as_bytes(), &signature)
        .map_err(|_| ApiError::unauthorized("invalid request signature"))
}

fn validate_timestamp(value: &str) -> ApiResult<()> {
    let timestamp = DateTime::parse_from_rfc3339(value)
        .map_err(|_| ApiError::unauthorized("invalid request timestamp"))?
        .with_timezone(&Utc);
    let now = Utc::now();
    let skew = if timestamp > now {
        (timestamp - now).to_std()
    } else {
        (now - timestamp).to_std()
    }
    .map_err(|_| ApiError::unauthorized("invalid request timestamp"))?;
    if skew > MAX_REST_SIGNATURE_SKEW {
        return Err(ApiError::unauthorized("request timestamp is stale"));
    }
    Ok(())
}

struct SignedRequestHeaders {
    device_id: DeviceId,
    timestamp: String,
    nonce: String,
    body_sha256: String,
    signature: String,
}

impl SignedRequestHeaders {
    fn from_headers(headers: &HeaderMap) -> ApiResult<Self> {
        let signature_alg = header_value(headers, AIRPASTE_SIGNATURE_ALG_HEADER)?;
        if signature_alg != AIRPASTE_REST_SIGNATURE_ALG {
            return Err(ApiError::unauthorized(
                "missing or unsupported request signature algorithm",
            ));
        }

        Ok(Self {
            device_id: device_id_from_headers(headers)?,
            timestamp: header_value(headers, AIRPASTE_TIMESTAMP_HEADER)?,
            nonce: header_value(headers, AIRPASTE_NONCE_HEADER)?,
            body_sha256: header_value(headers, AIRPASTE_BODY_SHA256_HEADER)?,
            signature: header_value(headers, AIRPASTE_SIGNATURE_HEADER)?,
        })
    }
}

fn device_id_from_headers(headers: &HeaderMap) -> ApiResult<DeviceId> {
    Ok(DeviceId::from(
        header_value(headers, AIRPASTE_DEVICE_ID_HEADER)
            .map_err(|_| ApiError::unauthorized("missing Air Paste device id header"))?,
    ))
}

fn header_value(headers: &HeaderMap, name: &'static str) -> ApiResult<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| ApiError::unauthorized(format!("missing {name} header")))
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: message.into(),
        }
    }

    fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.into(),
        }
    }
}

impl From<StoreError> for ApiError {
    fn from(value: StoreError) -> Self {
        match value {
            StoreError::NotFound => Self::not_found("not found"),
            other => Self {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                message: other.to_string(),
            },
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(serde_json::json!({
            "error": self.message,
        }));
        (self.status, body).into_response()
    }
}
