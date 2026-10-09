use std::collections::HashSet;

use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
};
use serde_json::json;
use tokio_util::io::ReaderStream;
use tower_http::trace::TraceLayer;

use crate::{
    AppState,
    auth::{Admin, Device, hash_token},
    db::{self, NewVersion},
    error::{AppError, AppResult},
    model::{
        AckRequest, CommitRequest, CommitResponse, DeviceInfo, EnrollRequest, EnrollResponse, FileEntry, GameStatus,
        Manifest, MissingRequest, MissingResponse, PushEndpointRequest, SubscribeRequest, VersionInfo, now_ms,
        random_hex, valid_game_id, valid_save_path, valid_sha256,
    },
    notify::{PushPayload, ServerMsg},
    ws,
};

const MAX_FILES_PER_VERSION: usize = 20_000;
const MAX_JSON_BYTES: usize = 16 * 1024 * 1024;

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/devices", post(enroll).get(list_devices))
        .route("/devices/{id}", delete(revoke_device))
        .route("/me", get(me))
        .route("/me/push", put(set_push))
        .route("/games", get(list_games))
        .route("/games/{id}", put(subscribe).get(game))
        .route("/games/{id}/subscription", delete(unsubscribe))
        .route("/games/{id}/versions", get(list_versions).post(commit))
        .route("/games/{id}/versions/{version}", get(manifest))
        .route("/games/{id}/ack", post(ack))
        .route("/pending", get(pending))
        .route("/blobs/missing", post(missing_blobs))
        .route(
            "/blobs/{sha}",
            put(upload_blob).get(download_blob).layer(DefaultBodyLimit::disable()),
        )
        .route("/ws", get(ws::handler));

    Router::new()
        .route("/health", get(health))
        .nest("/api/v1", api)
        .layer(DefaultBodyLimit::max(MAX_JSON_BYTES))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({"status": "ok", "version": env!("CARGO_PKG_VERSION")}))
}

// ---------------------------------------------------------------- devices

async fn enroll(State(state): State<AppState>, _: Admin, Json(req): Json<EnrollRequest>) -> AppResult<(StatusCode, Json<EnrollResponse>)> {
    let name = req.name.trim().to_string();
    let platform = req.platform.trim().to_lowercase();
    if name.is_empty() || name.len() > 100 || platform.is_empty() || platform.len() > 32 {
        return Err(AppError::BadRequest("name (1-100 chars) and platform (1-32 chars) are required".into()));
    }
    let device_id = format!("dev_{}", random_hex(8));
    let token = random_hex(32);
    let hash = hash_token(&token);
    let id = device_id.clone();
    state
        .db
        .call(move |c| db::create_device(c, &id, &name, &platform, &hash, now_ms()))
        .await?;
    tracing::info!("enrolled device {device_id}");
    Ok((StatusCode::CREATED, Json(EnrollResponse { device_id, token })))
}

async fn list_devices(State(state): State<AppState>, _: Admin) -> AppResult<Json<Vec<DeviceInfo>>> {
    Ok(Json(state.db.call(|c| db::list_devices(c)).await?))
}

async fn revoke_device(State(state): State<AppState>, _: Admin, Path(id): Path<String>) -> AppResult<StatusCode> {
    let device_id = id.clone();
    if !state.db.call(move |c| db::revoke_device(c, &device_id)).await? {
        return Err(AppError::NotFound("device"));
    }
    state.hub.disconnect(&id);
    tracing::info!("revoked device {id}");
    Ok(StatusCode::NO_CONTENT)
}

async fn me(State(state): State<AppState>, device: Device) -> AppResult<Json<DeviceInfo>> {
    Ok(Json(state.db.call(move |c| db::device_info(c, &device.id)).await?))
}

