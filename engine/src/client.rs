//! HTTP + WebSocket client for the SaveSync server.

use std::{path::Path, time::Duration};

use bytes::Bytes;
use futures_util::Stream;
use reqwest::{Response, StatusCode};
use savesync_protocol::{
    AckRequest, AckState, CommitRequest, CommitResponse, EnrollRequest, EnrollResponse, ErrorBody, GameStatus, Manifest,
    MissingRequest, MissingResponse, PushEndpointRequest, SubscribeRequest,
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{client::IntoClientRequest, http::HeaderValue},
};
use tokio_util::io::ReaderStream;

use crate::error::{EngineError, Result};

pub type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
}

pub enum CommitOutcome {
    Committed(CommitResponse),
    Conflict { current_version: i64 },
    MissingBlobs(Vec<String>),
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        // Generous: large saves over a slow connection. Stalls are caught by read_timeout.
        .timeout(Duration::from_secs(30 * 60))
        .read_timeout(Duration::from_secs(60))
        .build()
        .expect("http client")
}

fn normalize_base(server_url: &str) -> String {
    server_url.trim().trim_end_matches('/').to_string()
}

async fn error_from(resp: Response) -> EngineError {
    let status = resp.status();
    if status == StatusCode::UNAUTHORIZED {
        return EngineError::Unauthorized;
    }
    match resp.json::<ErrorBody>().await {
        Ok(body) => EngineError::Api { status: status.as_u16(), code: body.error, message: body.message },
        Err(_) => EngineError::Api {
            status: status.as_u16(),
            code: "unknown".into(),
            message: status.canonical_reason().unwrap_or("error").into(),
        },
    }
}

async fn ok(resp: Response) -> Result<Response> {
    if resp.status().is_success() { Ok(resp) } else { Err(error_from(resp).await) }
}

impl Client {
    pub fn new(server_url: &str, token: &str) -> Self {
        Self { http: http_client(), base: normalize_base(server_url), token: token.to_string() }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v1{path}", self.base)
    }

    pub async fn enroll(server_url: &str, enroll_key: &str, name: &str, platform: &str) -> Result<EnrollResponse> {
        let resp = http_client()
            .post(format!("{}/api/v1/devices", normalize_base(server_url)))
            .bearer_auth(enroll_key)
            .json(&EnrollRequest { name: name.into(), platform: platform.into() })
            .send()
            .await?;
        if resp.status() == StatusCode::UNAUTHORIZED {
            return Err(EngineError::Invalid("the enroll key was not accepted".into()));
        }
        Ok(ok(resp).await?.json().await?)
    }

    pub async fn subscribe(&self, game_id: &str, name: &str) -> Result<GameStatus> {
        let resp = self
            .http
            .put(self.url(&format!("/games/{game_id}")))
            .bearer_auth(&self.token)
            .json(&SubscribeRequest { name: name.into() })
            .send()
            .await?;
        Ok(ok(resp).await?.json().await?)
    }

    pub async fn unsubscribe(&self, game_id: &str) -> Result<()> {
        let resp = self
            .http
            .delete(self.url(&format!("/games/{game_id}/subscription")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        if resp.status() == StatusCode::NOT_FOUND {
            return Ok(());
        }
        ok(resp).await?;
        Ok(())
    }

    pub async fn pending(&self) -> Result<Vec<GameStatus>> {
        let resp = self.http.get(self.url("/pending")).bearer_auth(&self.token).send().await?;
        Ok(ok(resp).await?.json().await?)
    }

    pub async fn manifest(&self, game_id: &str, version: i64) -> Result<Manifest> {
        let resp = self
            .http
            .get(self.url(&format!("/games/{game_id}/versions/{version}")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        Ok(ok(resp).await?.json().await?)
    }

    pub async fn missing(&self, shas: Vec<String>) -> Result<Vec<String>> {
        let resp = self
            .http
            .post(self.url("/blobs/missing"))
            .bearer_auth(&self.token)
            .json(&MissingRequest { sha256: shas })
            .send()
            .await?;
        Ok(ok(resp).await?.json::<MissingResponse>().await?.missing)
    }

    pub async fn upload_blob(&self, sha: &str, file: &Path) -> Result<()> {
        let file = tokio::fs::File::open(file).await?;
        let len = file.metadata().await?.len();
        let resp = self
            .http
            .put(self.url(&format!("/blobs/{sha}")))
            .bearer_auth(&self.token)
            .header(reqwest::header::CONTENT_LENGTH, len)
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(reqwest::Body::wrap_stream(ReaderStream::new(file)))
            .send()
            .await?;
        ok(resp).await?;
        Ok(())
    }

    pub async fn download_blob(&self, sha: &str) -> Result<impl Stream<Item = reqwest::Result<Bytes>> + Unpin> {
        let resp = self
            .http
            .get(self.url(&format!("/blobs/{sha}")))
            .bearer_auth(&self.token)
            .send()
            .await?;
        Ok(ok(resp).await?.bytes_stream())
    }

    pub async fn commit(&self, game_id: &str, req: &CommitRequest) -> Result<CommitOutcome> {
        let resp = self
            .http
            .post(self.url(&format!("/games/{game_id}/versions")))
            .bearer_auth(&self.token)
            .json(req)
            .send()
            .await?;
        match resp.status() {
            s if s.is_success() => Ok(CommitOutcome::Committed(resp.json().await?)),
            StatusCode::CONFLICT => {
                let body: ErrorBody = resp.json().await?;
                Ok(CommitOutcome::Conflict { current_version: body.current_version.unwrap_or_default() })
            }
            StatusCode::UNPROCESSABLE_ENTITY => {
                let body: ErrorBody = resp.json().await?;
                Ok(CommitOutcome::MissingBlobs(body.missing.unwrap_or_default()))
            }
            _ => Err(error_from(resp).await),
        }
    }

    pub async fn ack(&self, game_id: &str, version: i64, state: AckState) -> Result<GameStatus> {
        let resp = self
            .http
            .post(self.url(&format!("/games/{game_id}/ack")))
            .bearer_auth(&self.token)
            .json(&AckRequest { version, state })
            .send()
            .await?;
        Ok(ok(resp).await?.json().await?)
    }

    pub async fn set_push_endpoint(&self, endpoint: Option<String>) -> Result<()> {
        let resp = self
            .http
            .put(self.url("/me/push"))
            .bearer_auth(&self.token)
            .json(&PushEndpointRequest { endpoint })
            .send()
            .await?;
        ok(resp).await?;
        Ok(())
    }

    pub async fn connect_ws(&self) -> Result<WsStream> {
        let ws_base = if let Some(rest) = self.base.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = self.base.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            return Err(EngineError::Invalid("server URL must start with http:// or https://".into()));
        };
        let mut req = format!("{ws_base}/api/v1/ws")
            .into_client_request()
            .map_err(|e| EngineError::Invalid(e.to_string()))?;
        req.headers_mut().insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {}", self.token)).map_err(|e| EngineError::Other(e.to_string()))?,
        );
        let connect = tokio_tungstenite::connect_async(req);
        match tokio::time::timeout(Duration::from_secs(15), connect).await {
            Err(_) => Err(EngineError::Offline("websocket connect timed out".into())),
            Ok(Ok((ws, _))) => Ok(ws),
            Ok(Err(tokio_tungstenite::tungstenite::Error::Http(resp))) if resp.status() == 401 => {
                Err(EngineError::Unauthorized)
            }
            Ok(Err(e)) => Err(EngineError::Offline(e.to_string())),
        }
    }
}
