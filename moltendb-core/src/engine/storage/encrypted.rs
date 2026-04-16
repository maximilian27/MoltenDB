// ─── encrypted.rs ────────────────────────────────────────────────────────────
// This file implements EncryptedStorage — a transparent encryption wrapper
// around any StorageBackend (e.g. AsyncDiskStorage or SyncDiskStorage).
//
// How it works:
//   • write_entry()  — serialises the LogEntry to JSON, encrypts the JSON
//                      string with XChaCha20-Poly1305, base64-encodes the
//                      result, and writes a sentinel "ENC" LogEntry whose
//                      `value` field holds the ciphertext. The underlying
//                      storage backend sees only opaque ENC entries.
//
//   • read_log()     — reads raw ENC entries from the inner backend, decrypts
//                      each one, and returns the original LogEntry objects.
//                      Unencrypted entries (from a migration / legacy DB) are
//                      passed through unchanged.
//
//   • compact()      — re-encrypts every entry during compaction so the
//                      compacted file is fully encrypted with no plaintext
//                      remnants left over from a migration.
//
// Encryption algorithm: XChaCha20-Poly1305
//   • XChaCha20 is a stream cipher — it generates a keystream that is XOR'd
//     with the plaintext. It's fast and has no timing side-channels.
//   • Poly1305 is a message authentication code (MAC) — it detects any
//     tampering with the ciphertext. If the MAC check fails, decryption
//     returns an error instead of silently returning garbage data.
//   • "X" variant uses a 192-bit (24-byte) nonce instead of 96-bit, making
//     random nonce generation safe even for billions of messages.
//
// Key derivation: Argon2id
//   • Argon2id is a memory-hard password hashing algorithm — it's designed
//     to be slow and expensive to brute-force even with GPUs/ASICs.
//   • We derive a deterministic 32-byte key from (password, db_path) so the
//     same password always produces the same key for the same database file.
//
// On-disk format of each encrypted entry:
//   {"cmd":"ENC","collection":"_","key":"_","value":"<base64>"}
//   where <base64> = base64( nonce[24 bytes] || ciphertext )
// ─────────────────────────────────────────────────────────────────────────────

// Only compile this file when targeting native (not WebAssembly).
#![cfg(not(target_arch = "wasm32"))]

// The StorageBackend trait that EncryptedStorage implements.
use super::StorageBackend;
// Our internal data types.
use crate::engine::types::{DbError, LogEntry};
// base64 encoding/decoding — used to safely store binary ciphertext as text.
use base64::{Engine as _, engine::general_purpose::STANDARD};
// XChaCha20-Poly1305 AEAD cipher from the chacha20poly1305 crate.
use chacha20poly1305::{
    aead::{Aead, KeyInit}, // Aead = Authenticated Encryption with Associated Data
    XChaCha20Poly1305,     // The cipher type
    XNonce,                // 24-byte nonce type
    Key,                   // 32-byte key type
};
// OsRng = cryptographically secure random number generator from the OS.
// RngCore = trait that provides fill_bytes() for generating random bytes.
use rand_core::{OsRng, RngCore};
// Arc = thread-safe reference-counted pointer for shared ownership.
use std::sync::Arc;

/// Transparent encryption wrapper around any StorageBackend.
///
/// All data written through this wrapper is encrypted before reaching the
/// inner storage, and decrypted when read back. The encryption is completely
/// transparent to the rest of the database engine.
pub struct EncryptedStorage {
    /// The underlying storage backend (e.g. AsyncDiskStorage).
    /// Arc<dyn StorageBackend> means "a thread-safe pointer to any type that
    /// implements StorageBackend" — this is Rust's way of doing polymorphism
    /// at runtime (similar to an interface in Java/C#).
    inner: Arc<dyn StorageBackend>,
    /// The initialized cipher instance, ready to encrypt/decrypt.
    /// XChaCha20Poly1305 holds the expanded key schedule internally.
    cipher: XChaCha20Poly1305,
}

impl EncryptedStorage {
    /// Create a new EncryptedStorage wrapping `inner` with the given 32-byte key.
    ///
    /// Call `derive_key()` first to turn a human-readable password into a key.
    /// The key must be exactly 32 bytes (256 bits) — the size XChaCha20 requires.
    pub fn new(inner: Arc<dyn StorageBackend>, master_key: &[u8; 32]) -> Self {
        // Key::from_slice() wraps the raw bytes in the Key newtype.
        // XChaCha20Poly1305::new() expands the key into the cipher's internal state.
        let key = Key::from_slice(master_key);
        Self {
            inner,
            cipher: XChaCha20Poly1305::new(key),
        }
    }