async fn set_push(State(state): State<AppState>, device: Device, Json(req): Json<PushEndpointRequest>) -> AppResult<StatusCode> {
    if let Some(ep) = &req.endpoint
        && !(ep.starts_with("https://") || ep.starts_with("http://"))
    {
        return Err(AppError::BadRequest("endpoint must be an http(s) URL".into()));
    }
    state
        .db
        .call(move |c| db::set_push_endpoint(c, &device.id, req.endpoint.as_deref()))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------- games

fn check_game_id(id: &str) -> AppResult<()> {
    if valid_game_id(id) {
        Ok(())
    } else {
        Err(AppError::BadRequest(
            "game id must be 1-64 chars of a-z, 0-9, '-', '_' or '.', starting with a letter or digit".into(),
        ))
    }
}

async fn list_games(State(state): State<AppState>, device: Device) -> AppResult<Json<Vec<GameStatus>>> {
    Ok(Json(state.db.call(move |c| db::game_statuses(c, &device.id)).await?))
}

async fn game(State(state): State<AppState>, device: Device, Path(id): Path<String>) -> AppResult<Json<GameStatus>> {
    Ok(Json(state.db.call(move |c| db::game_status(c, &device.id, &id)).await?))
}

/// Creates the game if needed and subscribes this device to it.
async fn subscribe(
    State(state): State<AppState>,
    device: Device,
    Path(id): Path<String>,
    Json(req): Json<SubscribeRequest>,
) -> AppResult<Json<GameStatus>> {
    check_game_id(&id)?;
    let name = req.name.trim().to_string();
    if name.is_empty() || name.len() > 200 {
        return Err(AppError::BadRequest("name must be 1-200 chars".into()));
    }
    Ok(Json(
        state.db.call(move |c| db::subscribe(c, &id, &name, &device.id, now_ms())).await?,
    ))
}

async fn unsubscribe(State(state): State<AppState>, device: Device, Path(id): Path<String>) -> AppResult<StatusCode> {
    if state.db.call(move |c| db::unsubscribe(c, &device.id, &id)).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound("subscription"))
    }
}

async fn pending(State(state): State<AppState>, device: Device) -> AppResult<Json<Vec<GameStatus>>> {
    Ok(Json(state.db.call(move |c| db::pending(c, &device.id)).await?))
}

// ---------------------------------------------------------------- versions

async fn list_versions(State(state): State<AppState>, _: Device, Path(id): Path<String>) -> AppResult<Json<Vec<VersionInfo>>> {
    Ok(Json(state.db.call(move |c| db::list_versions(c, &id)).await?))
}

async fn manifest(
    State(state): State<AppState>,
    _: Device,
    Path((id, version)): Path<(String, String)>,
) -> AppResult<Json<Manifest>> {
    let version = match version.as_str() {
        "latest" => None,
        v => Some(v.parse::<i64>().map_err(|_| AppError::BadRequest("version must be a number or 'latest'".into()))?),
    };
    Ok(Json(state.db.call(move |c| db::manifest(c, &id, version)).await?))
}

/// Step 2 of an upload, after the files' contents are uploaded to `/blobs`.
async fn commit(
    State(state): State<AppState>,
    device: Device,
    Path(id): Path<String>,
    Json(body): Json<CommitRequest>,
) -> AppResult<(StatusCode, Json<CommitResponse>)> {
    check_game_id(&id)?;
    validate_files(&body.files)?;
    if body.note.as_ref().is_some_and(|n| n.len() > 500) {
        return Err(AppError::BadRequest("note must be at most 500 chars".into()));
    }
    let req = NewVersion {
        base_version: body.base_version,
        files: body.files,
        played_at: body.played_at,
        note: body.note,
    };
    let blobs = state.blobs.clone();
    let keep = state.config.keep_versions;
    let device_id = device.id.clone();
    let outcome = state
        .db
        .call(move |c| db::commit(c, &blobs, keep, &id, &device_id, req, now_ms()))
        .await?;

    if outcome.created {
        let info = &outcome.manifest.info;
        tracing::info!("{} v{} uploaded by {}", info.game_id, info.version, device.name);
        let ws_msg = serde_json::to_string(&ServerMsg::VersionAvailable {
            game_name: outcome.game_name.clone(),
            manifest: outcome.manifest.clone(),
        })
        .expect("serializable");
        let push_body = serde_json::to_string(&PushPayload {
            kind: "version_available".into(),
            game_id: info.game_id.clone(),
            game_name: outcome.game_name.clone(),
            version: info.version,
            from_device: info.device_name.clone(),
        })
        .expect("serializable");
        for r in &outcome.recipients {
            // Live socket first; push only reaches devices that aren't connected.
            // Either way the device stays "pending" until it acks.
            if !state.hub.send(&r.device_id, &ws_msg)
                && let Some(endpoint) = &r.push_endpoint
            {
                state.pusher.push(endpoint.clone(), push_body.clone());
            }
        }
    }

    let status = if outcome.created { StatusCode::CREATED } else { StatusCode::OK };
    Ok((status, Json(CommitResponse { created: outcome.created, manifest: outcome.manifest })))
}

