// ─── constants.rs ─────────────────────────────────────────────────────────────
// Server-level constants — magic numbers and fixed strings used across the
// moltendb-server crate.
// ─────────────────────────────────────────────────────────────────────────────

// ─── Token TTL defaults ───────────────────────────────────────────────────────

/// Default TTL (in seconds) for scoped/delegated tokens minted via POST /auth/delegate.
/// 3 600 s = 1 hour.
pub const DEFAULT_DELEGATE_TTL_SECS: u64 = 3_600;

/// Default TTL (in seconds) used as the revocation prune deadline when no `exp`
/// field is supplied to DELETE /auth/tokens/:jti.
/// 86 400 s = 24 hours.
pub const DEFAULT_REVOKE_TTL_SECS: u64 = 86_400;

/// Default TTL (in seconds) for the root token issued by POST /auth/login.
/// 86 400 s = 24 hours. Overridable via --root-token-ttl / MOLTENDB_ROOT_TOKEN_TTL.
pub const DEFAULT_ROOT_TOKEN_TTL_SECS: u64 = 86_400;

// ─── Scope action strings ─────────────────────────────────────────────────────

/// Scope action for read operations.
pub const ACTION_READ: &str = "read";

/// Scope action for write (insert/upsert) operations.
pub const ACTION_WRITE: &str = "write";

/// Scope action for delete operations.
pub const ACTION_DELETE: &str = "delete";

/// The root/admin scope that grants full access to all collections and keys.
pub const ADMIN_SCOPE: &str = "*:*:*";

// ─── Background task intervals ────────────────────────────────────────────────

/// How often (in seconds) the revocation store is pruned and persisted to disk.
pub const REVOCATION_PRUNE_INTERVAL_SECS: u64 = 60;

/// How often (in seconds) the rate-limiter cleans up stale per-IP entries.
pub const RATE_LIMIT_CLEANUP_INTERVAL_SECS: u64 = 300;

/// Grace period (in seconds) given to in-flight requests during graceful shutdown.
pub const GRACEFUL_SHUTDOWN_TIMEOUT_SECS: u64 = 30;
