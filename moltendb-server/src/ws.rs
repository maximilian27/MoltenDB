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
    Extension,
};
use futures::{sink::SinkExt, stream::StreamExt};
use tokio::time::{interval, Duration};
use tracing::warn;

// ─── WebSocket handler ────────────────────────────────────────────────────────

/// GET /ws — upgrade an HTTP connection to a WebSocket connection.
///
/// `WebSocketUpgrade` is an Axum extractor that handles the HTTP → WS upgrade
/// handshake. The actual socket logic runs in `handle_socket`.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State((db, _, _max_body_size, _, _)): State<(engine::Db, auth::UserStore, usize, usize, String)>,
    Extension(revocation_store): Extension<auth::RevocationStore>,
) -> impl axum::response::IntoResponse {
    // `on_upgrade` completes the handshake and calls our handler with the socket.
    ws.on_upgrade(|socket| handle_socket(socket, db, revocation_store))
}

/// Handle an authenticated WebSocket connection.
///
/// Protocol:
///   1. The first message MUST be `{ "action": "AUTH", "token": "<jwt>" }`.
///      The token is verified AND checked against the revocation store.
///      If authentication fails the connection is closed immediately.
///   2. After authentication the client can send `{ "action": "SUBSCRIBE", "collection": "<name>" }`
///      to register interest in a collection, or `{ "action": "UNSUBSCRIBE", "collection": "<name>" }`
///      to deregister. Subscriptions are purely advisory — the server already filters events
///      by the token's scopes, so only authorised collections are pushed.
///   3. The server pushes a change event to the client whenever a write (insert, update,
///      delete, drop) occurs on a collection the token is authorised to read:
///        `{ "event": "change", "collection": "<name>", "key": "<key>", "new_v": <version> }`
///      Events for collections outside the token's scopes are silently dropped.
///      All CRUD operations must be performed via the HTTP endpoints (POST /get, /set, /update,
///      /delete). WebSockets are exclusively for real-time push notifications.
///
/// The socket is split into a sender and receiver, each running in their own task.
/// This allows sending and receiving to happen concurrently without blocking each other.
async fn handle_socket(mut socket: WebSocket, db: engine::Db, revocation_store: auth::RevocationStore) {
    // Step 1: Require the first message to be an AUTH frame.
    // We return a distinct error string for each failure mode so the client
    // knows exactly what went wrong instead of getting a generic message.
    enum AuthResult {
        Ok(auth::Claims),
        Err(&'static str),
    }

    let auth_result = match socket.next().await {
        Some(Ok(Message::Text(text))) => {
            match serde_json::from_str::<serde_json::Value>(&text) {
                Err(_) => AuthResult::Err(
                    r#"{"error":"invalid_message","detail":"Could not parse JSON. Expected {\"action\":\"AUTH\",\"token\":\"<jwt>\"}"}"#,
                ),
                Ok(payload) => {
                    if payload["action"].as_str() != Some("AUTH") {
                        AuthResult::Err(
                            r#"{"error":"invalid_action","detail":"First message must have \"action\":\"AUTH\". Use HTTP endpoints for CRUD operations."}"#,
                        )
                    } else if let Some(token) = payload["token"].as_str() {
                        match auth::verify_token(token) {
                            Err(_) => AuthResult::Err(
                                r#"{"error":"invalid_token","detail":"JWT verification failed. The token may be expired, malformed, or signed with the wrong secret."}"#,
                            ),
                            Ok(c) => {
                                if revocation_store.is_revoked(&c.jti) {
                                    warn!("🔒 Rejected WebSocket connection: token JTI '{}' is revoked.", c.jti);
                                    AuthResult::Err(
                                        r#"{"error":"token_revoked","detail":"This token has been revoked. Mint a new token via POST /auth/tokens."}"#,
                                    )
                                } else {
                                    AuthResult::Ok(c)
                                }
                            }
                        }
                    } else {
                        AuthResult::Err(
                            r#"{"error":"missing_token","detail":"AUTH message is missing the \"token\" field. Expected {\"action\":\"AUTH\",\"token\":\"<jwt>\"}"}"#,
                        )
                    }
                }
            }
        }
        _ => AuthResult::Err(
            r#"{"error":"invalid_message","detail":"First message must be a text frame containing {\"action\":\"AUTH\",\"token\":\"<jwt>\"}"}"#,
        ),
    };

    let claims = match auth_result {
        AuthResult::Ok(c) => c,
        AuthResult::Err(msg) => {
            let _ = socket.send(Message::Text(Utf8Bytes::from(msg))).await;
            let _ = socket.close().await;
            warn!("🔒 Rejected WebSocket connection: {}", msg);
            return;
        }
    };

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
    // Only events for collections the token is authorised to read are forwarded.
    // Admin tokens (scope "*:*:*") receive all events.
    // Every 30 s the task re-checks the revocation store so that revoking a token
    // terminates any already-open connection within that window.
    let mut send_task = tokio::spawn(async move {
        let mut revocation_check = interval(Duration::from_secs(30));
        revocation_check.tick().await; // consume the immediate first tick
        loop {
            tokio::select! {
                _ = revocation_check.tick() => {
                    if revocation_store.is_revoked(&claims.jti) {
                        warn!("🔒 Closing WebSocket: token JTI '{}' was revoked after connection was established.", claims.jti);
                        let _ = sender.send(Message::Text(Utf8Bytes::from(
                            r#"{"error":"token_revoked","detail":"Your token has been revoked. The connection is being closed."}"#,
                        ))).await;
                        break;
                    }
                }
                recv_result = rx.recv() => {
                    match recv_result {
                        Ok(msg) => {
                            // Parse the collection name from the broadcast event so we can
                            // check whether this client's token covers it.
                            // Event shape: {"event":"change","collection":"<name>","key":"<key>","new_v":<v>}
                            let allowed = if let Ok(event) = serde_json::from_str::<serde_json::Value>(&msg) {
                                if let Some(collection) = event.get("collection").and_then(|v| v.as_str()) {
                                    claims.has_collection_access("read", collection)
                                } else {
                                    // Malformed event — skip it.
                                    false
                                }
                            } else {
                                false
                            };

                            if allowed {
                                if sender.send(Message::Text(Utf8Bytes::from(msg))).await.is_err() {
                                    break; // Client disconnected.
                                }
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            // The broadcast buffer overflowed — we missed n events.
                            // Log a warning but keep the connection alive.
                            warn!("⚠️  WebSocket send task lagged: {} events dropped for this client.", n);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            // Broadcast channel closed (server shutting down) — exit.
                            break;
                        }
                    }
                }
            }
        }
    });

    // Wait for either task to finish (client disconnect or server shutdown).
    tokio::select! {
        _ = (&mut recv_task) => send_task.abort(),
        _ = (&mut send_task) => recv_task.abort(),
    };
}
