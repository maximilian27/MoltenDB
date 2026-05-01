/// Stress-insert synthetic documents into a live MoltenDB server
/// as a **single unbatched HTTP request** (no chunking).
///
/// Usage:
///   cargo run --example stress_insert_unbatched
///
/// Environment variables (all optional):
///   MOLTENDB_URL   — base URL, default https://localhost:1538
///   MOLTENDB_USER  — username,  default admin
///   MOLTENDB_PASS  — password,  default admin123
///   MOLTENDB_TOKEN — skip login and use this JWT directly
///
/// Run `cargo run --example generate_stress_data` first to produce
/// `tests/stress_data.json`.
///
/// ⚠️  The server must be started with `--max-keys-per-request` set to at
///     least the total number of docs in stress_data.json, and
///     `--max-body-size` large enough to accept the payload.
use serde_json::Value;
use std::time::Instant;

fn env(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn main() {
    let base_url = env("MOLTENDB_URL", "https://localhost:1538");
    let user = env("MOLTENDB_USER", "admin");
    let pass = env("MOLTENDB_PASS", "admin123");

    // Build a client that accepts self-signed TLS certs (dev server).
    // Increase the timeout to 10 minutes — a large payload can be slow.
    let client = reqwest::blocking::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .expect("build HTTP client");

    // ── 1. Authenticate ──────────────────────────────────────────────────────
    let token = if let Ok(t) = std::env::var("MOLTENDB_TOKEN") {
        println!("Using provided token.");
        t
    } else {
        println!("Logging in as '{}'…", user);
        let resp: Value = client
            .post(format!("{}/login", base_url))
            .json(&serde_json::json!({ "username": user, "password": pass }))
            .send()
            .expect("login request")
            .json()
            .expect("login response JSON");
        let t = resp["token"]
            .as_str()
            .expect("no token in login response")
            .to_string();
        println!("Login OK.");
        t
    };

    // ── 2. Load stress_data.json and merge all batches into one payload ───────
    println!("Loading tests/stress_data.json…");
    let raw = std::fs::read_to_string("tests/stress_data.json")
        .expect("tests/stress_data.json not found — run generate_stress_data first");

    let batches: Vec<Value> = serde_json::from_str(&raw).expect("parse stress_data.json");

    // Merge all batch data maps into a single map and capture the collection name.
    let mut merged = serde_json::Map::new();
    let mut collection = String::from("stress");
    for batch in &batches {
        if let Some(col) = batch["collection"].as_str() {
            collection = col.to_string();
        }
        if let Some(data_obj) = batch["data"].as_object() {
            for (k, v) in data_obj {
                merged.insert(k.clone(), v.clone());
            }
        }
    }

    let total_docs = merged.len();
    let payload = serde_json::json!({
        "collection": collection,
        "data": Value::Object(merged)
    });

    println!(
        "Loaded {} docs — sending as a single HTTP request…",
        total_docs
    );

    // ── 3. Insert — single unbatched request ─────────────────────────────────
    let overall = Instant::now();

    // Retry loop: back off on 429 rate-limit responses.
    let (status, body) = loop {
        let resp = client
            .post(format!("{}/set", base_url))
            .header("Authorization", format!("Bearer {}", token))
            .json(&payload)
            .send()
            .expect("set request failed");

        let s = resp.status();
        let b: Value = resp.json().unwrap_or(Value::Null);

        if s.as_u16() == 429 {
            eprintln!("Rate-limited (429) — waiting 30s…");
            std::thread::sleep(std::time::Duration::from_secs(30));
        } else {
            break (s, b);
        }
    };

    let total_elapsed = overall.elapsed().as_secs_f64();

    if !status.is_success() {
        eprintln!("INSERT FAILED (HTTP {}): {}", status, body);
    } else {
        println!(
            "\nDone. {} docs in {:.2}s  ({:.0} docs/s overall)",
            total_docs,
            total_elapsed,
            total_docs as f64 / total_elapsed
        );
        println!("Server response: {}", body);
    }

    // ── 4. Spot-check a few known keys ────────────────────────────────────────
    let spot_keys = ["stress_000000", "stress_001000", "stress_050000", "stress_099999"];
    println!("\nSpot-checking {} keys…", spot_keys.len());
    for key in &spot_keys {
        let resp: Value = client
            .post(format!("{}/get", base_url))
            .header("Authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "collection": collection,
                "keys": key
            }))
            .send()
            .expect("get request")
            .json()
            .expect("get response JSON");

        let brand = resp["brand"].as_str().unwrap_or("<missing>");
        let model = resp["model"].as_str().unwrap_or("<missing>");
        println!("  {} → brand={}, model={}", key, brand, model);
    }
}
