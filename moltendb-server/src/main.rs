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
use moltendb_core::handlers;   // Business logic for each API endpoint (process_set, process_get, etc.)
mod rate_limit;  // Per-IP sliding-window rate limiter

// Core engine — imported from the moltendb-core crate
use moltendb_core::engine::{self, StorageBackend};

// Path = extracts path parameters from the URL, e.g. /collections/{collection}
use axum::extract::Path;
// AxumQuery = extracts URL query string parameters, e.g. ?limit=10&offset=0
use axum::extract::Query as AxumQuery;
// QueryMap = a HashMap<String, String> used to hold parsed query string params.
use std::collections::HashMap as QueryMap;
// Utf8Bytes = a WebSocket text frame body (UTF-8 encoded bytes).
use axum::extract::ws::Utf8Bytes;
use axum::{
    extract::{
        // WebSocket types for the /ws endpoint.
        ws::{Message, WebSocket, WebSocketUpgrade},
        // Extension = extracts typed data inserted into request extensions by middleware.
        Extension,
        // State = shared application state injected into every handler.
        State,
    },
    http::{StatusCode, HeaderValue, header},
    // middleware = lets us insert async functions between the router and handlers.
    middleware,
    // routing = defines which HTTP methods map to which handlers.
    routing::{get, post},
    // Json = deserializes request bodies and serializes response bodies as JSON.
    Json,
    Router,
};
// RustlsConfig = TLS configuration loaded from PEM certificate and key files.
use axum_server::tls_rustls::RustlsConfig;
// futures = async stream utilities used in the WebSocket handler.
use futures::{sink::SinkExt, stream::StreamExt};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
// signal = OS signal handling (Ctrl+C, SIGTERM) for graceful shutdown.
use tokio::signal;
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

    /// Storage mode: "standard" or "tiered" (hot+cold log, recommended for 100k+ docs) [env: MOLTENDB_STORAGE_MODE]
    #[arg(long, default_value = "standard", env = "MOLTENDB_STORAGE_MODE")]
    storage_mode: String,

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

    /// Path to a script file to execute after a successful backup.
    /// The script will be called with the absolute path of the snapshot as its first argument. [env: MOLTENDB_POST_BACKUP_SCRIPT]
    #[arg(long, env = "MOLTENDB_POST_BACKUP_SCRIPT")]
    pub post_backup_script: Option<String>,

    /// Maximum documents per collection to keep in RAM (Hot threshold). [env: MOLTENDB_HOT_THRESHOLD]
    /// If a collection exceeds this, older documents are moved to the Cold tier (disk).
    /// Higher values use more RAM but provide sub-microsecond speeds for more documents.
    #[arg(long, default_value = "50000", env = "MOLTENDB_HOT_THRESHOLD")]
    hot_threshold: usize,
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

        let password = encryption_key.as_ref().map(|s| s.clone()).unwrap_or_else(|| "default_molten_password".to_string());
        let master_key = engine::EncryptedStorage::derive_key(&password, "moltendb_log_salt");

        // Open storage
        let base_storage = Arc::new(engine::SyncDiskStorage::new(&log).expect("Failed to open log file"));
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
                std::fs::rename(snapshot_path, &out).expect("Failed to move snapshot to output path");
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

    // Determine the storage mode from the --storage-mode flag (or STORAGE_MODE env var).
    // "tiered" = TieredStorage: hot log (active writes, kept < 50 MB) + cold log
    //            (archived data, read via memory-mapped file on startup). Recommended
    //            for large datasets (100k+ documents) because the OS pages in only
    //            the cold data that's actually needed, reducing startup RAM usage.
    // anything else = single-file mode (AsyncDiskStorage or SyncDiskStorage).
    let is_tiered_mode = cfg.storage_mode.to_lowercase() == "tiered";

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
        tiered_mode: is_tiered_mode,
        hot_threshold: cfg.hot_threshold,
        rate_limit_requests,
        rate_limit_window,
        max_body_size: cfg.max_body_size,
        encryption_key: encryption_key.cloned(),
        post_backup_script: cfg.post_backup_script,
    };

    let db = match engine::Db::open(db_config) {
        Ok(database) => database,
        Err(e) => {
            error!("🔥 CRITICAL: Failed to start MoltenDB! Details: {}", e);
            std::process::exit(1);
        }
    };

    // Spawn a background task for log compaction.
    // `db.clone()` is cheap — Db is Arc-backed, so this just increments a counter.
    // `tokio::spawn` runs the async block concurrently with the main server task.
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

    // Initialize the user store with the admin user and password from Config.
    // We've already verified they are present above.
    let users = auth::UserStore::new(root_user.clone(), root_password);
    info!("👤 User authentication initialized");

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
    let app_state = (db.clone(), users, cfg.max_body_size, root_user);

    let mut protected_routes = Router::new()
        .route("/set", post(handle_set))           // Insert/upsert documents
        .route("/update", post(handle_update))     // Patch/merge documents
        .route("/delete", post(handle_delete))     // Delete documents or drop collection
        .route("/snapshot", post(handle_snapshot))   // Take a snapshot on demand
        .route("/get", post(handle_get))           // Query documents (with WHERE, fields, joins, etc.)
        .route("/collections/{collection}", get(handle_rest_get_collection))       // GET all docs (paginated)
        .route("/collections/{collection}/docs/{key}", get(handle_rest_get))       // GET single doc
        .route("/auth/delegate", post(handle_delegate));                           // Mint scoped tokens (admin only)

    #[cfg(feature = "schema")]
    {
        protected_routes = protected_routes.route("/schema", post(handle_schema));
    }

    let protected_routes = protected_routes
        // Apply the auth middleware to all routes in this sub-router.
        // `from_fn` wraps an async function as an Axum middleware layer.
        .layer(middleware::from_fn(auth::auth_middleware));

    // Public routes are accessible without authentication.
    let public_routes = Router::new()
        .route("/login", post(handle_login))  // Returns a JWT token on valid credentials
        .route("/ws", get(ws_handler));       // WebSocket upgrade endpoint

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
        // Request body size limit — rejects bodies larger than the configured limit at the HTTP layer
        // before the application code even sees them, preventing memory exhaustion.
        .layer(RequestBodyLimitLayer::new(cfg.max_body_size))
        // Rate limiting middleware — checks every request against the per-IP counter.
        .layer(middleware::from_fn(rate_limit::rate_limit_middleware))
        // Insert the RateLimiter into Axum's extension map so the middleware can access it.
        .layer(axum::Extension(rate_limiter))
        // Inject the app state (db + users) into all handlers.
        .with_state(app_state);

    // Bind to all network interfaces (0.0.0.0) on the configured port.
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    info!("🔒 TLS enabled - loading certificates...");
    info!("🛡️  Security headers enabled");

    // Create an axum_server Handle — used to trigger graceful shutdown from outside
    // the server loop (i.e. from the shutdown signal watcher task below).
    let handle = axum_server::Handle::new();

    // Spawn a task that waits for Ctrl+C or SIGTERM, then initiates graceful shutdown.
    // `handle.clone()` is cheap — Handle is Arc-backed.
    let shutdown_handle = handle.clone();
    tokio::spawn(async move {
        // Block until a shutdown signal is received.
        shutdown_signal().await;
        info!("⏳ Draining in-flight requests (up to 30s)...");
        // Tell the server to stop accepting new connections and wait up to 30s
        // for all in-flight requests to complete before forcibly closing them.
        shutdown_handle.graceful_shutdown(Some(std::time::Duration::from_secs(30)));
    });

    // Load TLS certificates and start the server.
    match load_tls_config(&cert_path, &key_path).await {
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

            // At this point all in-flight requests have finished (or timed out).
            // Dropping `db` closes the MPSC channel to the AsyncDiskStorage background
            // thread, which causes it to flush its BufWriter and exit cleanly.
            // This guarantees no buffered writes are lost on graceful shutdown.
            drop(db);
            info!("✅ Database flushed. Shutdown complete.");
        }
        Err(e) => {
            error!("🔥 Failed to load TLS certificates: {}", e);
            error!("   Cert path: {}", cert_path);
            error!("   Key path: {}", key_path);
            std::process::exit(1);
        }
    }
}

