// ─── Token revocation ────────────────────────────────────────────────────────

use dashmap::DashMap;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::hmac::{hmac_sign, hmac_verify};

/// Newtype wrapper for the revocation store file path.
///
/// Injected as an Axum `Extension` so `handle_revoke` can call
/// `save_to_file` after adding a new entry, without needing to
/// thread the path through the app state tuple.
#[derive(Clone, Debug)]
pub struct RevocationsPath(pub String);

/// In-memory store of revoked JWT IDs (jti).
///
/// When a token is revoked via DELETE /auth/tokens/:jti, its jti is added here.
/// The auth_middleware checks this store on every request and rejects revoked tokens
/// even if they have not yet expired.
///
/// Entries are pruned automatically by a background task every 60 seconds once
/// the token's original expiry has passed (no need to keep them forever).
#[derive(Clone, Default)]
pub struct RevocationStore {
    /// Maps jti → the Instant at which the entry can be safely pruned
    /// (i.e. when the original token would have expired anyway).
    revoked: Arc<DashMap<String, Instant>>,
}

impl RevocationStore {
    /// Create a new, empty revocation store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Revoke a token by its jti. `prune_after` is when the entry can be
    /// discarded (typically the token's own expiry time).
    pub fn revoke(&self, jti: &str, prune_after: Instant) {
        self.revoked.insert(jti.to_string(), prune_after);
    }

    /// Returns true if the given jti has been revoked.
    pub fn is_revoked(&self, jti: &str) -> bool {
        self.revoked.contains_key(jti)
    }

    /// Remove entries whose prune_after time has passed.
    /// Call this from a background task every 60 seconds.
    pub fn prune(&self) {
        self.revoked
            .retain(|_, prune_at| *prune_at > Instant::now());
    }

    /// Persist the current revocation list to a JSON file.
    ///
    /// The file format is:
    ///   `{ "entries": { "<jti>": <prune_unix_secs>, ... }, "sig": "<hmac-sha256-hex>" }`
    ///
    /// The `sig` field is an HMAC-SHA256 of the canonical JSON of `entries`,
    /// keyed with `JWT_SECRET`. On load, the signature is verified before the
    /// entries are trusted — a missing or invalid signature causes the server
    /// to refuse startup (fail-closed).
    ///
    /// This is an async function — it uses `tokio::fs::write` so it does not
    /// block the Tokio worker thread during the disk flush.
    pub async fn save_to_file(&self, path: &str) {
        // Convert Instant → u64 unix seconds for serialization.
        // Instant is not directly serializable, so we compute the remaining
        // duration from now and add it to the current unix timestamp.
        let now_instant = Instant::now();
        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let map: std::collections::HashMap<String, u64> = self
            .revoked
            .iter()
            .map(|entry| {
                let jti = entry.key().clone();
                let prune_at = *entry.value();
                // How many seconds remain until prune_at from now?
                let remaining_secs = if prune_at > now_instant {
                    prune_at.duration_since(now_instant).as_secs()
                } else {
                    0
                };
                (jti, now_unix + remaining_secs)
            })
            .collect();

        // Serialize entries to a canonical JSON string — this is what we sign.
        let entries_json = match serde_json::to_string(&map) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("⚠️  Failed to serialize revocation store: {}", e);
                return;
            }
        };

        // Compute HMAC-SHA256 of the entries JSON using JWT_SECRET as the key.
        let sig = hmac_sign(entries_json.as_bytes());

        // Wrap entries + signature into the final file payload.
        let payload = serde_json::json!({
            "entries": map,
            "sig": sig,
        });

        match serde_json::to_string(&payload) {
            Ok(json) => {
                if let Err(e) = tokio::fs::write(path, json).await {
                    eprintln!(
                        "⚠️  Failed to persist revocation store to '{}': {}",
                        path, e
                    );
                }
            }
            Err(e) => eprintln!("⚠️  Failed to serialize revocation store payload: {}", e),
        }
    }

    /// Load the revocation list from a JSON file written by `save_to_file`.
    ///
    /// The file must contain a valid HMAC-SHA256 signature over the `entries`
    /// field (keyed with `JWT_SECRET`). If the file exists but the signature is
    /// missing or invalid, this function returns `Err` — the caller must treat
    /// this as a fatal startup error (fail-closed: do not boot with an untrusted
    /// revocation store).
    ///
    /// A missing file is `Ok(empty store)` — normal on first startup.
    /// Entries whose prune deadline has already passed are silently skipped.
    pub fn load_from_file(path: &str) -> Result<Self, String> {
        let store = Self::default();

        let contents = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // File missing — fresh start, nothing to load.
                return Ok(store);
            }
            Err(e) => {
                return Err(format!("Failed to read revocation store '{}': {}", path, e));
            }
        };

        // Parse the outer envelope: { "entries": {...}, "sig": "..." }
        let envelope: serde_json::Value = match serde_json::from_str(&contents) {
            Ok(v) => v,
            Err(e) => {
                return Err(format!(
                    "Failed to parse revocation store '{}': {}",
                    path, e
                ));
            }
        };

        // Extract and verify the HMAC signature before trusting any entries.
        let sig = match envelope.get("sig").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => {
                return Err(format!(
                    "Revocation store '{}' is missing the 'sig' field — \
                     file may have been tampered with or was written by an older version. \
                     Delete the file to start fresh, or restore it from a trusted backup.",
                    path
                ));
            }
        };

        // Re-serialize the entries map to the canonical JSON string that was signed.
        let entries_value = match envelope.get("entries") {
            Some(v) => v,
            None => {
                return Err(format!(
                    "Revocation store '{}' is missing the 'entries' field.",
                    path
                ));
            }
        };
        let entries_json = match serde_json::to_string(entries_value) {
            Ok(j) => j,
            Err(e) => {
                return Err(format!(
                    "Failed to re-serialize entries from '{}': {}",
                    path, e
                ));
            }
        };

        // Verify the HMAC — fail-closed if it doesn't match.
        if !hmac_verify(entries_json.as_bytes(), &sig) {
            return Err(format!(
                "Revocation store '{}' has an invalid HMAC signature — \
                 the file may have been tampered with. \
                 Delete the file to start fresh, or restore it from a trusted backup.",
                path
            ));
        }

        // Signature is valid — deserialize the entries map.
        let map: std::collections::HashMap<String, u64> =
            match serde_json::from_value(entries_value.clone()) {
                Ok(m) => m,
                Err(e) => {
                    return Err(format!(
                        "Failed to deserialize revocation entries from '{}': {}",
                        path, e
                    ));
                }
            };

        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let now_instant = Instant::now();

        for (jti, prune_unix) in map {
            if prune_unix <= now_unix {
                // Already expired — skip it.
                continue;
            }
            let remaining = std::time::Duration::from_secs(prune_unix - now_unix);
            let prune_instant = now_instant + remaining;
            store.revoked.insert(jti, prune_instant);
        }

        Ok(store)
    }
}

