// ─── route_handlers.rs ────────────────────────────────────────────────────────
// HTTP route handlers — one function per API endpoint.
//
// Each handler is a thin async function that:
//   1. Extracts the request body (via `Json(payload)`).
//   2. Calls the corresponding `handlers::process_*` function.
//   3. Returns the result wrapped in `Json(...)` (serialized as JSON).
//
// `State((db, _))` destructures the app state tuple — `db` is the Db handle,
// `_` discards the UserStore (not needed in most handlers).
// ─────────────────────────────────────────────────────────────────────────────

use moltendb_auth as auth;
use moltendb_core::{engine, handlers};

use axum::{
    extract::{Extension, Path, Query as AxumQuery, State},
    http::StatusCode,
    Json,
};
use sysinfo::{Disks, System};
use serde_json::{json, Value};
use std::collections::HashMap as QueryMap;
// Duration and Instant are used in handle_revoke to compute the prune deadline.
use std::time::{Duration, Instant};

/// POST /auth/delegate — mint a scoped JWT for a client.
///
/// Strictly admin-only. The caller must present a valid admin token.
/// Accepts a JSON body with `client_id`, `scopes`, and an optional `ttl_secs`.
/// Returns a signed JWT containing exactly the requested scopes — the client
/// can use this token to access MoltenDB without ever seeing the root password.
///
/// Scope format: "action:collection:document_key"
/// Examples: "read:laptops:lp1", "write:users:*", "read:*:*", "*:*:*"
pub async fn handle_delegate(
    State((_, _, _, _, root_username)): State<(engine::Db, auth::UserStore, usize, usize, String)>,
    Extension(claims): axum::extract::Extension<auth::Claims>,
    Json(payload): Json<auth::DelegateRequest>,
) -> Result<Json<auth::DelegateResponse>, (StatusCode, Json<Value>)> {
    // Only root/admin tokens may mint new tokens.
    if !claims.is_admin() {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error": "Admin access required to delegate tokens"})),
        ));
    }

    // Only the root user may mint *:*:* (admin) tokens.
    if payload.scopes.iter().any(|s| s == "*:*:*") && claims.sub != root_username {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error": "Only the root user can mint '*:*:*' (admin) tokens"})),
        ));
    }

    // Validate that every scope is well-formed.
    for scope in &payload.scopes {
        if scope != "*:*:*" {
            let parts: Vec<&str> = scope.splitn(3, ':').collect();
            if parts.len() != 3 {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "error": format!("Invalid scope '{}'. Expected format: 'action:collection:key'", scope)
                    })),
                ));
            }
        }
    }

    let ttl = payload.ttl_secs.unwrap_or(3600);
    let (token, jti) = auth::create_scoped_token(&payload.client_id, payload.scopes.clone(), ttl)
        .map_err(|e| (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Token creation failed: {}", e)})),
        ))?;

    Ok(Json(auth::DelegateResponse {
        token,
        client_id: payload.client_id,
        scopes: payload.scopes,
        jti,
    }))
}

/// POST /login — authenticate and return a JWT token.
///
/// This is a public endpoint (no auth middleware).
/// Returns 200 + `{ "token": "..." }` on success.
/// Returns 401 Unauthorized if credentials are wrong.
/// Returns 500 Internal Server Error if token creation fails.
pub async fn handle_login(
    State((_, users, _, _, _)): State<(engine::Db, auth::UserStore, usize, usize, String)>,
    Json(payload): Json<auth::LoginRequest>,
) -> Result<Json<auth::LoginResponse>, (StatusCode, Json<Value>)> {
    // Verify the username and password against the in-memory user store.
    if users.verify_user(&payload.username, &payload.password) {
        // Credentials valid — create a signed JWT token for this user.
        match auth::create_token(&payload.username) {
            Ok(token) => Ok(Json(auth::LoginResponse { token })),
            Err(_) => Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "Failed to create token"})),
            )),
        }
    } else {
        // Wrong username or password.
        Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Invalid credentials"})),
        ))
    }
}

