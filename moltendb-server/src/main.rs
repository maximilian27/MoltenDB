#![deny(warnings)]
// ─── main.rs ──────────────────────────────────────────────────────────────────
// This is the server entry point. It starts the HTTP/WebSocket server,
// wires up all routes, middleware, and background tasks, then listens for
// incoming connections.
//
// Responsibilities:
//   1. Read configuration from environment variables.
//   2. Open the database (with optional encryption).
//   3. Spawn background tasks (compaction, rate-limit cleanup).
//   4. Build the Axum router with all routes and middleware layers.
//   5. Start the TLS server and block until shutdown.
//   6. On shutdown: drain in-flight requests, flush the DB, exit cleanly.
//
// This file only compiles for native (non-WASM) targets.
// ─────────────────────────────────────────────────────────────────────────────

// This attribute prevents the file from being compiled when targeting WASM.
// The server uses OS networking and file I/O that don't exist in the browser.
#![cfg(not(target_arch = "wasm32"))]

// Declare the modules that make up the server.
// Each `mod X` tells Rust to look for src/X.rs and compile it as part of this crate.
use moltendb_auth as auth; // JWT authentication, user store, auth middleware
mod rate_limit;       // Per-IP sliding-window rate limiter
mod route_handlers;   // HTTP route handlers (one per API endpoint)
mod server;           // TLS config loading and graceful shutdown signal
mod ws;               // WebSocket upgrade handler and per-connection logic

// Re-export handlers into scope for use in the router below.
use route_handlers::{
    handle_delegate, handle_delete, handle_get, handle_health, handle_login, handle_metrics,
    handle_rest_get, handle_rest_get_collection, handle_revoke, handle_set, handle_snapshot,
    handle_update,
};
use ws::ws_handler;

// Core engine — imported from the moltendb-core crate
use moltendb_core::engine::{self, StorageBackend};

use axum::{
    http::{HeaderValue, header},
    // middleware = lets us insert async functions between the router and handlers.
    middleware,
    // routing = defines which HTTP methods map to which handlers.
    routing::{delete, get, post},
    Extension,
    Router,
};
// RustlsConfig = TLS configuration loaded from PEM certificate and key files.
use std::net::SocketAddr;
use std::sync::Arc;
use axum::extract::DefaultBodyLimit;
// RequestBodyLimitLayer = middleware that rejects request bodies exceeding a size limit.
use tower_http::limit::RequestBodyLimitLayer;
// SetResponseHeaderLayer = middleware that adds a fixed header to every response.
use tower_http::set_header::SetResponseHeaderLayer;
// CorsLayer = middleware that adds CORS headers to responses.
// Any = a CORS policy that allows any origin (⚠️ not suitable for production).
use tower_http::cors::{AllowOrigin, Any, CorsLayer};
use tracing::{error, info, warn};

use clap::Parser;

/// MoltenDB — a local-first embedded database server.
///
/// All options can also be set via environment variables (shown in brackets).
/// CLI flags take priority over environment variables.
#[derive(Parser, Debug)]
#[command(name = "moltendb", version, about)]
struct Config {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Host address to bind to [env: MOLTENDB_HOST]
    /// Use "0.0.0.0" to accept connections from any interface (required for Docker).
    /// Use "127.0.0.1" to restrict to localhost only.
    #[arg(long, default_value = "0.0.0.0", env = "MOLTENDB_HOST")]
    host: String,

    /// Port to listen on [env: MOLTENDB_PORT]
    #[arg(long, default_value = "1538", env = "MOLTENDB_PORT")]
    port: u16,

    /// Path to the database log file [env: MOLTENDB_DB_PATH]
    #[arg(long, default_value = "my_database.log", env = "MOLTENDB_DB_PATH")]
    db_path: String,

    /// Path to the TLS certificate PEM file [env: MOLTENDB_TLS_CERT]
    #[arg(long, default_value = "cert.pem", env = "MOLTENDB_TLS_CERT")]
    cert: String,