// ─── load_tls_config ──────────────────────────────────────────────────────────

/// Load TLS certificate and private key from PEM files.
///
/// Returns a `RustlsConfig` that axum_server uses to terminate TLS connections.
/// Returns an error if either file doesn't exist or can't be parsed.
async fn load_tls_config(
    cert_path: &str,
    key_path: &str,
) -> Result<RustlsConfig, Box<dyn std::error::Error>> {
    let cert = PathBuf::from(cert_path);
    let key = PathBuf::from(key_path);

    // Check that both files exist before trying to load them.
    // This gives a clearer error message than the one from rustls.
    if !cert.exists() {
        return Err(format!("Certificate file not found: {}", cert_path).into());
    }
    if !key.exists() {
        return Err(format!("Key file not found: {}", key_path).into());
    }

    // Load and parse the PEM files. This is async because it reads from disk.
    Ok(RustlsConfig::from_pem_file(cert, key).await?)
}

// ─── shutdown_signal ──────────────────────────────────────────────────────────

/// Wait for a shutdown signal (Ctrl+C or SIGTERM) and then return.
///
/// This function is used by the graceful shutdown task in main().
/// It resolves as soon as either signal is received.
///
/// `tokio::select!` waits for the first of multiple futures to complete.
/// On Unix systems, both Ctrl+C and SIGTERM are handled.
/// On Windows, only Ctrl+C is handled (SIGTERM is not a real signal on Windows).
async fn shutdown_signal() {
    // Future that resolves when Ctrl+C is pressed.
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    // On Unix: future that resolves when SIGTERM is received (e.g. `kill <pid>`).
    // `#[cfg(unix)]` means this block only compiles on Unix-like systems.
    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    // On non-Unix (Windows): SIGTERM doesn't exist, so use a future that never resolves.
    // `std::future::pending()` is a future that is always Pending — it never wakes up.
    // This means on Windows, only Ctrl+C triggers shutdown.
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    // Wait for whichever signal arrives first.
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    info!("🛑 Shutting down gracefully...");
}