/// POST /set — insert or overwrite one or more documents.
///
/// Body: `{ "collection": "users", "data": { "u1": { "name": "Alice" } } }`
/// Requires: write:{collection}:* scope (or admin).
pub async fn handle_set(
    State((db, _, max_body_size, max_keys_per_request, _)): State<(engine::Db, auth::UserStore, usize, usize, String)>,
    Extension(claims): Extension<auth::Claims>,
    Json(payload): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let collection = payload.get("collection").and_then(|v| v.as_str()).unwrap_or("");
    if !claims.has_collection_access("write", collection) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": format!("Forbidden: token requires 'write:{}:*' scope", collection) })),
        );
    }
    let (code, body) = handlers::process_set(&db, &payload, max_body_size, max_keys_per_request);
    (StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), Json(body))
}

/// POST /update — merge new fields into existing documents (patch semantics).
///
/// Body: `{ "collection": "users", "data": { "u1": { "role": "admin" } } }`
/// Requires: write:{collection}:* scope (or admin).
pub async fn handle_update(
    State((db, _, max_body_size, max_keys_per_request, _)): State<(engine::Db, auth::UserStore, usize, usize, String)>,
    Extension(claims): Extension<auth::Claims>,
    Json(payload): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let collection = payload.get("collection").and_then(|v| v.as_str()).unwrap_or("");
    if !claims.has_collection_access("write", collection) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": format!("Forbidden: token requires 'write:{}:*' scope", collection) })),
        );
    }
    let (code, body) = handlers::process_update(&db, &payload, max_body_size, max_keys_per_request);
    (StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), Json(body))
}

/// POST /get — query documents with optional WHERE, fields, joins, count, offset.
///
/// Body: `{ "collection": "users", "where": { "role": "admin" }, "fields": ["name"] }`
///
/// Scope rules:
///   - `read:{collection}:*` (or `read:*:*` or `admin`) → full access, all docs returned.
///   - Document-level scopes (`read:{collection}:key1`, `read:{collection}:key2`, …):
///     - If `"keys"` is specified, all requested keys must be covered by the token.
///     - If no `"keys"` is specified, the result is filtered to only the docs the
///       token is allowed to read.
pub async fn handle_get(
    State((db, _, max_body_size, max_keys_per_request, _)): State<(engine::Db, auth::UserStore, usize, usize, String)>,
    Extension(claims): Extension<auth::Claims>,
    Json(payload): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let collection = payload.get("collection").and_then(|v| v.as_str()).unwrap_or("");

    // Fast path: collection-level (or broader) access — no filtering needed.
    if claims.has_collection_access("read", collection) {
        let (code, body) = handlers::process_get(&db, &payload, max_body_size, max_keys_per_request);
        return (StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), Json(body));
    }

    // Slow path: token only has document-level scopes.
    // Collect the explicit keys this token may read in this collection.
    let allowed_keys: Vec<String> = claims.allowed_keys("read", collection);
    if allowed_keys.is_empty() {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": format!("Forbidden: token has no read access to collection '{}'", collection) })),
        );
    }

    // If the caller specified explicit keys, verify every one is allowed.
    if let Some(keys_val) = payload.get("keys") {
        let requested: Vec<String> = match keys_val {
            Value::String(s) => vec![s.clone()],
            Value::Array(arr) => arr.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
            _ => vec![],
        };
        for k in &requested {
            if !claims.has_access("read", collection, k) {
                return (
                    StatusCode::FORBIDDEN,
                    Json(json!({ "error": format!("Forbidden: token lacks 'read:{}:{}' scope", collection, k) })),
                );
            }
        }
        // All requested keys are allowed — run the query as-is.
        let (code, body) = handlers::process_get(&db, &payload, max_body_size, max_keys_per_request);
        return (StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), Json(body));
    }

    // No keys specified.
    // If the token has prefix-wildcard scopes (e.g. "read:laptops:store_A_*"), inject
    // the prefixes into the payload so the core engine can gate keys before the expensive
    // AST evaluator runs (Prefix Gatekeeper). Otherwise pre-scope to exact allowed keys.
    if claims.has_prefix_wildcard("read", collection) {
        let prefixes = claims.extract_prefixes("read", collection);
        // Strip any client-supplied _allowed_prefixes first (never trust client input).
        let mut scoped_payload = payload.clone();
        scoped_payload.as_object_mut().map(|o| o.remove("_allowed_prefixes"));
        if !prefixes.is_empty() {
            scoped_payload["_allowed_prefixes"] = json!(prefixes);
        }
        let (code, body) = handlers::process_get(&db, &scoped_payload, max_body_size, max_keys_per_request);
        (StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), Json(body))
    } else {
        let mut scoped_payload = payload.clone();
        scoped_payload["keys"] = json!(allowed_keys);
        let (code, body) = handlers::process_get(&db, &scoped_payload, max_body_size, max_keys_per_request);
        (StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), Json(body))
    }
}

