use crate::identity::{verify_peer_file_request, PEER_FILE_SIGNATURE_ALG};
use airpaste_core::{ClipId, DeviceId, TransferToken};
use axum::{
    body::{Body, Bytes},
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use futures_util::Stream;
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::{fs::File, net::TcpListener};
use tokio_util::io::ReaderStream;

#[derive(Clone, Default)]
pub struct PeerFileRegistry {
    inner: Arc<Mutex<HashMap<String, PeerFileGrant>>>,
}

struct PeerFileGrant {
    clip_id: Option<ClipId>,
    source_device_id: DeviceId,
    authorized_public_keys: HashMap<DeviceId, String>,
    paths: Vec<PathBuf>,
    expires_at: Instant,
    /// Indexes whose byte stream completed successfully. The one-time-per-index guarantee
    /// only kicks in once a transfer finishes, so a failed transfer can be retried.
    served_indexes: HashSet<usize>,
    /// Indexes currently being streamed (reserved but not yet completed). Prevents a
    /// concurrent second claim of the same index while one is in flight; cleared on
    /// completion (-> `served_indexes`) or on failure (released so a retry can re-claim).
    in_flight: HashSet<usize>,
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
        clip_id: Option<ClipId>,
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
                in_flight: HashSet::new(),
            },
        );
        Ok(())
    }

    pub fn bind_clip_id(&self, token: &TransferToken, clip_id: ClipId) -> anyhow::Result<()> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("peer file registry lock poisoned"))?;
        let Some(grant) = guard.get_mut(token.as_str()) else {
            return Ok(());
        };
        grant.clip_id = Some(clip_id);
        Ok(())
    }

    /// Claim a file for relay delivery, reusing the same signed-request authorization,
    /// clip binding, and one-time-per-index checks as the direct HTTP path.
    /// Returns `Ok(path)` to serve, or `Err(reason)` to reject.
    #[allow(clippy::too_many_arguments)]
    pub fn claim_relay_file(
        &self,
        token: &str,
        index: usize,
        clip_id: &str,
        source_device_id: &str,
        requester_device_id: &str,
        signature_alg: &str,
        signature: &str,
    ) -> anyhow::Result<Result<PathBuf, &'static str>> {
        let request = PeerFileRequest {
            clip_id: clip_id.to_string(),
            source_device_id: source_device_id.to_string(),
            requester_device_id: requester_device_id.to_string(),
            signature_alg: signature_alg.to_string(),
            signature: signature.to_string(),
        };
        Ok(match self.claim(token, index, request)? {
            PeerFileClaim::Available(path) => Ok(path),
            PeerFileClaim::NotFound => Err("file not found"),
            PeerFileClaim::Expired => Err("transfer token expired"),
            PeerFileClaim::AlreadyServed => Err("file already downloaded"),
            PeerFileClaim::Unauthorized(reason) => Err(reason),
        })
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
        if let Some(clip_id) = &grant.clip_id {
            if request.clip_id != clip_id.as_str() {
                return Ok(PeerFileClaim::Unauthorized("clip mismatch"));
            }
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
            &request.clip_id,
            grant.source_device_id.as_str(),
            &request.requester_device_id,
            token,
            index,
        )
        .is_err()
        {
            return Ok(PeerFileClaim::Unauthorized("invalid requester signature"));
        }
        if grant.served_indexes.contains(&index) || grant.in_flight.contains(&index) {
            return Ok(PeerFileClaim::AlreadyServed);
        }
        let Some(path) = grant.paths.get(index).cloned() else {
            return Ok(PeerFileClaim::NotFound);
        };
        // Reserve, but do not consume the one-time grant until the bytes are fully sent.
        grant.in_flight.insert(index);
        Ok(PeerFileClaim::Available(path))
    }

    /// Mark a reserved index as fully delivered: it now counts against the one-time grant
    /// and any later claim returns `AlreadyServed`. Best-effort; a missing grant (expired
    /// or removed) is a no-op.
    pub fn commit_served(&self, token: &str, index: usize) {
        if let Ok(mut guard) = self.inner.lock() {
            if let Some(grant) = guard.get_mut(token) {
                grant.in_flight.remove(&index);
                grant.served_indexes.insert(index);
            }
        }
    }

    /// Release a reserved index after a failed/aborted transfer so it can be claimed again
    /// (e.g. the recipient retries the same file over the relay). Best-effort.
    pub fn release(&self, token: &str, index: usize) {
        if let Ok(mut guard) = self.inner.lock() {
            if let Some(grant) = guard.get_mut(token) {
                grant.in_flight.remove(&index);
            }
        }
    }
}