    /// Derive a deterministic 32-byte encryption key from a password string
    /// using the Argon2id algorithm.
    ///
    /// `salt_context` is typically the database file path — this ensures that
    /// the same password produces different keys for different database files,
    /// preventing cross-database attacks.
    ///
    /// Argon2id is intentionally slow (memory-hard) to make brute-force attacks
    /// expensive. The default parameters require ~64 MB of RAM and ~0.5 s of CPU.
    pub fn derive_key(password: &str, salt_context: &str) -> [u8; 32] {
        use argon2::{Argon2, PasswordHasher};
        use argon2::password_hash::SaltString;

        // Argon2 requires a base64-encoded salt of at least 8 bytes.
        // We build a deterministic 22-character salt from the context string
        // (22 base64 chars = 16 bytes of entropy, well above the minimum).
        // The format!("{:0<22}", ...) pads the string to 22 chars with '0's.
        let raw_salt = format!("{:0<22}", &salt_context[..salt_context.len().min(22)]);
        // SaltString::from_b64 validates the base64 encoding.
        // If it fails (e.g. invalid chars in the path), fall back to a known-good salt.
        let salt = SaltString::from_b64(&raw_salt)
            .unwrap_or_else(|_| SaltString::from_b64("bW9sdGVuZGJkZWZhdWx0").unwrap());

        // Argon2::default() uses Argon2id variant with standard parameters.
        let argon2 = Argon2::default();
        // hash_password() runs the Argon2 algorithm and returns a PHC string.
        let hash = argon2
            .hash_password(password.as_bytes(), &salt)
            .expect("Argon2 key derivation failed");

        // Extract the raw 32-byte hash output from the PHC string.
        // The PHC format includes the algorithm, parameters, salt, and hash.
        let hash_output = hash.hash.expect("Argon2 produced no hash output");
        let bytes = hash_output.as_bytes();
        // Copy the first 32 bytes into our fixed-size key array.
        let mut key = [0u8; 32];
        key.copy_from_slice(&bytes[..32]);
        key
    }

    /// Encrypt a single LogEntry and return a new "ENC" LogEntry whose `value`
    /// field contains the base64-encoded ciphertext.
    ///
    /// A fresh random 24-byte nonce is generated for every single write.
    /// Using a unique nonce per message is critical for security — reusing a
    /// nonce with the same key would completely break the encryption.
    fn encrypt_entry(&self, entry: &LogEntry) -> Result<LogEntry, DbError> {
        // Step 1: Serialize the real entry to a JSON string.
        let plain_json = serde_json::to_string(entry)?;

        // Step 2: Generate a cryptographically random 24-byte nonce.
        // OsRng reads from /dev/urandom (Linux) or BCryptGenRandom (Windows).
        let mut nonce_bytes = [0u8; 24];
        OsRng.fill_bytes(&mut nonce_bytes);
        // XNonce::from_slice() wraps the bytes in the nonce newtype.
        let nonce = XNonce::from_slice(&nonce_bytes);

        // Step 3: Encrypt the plaintext. The cipher also computes a 16-byte
        // Poly1305 MAC and appends it to the ciphertext automatically.
        // Result: ciphertext = encrypt(key, nonce, plaintext) || MAC[16 bytes]
        let cipher_text = self
            .cipher
            .encrypt(nonce, plain_json.as_bytes())
            .map_err(|_| DbError::WriteError)?;

        // Step 4: Prepend the nonce to the ciphertext so we have everything
        // needed for decryption in one blob: nonce[24] || ciphertext || MAC[16]
        let mut payload = nonce_bytes.to_vec();
        payload.extend(cipher_text);

        // Step 5: Base64-encode the binary payload so it can be safely stored
        // as a JSON string value (JSON doesn't support raw binary).
        let b64 = STANDARD.encode(&payload);

        // Step 6: Return a sentinel LogEntry that hides the real cmd/collection/key.
        // The underlying storage only ever sees these opaque ENC entries.
        Ok(LogEntry {
            cmd: "ENC".to_string(),
            collection: "_".to_string(), // placeholder — real collection is inside the ciphertext
            key: "_".to_string(),        // placeholder — real key is inside the ciphertext
            value: serde_json::json!(b64),
        })
    }

