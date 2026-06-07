//! Thin CLI entry point. All logic lives in the `airpaste_agent` library so it can also be
//! embedded by the tray UI.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    airpaste_agent::run_cli().await
}
