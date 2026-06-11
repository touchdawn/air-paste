use clap::Parser;
use std::{net::SocketAddr, path::PathBuf};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Debug, Parser)]
#[command(name = "airpaste-server")]
#[command(about = "Air Paste control-plane server")]
struct Args {
    #[arg(long, env = "AIRPASTE_BIND", default_value = "0.0.0.0:14444")]
    bind: SocketAddr,

    #[arg(long, env = "AIRPASTE_DB", default_value = "airpaste.redb")]
    db: PathBuf,

    #[arg(long, env = "AIRPASTE_AUTH_TOKEN")]
    auth_token: Option<String>,

    /// Enable the `/v1/simple/*` text endpoints for simple devices (e.g. iPhone Shortcuts),
    /// protected by this dedicated bearer token. Simple clips are plaintext to the server.
    #[arg(long, env = "AIRPASTE_SIMPLE_TOKEN")]
    simple_token: Option<String>,
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
    airpaste_server::serve(
        args.bind,
        &args.db,
        args.auth_token,
        args.simple_token,
        shutdown_signal(),
    )
    .await
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
