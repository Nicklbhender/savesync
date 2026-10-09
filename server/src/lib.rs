pub mod api;
pub mod auth;
pub mod blobs;
pub mod config;
pub mod db;
pub mod error;
pub mod model;
pub mod notify;
pub mod ws;

use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};

use axum::Router;

use crate::{
    blobs::BlobStore,
    config::Config,
    db::Db,
    notify::{Hub, Pusher},
};

/// Blobs not referenced by any version are deleted once they're this old. The grace
/// period covers files uploaded for a commit that hasn't happened yet.
const ORPHAN_GRACE: Duration = Duration::from_secs(24 * 60 * 60);
const SWEEP_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: Db,
    pub blobs: BlobStore,
    pub hub: Hub,
    pub pusher: Pusher,
}

pub fn build(config: Config) -> anyhow::Result<(Router, AppState)> {
    std::fs::create_dir_all(&config.data_dir)?;
    let db = Db::open(&config.data_dir.join("savesync.db"))?;
    let blobs = BlobStore::open(&config.data_dir, config.max_file_bytes)?;
    let pusher = Pusher::new(config.push_rewrite.clone());
    let state = AppState {
        config: Arc::new(config),
        db,
        blobs,
        hub: Hub::default(),
        pusher,
    };
    Ok((api::router(state.clone()), state))
}

/// Deletes blobs that no version references, once a day.
pub fn spawn_maintenance(state: AppState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(SWEEP_INTERVAL);
        loop {
            interval.tick().await;
            match sweep_orphans(&state).await {
                Ok(0) => {}
                Ok(n) => tracing::info!("removed {n} unreferenced files"),
                Err(e) => tracing::warn!("orphan sweep failed: {e}"),
            }
        }
    });
}

pub async fn sweep_orphans(state: &AppState) -> error::AppResult<usize> {
    let blobs = state.blobs.clone();
    state
        .db
        .call(move |c| {
            let cutoff = SystemTime::now() - ORPHAN_GRACE;
            let mut removed = 0;
            for (sha, modified) in blobs.list() {
                if modified < cutoff && !db::blob_referenced(c, &sha)? {
                    blobs.remove(&sha);
                    removed += 1;
                }
            }
            Ok(removed)
        })
        .await
}