// ─── Route handlers ───────────────────────────────────────────────────────────
// Each handler is a thin async function that:
//   1. Extracts the request body (via `Json(payload)`).
//   2. Calls the corresponding `handlers::process_*` function.
//   3. Returns the result wrapped in `Json(...)` (serialized as JSON).
//
// `State((db, _))` destructures the app state tuple — `db` is the Db handle,
// `_` discards the UserStore (not needed in most handlers).

/// POST /auth/delegate — mint a scoped JWT for a client.
///
/// Strictly admin-only. The caller must present a valid admin token.
/// Accepts a JSON body with `client_id`, `scopes`, and an optional `ttl_secs`.
/// Returns a signed JWT containing exactly the requested scopes — the client
/// can use this token to access MoltenDB without ever seeing the root password.
///
/// Scope format: "action:collection:document_key"
/// Examples: "read:laptops:lp1", "write:users:*", "read:*:*", "*:*:*"
async fn handle_delegate(
    State((_, _, _, root_username)): State<(engine::Db, auth::UserStore, usize, String)>,
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
    let token = auth::create_scoped_token(&payload.client_id, payload.scopes.clone(), ttl)
        .map_err(|e| (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Token creation failed: {}", e)})),
        ))?;

    Ok(Json(auth::DelegateResponse {
        token,
        client_id: payload.client_id,
        scopes: payload.scopes,
    }))
}

/// POST /login — authenticate and return a JWT token.
///
/// This is a public endpoint (no auth middleware).
/// Returns 200 + `{ "token": "..." }` on success.
/// Returns 401 Unauthorized if credentials are wrong.
/// Returns 500 Internal Server Error if token creation fails.
async fn handle_login(
    State((_, users, _, _)): State<(engine::Db, auth::UserStore, usize, String)>,
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
async fn handle_set(
    State((db, _, max_body_size, _)): State<(engine::Db, auth::UserStore, usize, String)>,
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
    let (code, body) = handlers::process_set(&db, &payload, max_body_size);
    (StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), Json(body))
}

/// POST /update — merge new fields into existing documents (patch semantics).
///
/// Body: `{ "collection": "users", "data": { "u1": { "role": "admin" } } }`
/// Requires: write:{collection}:* scope (or admin).
async fn handle_update(
    State((db, _, max_body_size, _)): State<(engine::Db, auth::UserStore, usize, String)>,
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
    let (code, body) = handlers::process_update(&db, &payload, max_body_size);
    (StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), Json(body))
}