fn cleanup_expired(entries: &mut HashMap<String, PeerFileGrant>) {
    let now = Instant::now();
    entries.retain(|_, grant| grant.expires_at > now);
}

/// Wraps the file body stream so the one-time grant for an index is committed only once all
/// bytes have been streamed, and released if the stream is dropped early (client disconnect,
/// mid-transfer error). This is what lets a partial direct download fall back to the relay
/// without hitting `already served`.
struct GrantStream<S> {
    inner: S,
    registry: PeerFileRegistry,
    token: String,
    index: usize,
    remaining: u64,
    settled: bool,
}

impl<S> GrantStream<S> {
    fn settle(&mut self) {
        if self.settled {
            return;
        }
        self.settled = true;
        if self.remaining == 0 {
            self.registry.commit_served(&self.token, self.index);
        } else {
            self.registry.release(&self.token, self.index);
        }
    }
}

impl<S> Stream for GrantStream<S>
where
    S: Stream<Item = std::io::Result<Bytes>> + Unpin,
{
    type Item = std::io::Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.inner).poll_next(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                self.remaining = self.remaining.saturating_sub(chunk.len() as u64);
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(None) => {
                self.settle();
                Poll::Ready(None)
            }
            other => other,
        }
    }
}

impl<S> Drop for GrantStream<S> {
    fn drop(&mut self) {
        self.settle();
    }
}

