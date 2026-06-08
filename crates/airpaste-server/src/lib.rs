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

    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("failed to bind {bind}"))?;

    tracing::info!(bind = %bind, auth_enabled, "airpaste-server listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .context("server failed")?;
    Ok(())
}