// ─── User store ───────────────────────────────────────────────────────────────

use crate::password::{hash_password, verify_password};
use crate::types::AuthError;

/// In-memory user store holding the single admin user's bcrypt password hash.
///
/// MoltenDB v1 supports exactly one user (the admin). The username and password
/// are loaded from environment variables at startup. There is no user management
/// API — adding or removing users requires a server restart with updated credentials.
#[derive(Clone)]
pub struct UserStore {
    /// Maps username → bcrypt hash of the password.
    /// Arc allows UserStore to be cheaply cloned and shared across Axum handlers.
    users: Arc<DashMap<String, String>>,
}

impl UserStore {
    /// Create a new UserStore and populate it with the admin user.
    ///
    /// The admin username and password are provided as arguments.
    /// The password is hashed with bcrypt before storing — the plaintext is
    /// never kept in memory after this function returns.
    ///
    /// Returns `Err` if bcrypt fails to hash the password (e.g. RNG exhaustion).
    /// The caller should treat this as a fatal startup error — a store with no
    /// users would permanently lock out the admin with no indication.
    pub fn new(username: String, password: String) -> Result<Self, AuthError> {
        let store = Self {
            users: Arc::new(DashMap::new()),
        };

        // Hash the password and store the hash (never the plaintext).
        // Propagate the error — a failed hash means zero users in the store,
        // which would silently lock out the admin on startup.
        let hash = hash_password(&password)?;
        store.users.insert(username, hash);

        Ok(store)
    }

    /// Verify a username + password pair against the stored hash.
    ///
    /// Returns true if the username exists and the password matches its hash.
    /// Returns false if the username doesn't exist or the password is wrong.
    /// bcrypt::verify() is timing-safe — it takes the same time regardless of
    /// whether the password is correct, preventing timing attacks.
    pub fn verify_user(&self, username: &str, password: &str) -> bool {
        if let Some(hash) = self.users.get(username) {
            // verify_password() returns Ok(true/false) or Err on internal error.
            // unwrap_or(false) treats internal errors as "password incorrect".
            verify_password(password, hash.value()).unwrap_or(false)
        } else {
            false // Username not found
        }
    }
}