    /// Path to the TLS private key PEM file [env: MOLTENDB_TLS_KEY]
    #[arg(long, default_value = "key.pem", env = "MOLTENDB_TLS_KEY")]
    key: String,

    /// Encryption password for at-rest encryption [env: MOLTENDB_ENCRYPTION_KEY]
    #[arg(long, env = "MOLTENDB_ENCRYPTION_KEY")]
    encryption_key: Option<String>,

    /// Write mode: "async" (high throughput) or "sync" (zero data loss) [env: MOLTENDB_WRITE_MODE]
    #[arg(long, default_value = "async", env = "MOLTENDB_WRITE_MODE")]
    write_mode: String,

    /// Maximum requests per IP per rate-limit window [env: MOLTENDB_RATE_LIMIT_REQS]
    #[arg(long, default_value = "100", env = "MOLTENDB_RATE_LIMIT_REQS")]
    rate_limit_requests: u32,

    /// Rate limit sliding window size in seconds [env: MOLTENDB_RATE_LIMIT_WINDOW]
    #[arg(long, default_value = "60", env = "MOLTENDB_RATE_LIMIT_WINDOW")]
    rate_limit_window: u64,

    /// JWT signing secret [env: MOLTENDB_JWT_SECRET]
    #[arg(long, env = "MOLTENDB_JWT_SECRET")]
    jwt_secret: Option<String>,
    // Note: jwt_secret is Option<String> so we can detect if it's unset and refuse to start.

    /// Root username [env: MOLTENDB_ROOT_USER]
    #[arg(long, env = "MOLTENDB_ROOT_USER")]
    root_user: Option<String>,

    /// Root password [env: MOLTENDB_ROOT_PASSWORD]
    #[arg(long, env = "MOLTENDB_ROOT_PASSWORD")]
    root_password: Option<String>,

    /// Maximum request body size in bytes. Requests exceeding this are rejected at the HTTP layer. [env: MOLTENDB_MAX_BODY_SIZE]
    #[arg(long, default_value = "10485760", env = "MOLTENDB_MAX_BODY_SIZE")]
    max_body_size: usize,

    /// Maximum keys allowed per request. [env: MOLTENDB_MAX_KEYS_PER_REQUEST]
    #[arg(long, default_value = "1000", env = "MOLTENDB_MAX_KEYS_PER_REQUEST")]
    max_keys_per_request: usize,

    /// Allowed CORS origin(s). Use "*" to allow any origin (default, dev only).
    /// For production, set to your frontend URL, e.g. "https://app.example.com".
    /// Multiple origins can be separated by commas. [env: MOLTENDB_CORS_ORIGIN]
    #[arg(long, default_value = "*", env = "MOLTENDB_CORS_ORIGIN")]
    cors_origin: String,

    /// Disable at-rest encryption (data stored as plain JSON). NOT recommended for production. [env: MOLTENDB_DISABLE_ENCRYPTION]
    #[arg(long, default_value = "false", env = "MOLTENDB_DISABLE_ENCRYPTION")]
    disable_encryption: bool,

    /// Enable verbose debug logging (optimizer, indexing, compaction). [env: MOLTENDB_DEBUG]
    #[arg(long, default_value = "false", env = "MOLTENDB_DEBUG")]
    debug: bool,

    /// Run over plain HTTP and WS instead of HTTPS/WSS. Ignores --cert and --key.
    /// ⚠️  NEVER use in production — all traffic is unencrypted. [env: MOLTENDB_DEV_MODE]
    #[arg(long, default_value = "false", env = "MOLTENDB_DEV_MODE")]
    dev_mode: bool,

    /// Path to a script file to execute after a successful backup.
    /// The script will be called with the absolute path of the snapshot as its first argument. [env: MOLTENDB_POST_BACKUP_SCRIPT]
    #[arg(long, env = "MOLTENDB_POST_BACKUP_SCRIPT")]
    pub post_backup_script: Option<String>,

