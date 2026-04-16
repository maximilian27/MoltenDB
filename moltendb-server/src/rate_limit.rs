// ─── rate_limit.rs ────────────────────────────────────────────────────────────
// This file implements per-IP rate limiting for all HTTP endpoints.
//
// What is rate limiting?
//   Rate limiting prevents a single client from sending too many requests in a
//   short time window. Without it, a malicious client could:
//     - Flood the server with requests (DoS attack)
//     - Brute-force the /login endpoint trying many passwords
//     - Exhaust server memory by triggering many expensive queries
//
// How it works:
//   Each incoming request is checked against a sliding window counter:
//     1. Extract the client's IP address from the request.
//     2. Look up the list of recent request timestamps for that IP.
//     3. Remove timestamps older than the window (e.g. older than 60 seconds).
//     4. If the remaining count >= max_requests → reject with 429 Too Many Requests.
//     5. Otherwise → record this request's timestamp and allow it through.
//
// Configuration (set via environment variables in main.rs):
//   RATE_LIMIT_REQUESTS     — max requests per window (default: 100)
//   RATE_LIMIT_WINDOW_SECS  — window size in seconds (default: 60)
//
// Known limitation:
//   The IP is read from X-Forwarded-For, which is spoofable by clients.
//   Behind a trusted reverse proxy, use the LAST IP in the header instead.
//   See extract_ip() for details.
// ─────────────────────────────────────────────────────────────────────────────

// This file only compiles for native (non-WASM) targets.
// The rate limiter uses std::time and network types that don't exist in WASM.
#![cfg(not(target_arch = "wasm32"))]

use axum::{
    // Request = the full incoming HTTP request (headers, body, extensions).
    extract::Request,
    // StatusCode = HTTP status codes like 200 OK, 429 Too Many Requests.
    http::StatusCode,
    // Next = the next middleware or handler in the chain.
    middleware::Next,
    // IntoResponse = trait that lets a type be returned as an HTTP response.
    response::{IntoResponse, Response},
    // Json = wraps a value and serializes it as JSON in the response body.
    Json,
};
// DashMap = a concurrent HashMap safe to share across async tasks without a Mutex.
// Each IP address maps to a Vec of timestamps of recent requests.
use dashmap::DashMap;
use serde_json::json;
// IpAddr = an IP address (either IPv4 or IPv6).
use std::net::IpAddr;
// Arc = Atomically Reference Counted pointer — lets multiple owners share the
// same data safely across threads. Required because the rate limiter is cloned
// into every request handler.
use std::sync::Arc;
// Duration = a span of time (e.g. 60 seconds).
// Instant = a point in time, used for measuring elapsed time.
use std::time::{Duration, Instant};

// ─── RateLimiter ──────────────────────────────────────────────────────────────

/// The rate limiter state, shared across all request handlers.
///
/// `#[derive(Clone)]` lets Axum clone this into each request handler.
/// The `Arc<DashMap<...>>` inside means all clones share the same underlying data —
/// cloning the struct is cheap (just increments a reference count).
#[derive(Clone)]
pub struct RateLimiter {
    /// Maps each client IP address to a list of timestamps of recent requests.
    /// DashMap is used instead of HashMap because it's safe to read/write from
    /// multiple async tasks simultaneously without a Mutex.
    requests: Arc<DashMap<IpAddr, Vec<Instant>>>,

    /// Maximum number of requests allowed per IP within the time window.
    /// Configured via RATE_LIMIT_REQUESTS env var (default: 100).
    max_requests: usize,

    /// The sliding time window. Requests older than this are discarded.
    /// Configured via RATE_LIMIT_WINDOW_SECS env var (default: 60 seconds).
    window: Duration,
}

impl RateLimiter {
    /// Create a new RateLimiter with the given limits.
    ///
    /// # Arguments
    /// * `max_requests`  — Maximum requests allowed per IP per window.
    /// * `window_secs`   — Window size in seconds.
    pub fn new(max_requests: usize, window_secs: u64) -> Self {
        Self {
            // Arc::new wraps the DashMap so it can be shared across clones.
            requests: Arc::new(DashMap::new()),
            max_requests,
            // Convert seconds to a Duration for time arithmetic.
            window: Duration::from_secs(window_secs),
        }
    }

