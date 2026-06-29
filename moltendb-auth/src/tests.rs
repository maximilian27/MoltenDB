// ─── moltendb-auth unit tests ─────────────────────────────────────────────────
//
// Coverage:
//   - refresh_scoped_token: happy path, admin token blocked, expired token,
//     revoked token, replay protection (old jti revoked after refresh)
//   - load_from_file: missing file → empty store, tampered sig → Err,
//     round-trip save → load → same entries
//   - verify_token: token without jti is rejected
//   - UserStore::new: valid credentials succeed, verify_user correct/wrong password
//   - Claims::is_admin / has_access / key_matches

use std::time::{Duration, Instant};

use crate::{
    create_scoped_token, create_token, refresh_scoped_token,
    store::{RevocationStore, UserStore},
    types::AuthError,
    verify_token,
};

// ─── helpers ──────────────────────────────────────────────────────────────────

/// Mint a short-lived scoped token that expires in `secs` seconds.
fn scoped_token(scopes: Vec<&str>, ttl_secs: u64) -> (String, String) {
    create_scoped_token(
        "test_user",
        scopes.into_iter().map(String::from).collect(),
        ttl_secs,
    )
    .expect("create_scoped_token should not fail in tests")
}

/// Mint an admin token (root *:*:*) with the given TTL.
fn admin_token(ttl_secs: u64) -> String {
    create_token("root", ttl_secs).expect("create_token should not fail in tests")
}

// ─── refresh_scoped_token ─────────────────────────────────────────────────────

#[test]
fn refresh_scoped_token_happy_path() {
    let store = RevocationStore::new();
    let (old_token, old_jti) = scoped_token(vec!["read:products:*"], 3600);

    let (new_token, new_jti) =
        refresh_scoped_token(&old_token, 3600, &store).expect("refresh should succeed");

    // New token must be different from the old one.
    assert_ne!(old_token, new_token);
    // New jti must be different from the old one.
    assert_ne!(old_jti, new_jti);

    // New token must be valid and carry the same scopes.
    let claims = verify_token(&new_token).expect("new token must be valid");
    assert_eq!(claims.sub, "test_user");
    assert!(claims.scopes.contains(&"read:products:*".to_string()));

    // Old jti must now be in the revocation store.
    assert!(
        store.is_revoked(&old_jti),
        "old jti must be revoked after refresh"
    );
    // New jti must NOT be in the revocation store.
    assert!(!store.is_revoked(&new_jti), "new jti must not be revoked");
}

#[test]
fn refresh_scoped_token_admin_token_blocked() {
    let store = RevocationStore::new();
    let token = admin_token(3600);

    let result = refresh_scoped_token(&token, 3600, &store);

    assert!(
        matches!(result, Err(AuthError::RefreshNotAllowed)),
        "admin tokens must not be refreshable, got: {:?}",
        result
    );
}

#[test]
fn refresh_scoped_token_expired_token_rejected() {
    let store = RevocationStore::new();
    // TTL of 0 produces a token that is already expired (exp == now).
    // Use 1 so it's definitely in the past by the time verify_token runs.
    // We can't easily travel time, so we create a token with a past exp by
    // manually building a Claims and encoding it.
    use crate::token::get_secret;
    use crate::types::Claims;
    use jsonwebtoken::{encode, EncodingKey, Header};
    use std::time::{SystemTime, UNIX_EPOCH};

    let past_exp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_sub(3600); // 1 hour in the past — well beyond jsonwebtoken's default leeway

    let claims = Claims {
        sub: "test_user".to_string(),
        exp: past_exp,
        scopes: vec!["read:products:*".to_string()],
        jti: uuid::Uuid::new_v4().to_string(),
    };

    let expired_token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(get_secret().as_bytes()),
    )
    .expect("encode should not fail");

    let result = refresh_scoped_token(&expired_token, 3600, &store);

    assert!(
        matches!(result, Err(AuthError::TokenExpired)),
        "expired tokens must be rejected, got: {:?}",
        result
    );
}

#[test]
fn refresh_scoped_token_revoked_token_rejected() {
    let store = RevocationStore::new();
    let (old_token, old_jti) = scoped_token(vec!["read:products:*"], 3600);

    // Manually revoke the token before attempting refresh.
    store.revoke(&old_jti, Instant::now() + Duration::from_secs(3600));

    let result = refresh_scoped_token(&old_token, 3600, &store);

    assert!(
        matches!(result, Err(AuthError::TokenRevoked)),
        "revoked tokens must be rejected, got: {:?}",
        result
    );
}