fn validate_files(files: &[FileEntry]) -> AppResult<()> {
    if files.is_empty() {
        return Err(AppError::BadRequest("a save must contain at least one file".into()));
    }
    if files.len() > MAX_FILES_PER_VERSION {
        return Err(AppError::BadRequest(format!("at most {MAX_FILES_PER_VERSION} files per save")));
    }
    let mut seen = HashSet::new();
    for f in files {
        if !valid_save_path(&f.path) {
            return Err(AppError::BadRequest(format!("invalid path: {:?}", f.path)));
        }
        if !seen.insert(f.path.as_str()) {
            return Err(AppError::BadRequest(format!("duplicate path: {}", f.path)));
        }
        if !valid_sha256(&f.sha256) {
            return Err(AppError::BadRequest(format!("{}: sha256 must be 64 lowercase hex chars", f.path)));
        }
        if f.size < 0 {
            return Err(AppError::BadRequest(format!("{}: negative size", f.path)));
        }
    }
    Ok(())
}

async fn ack(
    State(state): State<AppState>,
    device: Device,
    Path(id): Path<String>,
    Json(body): Json<AckRequest>,
) -> AppResult<Json<GameStatus>> {
    Ok(Json(
        state
            .db
            .call(move |c| db::ack(c, &device.id, &id, body.version, body.state, now_ms()))
            .await?,
    ))
}

// ---------------------------------------------------------------- blobs

/// Step 1 of an upload: which of these file contents does the server not have yet?
async fn missing_blobs(State(state): State<AppState>, _: Device, Json(req): Json<MissingRequest>) -> AppResult<Json<MissingResponse>> {
    if req.sha256.len() > MAX_FILES_PER_VERSION {
        return Err(AppError::BadRequest("too many hashes".into()));
    }
    let blobs = state.blobs.clone();
    let missing = tokio::task::spawn_blocking(move || {
        let mut missing = Vec::new();
        for sha in req.sha256 {
            if !valid_sha256(&sha) {
                continue;
            }
            if blobs.size(&sha).is_some() {
                blobs.touch(&sha);
            } else {
                missing.push(sha);
            }
        }
        missing.sort();
        missing.dedup();
        missing
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(Json(MissingResponse { missing }))
}

fn check_sha(sha: &str) -> AppResult<()> {
    if valid_sha256(sha) {
        Ok(())
    } else {
        Err(AppError::BadRequest("sha256 must be 64 lowercase hex chars".into()))
    }
}

async fn upload_blob(State(state): State<AppState>, _: Device, Path(sha): Path<String>, body: Body) -> AppResult<StatusCode> {
    check_sha(&sha)?;
    let created = state.blobs.put(&sha, body.into_data_stream()).await?;
    Ok(if created { StatusCode::CREATED } else { StatusCode::OK })
}

async fn download_blob(State(state): State<AppState>, _: Device, Path(sha): Path<String>) -> AppResult<Response> {
    check_sha(&sha)?;
    let file = match tokio::fs::File::open(state.blobs.path(&sha)).await {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(AppError::NotFound("blob")),
        Err(e) => return Err(e.into()),
    };
    let len = file.metadata().await?.len();
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (header::CONTENT_LENGTH, len.to_string()),
            (header::ETAG, format!("\"{sha}\"")),
            (header::CACHE_CONTROL, "private, max-age=31536000, immutable".to_string()),
        ],
        Body::from_stream(ReaderStream::new(file)),
    )
        .into_response())
}
