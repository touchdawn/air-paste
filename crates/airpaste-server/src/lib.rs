//! Air Paste control-plane server as a library, so it can be run both by the `airpaste-server`
//! binary and embedded in another process (the tray's "run a server on this machine" toggle).

mod relay;
mod routes;
mod state;
mod store;
mod ws;

use crate::{routes::router, state::AppState, store::Store};
use anyhow::Context;
use std::{future::Future, net::SocketAddr, path::Path, sync::Arc};
use tokio::net::TcpListener;
use tower_http::{cors::CorsLayer, trace::TraceLayer};

/// Open the database, build the router, bind, and serve until `shutdown` resolves.
pub async fn serve(
    bind: SocketAddr,
    db: &Path,
    auth_token: Option<String>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let store = Store::open(db)
        .with_context(|| format!("failed to open database at {}", db.display()))?;
    let auth_token = auth_token.filter(|token| !token.is_empty());
    let auth_enabled = auth_token.is_some();
    let state = Arc::new(AppState::new(store, auth_token));
    let app = router(state)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http());

    let listener = bind_with_retry(bind).await?;

    tracing::info!(bind = %bind, auth_enabled, "airpaste-server listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .context("server failed")?;
    Ok(())
}

/// Bind `addr`, retrying briefly while it is still in use. When the embedded server is restarted
/// by a tray re-exec, the previous process may not have released the port yet; a few short retries
/// turn a fatal "address in use" into a brief wait. Any other bind error fails fast.
async fn bind_with_retry(addr: SocketAddr) -> anyhow::Result<TcpListener> {
    const ATTEMPTS: usize = 20;
    const INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
    for attempt in 1..=ATTEMPTS {
        match TcpListener::bind(addr).await {
            Ok(listener) => return Ok(listener),
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse && attempt < ATTEMPTS => {
                tracing::warn!(%addr, attempt, "server port in use, retrying");
                tokio::time::sleep(INTERVAL).await;
            }
            Err(error) => {
                return Err(anyhow::Error::new(error)
                    .context(format!("failed to bind {addr} after {attempt} attempt(s)")));
            }
        }
    }
    unreachable!("the final attempt returns instead of looping")
}
