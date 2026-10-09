use std::{
    io::{IsTerminal, Read, Write},
    net::{SocketAddr, TcpStream},
    time::Duration,
};

use savesync_server::{build, config::Config, spawn_maintenance};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        return healthcheck();
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,tower_http=warn")),
        )
        .with_ansi(std::io::stdout().is_terminal())
        .init();

    let config = Config::from_env()?;
    let bind = config.bind;
    let (app, state) = build(config)?;
    spawn_maintenance(state.clone());

    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!("savesync-server {} listening on {bind}", env!("CARGO_PKG_VERSION"));
    let hub = state.hub.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            tracing::info!("shutting down");
            hub.disconnect_all();
        })
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = ctrl_c.await;
}

/// `savesync-server healthcheck`: used by Docker, so the image doesn't need curl.
fn healthcheck() -> anyhow::Result<()> {
    let bind: SocketAddr = std::env::var("SAVESYNC_BIND")
        .unwrap_or_else(|_| "0.0.0.0:8420".into())
        .parse()?;
    let addr = SocketAddr::from(([127, 0, 0, 1], bind.port()));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3))?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.write_all(b"GET /health HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    if response.starts_with("HTTP/1.0 200") || response.starts_with("HTTP/1.1 200") {
        Ok(())
    } else {
        anyhow::bail!("unhealthy: {}", response.lines().next().unwrap_or(""))
    }
}