pub async fn run_peer_server(bind: SocketAddr, registry: PeerFileRegistry) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/v1/files/:token/:index", get(download_file))
        .with_state(registry);
    let listener = bind_with_retry(bind).await?;
    tracing::info!(%bind, "peer file server listening");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Bind `addr`, retrying briefly while it is still in use. On a fast relaunch (the tray re-execs
/// itself after a settings change) the previous process may not have released the peer port yet;
/// a few short retries turn a fatal "address in use" (Windows `os error 10048`) into a brief wait.
/// Any other bind error fails fast.
async fn bind_with_retry(addr: SocketAddr) -> anyhow::Result<TcpListener> {
    const ATTEMPTS: usize = 20;
    const INTERVAL: Duration = Duration::from_millis(250);
    for attempt in 1..=ATTEMPTS {
        match TcpListener::bind(addr).await {
            Ok(listener) => return Ok(listener),
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse && attempt < ATTEMPTS => {
                tracing::warn!(%addr, attempt, "peer port in use, retrying");
                tokio::time::sleep(INTERVAL).await;
            }
            Err(error) => {
                return Err(anyhow::Error::new(error).context(format!(
                    "failed to bind peer port {addr} after {attempt} attempt(s)"
                )));
            }
        }
    }
    unreachable!("the final attempt returns instead of looping")
}

async fn download_file(
    State(registry): State<PeerFileRegistry>,
    headers: HeaderMap,
    Path((token, index)): Path<(String, usize)>,
) -> Response {
    match download_file_inner(registry, headers, &token, index).await {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(%error, "peer file download failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "download failed").into_response()
        }
    }
}

async fn download_file_inner(
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
    let metadata = tokio::fs::metadata(&path).await?;
    if !metadata.is_file() {
        return Ok((
            StatusCode::BAD_REQUEST,
            "directories are not transferable yet",
        )
            .into_response());
    }

    let file = File::open(&path).await?;
    let body = Body::from_stream(GrantStream {
        inner: ReaderStream::new(file),
        registry: registry.clone(),
        token: token.to_string(),
        index,
        remaining: metadata.len(),
        settled: false,
    });
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("download.bin");
    let response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_LENGTH, metadata.len().to_string())
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", filename.replace('"', "")),
        )
        .body(body)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::DeviceIdentity;

    struct Fixture {
        registry: PeerFileRegistry,
        token: String,
        clip_id: ClipId,
        source_device_id: DeviceId,
        requester_device_id: DeviceId,
        requester: DeviceIdentity,
    }

    impl Fixture {
        fn new(test_name: &str) -> Self {
            let registry = PeerFileRegistry::default();
            let token = TransferToken::from(format!("test-token-{test_name}"));
            let clip_id = ClipId::from("clip-1".to_string());
            let source_device_id = DeviceId::from("source-device".to_string());
            let requester = DeviceIdentity::generate();
            let requester_device_id = DeviceId::from("requester-device".to_string());

            let path = std::env::temp_dir().join(format!("airpaste-peer-test-{test_name}.bin"));
            std::fs::write(&path, b"hello relay fallback").expect("write temp file");

            let mut keys = HashMap::new();
            keys.insert(requester_device_id.clone(), requester.public_key_base64());

            registry
                .register(
                    &token,
                    Some(clip_id.clone()),
                    source_device_id.clone(),
                    keys,
                    vec![path],
                    Duration::from_secs(600),
                )
                .expect("register grant");

            Self {
                registry,
                token: token.as_str().to_string(),
                clip_id,
                source_device_id,
                requester_device_id,
                requester,
            }
        }

        fn claim(&self, index: usize) -> Result<PathBuf, &'static str> {
            let signature = self.requester.sign_peer_file_request(
                &self.clip_id,
                &self.source_device_id,
                &self.requester_device_id,
                &TransferToken::from(self.token.clone()),
                index,
            );
            self.registry
                .claim_relay_file(
                    &self.token,
                    index,
                    self.clip_id.as_str(),
                    self.source_device_id.as_str(),
                    self.requester_device_id.as_str(),
                    PEER_FILE_SIGNATURE_ALG,
                    &signature,
                )
                .expect("claim_relay_file lock")
        }
    }

    #[test]
    fn failed_transfer_releases_grant_for_retry() {
        let fixture = Fixture::new("release");
        // First claim reserves the index.
        assert!(fixture.claim(0).is_ok());
        // A second claim while in flight is rejected.
        assert_eq!(fixture.claim(0), Err("file already downloaded"));
        // Releasing (transfer failed) lets the recipient retry the same index.
        fixture.registry.release(&fixture.token, 0);
        assert!(fixture.claim(0).is_ok());
    }

    #[test]
    fn completed_transfer_is_one_time() {
        let fixture = Fixture::new("commit");
        assert!(fixture.claim(0).is_ok());
        fixture.registry.commit_served(&fixture.token, 0);
        // Once committed, the one-time grant is consumed.
        assert_eq!(fixture.claim(0), Err("file already downloaded"));
        // Releasing a committed index must not re-open it.
        fixture.registry.release(&fixture.token, 0);
        assert_eq!(fixture.claim(0), Err("file already downloaded"));
    }

    #[test]
    fn rejected_claim_does_not_reserve() {
        let fixture = Fixture::new("rejected");
        // Out-of-range index is NotFound and leaves no reservation behind.
        assert_eq!(fixture.claim(5), Err("file not found"));
        // A wrong signature is rejected without consuming the index.
        let result = fixture
            .registry
            .claim_relay_file(
                &fixture.token,
                0,
                fixture.clip_id.as_str(),
                fixture.source_device_id.as_str(),
                fixture.requester_device_id.as_str(),
                PEER_FILE_SIGNATURE_ALG,
                "not-a-valid-signature",
            )
            .expect("claim_relay_file lock");
        assert_eq!(result, Err("invalid requester signature"));
        // The index is still claimable afterwards.
        assert!(fixture.claim(0).is_ok());
    }
}
