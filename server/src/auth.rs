use axum::{extract::FromRequestParts, http::request::Parts};
use sha2::{Digest, Sha256};

use crate::{AppState, db, error::AppError, model::now_ms};

/// Only refresh `last_seen_at` this often, so idle polling doesn't keep the NAS drives awake.
const LAST_SEEN_RESOLUTION_MS: i64 = 15 * 60 * 1000;

pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn bearer(parts: &Parts) -> Option<&str> {
    parts
        .headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A request authenticated with a device token.
pub struct Device {
    pub id: String,
    pub name: String,
}

impl FromRequestParts<AppState> for Device {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let token = bearer(parts).ok_or(AppError::Unauthorized)?;
        let hash = hash_token(token);
        let device = state
            .db
            .call(move |c| {
                let Some(d) = db::device_by_token(c, &hash)? else { return Ok(None) };
                let now = now_ms();
                if now - d.last_seen_at > LAST_SEEN_RESOLUTION_MS {
                    db::touch_device(c, &d.id, now)?;
                }
                Ok(Some(d))
            })
            .await?
            .ok_or(AppError::Unauthorized)?;
        Ok(Device { id: device.id, name: device.name })
    }
}

/// A request authenticated with the enrollment key (adding/removing devices).
pub struct Admin;

impl FromRequestParts<AppState> for Admin {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let token = bearer(parts).ok_or(AppError::Unauthorized)?;
        if constant_time_eq(token.as_bytes(), state.config.enroll_key.as_bytes()) {
            Ok(Admin)
        } else {
            Err(AppError::Unauthorized)
        }
    }
}
