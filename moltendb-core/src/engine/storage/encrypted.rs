use super::StorageBackend;
use crate::engine::types::{DbError, LogEntry};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    XChaCha20Poly1305, XNonce, Key,
};
use rand_core::{OsRng, RngCore};
use std::ops::ControlFlow;
use std::sync::Arc;

// Wraps any StorageBackend and transparently encrypts every log entry with XChaCha20-Poly1305.
pub struct EncryptedStorage {
    inner: Arc<dyn StorageBackend>,
    cipher: XChaCha20Poly1305,
}

impl EncryptedStorage {
    pub fn new(inner: Arc<dyn StorageBackend>, master_key: &[u8; 32]) -> Self {
        let key = Key::from_slice(master_key);
        Self { inner, cipher: XChaCha20Poly1305::new(key) }
    }

    // Derives a 32-byte key from a password using Argon2id. `salt_context` is typically the db path.
    pub fn derive_key(password: &str, salt_context: &str) -> [u8; 32] {
        use argon2::{Argon2, PasswordHasher};
        use argon2::password_hash::SaltString;

        let raw_salt = format!("{:0<22}", &salt_context[..salt_context.len().min(22)]);
        let salt = SaltString::from_b64(&raw_salt)
            .unwrap_or_else(|_| SaltString::from_b64("bW9sdGVuZGJkZWZhdWx0").unwrap());

        let hash = Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .expect("Argon2 key derivation failed");

        let hash_output = hash.hash.expect("Argon2 produced no hash output");
        let bytes = hash_output.as_bytes();
        let mut key = [0u8; 32];
        key.copy_from_slice(&bytes[..32]);
        key
    }

    // Serialises the entry to JSON, encrypts it, and wraps the result in an "ENC" log entry.
    // Layout: [24-byte nonce][ciphertext], base64-encoded as the entry value.
    fn encrypt_entry(&self, entry: &LogEntry) -> Result<LogEntry, DbError> {
        let plain_json = serde_json::to_string(entry)?;

        let mut nonce_bytes = [0u8; 24];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = XNonce::from_slice(&nonce_bytes);

        let cipher_text = self.cipher
            .encrypt(nonce, plain_json.as_bytes())
            .map_err(|_| DbError::WriteError)?;

        let mut payload = nonce_bytes.to_vec();
        payload.extend(cipher_text);

        Ok(LogEntry::new(
            "ENC".to_string(),
            "_".to_string(),
            "_".to_string(),
            serde_json::json!(STANDARD.encode(&payload)),
        ))
    }

    // Reverses encrypt_entry: base64-decode → split nonce/ciphertext → decrypt → deserialise.
    fn decrypt_entry(&self, entry: &LogEntry) -> Result<LogEntry, DbError> {
        let b64 = entry.value.as_str().unwrap_or("");
        let payload = STANDARD.decode(b64).map_err(|_| DbError::WriteError)?;

        if payload.len() < 24 {
            return Err(DbError::WriteError);
        }

        let (nonce_bytes, cipher_text) = payload.split_at(24);
        let nonce = XNonce::from_slice(nonce_bytes);

        let plain_bytes = self.cipher
            .decrypt(nonce, cipher_text)
            .map_err(|_| DbError::WriteError)?;

        let plain_json = String::from_utf8(plain_bytes).map_err(|_| DbError::WriteError)?;
        serde_json::from_str::<LogEntry>(&plain_json).map_err(DbError::Serialization)
    }
}

impl StorageBackend for EncryptedStorage {
    fn write_entry(&self, entry: &LogEntry) -> Result<(), DbError> {
        let encrypted = self.encrypt_entry(entry)?;
        self.inner.write_entry(&encrypted)
    }

    fn read_log(&self) -> Result<Vec<LogEntry>, DbError> {
        let raw_entries = self.inner.read_log()?;
        let mut decrypted = Vec::with_capacity(raw_entries.len());
        for entry in raw_entries {
            if entry.cmd == "ENC" {
                match self.decrypt_entry(&entry) {
                    Ok(real_entry) => decrypted.push(real_entry),
                    Err(e) => tracing::warn!("⚠️  Skipping undecryptable log entry: {}", e),
                }
            } else {
                decrypted.push(entry);
            }
        }
        Ok(decrypted)
    }

    fn stream_log_into(&self, f: &mut dyn FnMut(LogEntry, u32) -> ControlFlow<(), ()>) -> Result<u64, DbError> {
        let mut count = 0u64;
        self.inner.stream_log_into(&mut |enc_entry, length| {
            if enc_entry.cmd == "ENC" {
                match self.decrypt_entry(&enc_entry) {
                    Ok(real_entry) => {
                        let res = f(real_entry, length);
                        if let ControlFlow::Continue(_) = res { count += 1; }
                        res
                    }
                    Err(e) => {
                        tracing::warn!("⚠️  Skipping undecryptable log entry during streaming: {}", e);
                        ControlFlow::Continue(())
                    }
                }
            } else {
                let res = f(enc_entry, length);
                if let ControlFlow::Continue(_) = res { count += 1; }
                res
            }
        })?;
        Ok(count)
    }
}