    /// Maximum documents per collection to keep in RAM (Hot threshold). [env: MOLTENDB_HOT_THRESHOLD]
    /// If a collection exceeds this, older documents are moved to the Cold tier (disk).
    /// Higher values use more RAM but provide sub-microsecond speeds for more documents.
    #[arg(long, default_value = "50000", env = "MOLTENDB_HOT_THRESHOLD")]
    hot_threshold: usize,

    /// Run entirely in RAM — bypass the WAL and disk storage completely.
    /// All data is lost when the server exits. Ideal for ephemeral caches,
    /// CI environments, or Redis-like use cases. [env: MOLTENDB_IN_MEMORY]
    #[arg(long, default_value = "false", env = "MOLTENDB_IN_MEMORY")]
    in_memory: bool,
}

#[derive(clap::Subcommand, Debug)]
enum Commands {
    /// Start the MoltenDB server (default)
    Serve,
    /// Point-in-Time Recovery: Recover a database to a specific time or sequence
    Recover {
        /// Path to the source database log file
        #[arg(long)]
        log: String,
        
        /// Optional path to an older snapshot to start from (faster)
        #[arg(long)]
        snapshot: Option<String>,
        
        /// Target timestamp (Unix ms) to recover to
        #[arg(long)]
        to_time: Option<u64>,
        
        /// Target sequence number (log line count) to recover to
        #[arg(long)]
        to_seq: Option<u64>,
        
        /// Output path for the recovered snapshot file
        #[arg(long)]
        out: String,
        
        /// Encryption password if the log is encrypted
        #[arg(long, env = "ENCRYPTION_KEY")]
        encryption_key: Option<String>,
    },
}

// ─── main ─────────────────────────────────────────────────────────────────────