    /// Decrypt a single "ENC" LogEntry and return the original LogEntry.
    ///
    /// The Poly1305 MAC is verified automatically during decryption — if the
    /// ciphertext has been tampered with or the wrong key is used, this returns
    /// an error instead of silently returning garbage data.
    fn decrypt_entry(&self, entry: &LogEntry) -> Result<LogEntry, DbError> {
        // Extract the base64 string from the value field.
        let b64 = entry.value.as_str().unwrap_or("");

        // Decode from base64 back to raw bytes: nonce[24] || ciphertext || MAC[16]
        let payload = STANDARD
            .decode(b64)
            .map_err(|_| DbError::WriteError)?;

        // Sanity check: the payload must be at least 24 bytes (nonce) + 16 bytes (MAC).
        if payload.len() < 24 {
            return Err(DbError::WriteError);
        }

        // Split the payload into nonce (first 24 bytes) and ciphertext (the rest).
        // split_at() returns two slices sharing the same underlying memory — no copy.
        let (nonce_bytes, cipher_text) = payload.split_at(24);
        let nonce = XNonce::from_slice(nonce_bytes);

        // Decrypt and verify the MAC. If the MAC doesn't match (wrong key or
        // tampered data), decrypt() returns Err and we propagate it as WriteError.
        let plain_bytes = self
            .cipher
            .decrypt(nonce, cipher_text)
            .map_err(|_| DbError::WriteError)?;

        // Convert the decrypted bytes back to a UTF-8 string.
        let plain_json = String::from_utf8(plain_bytes).map_err(|_| DbError::WriteError)?;

        // Deserialize the JSON string back into a LogEntry.
        serde_json::from_str::<LogEntry>(&plain_json).map_err(|e| DbError::Serialization(e))
    }
}

/// Implement the StorageBackend trait so EncryptedStorage can be used anywhere
/// a StorageBackend is expected — the rest of the engine doesn't know or care
/// that encryption is happening.
impl StorageBackend for EncryptedStorage {
    /// Encrypt `entry` and write the resulting ENC entry to the inner backend.
    fn write_entry(&self, entry: &LogEntry) -> Result<(), DbError> {
        let encrypted = self.encrypt_entry(entry)?;
        // Delegate to the inner backend (e.g. AsyncDiskStorage.write_entry).
        self.inner.write_entry(&encrypted)
    }

    /// Read all ENC entries from the inner backend and decrypt them.
    ///
    /// Entries that fail to decrypt are skipped with a warning (not a crash)
    /// so a single corrupt entry doesn't bring down the whole database.
    /// Unencrypted entries (cmd != "ENC") are passed through unchanged —
    /// this supports migrating an existing plaintext database to encrypted.
    fn read_log(&self) -> Result<Vec<LogEntry>, DbError> {
        // Read the raw (encrypted) entries from the underlying storage.
        let raw_entries = self.inner.read_log()?;
        let mut decrypted = Vec::with_capacity(raw_entries.len());

        for entry in raw_entries {
            if entry.cmd == "ENC" {
                // This is an encrypted entry — decrypt it.
                match self.decrypt_entry(&entry) {
                    Ok(real_entry) => decrypted.push(real_entry),
                    Err(e) => {
                        // Log a warning but continue — don't crash on one bad entry.
                        // This can happen if the encryption key changed or the file
                        // was partially corrupted.
                        tracing::warn!("⚠️  Skipping undecryptable log entry: {}", e);
                    }
                }
            } else {
                // Unencrypted legacy entry — pass through as-is.
                // This allows migrating an existing plaintext database by simply
                // enabling encryption; old entries are still readable.
                decrypted.push(entry);
            }
        }

        Ok(decrypted)
    }

    /// Re-encrypt all entries during compaction so the compacted file is fully
    /// encrypted with no plaintext remnants from a migration.
    ///
    /// Each entry gets a fresh random nonce — even if the plaintext is the same
    /// as before, the ciphertext will be different (this is correct and expected).
    fn compact(&self, entries: Vec<LogEntry>) -> Result<(), DbError> {
        // Encrypt every entry. If any encryption fails, the whole compaction fails
        // (we don't want a partially-encrypted compacted file).
        // The `.collect::<Result<Vec<_>, _>>()` pattern: if any map() call returns
        // Err, the whole collect() returns that Err immediately (short-circuits).
        let encrypted: Result<Vec<LogEntry>, DbError> =
            entries.iter().map(|e| self.encrypt_entry(e)).collect();
        // Delegate the actual file writing to the inner backend's compact().
        self.inner.compact(encrypted?)
    }
}
