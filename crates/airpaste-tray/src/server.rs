//! Embedded control-plane server, controlled from the tray ("run a server on this machine") so
//! the host device needs no command line. Runs `airpaste_server::serve` on the agent's Tokio
//! runtime, with a oneshot for graceful shutdown.

use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Clone)]
pub enum ServerStatus {
    Off,
    Running,
    Failed(String),
}

#[derive(Clone)]
pub struct ServerController {
    runtime: tokio::runtime::Handle,
    bind: SocketAddr,
    db: PathBuf,
    // Token enabling the embedded server's /v1/simple/* endpoints (None keeps them disabled).
    simple_token: Option<String>,
    status: Arc<Mutex<ServerStatus>>,
    shutdown: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
}

impl ServerController {
    pub fn new(runtime: tokio::runtime::Handle, simple_token: Option<String>) -> Self {
        let bind: SocketAddr = "0.0.0.0:14444".parse().expect("valid default bind");
        let db = airpaste_agent::app_support_dir().join("server.redb");
        Self {
            runtime,
            bind,
            db,
            simple_token: simple_token.filter(|token| !token.trim().is_empty()),
            status: Arc::new(Mutex::new(ServerStatus::Off)),
            shutdown: Arc::new(Mutex::new(None)),
        }
    }

    pub fn bind(&self) -> SocketAddr {
        self.bind
    }

    pub fn status(&self) -> ServerStatus {
        self.status.lock().unwrap().clone()
    }

    pub fn is_running(&self) -> bool {
        matches!(*self.status.lock().unwrap(), ServerStatus::Running)
    }

    /// Start the embedded server (idempotent). Binds asynchronously; a bind failure (e.g. the
    /// port is taken) lands in `status()` as `Failed`.
    pub fn start(&self) {
        if self.is_running() {
            return;
        }
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        *self.shutdown.lock().unwrap() = Some(tx);
        *self.status.lock().unwrap() = ServerStatus::Running;
        let status = self.status.clone();
        let db = self.db.clone();
        let bind = self.bind;
        let simple_token = self.simple_token.clone();
        self.runtime.spawn(async move {
            let result = airpaste_server::serve(bind, &db, None, simple_token, async move {
                let _ = rx.await;
            })
            .await;
            *status.lock().unwrap() = match result {
                Ok(()) => ServerStatus::Off,
                Err(error) => ServerStatus::Failed(format!("{error:#}")),
            };
        });
    }

    /// Stop the embedded server gracefully (frees the port).
    pub fn stop(&self) {
        if let Some(tx) = self.shutdown.lock().unwrap().take() {
            let _ = tx.send(());
        }
        *self.status.lock().unwrap() = ServerStatus::Off;
    }

    /// Wait (briefly) until the embedded server accepts local connections, so an agent that
    /// connects right after start-up does not race the bind. Call from within the runtime.
    pub async fn wait_until_ready(&self) {
        let local = SocketAddr::new(std::net::Ipv4Addr::LOCALHOST.into(), self.bind.port());
        for _ in 0..50 {
            if tokio::net::TcpStream::connect(local).await.is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}