    /// Check whether the given IP address is within its rate limit.
    ///
    /// This is the core of the sliding window algorithm:
    ///   1. Compute the cutoff time (now - window).
    ///   2. Remove all timestamps older than the cutoff (they're outside the window).
    ///   3. If the remaining count >= max_requests, reject with a RateLimitError.
    ///   4. Otherwise, record this request and return Ok(()).
    ///
    /// Returns `Ok(())` if the request is allowed, or `Err(RateLimitError)` if
    /// the limit has been exceeded.
    pub fn check_rate_limit(&self, ip: IpAddr) -> Result<(), RateLimitError> {
        let now = Instant::now();
        // `cutoff` is the oldest timestamp we still care about.
        // Any request older than this is outside the sliding window.
        let cutoff = now - self.window;

        // `entry(ip).or_insert_with(Vec::new)` gets the existing Vec for this IP,
        // or creates an empty one if this IP hasn't been seen before.
        // The returned `entry` is a mutable reference to the Vec.
        let mut entry = self.requests.entry(ip).or_insert_with(Vec::new);

        // Remove timestamps that are outside the current window.
        // `retain` keeps only elements where the closure returns true.
        // After this, `entry` contains only requests from the last `window` seconds.
        entry.retain(|&timestamp| timestamp > cutoff);

        // If the number of recent requests equals or exceeds the limit, reject.
        if entry.len() >= self.max_requests {
            // Calculate how many seconds until the oldest request falls out of the window.
            // This tells the client when they can retry.
            let oldest = entry.first().unwrap();
            // `saturating_sub` prevents underflow (returns 0 instead of wrapping).
            // `.max(1)` ensures we never tell the client to retry in 0 seconds.
            let retry_after = (oldest.elapsed().as_secs() as u64)
                .saturating_sub(self.window.as_secs())
                .max(1);

            return Err(RateLimitError {
                retry_after,
                limit: self.max_requests,
            });
        }

        // Record this request's timestamp so future requests can count it.
        entry.push(now);

        Ok(())
    }

    /// Remove stale entries from the map to prevent unbounded memory growth.
    ///
    /// This is called by a background task in main.rs every 300 seconds.
    /// Without cleanup, the map would grow forever as new IPs make requests.
    ///
    /// The cutoff is set to `window + 60 seconds` to be conservative —
    /// we keep entries slightly longer than the window in case of clock skew.
    pub fn cleanup(&self) {
        // Use a cutoff slightly older than the window to be safe.
        let cutoff = Instant::now() - self.window - Duration::from_secs(60);

        // `retain` on DashMap removes entries where the closure returns false.
        // For each IP, first remove old timestamps, then remove the IP entirely
        // if it has no remaining timestamps (i.e. it's been idle long enough).
        self.requests.retain(|_, timestamps| {
            timestamps.retain(|&ts| ts > cutoff);
            // Keep this IP's entry only if it still has recent timestamps.
            !timestamps.is_empty()
        });
    }
}

// ─── RateLimitError ───────────────────────────────────────────────────────────

/// Error returned when a client exceeds their rate limit.
///
/// Contains enough information to build a helpful 429 response:
///   - `retry_after`: how many seconds until the client can try again.
///   - `limit`: the maximum requests per window (for informational purposes).
#[derive(Debug)]
pub struct RateLimitError {
    /// Seconds until the oldest request in the window expires.
    /// Sent to the client as `retry_after_seconds` in the JSON response.
    pub retry_after: u64,

    /// The configured maximum requests per window.
    /// Sent to the client as `limit` in the JSON response.
    pub limit: usize,
}

// ─── rate_limit_middleware ────────────────────────────────────────────────────