/// POST /delete — delete one key, multiple keys, or an entire collection.
///
/// Body (single):   `{ "collection": "users", "keys": "u1" }`
/// Body (batch):    `{ "collection": "users", "keys": ["u1", "u2"] }`
/// Body (drop all): `{ "collection": "users", "drop": true }`
/// Requires: delete:{collection}:* scope (or admin).
pub async fn handle_delete(
    State((db, _, max_body_size, max_keys_per_request, _)): State<(engine::Db, auth::UserStore, usize, usize, String)>,
    Extension(claims): Extension<auth::Claims>,
    Json(payload): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let collection = payload.get("collection").and_then(|v| v.as_str()).unwrap_or("");
    if !claims.has_collection_access("delete", collection) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": format!("Forbidden: token requires 'delete:{}:*' scope", collection) })),
        );
    }
    let (code, body) = handlers::process_delete(&db, &payload, max_body_size, max_keys_per_request);
    (StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), Json(body))
}

#[cfg(feature = "schema")]
pub async fn handle_schema(
    State((db, _, max_body_size, max_keys_per_request, _)): State<(engine::Db, auth::UserStore, usize, usize, String)>,
    Json(payload): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let (code, body) = handlers::process_schema(&db, &payload, max_body_size, max_keys_per_request);
    (StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), Json(body))
}

/// POST /snapshot — take a snapshot of the database on demand.
/// Requires: admin scope.
pub async fn handle_snapshot(
    State((db, _, _, _, _)): State<(engine::Db, auth::UserStore, usize, usize, String)>,
    Extension(claims): Extension<auth::Claims>,
) -> (StatusCode, Json<Value>) {
    if !claims.is_admin() {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "Forbidden: snapshot requires admin scope" })),
        );
    }
    let (code, body) = handlers::process_snapshot(&db);
    (StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), Json(body))
}

/// GET /collections/{collection}/docs/{key} — fetch a single document by key.
///
/// RESTful convenience endpoint. Equivalent to:
///   POST /get { "collection": collection, "keys": key }
/// Requires: read:{collection}:{key} scope (or read:{collection}:* or read:*:* or admin).
pub async fn handle_rest_get(
    State((db, _, max_body_size, max_keys_per_request, _)): State<(engine::Db, auth::UserStore, usize, usize, String)>,
    Extension(claims): Extension<auth::Claims>,
    Path((collection, key)): Path<(String, String)>,
) -> (StatusCode, Json<Value>) {
    if !claims.has_access("read", &collection, &key) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": format!("Forbidden: token lacks 'read:{}:{}' scope", collection, key) })),
        );
    }
    let payload = json!({
        "collection": collection,
        "keys": key
    });
    let (code, body) = handlers::process_get(&db, &payload, max_body_size, max_keys_per_request);
    (StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), Json(body))
}

/// GET /collections/{collection}?limit=N&offset=M — fetch all documents (paginated).
///
/// Used by `_syncFromServer()` in analytics-client.js on page load to seed
/// the local WASM DB with the server's current state.
///
/// Query params:
///   - `limit`  (optional) — maximum number of documents to return.
///   - `offset` (optional) — number of documents to skip before returning.
///
/// Requires: read:{collection}:* scope (or admin).
pub async fn handle_rest_get_collection(
    State((db, _, max_body_size, max_keys_per_request, _)): State<(engine::Db, auth::UserStore, usize, usize, String)>,
    Extension(claims): Extension<auth::Claims>,
    Path(collection): Path<String>,
    AxumQuery(params): AxumQuery<QueryMap<String, String>>,
) -> (StatusCode, Json<Value>) {
    if !claims.has_collection_access("read", &collection) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": format!("Forbidden: token requires 'read:{}:*' scope", collection) })),
        );
    }
    let mut payload = json!({ "collection": collection });
    if let Some(limit) = params.get("limit").and_then(|v| v.parse::<u64>().ok()) {
        payload["count"] = json!(limit);
    }
    if let Some(offset) = params.get("offset").and_then(|v| v.parse::<u64>().ok()) {
        payload["offset"] = json!(offset);
    }
    let (code, body) = handlers::process_get(&db, &payload, max_body_size, max_keys_per_request);
    (StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), Json(body))
}

