//! The hub: an HTTP API on loopback, live SSE streams, and the embedded web UI.
//!
//! Security model (single user, docs/07-remote.md): the server binds to 127.0.0.1
//! only, rejects requests whose `Host` isn't loopback (DNS-rebinding defence), and
//! requires the per-user token from `~/.trun/hub.token` on every `/api` call, as a
//! bearer header or the cookie set by visiting `/?t=<token>` (what `trun ui` opens).

pub mod paths;
mod routes;
mod ui;

use anyhow::{Context, Result};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Notify;
use trun_agent::Agent;
use trun_proto::now_ms;
use trun_store::Store;

pub use paths::{HubInfo, Paths};

#[derive(Debug, Clone)]
pub struct HubConfig {
    pub paths: Paths,
    pub port: u16,
}

#[derive(Clone)]
pub struct AppState {
    pub agent: Agent,
    pub store: Store,
    pub token: Arc<String>,
    pub port: u16,
    pub started_at: i64,
    pub data_dir: String,
    pub shutdown: Arc<Notify>,
}

/// Run the hub until Ctrl-C or `POST /api/shutdown`.
pub async fn serve(cfg: HubConfig) -> Result<()> {
    let token = cfg.paths.load_or_create_token()?;
    let data_dir = cfg.paths.data_dir();
    let store = Store::open(&data_dir)?;
    let lost = tokio::task::spawn_blocking({
        let store = store.clone();
        move || store.mark_orphans_lost()
    })
    .await??;
    if !lost.is_empty() {
        tracing::warn!(
            count = lost.len(),
            "runs from a previous hub were still active; marked lost"
        );
    }

    let addr = SocketAddr::from(([127, 0, 0, 1], cfg.port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr} (is another hub already running?)"))?;
    let url = format!("http://127.0.0.1:{}", cfg.port);

    let agent = Agent::new(store.clone(), url.clone());
    let state = AppState {
        agent,
        store,
        token: Arc::new(token),
        port: cfg.port,
        started_at: now_ms(),
        data_dir: data_dir.to_string_lossy().into_owned(),
        shutdown: Arc::new(Notify::new()),
    };

    cfg.paths.write_hub_info(&HubInfo {
        pid: std::process::id(),
        port: cfg.port,
        url: url.clone(),
        started_at: state.started_at,
    })?;
    tracing::info!(%url, data_dir = %state.data_dir, "hub listening");

    let shutdown = state.shutdown.clone();
    let app = routes::router(state.clone());
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = shutdown.notified() => {}
            }
            tracing::info!("hub shutting down");
        })
        .await?;

    // Only remove hub.json if it's still ours.
    if cfg
        .paths
        .read_hub_info()
        .is_some_and(|i| i.pid == std::process::id())
    {
        let _ = std::fs::remove_file(cfg.paths.hub_info_file());
    }
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || store.flush()).await?;
    Ok(())
}
