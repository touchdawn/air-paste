use airpaste_core::TransferToken;
use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, StatusCode},
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
    paths: Vec<PathBuf>,
    expires_at: Instant,
    served_indexes: HashSet<usize>,
}

enum PeerFileClaim {
    Available(PathBuf),
    NotFound,
    Expired,
    AlreadyServed,
}

impl PeerFileRegistry {
    pub fn register(
        &self,
        token: &TransferToken,
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
                paths,
                expires_at: Instant::now() + ttl,
                served_indexes: HashSet::new(),
            },
        );
        Ok(())
    }

    fn claim(&self, token: &str, index: usize) -> anyhow::Result<PeerFileClaim> {
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
    Path((token, index)): Path<(String, usize)>,
) -> Response {
    match download_file_inner(registry, &token, index) {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(%error, "peer file download failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "download failed").into_response()
        }
    }
}

fn download_file_inner(
    registry: PeerFileRegistry,
    token: &str,
    index: usize,
) -> anyhow::Result<Response> {
    let path = match registry.claim(token, index)? {
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