/// DELETE /auth/tokens/:jti — revoke a JWT by its unique token ID.
///
/// Only admin-scoped tokens may call this endpoint.
/// Once revoked, the token is rejected by auth_middleware on every subsequent
/// request, even if it has not yet expired.
///
/// Request body (JSON):
///   { "exp": <unix_timestamp> }   — the token's expiry (used to set the prune deadline)
///
/// Returns 200 on success, 403 if the caller lacks admin privileges.
pub async fn handle_revoke(
    Extension(claims): Extension<auth::Claims>,
    Extension(revocation_store): Extension<auth::RevocationStore>,
    Extension(revocations_path): Extension<auth::RevocationsPath>,
    Path(jti): Path<String>,
    Json(payload): Json<Value>,
) -> (StatusCode, Json<Value>) {
    // Only root/admin tokens may revoke tokens.
    if !claims.is_admin() {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "Admin access required to revoke tokens"})),
        );
    }

    if jti.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "jti must not be empty"})),
        );
    }

    // Compute the prune deadline from the caller-supplied `exp` field.
    // If not provided, default to 24 hours from now (safe upper bound).
    let prune_after = if let Some(exp_secs) = payload.get("exp").and_then(|v| v.as_u64()) {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let remaining = exp_secs.saturating_sub(now_secs);
        Instant::now() + Duration::from_secs(remaining)
    } else {
        Instant::now() + Duration::from_secs(86400)
    };

    revocation_store.revoke(&jti, prune_after);
    // Persist immediately so the revocation survives a server restart.
    revocation_store.save_to_file(&revocations_path.0);

    (
        StatusCode::OK,
        Json(json!({"revoked": jti, "message": "Token has been revoked successfully"})),
    )
}


/// GET /system/health — public liveness check.
///
/// Returns 200 OK with a simple JSON payload. No authentication required.
pub async fn handle_health() -> (StatusCode, Json<Value>) {
    (StatusCode::OK, Json(json!({ "status": "ok", "message": "MoltenDB is running" })))
}

/// GET /system/metrics — resource usage snapshot.
///
/// Admin-only. Returns uptime, process memory, host RAM/disk, and database internals.
pub async fn handle_metrics(
    State((db, _, _, _, _)): State<(moltendb_core::engine::Db, auth::UserStore, usize, usize, String)>,
    Extension(claims): Extension<auth::Claims>,
) -> (StatusCode, Json<Value>) {
    if !claims.is_admin() {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "Admin access required" })),
        );
    }

    let uptime_seconds = db.started_at.elapsed().as_secs();

    let mut sys = System::new_all();
    sys.refresh_all();

    let total_ram = sys.total_memory();
    let used_ram  = sys.used_memory();
    let free_ram  = sys.free_memory();

    let pid = sysinfo::get_current_pid().ok();
    let process_memory_bytes = pid
        .and_then(|p| sys.process(p))
        .map(|p| p.memory())
        .unwrap_or(0);

    let disks = Disks::new_with_refreshed_list();
    let disk_info: Vec<Value> = disks.iter().map(|d| {
        let total = d.total_space();
        let avail = d.available_space();
        let used  = total.saturating_sub(avail);
        json!({
            "mount":           d.mount_point().to_string_lossy(),
            "total_bytes":     total,
            "used_bytes":      used,
            "available_bytes": avail,
        })
    }).collect();

    // Database internals
    let hot_keys_count: usize = db.hot_keys_count();
    let wal_size_bytes = db.storage.get_size().unwrap_or(0);
    let storage_mode = "tiered";

    (StatusCode::OK, Json(json!({
        "uptime_seconds": uptime_seconds,
        "process": {
            "memory_used_bytes": process_memory_bytes,
        },
        "host": {
            "memory": {
                "total_bytes": total_ram,
                "used_bytes":  used_ram,
                "free_bytes":  free_ram,
            },
            "disks": disk_info,
        },
        "database": {
            "hot_keys_count":    hot_keys_count,
            "hot_tier_threshold": db.hot_threshold,
            "wal_size_bytes":    wal_size_bytes,
            "storage_mode":      storage_mode,
        },
    })))
}
