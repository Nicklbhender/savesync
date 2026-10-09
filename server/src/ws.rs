use std::time::Duration;

use axum::{
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::Response,
};
use futures_util::{SinkExt, StreamExt};

use crate::{
    AppState, db,
    auth::Device,
    model::now_ms,
    notify::{ClientMsg, ServerMsg},
};

const PING_INTERVAL: Duration = Duration::from_secs(30);

pub async fn handler(State(state): State<AppState>, device: Device, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| run(state, device, socket))
}

async fn run(state: AppState, device: Device, socket: WebSocket) {
    // Register before reading pending so nothing committed in between is missed.
    let (conn_id, mut outbox) = state.hub.register(&device.id);
    let (mut tx, mut rx) = socket.split();

    let device_id = device.id.clone();
    let hello = match state.db.call(move |c| db::pending(c, &device_id)).await {
        Ok(pending) => ServerMsg::Hello { device_id: device.id.clone(), server_time: now_ms(), pending },
        Err(e) => ServerMsg::Error { message: e.to_string() },
    };
    if send(&mut tx, &hello).await.is_err() {
        state.hub.unregister(&device.id, conn_id);
        return;
    }
    tracing::debug!("{} connected", device.name);

    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.tick().await;
    loop {
        tokio::select! {
            queued = outbox.recv() => {
                // None means the hub dropped us (revoked or shutting down).
                let Some(text) = queued else { break };
                if tx.send(Message::Text(text.into())).await.is_err() { break }
            }
            incoming = rx.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        let reply = handle_client_msg(&state, &device, text.as_str()).await;
                        if send(&mut tx, &reply).await.is_err() { break }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => {}
                }
            }
            _ = ping.tick() => {
                if tx.send(Message::Ping(Default::default())).await.is_err() { break }
            }
        }
    }

    state.hub.unregister(&device.id, conn_id);
    let _ = tx.close().await;
    tracing::debug!("{} disconnected", device.name);
}

async fn handle_client_msg(state: &AppState, device: &Device, text: &str) -> ServerMsg {
    match serde_json::from_str::<ClientMsg>(text) {
        Ok(ClientMsg::Ack { game_id, version, state: ack_state }) => {
            let device_id = device.id.clone();
            match state
                .db
                .call(move |c| db::ack(c, &device_id, &game_id, version, ack_state, now_ms()))
                .await
            {
                Ok(status) => ServerMsg::Acked { status },
                Err(e) => ServerMsg::Error { message: e.to_string() },
            }
        }
        Err(e) => ServerMsg::Error { message: format!("unrecognized message: {e}") },
    }
}

async fn send<S>(tx: &mut S, msg: &ServerMsg) -> Result<(), axum::Error>
where
    S: SinkExt<Message, Error = axum::Error> + Unpin,
{
    let text = serde_json::to_string(msg).expect("serializable");
    tx.send(Message::Text(text.into())).await
}
