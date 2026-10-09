//! Getting the word out about new versions: WebSocket for connected devices,
//! UnifiedPush (e.g. ntfy) for sleeping phones.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use tokio::sync::mpsc;

pub use savesync_protocol::{ClientMsg, PushPayload, ServerMsg};

type Sender = mpsc::UnboundedSender<String>;
type Connections = HashMap<String, Vec<(u64, Sender)>>;

/// Tracks live WebSocket connections per device.
#[derive(Clone, Default)]
pub struct Hub {
    conns: Arc<Mutex<Connections>>,
    next_id: Arc<AtomicU64>,
}

impl Hub {
    pub fn register(&self, device_id: &str) -> (u64, mpsc::UnboundedReceiver<String>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.lock().entry(device_id.to_string()).or_default().push((id, tx));
        (id, rx)
    }

    pub fn unregister(&self, device_id: &str, conn_id: u64) {
        let mut conns = self.lock();
        if let Some(list) = conns.get_mut(device_id) {
            list.retain(|(id, _)| *id != conn_id);
            if list.is_empty() {
                conns.remove(device_id);
            }
        }
    }

    /// Queues `msg` on every connection for the device. Returns whether any were live.
    pub fn send(&self, device_id: &str, msg: &str) -> bool {
        let mut conns = self.lock();
        let Some(list) = conns.get_mut(device_id) else { return false };
        list.retain(|(_, tx)| tx.send(msg.to_string()).is_ok());
        !list.is_empty()
    }

    /// Drops a device's connections (used when it's revoked).
    pub fn disconnect(&self, device_id: &str) {
        self.lock().remove(device_id);
    }

    /// Drops every connection so graceful shutdown doesn't wait on idle sockets.
    pub fn disconnect_all(&self) {
        self.lock().clear();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connections> {
        self.conns.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[derive(Clone)]
pub struct Pusher {
    client: reqwest::Client,
    rewrite: Option<(String, String)>,
}

impl Pusher {
    pub fn new(rewrite: Option<(String, String)>) -> Self {
        // reqwest is built without a bundled TLS backend; use ring (no cmake/clang needed in Docker).
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("http client");
        Self { client, rewrite }
    }

    fn resolve(&self, endpoint: String) -> String {
        match &self.rewrite {
            Some((from, to)) if endpoint.starts_with(from.as_str()) => format!("{to}{}", &endpoint[from.len()..]),
            _ => endpoint,
        }
    }

    /// Fires a UnifiedPush message in the background. Distributors like ntfy
    /// hold messages for devices that are asleep and deliver them on wake.
    pub fn push(&self, endpoint: String, body: String) {
        let endpoint = self.resolve(endpoint);
        let client = self.client.clone();
        tokio::spawn(async move {
            let result = client
                .post(&endpoint)
                .header("Content-Type", "application/json")
                // Ask the distributor to keep the message for up to 3 days.
                .header("TTL", "259200")
                .header("Urgency", "high")
                .body(body)
                .send()
                .await
                .and_then(|r| r.error_for_status());
            if let Err(e) = result {
                tracing::warn!("push to {endpoint} failed: {e}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_matching_prefix_only() {
        let p = Pusher::new(Some(("http://your-nas:8421".into(), "http://ntfy".into())));
        assert_eq!(p.resolve("http://your-nas:8421/upAbC?up=1".into()), "http://ntfy/upAbC?up=1");
        assert_eq!(p.resolve("https://ntfy.sh/upAbC".into()), "https://ntfy.sh/upAbC");
        assert_eq!(Pusher::new(None).resolve("http://x/y".into()), "http://x/y");
    }
}
