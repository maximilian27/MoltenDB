// Regression test for the WASM/refresh TTL bug.
//
// The collection-level TTL expiry is persisted to the WAL as a `LogEntry`
// whose `cmd` is the plain string "TTL_EXPIRY" (it is not one of the tokenized
// commands). A decoding bug ran every unknown plain-string command through
// `LogCommand::to_ikey`, which returns "" for anything it doesn't recognise —
// so "TTL_EXPIRY" decoded to an empty `cmd` and its replay handler in
// `apply_entry` never fired. As a result the in-memory `ttl_expiry` map was
// never repopulated after a reload, so expired collections re-appeared after a
// browser tab refresh.
//
// This test locks in the fix: an unknown plain-string command must survive the
// encode → decode round-trip unchanged.

use moltendb_core::common::log_commands::LogCommand;
use moltendb_core::common::system_field_tokens::{log_entry_from_msgpack, log_entry_to_msgpack};
use moltendb_core::engine::LogEntry;
use serde_json::Value;

#[test]
fn ttl_expiry_cmd_survives_wal_roundtrip() {
    let expires_at: u64 = 1_700_000_000_000;
    let entry = LogEntry::new(
        "TTL_EXPIRY".to_string(),
        "analytics".to_string(),
        expires_at.to_string(),
        Value::Null,
    );

    let encoded = log_entry_to_msgpack(&entry).expect("encode TTL_EXPIRY entry");
    let decoded = log_entry_from_msgpack(&encoded).expect("decode TTL_EXPIRY entry");

    // The command name must be preserved so `apply_entry`'s "TTL_EXPIRY" arm
    // fires during replay. Before the fix this was an empty string.
    assert_eq!(decoded.cmd, "TTL_EXPIRY");
    assert_eq!(decoded.collection, "analytics");
    assert_eq!(decoded.key, expires_at.to_string());
}

#[test]
fn drop_cmd_survives_wal_roundtrip() {
    // DROP is a tokenized command; confirm it still round-trips to its IKEY form
    // so an explicit collection drop stays gone across a reload.
    let entry = LogEntry::new(
        LogCommand::IKEY_DROP.to_string(),
        "analytics".to_string(),
        "*".to_string(),
        Value::Null,
    );

    let encoded = log_entry_to_msgpack(&entry).expect("encode DROP entry");
    let decoded = log_entry_from_msgpack(&encoded).expect("decode DROP entry");

    assert_eq!(decoded.cmd, LogCommand::IKEY_DROP);
    assert_eq!(decoded.collection, "analytics");
}
