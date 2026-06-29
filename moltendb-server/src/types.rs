// ─── types.rs ─────────────────────────────────────────────────────────────────
// Shared types used across the moltendb-server crate.
// ─────────────────────────────────────────────────────────────────────────────

use moltendb_auth as auth;
use moltendb_core::engine;

/// The Axum application state tuple injected into every handler via `State<AppState>`.
///
/// Fields (in order):
///   0. `engine::Db`       — database handle (cheap to clone, Arc-backed).
///   1. `auth::UserStore`  — in-memory user store for login verification.
///   2. `usize`            — max request body size in bytes.
///   3. `usize`            — max keys allowed per request.
///   4. `String`           — root username (used to guard admin-token minting).
///   5. `u64`              — root token TTL in seconds.
pub type AppState = (engine::Db, auth::UserStore, usize, usize, String, u64);