#[test]
fn refresh_scoped_token_replay_protection() {
    // After a successful refresh, the old token's jti is in the revocation store.
    // A second refresh attempt with the old token must fail.
    let store = RevocationStore::new();
    let (old_token, old_jti) = scoped_token(vec!["write:orders:*"], 3600);

    // First refresh — must succeed.
    refresh_scoped_token(&old_token, 3600, &store).expect("first refresh should succeed");

    // Old jti is now revoked.
    assert!(store.is_revoked(&old_jti));

    // Second refresh with the same old token — must fail.
    let result = refresh_scoped_token(&old_token, 3600, &store);
    assert!(
        matches!(result, Err(AuthError::TokenRevoked)),
        "replayed old token must be rejected, got: {:?}",
        result
    );
}

#[test]
fn refresh_scoped_token_preserves_scopes() {
    let store = RevocationStore::new();
    let scopes = vec!["read:products:*", "write:inventory:item_1"];
    let (old_token, _) = scoped_token(scopes.clone(), 3600);

    let (new_token, _) =
        refresh_scoped_token(&old_token, 3600, &store).expect("refresh should succeed");

    let claims = verify_token(&new_token).expect("new token must be valid");
    for scope in &scopes {
        assert!(
            claims.scopes.contains(&scope.to_string()),
            "scope '{}' must be preserved after refresh",
            scope
        );
    }
}

// ─── verify_token ─────────────────────────────────────────────────────────────

#[test]
fn verify_token_valid_token_succeeds() {
    let (token, jti) = scoped_token(vec!["read:*:*"], 3600);
    let claims = verify_token(&token).expect("valid token must verify");
    assert_eq!(claims.sub, "test_user");
    assert_eq!(claims.jti, jti);
}

#[test]
fn verify_token_tampered_token_rejected() {
    let (token, _) = scoped_token(vec!["read:*:*"], 3600);
    // Flip the last character of the signature to invalidate it.
    let mut tampered = token.clone();
    let last = tampered.pop().unwrap();
    tampered.push(if last == 'a' { 'b' } else { 'a' });

    let result = verify_token(&tampered);
    assert!(
        matches!(result, Err(AuthError::InvalidToken(_))),
        "tampered token must be rejected, got: {:?}",
        result
    );
}

#[test]
fn verify_token_expired_token_returns_token_expired() {
    use crate::token::get_secret;
    use crate::types::Claims;
    use jsonwebtoken::{encode, EncodingKey, Header};
    use std::time::{SystemTime, UNIX_EPOCH};

    let past_exp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_sub(3600); // 1 hour in the past — well beyond jsonwebtoken's default leeway

    let claims = Claims {
        sub: "test_user".to_string(),
        exp: past_exp,
        scopes: vec!["read:*:*".to_string()],
        jti: uuid::Uuid::new_v4().to_string(),
    };

    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(get_secret().as_bytes()),
    )
    .unwrap();

    assert!(
        matches!(verify_token(&token), Err(AuthError::TokenExpired)),
        "expired token must return TokenExpired"
    );
}

// ─── load_from_file ───────────────────────────────────────────────────────────

#[test]
fn load_from_file_missing_file_returns_empty_store() {
    let result = RevocationStore::load_from_file("/tmp/moltendb_test_nonexistent_revocations.json");
    assert!(result.is_ok(), "missing file must return Ok(empty store)");
    let store = result.unwrap();
    // An empty store should not consider any jti revoked.
    assert!(!store.is_revoked("any-jti"));
}

#[test]
fn load_from_file_tampered_entries_returns_err() {
    use std::io::Write;

    // Write a file with a valid structure but a wrong signature.
    let path = std::env::temp_dir().join("moltendb_test_tampered_revocations.json");
    let path_str = path.to_str().unwrap();

    let fake_payload = serde_json::json!({
        "entries": { "some-jti": 9999999999u64 },
        "sig": "0000000000000000000000000000000000000000000000000000000000000000"
    });

    let mut f = std::fs::File::create(&path).unwrap();
    write!(f, "{}", fake_payload).unwrap();

    let result = RevocationStore::load_from_file(path_str);
    let _ = std::fs::remove_file(&path);

    assert!(
        result.is_err(),
        "tampered revocation file must return Err, got Ok"
    );
}

#[test]
fn load_from_file_missing_sig_field_returns_err() {
    use std::io::Write;

    let path = std::env::temp_dir().join("moltendb_test_nosig_revocations.json");
    let path_str = path.to_str().unwrap();

    // Valid JSON but no "sig" field.
    let payload = serde_json::json!({ "entries": { "some-jti": 9999999999u64 } });

    let mut f = std::fs::File::create(&path).unwrap();
    write!(f, "{}", payload).unwrap();

    let result = RevocationStore::load_from_file(path_str);
    let _ = std::fs::remove_file(&path);

    assert!(result.is_err(), "file without sig field must return Err");
}

