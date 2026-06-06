use crate::{state::AppState, store::StoreError, ws::ws_handler};
use airpaste_core::{now, ClipId, ClipRecord};
use airpaste_protocol::{
    ClipSummary, ConfirmPairingRequest, ConfirmPairingResponse, CreateClipRequest,
    CreateClipResponse, CreateRelaySessionRequest, CreateRelaySessionResponse, HealthResponse,
    RegisterDeviceRequest, RegisterDeviceResponse, ServerEvent, StartPairingRequest,
    StartPairingResponse,
};
use axum::{
    body::Body,
    extract::{Path, Query, State, WebSocketUpgrade},
    http::{header::AUTHORIZATION, HeaderMap, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/health", get(health))
        .route("/v1/devices", get(list_devices).post(register_device))
        .route("/v1/pair/start", post(start_pairing))
        .route("/v1/pair/confirm", post(confirm_pairing))
        .route("/v1/clips", post(create_clip))
        .route("/v1/clips/latest", get(latest_clip))
        .route("/v1/clips/history", get(clip_history))
        .route("/v1/clips/:clip_id", get(get_clip).delete(delete_clip))
        .route("/v1/relay/sessions", post(create_relay_session))
        .route("/v1/ws", get(ws_upgrade))
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
        .register_device(request.name, request.public_key)
        .map_err(ApiError::from)?;
    Ok(Json(RegisterDeviceResponse { device }))
}

async fn list_devices(
    State(state): State<Arc<AppState>>,
) -> ApiResult<Json<Vec<airpaste_core::Device>>> {
    Ok(Json(state.store.list_devices().map_err(ApiError::from)?))
}

async fn start_pairing(
    State(state): State<Arc<AppState>>,
    Json(request): Json<StartPairingRequest>,
) -> ApiResult<Json<StartPairingResponse>> {
    let session = state
        .store
        .start_pairing(request.created_by, request.ttl_seconds)
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
    Json(request): Json<CreateClipRequest>,
) -> ApiResult<Json<CreateClipResponse>> {
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
    Ok(Json(CreateClipResponse {
        clip_id: clip.clip_id,
        created_at: clip.created_at,
    }))
}

async fn get_clip(
    State(state): State<Arc<AppState>>,
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

async fn latest_clip(State(state): State<Arc<AppState>>) -> ApiResult<Json<Option<ClipRecord>>> {
    Ok(Json(state.store.latest_clip().map_err(ApiError::from)?))
}

#[derive(Debug, Deserialize)]
struct HistoryQuery {
    limit: Option<usize>,
}

async fn clip_history(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HistoryQuery>,
) -> ApiResult<Json<Vec<ClipSummary>>> {
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let clips = state.store.clip_history(limit).map_err(ApiError::from)?;
    Ok(Json(clips.into_iter().map(clip_summary).collect()))
}

async fn delete_clip(
    State(state): State<Arc<AppState>>,
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
    Json(request): Json<CreateRelaySessionRequest>,
) -> ApiResult<Json<CreateRelaySessionResponse>> {
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

async fn ws_upgrade(State(state): State<Arc<AppState>>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| ws_handler(socket, state))
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

    let Some(expected) = state.auth_token.as_deref() else {
        return next.run(request).await;
    };
    let authorized = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|actual| constant_time_eq(actual.as_bytes(), expected.as_bytes()));

    if authorized {
        next.run(request).await
    } else {
        ApiError::unauthorized("missing or invalid bearer token").into_response()
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

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
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
