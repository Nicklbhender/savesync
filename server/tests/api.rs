//! End-to-end tests against a real server on a random port.

use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::StreamExt;
use reqwest::{Client, StatusCode};
use savesync_server::{build, config::Config};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const ENROLL_KEY: &str = "test-enroll-key-0123456789";

struct Server {
    base: String,
    addr: SocketAddr,
    data_dir: PathBuf,
    _tmp: tempfile::TempDir,
}

async fn start(keep_versions: u32) -> Server {
    let tmp = tempfile::tempdir().unwrap();
    let config = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        data_dir: tmp.path().to_path_buf(),
        enroll_key: ENROLL_KEY.into(),
        keep_versions,
        max_file_bytes: 1024 * 1024,
        push_rewrite: None,
    };
    let (app, _state) = build(config).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Server { base: format!("http://{addr}/api/v1"), addr, data_dir: tmp.path().to_path_buf(), _tmp: tmp }
}

struct Dev {
    id: String,
    token: String,
}

async fn enroll(http: &Client, s: &Server, name: &str, platform: &str) -> Dev {
    let r: Value = http
        .post(format!("{}/devices", s.base))
        .bearer_auth(ENROLL_KEY)
        .json(&json!({"name": name, "platform": platform}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    Dev { id: r["device_id"].as_str().unwrap().into(), token: r["token"].as_str().unwrap().into() }
}

fn sha(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// Uploads files the server is missing, then commits. Returns (status, body).
async fn upload_save(http: &Client, s: &Server, dev: &Dev, game: &str, base: i64, files: &[(&str, &[u8])]) -> (StatusCode, Value) {
    let hashes: Vec<String> = files.iter().map(|(_, d)| sha(d)).collect();
    let missing: Value = http
        .post(format!("{}/blobs/missing", s.base))
        .bearer_auth(&dev.token)
        .json(&json!({"sha256": hashes}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    for (_, data) in files {
        let h = sha(data);
        if missing["missing"].as_array().unwrap().iter().any(|m| m == &h) {
            let r = http
                .put(format!("{}/blobs/{h}", s.base))
                .bearer_auth(&dev.token)
                .body(data.to_vec())
                .send()
                .await
                .unwrap();
            assert!(r.status().is_success(), "blob upload: {}", r.status());
        }
    }
    let entries: Vec<Value> = files
        .iter()
        .map(|(p, d)| json!({"path": p, "sha256": sha(d), "size": d.len()}))
        .collect();
    let r = http
        .post(format!("{}/games/{game}/versions", s.base))
        .bearer_auth(&dev.token)
        .json(&json!({"base_version": base, "files": entries, "played_at": 1_700_000_000_000i64}))
        .send()
        .await
        .unwrap();
    let status = r.status();
    (status, r.json().await.unwrap())
}

async fn subscribe(http: &Client, s: &Server, dev: &Dev, game: &str) -> Value {
    http.put(format!("{}/games/{game}", s.base))
        .bearer_auth(&dev.token)
        .json(&json!({"name": "My Game"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn get_json(http: &Client, url: String, token: &str) -> Value {
    http.get(url).bearer_auth(token).send().await.unwrap().json().await.unwrap()
}

#[tokio::test]
async fn upload_pending_ack_flow() {
    let s = start(20).await;
    let http = Client::new();
    let pc = enroll(&http, &s, "Gaming PC", "windows").await;
    let phone = enroll(&http, &s, "Handheld", "android").await;

    // Auth is required.
    let r = http.get(format!("{}/games", s.base)).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let r = http.post(format!("{}/devices", s.base)).bearer_auth(&pc.token)
        .json(&json!({"name": "x", "platform": "y"})).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED, "device tokens can't enroll devices");

    subscribe(&http, &s, &pc, "my-game").await;
    let status = subscribe(&http, &s, &phone, "my-game").await;
    assert_eq!(status["pending"], false, "nothing uploaded yet");

    let save: &[u8] = b"game save data v1";
    let (code, body) = upload_save(&http, &s, &pc, "my-game", 0, &[("game.sav", save)]).await;
    assert_eq!(code, StatusCode::CREATED, "{body}");
    assert_eq!(body["version"], 1);
    assert_eq!(body["device_name"], "Gaming PC");

    // The uploader is up to date; the phone has it pending.
    let pc_pending = get_json(&http, format!("{}/pending", s.base), &pc.token).await;
    assert_eq!(pc_pending.as_array().unwrap().len(), 0);
    let pending = get_json(&http, format!("{}/pending", s.base), &phone.token).await;
    assert_eq!(pending.as_array().unwrap().len(), 1);
    assert_eq!(pending[0]["current_version"], 1);
    assert_eq!(pending[0]["latest"]["device_name"], "Gaming PC");

    // Phone downloads the manifest and file.
    let manifest = get_json(&http, format!("{}/games/my-game/versions/latest", s.base), &phone.token).await;
    let file = &manifest["files"][0];
    assert_eq!(file["path"], "game.sav");
    let bytes = http
        .get(format!("{}/blobs/{}", s.base, file["sha256"].as_str().unwrap()))
        .bearer_auth(&phone.token)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(&bytes[..], save);

    // Delivered clears pending; applied is tracked separately.
    let r: Value = http.post(format!("{}/games/my-game/ack", s.base)).bearer_auth(&phone.token)
        .json(&json!({"version": 1, "state": "delivered"})).send().await.unwrap().json().await.unwrap();
    assert_eq!(r["pending"], false);
    assert_eq!(r["delivered_version"], 1);
    assert_eq!(r["applied_version"], 0);
    let r: Value = http.post(format!("{}/games/my-game/ack", s.base)).bearer_auth(&phone.token)
        .json(&json!({"version": 1, "state": "applied"})).send().await.unwrap().json().await.unwrap();
    assert_eq!(r["applied_version"], 1);

    // Can't ack a version that doesn't exist.
    let r = http.post(format!("{}/games/my-game/ack", s.base)).bearer_auth(&phone.token)
        .json(&json!({"version": 5, "state": "applied"})).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);

    // A device subscribing later sees the existing save as pending immediately.
    let laptop = enroll(&http, &s, "MacBook", "macos").await;
    let status = subscribe(&http, &s, &laptop, "my-game").await;
    assert_eq!(status["pending"], true);
}

#[tokio::test]
async fn conflicts_and_identical_uploads() {
    let s = start(20).await;
    let http = Client::new();
    let pc = enroll(&http, &s, "PC", "windows").await;
    let phone = enroll(&http, &s, "Phone", "android").await;
    subscribe(&http, &s, &pc, "game").await;
    subscribe(&http, &s, &phone, "game").await;

    let (code, _) = upload_save(&http, &s, &pc, "game", 0, &[("save.srm", b"pc v1")]).await;
    assert_eq!(code, StatusCode::CREATED);

    // Phone played offline from nothing (base 0) and uploads different data: conflict.
    let (code, body) = upload_save(&http, &s, &phone, "game", 0, &[("save.srm", b"phone offline")]).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["current_version"], 1);
    assert_eq!(body["latest"]["device_name"], "PC");

    // User picks "keep phone": re-commit on top of the current version.
    let (code, body) = upload_save(&http, &s, &phone, "game", 1, &[("save.srm", b"phone offline")]).await;
    assert_eq!(code, StatusCode::CREATED);
    assert_eq!(body["version"], 2);
    assert_eq!(body["base_version"], 1);

    // PC uploads exactly what the server already has, with a stale base: not a conflict.
    let (code, body) = upload_save(&http, &s, &pc, "game", 1, &[("save.srm", b"phone offline")]).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    assert_eq!(body["created"], false);
    assert_eq!(body["version"], 2);
    let pending = get_json(&http, format!("{}/pending", s.base), &pc.token).await;
    assert_eq!(pending.as_array().unwrap().len(), 0, "identical upload marks the device up to date");
}

#[tokio::test]
async fn upload_validation() {
    let s = start(20).await;
    let http = Client::new();
    let pc = enroll(&http, &s, "PC", "windows").await;
    subscribe(&http, &s, &pc, "game").await;

    // Committing before uploading contents lists what's missing.
    let h = sha(b"never uploaded");
    let r = http.post(format!("{}/games/game/versions", s.base)).bearer_auth(&pc.token)
        .json(&json!({"base_version": 0, "files": [{"path": "a.srm", "sha256": h, "size": 14}]}))
        .send().await.unwrap();
    assert_eq!(r.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["missing"][0], h);

    // Content that doesn't match its hash is rejected and not stored.
    let r = http.put(format!("{}/blobs/{}", s.base, sha(b"expected"))).bearer_auth(&pc.token)
        .body("something else").send().await.unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    let missing: Value = http.post(format!("{}/blobs/missing", s.base)).bearer_auth(&pc.token)
        .json(&json!({"sha256": [sha(b"expected")]})).send().await.unwrap().json().await.unwrap();
    assert_eq!(missing["missing"].as_array().unwrap().len(), 1);

    // Oversized files are refused.
    let big = vec![7u8; 2 * 1024 * 1024];
    let r = http.put(format!("{}/blobs/{}", s.base, sha(&big))).bearer_auth(&pc.token)
        .body(big).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::PAYLOAD_TOO_LARGE);

    // Paths that could escape the save folder are refused.
    for bad in ["../evil", "/abs", "C:/x", "a/../b"] {
        let (code, _) = upload_save(&http, &s, &pc, "game", 0, &[(bad, b"x")]).await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{bad}");
    }

    // Bad game ids are refused.
    let r = http.put(format!("{}/games/Bad%20Id", s.base)).bearer_auth(&pc.token)
        .json(&json!({"name": "x"})).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn prunes_old_versions_and_their_files() {
    let s = start(2).await;
    let http = Client::new();
    let pc = enroll(&http, &s, "PC", "windows").await;
    subscribe(&http, &s, &pc, "game").await;

    let shared: &[u8] = b"unchanged memory card";
    for (i, data) in [b"v1".as_slice(), b"v2", b"v3"].iter().enumerate() {
        let (code, _) = upload_save(&http, &s, &pc, "game", i as i64, &[("save.srm", data), ("card.mcd", shared)]).await;
        assert_eq!(code, StatusCode::CREATED);
    }
    let versions = get_json(&http, format!("{}/games/game/versions", s.base), &pc.token).await;
    let numbers: Vec<i64> = versions.as_array().unwrap().iter().map(|v| v["version"].as_i64().unwrap()).collect();
    assert_eq!(numbers, vec![3, 2]);

    let blob = |d: &[u8]| {
        let h = sha(d);
        s.data_dir.join("blobs").join(&h[..2]).join(h)
    };
    assert!(!blob(b"v1").exists(), "v1's unique file is deleted");
    assert!(blob(b"v2").exists());
    assert!(blob(shared).exists(), "files still used by newer versions are kept");
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn next_json(ws: &mut Ws) -> Value {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next()).await.unwrap().unwrap().unwrap();
        if let tokio_tungstenite::tungstenite::Message::Text(t) = msg {
            return serde_json::from_str(t.as_str()).unwrap();
        }
    }
}

#[tokio::test]
async fn websocket_hello_and_live_notification() {
    let s = start(20).await;
    let http = Client::new();
    let pc = enroll(&http, &s, "PC", "windows").await;
    let phone = enroll(&http, &s, "Phone", "android").await;
    subscribe(&http, &s, &pc, "game").await;
    subscribe(&http, &s, &phone, "game").await;

    // Uploaded while the phone was "asleep".
    upload_save(&http, &s, &pc, "game", 0, &[("save.srm", b"v1")]).await;

    let mut req = format!("ws://{}/api/v1/ws", s.addr).into_client_request().unwrap();
    req.headers_mut().insert("Authorization", format!("Bearer {}", phone.token).parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();

    // On connect, the phone hears what it missed.
    let hello = next_json(&mut ws).await;
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["device_id"], phone.id);
    assert_eq!(hello["pending"][0]["current_version"], 1);

    // While connected, new uploads arrive live with their manifest.
    upload_save(&http, &s, &pc, "game", 1, &[("save.srm", b"v2")]).await;
    let msg = next_json(&mut ws).await;
    assert_eq!(msg["type"], "version_available");
    assert_eq!(msg["manifest"]["version"], 2);
    assert_eq!(msg["manifest"]["files"][0]["sha256"], sha(b"v2"));

    // Acks work over the socket too.
    use futures_util::SinkExt;
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        json!({"type": "ack", "game_id": "game", "version": 2, "state": "applied"}).to_string().into(),
    ))
    .await
    .unwrap();
    let msg = next_json(&mut ws).await;
    assert_eq!(msg["type"], "acked");
    assert_eq!(msg["status"]["pending"], false);
    assert_eq!(msg["status"]["applied_version"], 2);
}

#[tokio::test]
async fn unified_push_for_disconnected_devices() {
    // A stand-in for ntfy that records what it receives.
    let received: Arc<Mutex<Vec<Value>>> = Arc::default();
    let sink = received.clone();
    let push_app = axum::Router::new().route(
        "/up/phone",
        axum::routing::post(move |body: String| {
            let sink = sink.clone();
            async move { sink.lock().unwrap().push(serde_json::from_str(&body).unwrap()) }
        }),
    );
    let push_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let push_addr = push_listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(push_listener, push_app).await.unwrap() });

    let s = start(20).await;
    let http = Client::new();
    let pc = enroll(&http, &s, "PC", "windows").await;
    let phone = enroll(&http, &s, "Phone", "android").await;
    subscribe(&http, &s, &pc, "game").await;
    subscribe(&http, &s, &phone, "game").await;
    let r = http.put(format!("{}/me/push", s.base)).bearer_auth(&phone.token)
        .json(&json!({"endpoint": format!("http://{push_addr}/up/phone")})).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::NO_CONTENT);

    upload_save(&http, &s, &pc, "game", 0, &[("save.srm", b"v1")]).await;

    for _ in 0..50 {
        if !received.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let got = received.lock().unwrap().clone();
    assert_eq!(got.len(), 1, "exactly one push for the phone, none for the uploader");
    assert_eq!(got[0]["type"], "version_available");
    assert_eq!(got[0]["game_id"], "game");
    assert_eq!(got[0]["version"], 1);
    assert_eq!(got[0]["from_device"], "PC");
}

#[tokio::test]
async fn revoked_devices_lose_access() {
    let s = start(20).await;
    let http = Client::new();
    let phone = enroll(&http, &s, "Lost phone", "android").await;
    let r = http.delete(format!("{}/devices/{}", s.base, phone.id)).bearer_auth(ENROLL_KEY).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
    let r = http.get(format!("{}/me", s.base)).bearer_auth(&phone.token).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let devices = get_json(&http, format!("{}/devices", s.base), ENROLL_KEY).await;
    assert_eq!(devices[0]["revoked"], true);
}
