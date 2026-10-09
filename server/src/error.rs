use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

use crate::model::VersionInfo;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("missing or invalid credentials")]
    Unauthorized,
    #[error("{0} not found")]
    NotFound(&'static str),
    #[error("{0}")]
    BadRequest(String),
    /// The upload was based on an older version than the server has.
    #[error("the server has a newer version than the one this upload was based on")]
    Conflict {
        current_version: i64,
        latest: Option<Box<VersionInfo>>,
    },
    /// A commit referenced file contents that haven't been uploaded.
    #[error("some files must be uploaded before committing")]
    MissingBlobs(Vec<String>),
    #[error("file exceeds the server's size limit")]
    TooLarge,
    #[error("internal error: {0}")]
    Internal(String),
}

pub type AppResult<T> = Result<T, AppError>;

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        AppError::Internal(format!("database: {e}"))
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::Internal(format!("io: {e}"))
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let message = self.to_string();
        let (status, body) = match self {
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, json!({"error": "unauthorized", "message": message})),
            AppError::NotFound(_) => (StatusCode::NOT_FOUND, json!({"error": "not_found", "message": message})),
            AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, json!({"error": "bad_request", "message": message})),
            AppError::Conflict { current_version, latest } => (
                StatusCode::CONFLICT,
                json!({"error": "conflict", "message": message, "current_version": current_version, "latest": latest}),
            ),
            AppError::MissingBlobs(missing) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                json!({"error": "missing_blobs", "message": message, "missing": missing}),
            ),
            AppError::TooLarge => (StatusCode::PAYLOAD_TOO_LARGE, json!({"error": "too_large", "message": message})),
            AppError::Internal(detail) => {
                tracing::error!("{detail}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({"error": "internal", "message": "internal server error"}),
                )
            }
        };
        (status, Json(body)).into_response()
    }
}