/// Server entry point.
///
/// `#[tokio::main]` transforms this async fn into a synchronous main() that
/// starts the Tokio async runtime and runs this function inside it.
/// Tokio is the async runtime — it manages the thread pool and schedules tasks.
#[tokio::main]
async fn main() {
    // `Config::parse()` reads CLI flags first, then falls back to environment
    // variables for any flag not provided. This is handled automatically by
    // clap's `env` feature — no manual `std::env::var()` calls needed.
    // If a required flag is missing and has no default, clap prints an error
    // and exits the process before this line even runs.
    let cfg = Config::parse();

        if let Some(Commands::Recover { log, snapshot: _, to_time, to_seq, out, encryption_key }) = &cfg.command {
        // Recovery Mode
        tracing_subscriber::fmt().init();
        info!("🕒 MoltenDB Point-in-Time Recovery Tool");
        info!("📖 Reading log: {}", log);

        let password = encryption_key.clone().unwrap_or_else(|| "default_molten_password".to_string());
        let master_key = engine::EncryptedStorage::derive_key(&password, "moltendb_log_salt");

        // Open storage
        let base_storage = Arc::new(engine::SyncDiskStorage::new(log).expect("Failed to open log file"));
        let storage: Arc<dyn engine::StorageBackend> = Arc::new(engine::EncryptedStorage::new(base_storage, &master_key));

        match engine::Db::recover_to(&*storage, *to_time, *to_seq) {
            Ok(entries) => {
                info!("✅ Recovered {} entries.", entries.len());
                // For recovery, we set seq = to_seq if provided, or 0 (it will be ignored by out-of-band loading anyway)
                // Actually, the snapshot we produce should probably have a seq that reflects the log state.
                // But for a 'recovered' snapshot, it's a fresh ground truth.
                
                // We reuse the disk-level write_snapshot if possible, but it's crate-private.
                // However, engine::Db::compact uses storage.compact(entries).
                // Let's use a temporary DB to write the snapshot.
                
                // For PITR, we want to write a snapshot file at `out`.
                // MoltenDB snapshots are normally `{log_path}.snapshot.bin`.
                // We can just use the provided `out` path directly.
                
                // Since write_snapshot is private to moltendb-core::engine::storage::disk,
                // we might need to expose a way to write a snapshot from the engine.
                // Or just use the recovered entries to create a new log and then compact it.
                
                info!("💾 Saving recovered state to: {}", out);
                
                // Create a temporary log file for the recovered state
                let temp_log = format!("{}.log", out);
                {
                    let recovered_storage = engine::SyncDiskStorage::new(&temp_log).expect("Failed to create recovery log");
                    for entry in &entries {
                        recovered_storage.write_entry(entry).expect("Failed to write entry to recovery log");
                    }
                    // Now compact it to produce the snapshot
                    recovered_storage.compact(entries).expect("Failed to compact recovery log");
                }
                
                // The snapshot is now at temp_log.snapshot.bin
                let snapshot_path = format!("{}.snapshot.bin", temp_log);
                std::fs::rename(snapshot_path, out).expect("Failed to move snapshot to output path");
                std::fs::remove_file(temp_log).ok();
                
                info!("✨ Recovery complete! You can now use {} as your database snapshot.", out);
            }
            Err(e) => {
                error!("❌ Recovery failed: {}", e);
                std::process::exit(1);
            }
        }
        return;
    }

    // Set up structured logging (tracing).
    // `tracing_subscriber::fmt()` configures a human-readable log formatter
    // that prints to stdout with timestamps, log levels, and source locations.
    // `with_env_filter` reads the RUST_LOG environment variable to control
    // verbosity at runtime (e.g. RUST_LOG=debug shows all log levels).
    // `.add_directive("info".parse().unwrap())` sets the default level to INFO
    // so that INFO, WARN, and ERROR messages are shown even if RUST_LOG is unset.
    // `.init()` installs this as the global tracing subscriber — must be called
    // before any `info!()`, `warn!()`, or `error!()` macros are used.
    let log_level = if cfg.debug { "debug" } else { "info" };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(log_level.parse().unwrap()),
        )
        .init();

    // Security warnings — printed at startup so they're impossible to miss in logs.
    // These warn about insecure defaults that are fine for local development
    // but must be overridden before deploying to production.

    // JWT_SECRET is used to sign authentication tokens. If it's not set, the
    // server refuses to start — a missing secret would fall back to a hardcoded
    // string that is publicly known, allowing anyone to forge valid tokens.
    if cfg.jwt_secret.is_none() {
        error!("🔥 CRITICAL: --jwt-secret (JWT_SECRET) not set! This is required for security.");
        std::process::exit(1);
    }

    // ENCRYPTION_KEY is used to derive the at-rest encryption key for the database
    // log file. If not set, a built-in default password is used — the database
    // is still encrypted, but with a key that anyone who reads this source code
    // could reproduce. Set a strong unique password in production.
    if cfg.encryption_key.is_none() {
        warn!("⚠️  --encryption-key not set — using built-in default key. Set it for production!");
    }

    // MOLTENDB_ROOT_USER and MOLTENDB_ROOT_PASSWORD are required for the built-in root account.
    // If not set, we stop the app for security reasons.
    if cfg.root_user.is_none() {
        error!("🔥 CRITICAL: --root-user (MOLTENDB_ROOT_USER) not set! This is required for security.");
        std::process::exit(1);
    }

    if cfg.root_password.is_none() {
        error!("🔥 CRITICAL: --root-password (MOLTENDB_ROOT_PASSWORD) not set! This is required for security.");
        std::process::exit(1);
    }

    let root_user = cfg.root_user.unwrap();
    let root_password = cfg.root_password.unwrap();

    // Unpack the parsed config into local variables.
    // These are used throughout the rest of main() to configure the server.
    // `cfg.db_path` — path to the database log file on disk (e.g. "my_database.log").
    let db_path = cfg.db_path;
    // `cfg.host` — IP address to bind to (e.g. "0.0.0.0" or "127.0.0.1").
    let host = cfg.host;
    // `cfg.port` — TCP port to listen on (e.g. 1538). Already parsed to u16 by clap.
    let port = cfg.port;
    // `cfg.cert` / `cfg.key` — paths to the TLS certificate and private key PEM files.
    // These are required for HTTPS. Generate them with openssl or use Let's Encrypt.
    let cert_path = cfg.cert;
    let key_path = cfg.key;
    // Rate limiting parameters — passed to RateLimiter::new() below.
    let rate_limit_requests = cfg.rate_limit_requests;
    let rate_limit_window = cfg.rate_limit_window;

    // Determine the write mode from the --write-mode flag (or WRITE_MODE env var).
    // "sync" = every write blocks until the OS confirms the data is on disk (zero
    //          data loss on crash, lower throughput).
    // anything else = async mode (writes buffered in memory, flushed every 50ms,
    //                 up to 50ms of data loss on crash, much higher throughput).
    let is_sync_mode = cfg.write_mode.to_lowercase() == "sync";

    let is_in_memory = cfg.in_memory;

    // ── Encryption key setup ──────────────────────────────────────────────────
    //
    // This two-variable pattern is required by Rust's borrow checker:
    //
    //   `encryption_key_storage` — owns the [u8; 32] key bytes. It must live
    //   long enough for the reference to remain valid throughout main().
    //
    //   `encryption_key` — a reference (&[u8; 32]) to those bytes, passed into
    //   Db::open(). The reference cannot outlive the owned value, so both must
    //   be declared in the same scope.
    //
    // If we wrote `let key = Some(derive_key(...)); Db::open(db_config)`
    // in a single expression, and db_config used key.as_ref(), the temporary 
    // `key` would be dropped before Db::open() could use the reference.
    // However, since DbConfig now takes an owned Option<[u8; 32]>, this 
    // specific borrow checker issue is simplified.
    let encryption_key_storage;
    let encryption_key: Option<&[u8; 32]> = if cfg.disable_encryption {
        warn!("⚠️  Encryption is DISABLED — data will be stored as plain JSON!");
        None
    } else {
        let password = cfg.encryption_key
            .unwrap_or_else(|| "moltendb-default-encryption-key".to_string());
        let key = engine::EncryptedStorage::derive_key(&password, &db_path);
        encryption_key_storage = Some(key);
        encryption_key_storage.as_ref()
    };

    // Open the database. This:
    //   1. Creates or opens the log file at db_path.
    //   2. Wraps it in EncryptedStorage if encryption_key is Some.
    //   3. Streams the log file line-by-line, replaying entries into RAM.
    //   4. Returns a Db handle (cheap to clone — it's Arc-backed internally).
    let db_config = engine::DbConfig {
        path: db_path.clone(),
        sync_mode: is_sync_mode,
        tiered_mode: true,
        rate_limit_requests: Some(rate_limit_requests),
        rate_limit_window: Some(rate_limit_window),
        max_body_size: cfg.max_body_size,
        max_keys_per_request: cfg.max_keys_per_request,
        encryption_key: encryption_key.cloned(),
        post_backup_script: cfg.post_backup_script,
        in_memory: cfg.in_memory,
    };

    let db = match engine::Db::open(db_config) {
        Ok(database) => database,
        Err(e) => {
            error!("🔥 CRITICAL: Failed to start MoltenDB! Details: {}", e);
            std::process::exit(1);
        }
    };

    if is_in_memory {
        warn!("⚡ IN-MEMORY MODE — all data is stored in RAM only. Nothing will be persisted to disk. Data will be lost on exit.");
    }

    // Spawn a background task for log compaction (skipped in --in-memory mode — there is no log to compact).
    // `db.clone()` is cheap — Db is Arc-backed, so this just increments a counter.
    // `tokio::spawn` runs the async block concurrently with the main server task.
    if !is_in_memory {
    let bg_db = db.clone();
    let bg_db_path = db_path.clone();
    tokio::spawn(async move {
        // Check every 60 seconds whether compaction is needed.
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        let max_log_bytes: u64 = 100 * 1024 * 1024; // 100 MB threshold
        let mut secs_since_compact: u64 = 0;
        loop {
            // `tick().await` waits until the next 60-second interval fires.
            interval.tick().await;
            secs_since_compact += 60;

            // Check the current log file size using OS metadata.
            // `.map(|m| m.len()).unwrap_or(0)` returns 0 if the file doesn't exist.
            let log_size = std::fs::metadata(&bg_db_path)
                .map(|m| m.len())
                .unwrap_or(0);

            // Compact if the log exceeds 100 MB OR if an hour has passed.
            // This prevents both unbounded file growth and stale data accumulation.
            let should_compact = log_size >= max_log_bytes || secs_since_compact >= 3600;
            if should_compact {
                if let Err(e) = bg_db.compact() {
                    warn!("⚠️ Background compaction failed: {}", e);
                } else {
                    info!("🗜️  Compaction complete (log was {} MB)", log_size / 1024 / 1024);
                }
                secs_since_compact = 0;
            }
        }
    });
    } // end if !is_in_memory (compaction task)

    // Initialize the user store with the admin user and password from Config.
    // We've already verified they are present above.
    let users = auth::UserStore::new(root_user.clone(), root_password);
    info!("👤 User authentication initialized");

    // Derive the revocation store file path from the database path.
    // e.g. "my_database.log" → "my_database.revocations.json"
    let revocations_path = {
        let base = std::path::Path::new(&db_path);
        let stem = base.file_stem().and_then(|s| s.to_str()).unwrap_or("my_database");
        let dir = base.parent().and_then(|p| p.to_str()).filter(|s| !s.is_empty()).unwrap_or(".");
        format!("{}/{}.revocations.json", dir, stem)
    };

    // Initialize the token revocation store, loading any previously revoked JTIs
    // from disk so revocations survive server restarts.
    let revocation_store = auth::RevocationStore::load_from_file(&revocations_path);
    info!("🔒 Revocation store loaded from '{}'", revocations_path);

    // Spawn a background task to prune expired revocation entries every 60 seconds
    // and persist the updated store to disk so the file stays clean.
    // In --in-memory mode we still prune in RAM but skip the disk save.
    let prune_store = revocation_store.clone();
    let prune_revocations_path = revocations_path.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            prune_store.prune();
            if !is_in_memory {
                prune_store.save_to_file(&prune_revocations_path);
            }
        }
    });
    info!("🔒 Token revocation store initialized");

    // Initialize the rate limiter with the configured limits.
    let rate_limiter = rate_limit::RateLimiter::new(rate_limit_requests as usize, rate_limit_window);
    info!("🚦 Rate limiting: {} requests per {} seconds", rate_limit_requests, rate_limit_window);

    // Spawn a background task to periodically clean up stale rate-limit entries.
    // Without this, the rate limiter's DashMap would grow forever as new IPs connect.
    let cleanup_limiter = rate_limiter.clone();
    tokio::spawn(async move {
        // Run cleanup every 5 minutes (300 seconds).
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
        loop {
            interval.tick().await;
            cleanup_limiter.cleanup();
        }
    });

    // The app state is a tuple of (Db, UserStore, max_body_size) injected into every handler via State<...>.
    // Axum clones this for each request — Db and UserStore are cheap to clone (Arc-backed).
    let app_state = (db.clone(), users, cfg.max_body_size, cfg.max_keys_per_request, root_user);

    let mut protected_routes = Router::new()
        .route("/set", post(handle_set))           // Insert/upsert documents
        .route("/update", post(handle_update))     // Patch/merge documents
        .route("/delete", post(handle_delete))     // Delete documents or drop collection
        .route("/snapshot", post(handle_snapshot))   // Take a snapshot on demand
        .route("/get", post(handle_get))           // Query documents (with WHERE, fields, joins, etc.)
        .route("/collections/{collection}", get(handle_rest_get_collection))       // GET all docs (paginated)
        .route("/collections/{collection}/docs/{key}", get(handle_rest_get))       // GET single doc
        .route("/auth/delegate", post(handle_delegate))                            // Mint scoped tokens (admin only)
        .route("/auth/tokens/{jti}", delete(handle_revoke))                        // Revoke a token by JTI (admin only)
        .route("/system/metrics", get(handle_metrics));                            // Resource usage — admin only

    #[cfg(feature = "schema")]
    {
        use route_handlers::handle_schema;
        protected_routes = protected_routes.route("/schema", post(handle_schema));
    }

    let protected_routes = protected_routes
        // Apply the auth middleware first (innermost layer — runs after extensions are set).
        // `from_fn` wraps an async function as an Axum middleware layer.
        .layer(middleware::from_fn(auth::auth_middleware))
        // Inject the RevocationStore as an extension — must wrap auth_middleware so it is
        // available in request.extensions() when auth_middleware executes.
        .layer(Extension(revocation_store.clone()))
        // Inject the revocations file path so handle_revoke can persist after revoking.
        .layer(Extension(auth::RevocationsPath(revocations_path)));

    // Public routes are accessible without authentication.
    let public_routes = Router::new()
        .route("/login", post(handle_login))          // Returns a JWT token on valid credentials
        .route("/ws", get(ws_handler))                // WebSocket upgrade endpoint
        .route("/system/health", get(handle_health))  // Liveness check — no auth required
        // Inject the RevocationStore so ws_handler can reject revoked tokens.
        .layer(Extension(revocation_store));

    // CORS layer — configured via --cors-origin / CORS_ORIGIN.
    // Defaults to "*" (any origin) for development convenience.
    // In production, set CORS_ORIGIN to your frontend URL, e.g. "https://app.example.com".
    // Multiple origins can be comma-separated: "https://a.com,https://b.com".
    let cors = {
        let origin_str = cfg.cors_origin.trim().to_string();
        if origin_str == "*" {
            if !cfg.debug {
                warn!("⚠️  CORS is open to any origin ('*'). Set --cors-origin for production!");
            }
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any)
        } else {
            let origins: Vec<HeaderValue> = origin_str
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .filter_map(|s| s.parse::<HeaderValue>().ok())
                .collect();
            if origins.is_empty() {
                error!("🔥 CRITICAL: --cors-origin value '{}' produced no valid origins.", origin_str);
                std::process::exit(1);
            }
            info!("🔒 CORS restricted to: {}", origin_str);
            CorsLayer::new()
                .allow_origin(AllowOrigin::list(origins))
                .allow_methods(Any)
                .allow_headers(Any)
        }
    };

    // Build the final application by merging routes and stacking middleware layers.
    // Layers are applied bottom-up: the last `.layer(...)` call wraps the outermost layer.
    // Request flow: rate_limit → security_headers → cors → auth (protected only) → handler
    let app = public_routes
        .merge(protected_routes)
        .layer(cors)
        // Security headers — added to every response to protect against common attacks.
        // X-Content-Type-Options: prevents MIME-type sniffing.
        .layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        // X-Frame-Options: prevents clickjacking by disallowing iframes.
        .layer(SetResponseHeaderLayer::overriding(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        // X-XSS-Protection: enables the browser's built-in XSS filter.
        .layer(SetResponseHeaderLayer::overriding(
            header::X_XSS_PROTECTION,
            HeaderValue::from_static("1; mode=block"),
        ))
        // Strict-Transport-Security: forces HTTPS for 1 year (HSTS).
        .layer(SetResponseHeaderLayer::overriding(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        ))
        // Referrer-Policy: prevents the browser from sending the Referer header.
        .layer(SetResponseHeaderLayer::overriding(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
        // Permissions-Policy: disables access to sensitive browser APIs.
        .layer(SetResponseHeaderLayer::overriding(
            header::HeaderName::from_static("permissions-policy"),
            HeaderValue::from_static("geolocation=(), microphone=(), camera=()"),
        ))
        // Content-Security-Policy: restricts which resources the page can load.
        .layer(SetResponseHeaderLayer::overriding(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("default-src 'self'; script-src 'self'; object-src 'none'"),
        ))
        // Disable Axum's built-in 2 MB default body limit so that
        // RequestBodyLimitLayer below is the sole enforcer.
        .layer(DefaultBodyLimit::disable())
        // Request body size limit — rejects bodies larger than the configured limit at the HTTP layer
        // before the application code even sees them, preventing memory exhaustion.
        .layer(RequestBodyLimitLayer::new(cfg.max_body_size))
        // Rate limiting middleware — checks every request against the per-IP counter.
        .layer(middleware::from_fn(rate_limit::rate_limit_middleware))
        // Insert the RateLimiter into Axum's extension map so the middleware can access it.
        .layer(axum::Extension(rate_limiter))
        // Inject the app state (db + users) into all handlers.
        .with_state(app_state);

    // Parse the configured host + port into a SocketAddr.
    // Supports both IPv4 ("0.0.0.0", "127.0.0.1") and IPv6 ("::" , "::1").
    let addr: SocketAddr = format!("{}:{}", host, port)
        .parse()
        .unwrap_or_else(|e| {
            error!("🔥 Invalid --host value '{}': {}", host, e);
            std::process::exit(1);
        });

    if cfg.dev_mode {
        warn!("⚠️  DEV MODE ENABLED — server is running over plain HTTP/WS. NEVER use in production!");
    } else {
        info!("🔒 TLS enabled - loading certificates...");
    }
    info!("🛡️  Security headers enabled");

    // Create an axum_server Handle — used to trigger graceful shutdown from outside
    // the server loop (i.e. from the shutdown signal watcher task below).
    let handle = axum_server::Handle::new();

    // Spawn a task that waits for Ctrl+C or SIGTERM, then initiates graceful shutdown.
    // `handle.clone()` is cheap — Handle is Arc-backed.
    let shutdown_handle = handle.clone();
    tokio::spawn(async move {
        // Block until a shutdown signal is received.
        server::shutdown_signal().await;
        info!("⏳ Draining in-flight requests (up to 30s)...");
        // Tell the server to stop accepting new connections and wait up to 30s
        // for all in-flight requests to complete before forcibly closing them.
        shutdown_handle.graceful_shutdown(Some(std::time::Duration::from_secs(30)));
    });

    if cfg.dev_mode {
        // Dev mode: plain HTTP/WS — no TLS.
        info!("🚀 MoltenDB running on http://{}:{} (HTTP + WS) [DEV MODE]", addr.ip(), addr.port());
        axum_server::bind(addr)
            .handle(handle)
            .serve(app.into_make_service())
            .await
            .unwrap();
    } else {
        // Production mode: HTTPS/WSS via rustls.
        match server::load_tls_config(&cert_path, &key_path).await {
            Ok(tls_config) => {
                info!("🚀 MoltenDB running on https://{}:{} (HTTPS + WSS)", addr.ip(), addr.port());

                // `.serve(...).await` blocks here until graceful shutdown completes.
                // `into_make_service()` converts the Router into a service factory
                // that creates a new service instance for each incoming connection.
                axum_server::bind_rustls(addr, tls_config)
                    .handle(handle)
                    .serve(app.into_make_service())
                    .await
                    .unwrap();
            }
            Err(e) => {
                error!("🔥 Failed to load TLS certificates: {}", e);
                error!("   Cert path: {}", cert_path);
                error!("   Key path: {}", key_path);
                std::process::exit(1);
            }
        }
    }

    // At this point all in-flight requests have finished (or timed out).
    // Dropping `db` closes the MPSC channel to the AsyncDiskStorage background
    // thread, which causes it to flush its BufWriter and exit cleanly.
    // This guarantees no buffered writes are lost on graceful shutdown.
    drop(db);
    info!("✅ Database flushed. Shutdown complete.");
}