/// Axum middleware that enforces rate limiting on every request.
///
/// Middleware in Axum is a function that wraps a request handler:
///   request → [rate_limit_middleware] → [auth_middleware] → [handler]
///
/// If the rate limit is exceeded, this middleware short-circuits the chain
/// and returns a 429 response immediately — the handler is never called.
///
/// The `RateLimiter` is retrieved from Axum's extension system, where it was
/// inserted in main.rs via `.layer(axum::Extension(rate_limiter))`.
pub async fn rate_limit_middleware(
    request: Request,
    next: Next,
) -> Result<Response, impl IntoResponse> {
    // Extract the client's IP address from the request headers.
    let ip = extract_ip(&request);

    // Retrieve the shared RateLimiter from Axum's extension map.
    // `.cloned()` gives us an owned copy (cheap — just increments Arc refcount).
    // `.expect(...)` panics if the RateLimiter wasn't added in main.rs — this
    // would be a programming error, not a runtime error.
    let limiter = request
        .extensions()
        .get::<RateLimiter>()
        .cloned()
        .expect("RateLimiter not found in extensions");

    // Check whether this IP is within its rate limit.
    match limiter.check_rate_limit(ip) {
        // Within limit — pass the request to the next handler in the chain.
        Ok(_) => Ok(next.run(request).await),

        // Limit exceeded — return 429 Too Many Requests immediately.
        // The client receives a JSON body explaining the limit and retry time.
        Err(err) => Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({
                "error": "Rate limit exceeded",
                "limit": err.limit,
                "retry_after_seconds": err.retry_after,
            })),
        )),
    }
}

// ─── extract_ip ───────────────────────────────────────────────────────────────

/// Extract the client's real IP address from the request.
///
/// When MoltenDB runs behind a reverse proxy (nginx, Cloudflare, etc.), the
/// actual client IP is not the TCP connection IP — it's forwarded in headers.
///
/// Header priority:
///   1. `X-Forwarded-For` — standard proxy header, may contain a comma-separated
///      list of IPs (client, proxy1, proxy2). We take the FIRST one.
///   2. `X-Real-IP` — simpler single-IP header set by nginx.
///   3. Fallback to `127.0.0.1` — used when running without a proxy.
///
/// ⚠️ SECURITY NOTE:
///   `X-Forwarded-For` is set by the client and is trivially spoofable.
///   A client can send `X-Forwarded-For: 1.2.3.4` to appear as any IP.
///   In production behind a trusted proxy, use the LAST IP in the list
///   (the one added by your own proxy) rather than the first.
fn extract_ip(request: &Request) -> IpAddr {
    // Try X-Forwarded-For first — this is the standard header set by most proxies.
    // Format: "client_ip, proxy1_ip, proxy2_ip"
    // We take the first entry (the original client IP).
    if let Some(forwarded) = request.headers().get("x-forwarded-for") {
        // `.to_str()` converts the header bytes to a UTF-8 string.
        // Returns Err if the header contains non-UTF-8 bytes (rare but possible).
        if let Ok(forwarded_str) = forwarded.to_str() {
            // Split on comma and take the first IP in the list.
            if let Some(ip_str) = forwarded_str.split(',').next() {
                // `.trim()` removes leading/trailing whitespace.
                // `.parse()` converts the string to an IpAddr (IPv4 or IPv6).
                if let Ok(ip) = ip_str.trim().parse() {
                    return ip;
                }
            }
        }
    }

    // Try X-Real-IP — a simpler single-IP header commonly set by nginx.
    if let Some(real_ip) = request.headers().get("x-real-ip") {
        if let Ok(ip_str) = real_ip.to_str() {
            if let Ok(ip) = ip_str.parse() {
                return ip;
            }
        }
    }

    // Fallback: no proxy headers found.
    // In development (no proxy), all requests come from localhost.
    // In production behind a proxy that doesn't set these headers,
    // all requests will appear to come from 127.0.0.1 — effectively
    // disabling per-IP rate limiting. This is a known limitation.
    "127.0.0.1".parse().unwrap()
}
