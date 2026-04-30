// ─── ws.rs ────────────────────────────────────────────────────────────────────
// WebSocket upgrade handler and per-connection socket logic.
// ─────────────────────────────────────────────────────────────────────────────

use moltendb_auth as auth;
use moltendb_core::engine;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    extract::ws::Utf8Bytes,
};
use futures::{sink::SinkExt, stream::StreamExt};
use tracing::warn;

// ─── WebSocket handler ────────────────────────────────────────────────────────

/// GET /ws — upgrade an HTTP connection to a WebSocket connection.
///
/// `WebSocketUpgrade` is an Axum extractor that handles the HTTP → WS upgrade
/// handshake. The actual socket logic runs in `handle_socket`.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State((db, _, _max_body_size, _)): State<(engine::Db, auth::UserStore, usize, String)>,
) -> impl axum::response::IntoResponse {
    // `on_upgrade` completes the handshake and calls our handler with the socket.
    ws.on_upgrade(|socket| handle_socket(socket, db))
}

/// Handle an authenticated WebSocket connection.
///
/// Protocol:
///   1. The first message MUST be `{ "action": "AUTH", "token": "<jwt>" }`.
///      If authentication fails the connection is closed immediately.
///   2. After authentication the client can send `{ "action": "SUBSCRIBE", "collection": "<name>" }`
///      to register interest in a collection, or `{ "action": "UNSUBSCRIBE", "collection": "<name>" }`
///      to deregister. Subscriptions are purely advisory — the server pushes change events
///      regardless of subscription state for now, but the field is reserved for future
///      per-collection filtering.
///   3. The server pushes a change event to the client whenever any write (insert, update,
///      delete, drop) occurs on the database:
///        `{ "event": "change", "collection": "<name>", "key": "<key>", "new_v": <version> }`
///      All CRUD operations must be performed via the HTTP endpoints (POST /get, /set, /update,
///      /delete). WebSockets are exclusively for real-time push notifications.
///
/// The socket is split into a sender and receiver, each running in their own task.
/// This allows sending and receiving to happen concurrently without blocking each other.
async fn handle_socket(mut socket: WebSocket, db: engine::Db) {
    // Step 1: Require the first message to be an AUTH message.
    let is_authenticated = match socket.next().await {
        Some(Ok(Message::Text(text))) => {
            if let Ok(payload) = serde_json::from_str::<serde_json::Value>(&text) {
                if payload["action"].as_str() == Some("AUTH") {
                    if let Some(token) = payload["token"].as_str() {
                        auth::verify_token(token).is_ok()
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                false
            }
        }
        _ => false,
    };

    if !is_authenticated {
        let _ = socket
            .send(Message::Text(Utf8Bytes::from(
                r#"{"error":"Authentication required. Send {\"action\":\"AUTH\",\"token\":\"<jwt>\"} as the first message."}"#,
            )))
            .await;
        let _ = socket.close().await;
        warn!("🔒 Rejected unauthenticated WebSocket connection.");
        return;
    }

    // Authentication succeeded — confirm and explain the subscription-only protocol.
    let _ = socket
        .send(Message::Text(Utf8Bytes::from(
            r#"{"status":"authenticated","message":"Connected to MoltenDB real-time feed. Use HTTP endpoints for CRUD. Send {\"action\":\"SUBSCRIBE\",\"collection\":\"<name>\"} to register interest."}"#,
        )))
        .await;

    // Step 2: Split the socket into independent sender and receiver halves.
    let (mut sender, mut receiver) = socket.split();

    // Subscribe to the database broadcast channel.
    // Every write (insert, update, delete, drop) broadcasts a JSON string here.
    let mut rx = db.subscribe();

    // Spawn a task that drains incoming client messages.
    // We only handle SUBSCRIBE / UNSUBSCRIBE — everything else gets a clear error
    // telling the client to use HTTP instead.
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(Message::Text(_text))) = receiver.next().await {
            // Client messages are intentionally ignored in this simplified model.
            // Future: parse SUBSCRIBE/UNSUBSCRIBE and maintain a per-connection
            // collection filter set to avoid sending irrelevant events.
        }
    });

    // Spawn a task that forwards database change events to the client.
    let mut send_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                // A broadcast event from the database is ready to push.
                Ok(msg) = rx.recv() => {
                    if sender.send(Message::Text(Utf8Bytes::from(msg))).await.is_err() {
                        break; // Client disconnected.
                    }
                }
                // Broadcast channel closed (server shutting down) — exit.
                else => break,
            }
        }
    });

    // Wait for either task to finish (client disconnect or server shutdown).
    tokio::select! {
        _ = (&mut recv_task) => send_task.abort(),
        _ = (&mut send_task) => recv_task.abort(),
    };
}
