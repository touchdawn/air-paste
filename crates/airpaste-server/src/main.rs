mod routes;
mod state;
mod store;
mod ws;

use crate::{routes::router, state::AppState, store::Store};
use anyhow::Context;
use clap::Parser;
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use tokio::net::TcpListener;
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Debug, Parser)]
#[command(name = "airpaste-server")]
#[command(about = "Air Paste control-plane server")]
struct Args {
    #[arg(long, env = "AIRPASTE_BIND", default_value = "0.0.0.0:8080")]
    bind: SocketAddr,

    #[arg(long, env = "AIRPASTE_DB", default_value = "airpaste.redb")]
    db: PathBuf,

    #[arg(long, env = "AIRPASTE_AUTH_TOKEN")]
    auth_token: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "airpaste_server=debug,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let args = Args::parse();
    let store = Store::open(&args.db)
        .with_context(|| format!("failed to open database at {}", args.db.display()))?;
    let auth_token = args.auth_token.filter(|token| !token.is_empty());
    let auth_enabled = auth_token.is_some();
    let state = Arc::new(AppState::new(store, auth_token));
    let app = router(state)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http());

    let listener = TcpListener::bind(args.bind)
        .await
        .with_context(|| format!("failed to bind {}", args.bind))?;

    tracing::info!(bind = %args.bind, auth_enabled, "airpaste-server listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server failed")?;

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install terminate handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