#[tokio::test]
async fn load_from_file_round_trip() {
    // Save a store with a known entry, then load it back and verify the entry is present.
    let path = std::env::temp_dir().join("moltendb_test_roundtrip_revocations.json");
    let path_str = path.to_str().unwrap();

    let store = RevocationStore::new();
    let jti = "round-trip-jti-1234";
    // Prune deadline: 1 hour from now.
    store.revoke(jti, Instant::now() + Duration::from_secs(3600));

    store.save_to_file(path_str).await;

    let loaded = RevocationStore::load_from_file(path_str).expect("round-trip load must succeed");

    let _ = std::fs::remove_file(&path);

    assert!(
        loaded.is_revoked(jti),
        "jti must still be revoked after round-trip save/load"
    );
}

#[tokio::test]
async fn load_from_file_expired_entries_are_skipped() {
    // Entries whose prune deadline is in the past must be silently dropped on load.
    use crate::hmac::hmac_sign;
    use std::io::Write;

    let path = std::env::temp_dir().join("moltendb_test_expired_entries.json");
    let path_str = path.to_str().unwrap();

    // Build a file with one entry whose unix prune time is in the past (unix ts 1).
    let entries = serde_json::json!({ "expired-jti": 1u64 });
    let entries_json = serde_json::to_string(&entries).unwrap();
    let sig = hmac_sign(entries_json.as_bytes());

    let payload = serde_json::json!({ "entries": entries, "sig": sig });

    let mut f = std::fs::File::create(&path).unwrap();
    write!(f, "{}", payload).unwrap();

    let loaded = RevocationStore::load_from_file(path_str)
        .expect("load must succeed even if all entries are expired");

    let _ = std::fs::remove_file(&path);

    assert!(
        !loaded.is_revoked("expired-jti"),
        "expired entries must be dropped on load"
    );
}

// ─── UserStore ────────────────────────────────────────────────────────────────

#[test]
fn user_store_new_valid_credentials() {
    let store = UserStore::new("admin".to_string(), "correct-password".to_string())
        .expect("UserStore::new must succeed with valid credentials");

    assert!(
        store.verify_user("admin", "correct-password"),
        "correct password must verify"
    );
}

#[test]
fn user_store_verify_wrong_password() {
    let store = UserStore::new("admin".to_string(), "correct-password".to_string()).unwrap();

    assert!(
        !store.verify_user("admin", "wrong-password"),
        "wrong password must not verify"
    );
}

#[test]
fn user_store_verify_unknown_user() {
    let store = UserStore::new("admin".to_string(), "password".to_string()).unwrap();

    assert!(
        !store.verify_user("unknown", "password"),
        "unknown username must not verify"
    );
}

// ─── Claims ───────────────────────────────────────────────────────────────────

#[test]
fn claims_is_admin_true_for_wildcard_scope() {
    let token = admin_token(3600);
    let claims = verify_token(&token).unwrap();
    assert!(claims.is_admin());
}

#[test]
fn claims_is_admin_false_for_scoped_token() {
    let (token, _) = scoped_token(vec!["read:products:*"], 3600);
    let claims = verify_token(&token).unwrap();
    assert!(!claims.is_admin());
}

#[test]
fn claims_has_access_exact_match() {
    let (token, _) = scoped_token(vec!["read:products:lp1"], 3600);
    let claims = verify_token(&token).unwrap();
    assert!(claims.has_access("read", "products", "lp1"));
    assert!(!claims.has_access("read", "products", "lp2"));
    assert!(!claims.has_access("write", "products", "lp1"));
}

#[test]
fn claims_has_access_collection_wildcard() {
    let (token, _) = scoped_token(vec!["write:inventory:*"], 3600);
    let claims = verify_token(&token).unwrap();
    assert!(claims.has_access("write", "inventory", "item_1"));
    assert!(claims.has_access("write", "inventory", "item_999"));
    assert!(!claims.has_access("write", "products", "item_1"));
    assert!(!claims.has_access("read", "inventory", "item_1"));
}

#[test]
fn claims_has_access_admin_grants_everything() {
    let token = admin_token(3600);
    let claims = verify_token(&token).unwrap();
    assert!(claims.has_access("read", "any_collection", "any_key"));
    assert!(claims.has_access("write", "any_collection", "any_key"));
    assert!(claims.has_access("delete", "any_collection", "any_key"));
}

// ─── key_matches ──────────────────────────────────────────────────────────────

#[test]
fn key_matches_full_wildcard() {
    use crate::hmac::key_matches;
    assert!(key_matches("*", "anything"));
    assert!(key_matches("*", ""));
}

#[test]
fn key_matches_prefix_wildcard() {
    use crate::hmac::key_matches;
    assert!(key_matches("store_A_*", "store_A_item1"));
    assert!(key_matches("store_A_*", "store_A_"));
    assert!(!key_matches("store_A_*", "store_B_item1"));
    assert!(!key_matches("store_A_*", "item1"));
}

#[test]
fn key_matches_exact() {
    use crate::hmac::key_matches;
    assert!(key_matches("lp1", "lp1"));
    assert!(!key_matches("lp1", "lp2"));
    assert!(!key_matches("lp1", "lp1_extra"));
}
