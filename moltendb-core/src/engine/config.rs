use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbConfig {
    /// Path to the database file (e.g., "my_database.log")
    pub path: String,
    /// Enable tiered storage (hot + cold log)
    pub tiered_mode: bool,
    /// Force synchronous writes (no data loss, lower performance)
    pub sync_mode: bool,
    /// Max documents to keep in RAM per collection
    pub hot_threshold: usize,
    /// Rate limiting: max requests per window (server-only, None = disabled/use default)
    pub rate_limit_requests: Option<u32>,
    /// Rate limiting: window size in seconds (server-only, None = disabled/use default)
    pub rate_limit_window: Option<u64>,
    /// Max request body size in bytes
    pub max_body_size: usize,
    /// Max keys allowed per request (default: 1000)
    pub max_keys_per_request: usize,
    /// Optional encryption key (32 bytes)
    #[serde(skip)]
    pub encryption_key: Option<[u8; 32]>,
    /// Optional script to run after backups
    pub post_backup_script: Option<String>,
    /// Run entirely in RAM — no disk I/O, all data lost on exit
    pub in_memory: bool,
}

impl Default for DbConfig {
    fn default() -> Self {
        Self {
            path: "molten.db".to_string(),
            tiered_mode: true,
            sync_mode: false,
            hot_threshold: 50000,
            rate_limit_requests: Some(1000),
            rate_limit_window: Some(60),
            max_body_size: 10 * 1024 * 1024,
            max_keys_per_request: 1000,
            encryption_key: None,
            post_backup_script: None,
            in_memory: false,
        }
    }
}
