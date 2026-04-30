// ─── server.rs ────────────────────────────────────────────────────────────────
// TLS configuration loading and graceful shutdown signal handling.
// ─────────────────────────────────────────────────────────────────────────────

use std::path::PathBuf;
use axum_server::tls_rustls::RustlsConfig;
use tokio::signal;
use tracing::info;

// ─── load_tls_config ──────────────────────────────────────────────────────────

/// Load TLS certificate and private key from PEM files.
///
/// Returns a `RustlsConfig` that axum_server uses to terminate TLS connections.
/// Returns an error if either file doesn't exist or can't be parsed.
pub async fn load_tls_config(
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
pub async fn shutdown_signal() {
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