/// POST /get — query documents with optional WHERE, fields, joins, count, offset.
///
/// Body: `{ "collection": "users", "where": { "role": "admin" }, "fields": ["name"] }`
///
/// Scope rules:
///   - `read:{collection}:*` (or `read:*:*` or `admin`) → full access, all docs returned.
///   - Document-level scopes (`read:{collection}:key1`, `read:{collection}:key2`, …):
///       • If `"keys"` is specified, all requested keys must be covered by the token.
///       • If no `"keys"` is specified, the result is filtered to only the docs the
///         token is allowed to read.
async fn handle_get(
    State((db, _, max_body_size, _)): State<(engine::Db, auth::UserStore, usize, String)>,
    Extension(claims): Extension<auth::Claims>,
    Json(payload): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let collection = payload.get("collection").and_then(|v| v.as_str()).unwrap_or("");

    // Fast path: collection-level (or broader) access — no filtering needed.
    if claims.has_collection_access("read", collection) {
        let (code, body) = handlers::process_get(&db, &payload, max_body_size);
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
        let (code, body) = handlers::process_get(&db, &payload, max_body_size);
        return (StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), Json(body));
    }

    // No keys specified.
    // If the token has prefix-wildcard scopes (e.g. "read:laptops:store_A_*"), fetch all
    // and post-filter. Otherwise pre-scope to the exact allowed keys (fast path).
    if claims.has_prefix_wildcard("read", collection) {
        let (code, body) = handlers::process_get(&db, &payload, max_body_size);
        if code != 200 {
            return (StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), Json(body));
        }
        let filtered: serde_json::Map<String, Value> = body
            .as_object()
            .map(|map| {
                map.iter()
                    .filter(|(k, _)| claims.has_access("read", collection, k))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            })
            .unwrap_or_default();
        (StatusCode::OK, Json(Value::Object(filtered)))
    } else {
        let mut scoped_payload = payload.clone();
        scoped_payload["keys"] = json!(allowed_keys);
        let (code, body) = handlers::process_get(&db, &scoped_payload, max_body_size);
        (StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), Json(body))
    }
}

/// POST /delete — delete one key, multiple keys, or an entire collection.
///
/// Body (single):   `{ "collection": "users", "keys": "u1" }`
/// Body (batch):    `{ "collection": "users", "keys": ["u1", "u2"] }`
/// Body (drop all): `{ "collection": "users", "drop": true }`
/// Requires: delete:{collection}:* scope (or admin).
async fn handle_delete(
    State((db, _, max_body_size, _)): State<(engine::Db, auth::UserStore, usize, String)>,
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
    let (code, body) = handlers::process_delete(&db, &payload, max_body_size);
    (StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), Json(body))
}

#[cfg(feature = "schema")]
async fn handle_schema(
    State((db, _, max_body_size, _)): State<(engine::Db, auth::UserStore, usize, String)>,
    Json(payload): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let (code, body) = handlers::process_schema(&db, &payload, max_body_size);
    (StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), Json(body))
}

/// POST /snapshot — take a snapshot of the database on demand.
/// Requires: admin scope.
async fn handle_snapshot(
    State((db, _, _, _)): State<(engine::Db, auth::UserStore, usize, String)>,
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
async fn handle_rest_get(
    State((db, _, max_body_size, _)): State<(engine::Db, auth::UserStore, usize, String)>,
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
    let (code, body) = handlers::process_get(&db, &payload, max_body_size);
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
/// Requires: read:{collection}:* scope (or admin).
async fn handle_rest_get_collection(
    State((db, _, max_body_size, _)): State<(engine::Db, auth::UserStore, usize, String)>,
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
    let (code, body) = handlers::process_get(&db, &payload, max_body_size);
    (StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), Json(body))
}

// ─── WebSocket handler ────────────────────────────────────────────────────────

/// GET /ws — upgrade an HTTP connection to a WebSocket connection.
///
/// `WebSocketUpgrade` is an Axum extractor that handles the HTTP → WS upgrade
/// handshake. The actual socket logic runs in `handle_socket`.
async fn ws_handler(
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
            if let Ok(payload) = serde_json::from_str::<Value>(&text) {
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
