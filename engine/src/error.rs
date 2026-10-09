#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("not paired with a server")]
    NotPaired,
    /// Network-level failure: no connection, DNS, timeout. Work stays queued.
    #[error("server unreachable: {0}")]
    Offline(String),
    #[error("this device is no longer authorized; pair it again")]
    Unauthorized,
    #[error("server error ({status}): {message}")]
    Api { status: u16, code: String, message: String },
    #[error("{0} is running; close it first")]
    GameRunning(String),
    #[error("unknown game: {0}")]
    UnknownGame(String),
    #[error("{0}")]
    Invalid(String),
    #[error("nothing to {0}")]
    NothingTo(&'static str),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("local database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("{0}")]
    Other(String),
}

impl EngineError {
    pub fn is_offline(&self) -> bool {
        matches!(self, EngineError::Offline(_))
    }
}

impl From<reqwest::Error> for EngineError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_connect() || e.is_timeout() || e.is_request() || e.is_body() {
            EngineError::Offline(e.to_string())
        } else {
            EngineError::Other(e.to_string())
        }
    }
}

impl From<serde_json::Error> for EngineError {
    fn from(e: serde_json::Error) -> Self {
        EngineError::Other(format!("invalid data: {e}"))
    }
}

pub type Result<T, E = EngineError> = std::result::Result<T, E>;
