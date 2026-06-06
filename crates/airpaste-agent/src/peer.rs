use crate::identity::{verify_peer_file_request, PEER_FILE_SIGNATURE_ALG};
use airpaste_core::{ClipId, DeviceId, TransferToken};
use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::net::TcpListener;

#[derive(Clone, Default)]
pub struct PeerFileRegistry {
    inner: Arc<Mutex<HashMap<String, PeerFileGrant>>>,
}

struct PeerFileGrant {
    clip_id: ClipId,
    source_device_id: DeviceId,
    authorized_public_keys: HashMap<DeviceId, String>,
    paths: Vec<PathBuf>,
    expires_at: Instant,
    served_indexes: HashSet<usize>,
}

struct PeerFileRequest {
    clip_id: String,
    source_device_id: String,
    requester_device_id: String,
    signature_alg: String,
    signature: String,
}

enum PeerFileClaim {
    Available(PathBuf),
    NotFound,
    Expired,
    AlreadyServed,
    Unauthorized(&'static str),
}

impl PeerFileRegistry {
    pub fn register(
        &self,
        token: &TransferToken,
        clip_id: ClipId,
        source_device_id: DeviceId,
        authorized_public_keys: HashMap<DeviceId, String>,
        paths: Vec<PathBuf>,
        ttl: Duration,
    ) -> anyhow::Result<()> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("peer file registry lock poisoned"))?;
        cleanup_expired(&mut guard);
        guard.insert(
            token.as_str().to_string(),
            PeerFileGrant {
                clip_id,
                source_device_id,
                authorized_public_keys,
                paths,
                expires_at: Instant::now() + ttl,
                served_indexes: HashSet::new(),
            },
        );
        Ok(())
    }

    fn claim(
        &self,
        token: &str,
        index: usize,
        request: PeerFileRequest,
    ) -> anyhow::Result<PeerFileClaim> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("peer file registry lock poisoned"))?;
        cleanup_expired(&mut guard);
        let Some(grant) = guard.get_mut(token) else {
            return Ok(PeerFileClaim::NotFound);
        };
        if Instant::now() > grant.expires_at {
            guard.remove(token);
            return Ok(PeerFileClaim::Expired);
        }
        if request.clip_id != grant.clip_id.as_str() {
            return Ok(PeerFileClaim::Unauthorized("clip mismatch"));
        }
        if request.source_device_id != grant.source_device_id.as_str() {
            return Ok(PeerFileClaim::Unauthorized("source device mismatch"));
        }
        if request.requester_device_id.is_empty()
            || request.requester_device_id == grant.source_device_id.as_str()
        {
            return Ok(PeerFileClaim::Unauthorized("invalid requester device"));
        }
        if request.signature_alg != PEER_FILE_SIGNATURE_ALG {
            return Ok(PeerFileClaim::Unauthorized(
                "unsupported signature algorithm",
            ));
        }
        let requester_device_id = DeviceId::from(request.requester_device_id.clone());
        let Some(public_key) = grant.authorized_public_keys.get(&requester_device_id) else {
            return Ok(PeerFileClaim::Unauthorized(
                "requester device is not trusted",
            ));
        };
        if verify_peer_file_request(
            public_key,
            &request.signature,
            grant.clip_id.as_str(),
            grant.source_device_id.as_str(),
            &request.requester_device_id,
            token,
            index,
        )
        .is_err()
        {
            return Ok(PeerFileClaim::Unauthorized("invalid requester signature"));
        }
        if grant.served_indexes.contains(&index) {
            return Ok(PeerFileClaim::AlreadyServed);
        }
        let Some(path) = grant.paths.get(index).cloned() else {
            return Ok(PeerFileClaim::NotFound);
        };
        grant.served_indexes.insert(index);
        Ok(PeerFileClaim::Available(path))
    }
}

fn cleanup_expired(entries: &mut HashMap<String, PeerFileGrant>) {
    let now = Instant::now();
    entries.retain(|_, grant| grant.expires_at > now);
}

pub async fn run_peer_server(bind: SocketAddr, registry: PeerFileRegistry) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/v1/files/:token/:index", get(download_file))
        .with_state(registry);
    let listener = TcpListener::bind(bind).await?;
    tracing::info!(%bind, "peer file server listening");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn download_file(
    State(registry): State<PeerFileRegistry>,
    headers: HeaderMap,
    Path((token, index)): Path<(String, usize)>,
) -> Response {
    match download_file_inner(registry, headers, &token, index) {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(%error, "peer file download failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "download failed").into_response()
        }
    }
}

fn download_file_inner(
    registry: PeerFileRegistry,
    headers: HeaderMap,
    token: &str,
    index: usize,
) -> anyhow::Result<Response> {
    let Some(request) = peer_file_request_from_headers(&headers) else {
        return Ok((StatusCode::UNAUTHORIZED, "missing peer transfer headers").into_response());
    };
    let path = match registry.claim(token, index, request)? {
        PeerFileClaim::Available(path) => path,
        PeerFileClaim::NotFound => {
            return Ok((StatusCode::NOT_FOUND, "file not found").into_response())
        }
        PeerFileClaim::Expired => {
            return Ok((StatusCode::GONE, "transfer token expired").into_response())
        }
        PeerFileClaim::AlreadyServed => {
            return Ok((StatusCode::GONE, "file already downloaded").into_response())
        }
        PeerFileClaim::Unauthorized(reason) => {
            return Ok((StatusCode::UNAUTHORIZED, reason).into_response())
        }
    };
    let metadata = std::fs::metadata(&path)?;
    if !metadata.is_file() {
        return Ok((
            StatusCode::BAD_REQUEST,
            "directories are not transferable yet",
        )
            .into_response());
    }

    let body = std::fs::read(&path)?;
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("download.bin");
    let response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_LENGTH, body.len().to_string())
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", filename.replace('"', "")),
        )
        .body(Body::from(body))?;
    Ok(response)
}

fn peer_file_request_from_headers(headers: &HeaderMap) -> Option<PeerFileRequest> {
    Some(PeerFileRequest {
        clip_id: header_value(headers, "x-airpaste-clip-id")?,
        source_device_id: header_value(headers, "x-airpaste-source-device-id")?,
        requester_device_id: header_value(headers, "x-airpaste-requester-device-id")?,
        signature_alg: header_value(headers, "x-airpaste-signature-alg")?,
        signature: header_value(headers, "x-airpaste-signature")?,
    })
}

fn header_value(headers: &HeaderMap, name: &'static str) -> Option<String> {
    let value = headers.get(name)?.to_str().ok()?.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}
