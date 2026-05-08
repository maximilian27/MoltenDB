/// Stress-insert synthetic documents into a live MoltenDB server in safe chunks.
///
/// Usage:
///   cargo run --example stress_insert
///
/// Environment variables (all optional):
///   MOLTENDB_URL   — base URL, default https://localhost:1538
///   MOLTENDB_USER  — username,  default admin
///   MOLTENDB_PASS  — password,  default admin123
///   MOLTENDB_TOKEN — skip login and use this JWT directly
///
/// Run `cargo run --example generate_stress_data` first to produce
/// `tests/stress_data.json`.
use serde_json::Value;
use std::time::Instant;

fn env(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn main() {
    let base_url = env("MOLTENDB_URL", "https://localhost:1538");
    let user = env("MOLTENDB_ROOT_USER", "admin");
    let pass = env("MOLTENDB_ROOT_PASSWORD", "admin123");

    // Build a client that accepts self-signed TLS certs (dev server).
    let client = reqwest::blocking::Client::builder()
        .danger_accept_invalid_certs(true)
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

    // ── 2. Load and Chunk batches ─────────────────────────────────────────────
    let raw = std::fs::read_to_string("tests/stress_data.json")
        .expect("tests/stress_data.json not found — run generate_stress_data first");
    let original_batches: Vec<Value> =
        serde_json::from_str(&raw).expect("parse stress_data.json");

    let chunk_size = 500; // ⚠️ SAFE LIMIT: 500 docs per HTTP request
    let mut safe_batches = Vec::new();

    for batch in original_batches {
        if let Some(data_obj) = batch["data"].as_object() {
            let mut current_chunk = serde_json::Map::new();

            for (k, v) in data_obj {
                current_chunk.insert(k.clone(), v.clone());

                // When we hit 500, push it as a new batch and clear the chunk
                if current_chunk.len() == chunk_size {
                    let mut new_batch = batch.clone();
                    new_batch["data"] = Value::Object(current_chunk.clone());
                    safe_batches.push(new_batch);
                    current_chunk.clear();
                }
            }

            // Push any remaining documents that didn't perfectly divide by 500
            if !current_chunk.is_empty() {
                let mut new_batch = batch.clone();
                new_batch["data"] = Value::Object(current_chunk);
                safe_batches.push(new_batch);
            }
        }
    }

    let total_batches = safe_batches.len();
    let total_docs: usize = safe_batches.iter()
        .filter_map(|b| b["data"].as_object().map(|d| d.len()))
        .sum();

    println!(
        "Inserting {} docs in {} safe chunks (max {} docs/req) …",
        total_docs, total_batches, chunk_size
    );

    // ── 3. Insert batches ─────────────────────────────────────────────────────
    let overall = Instant::now();
    for (idx, batch) in safe_batches.iter().enumerate() {
        let t0 = Instant::now();
        let current_batch_size = batch["data"].as_object().unwrap().len();

        // Retry loop: back off on 429 rate-limit responses.
        let (status, body) = loop {
            let resp = client
                .post(format!("{}/set", base_url))
                .header("Authorization", format!("Bearer {}", token))
                .json(batch)
                .send()
                .unwrap_or_else(|e| panic!("batch {} send error: {}", idx, e));

            let s = resp.status();
            let b: Value = resp.json().unwrap_or(Value::Null);

            if s.as_u16() == 429 {
                eprintln!(
                    "Batch {:>3}/{} rate-limited (429) — waiting 30s…",
                    idx + 1,
                    total_batches
                );
                std::thread::sleep(std::time::Duration::from_secs(30));
            } else {
                break (s, b);
            }
        };

        let elapsed = t0.elapsed().as_secs_f64();
        let docs_per_sec = current_batch_size as f64 / elapsed;

        if !status.is_success() {
            eprintln!(
                "Batch {:>3}/{} FAILED (HTTP {}): {}",
                idx + 1,
                total_batches,
                status,
                body
            );
        } else {
            println!(
                "Batch {:>3}/{} — {:.3}s  ({:.0} docs/s)",
                idx + 1,
                total_batches,
                elapsed,
                docs_per_sec
            );
        }
    }

    let total_elapsed = overall.elapsed().as_secs_f64();
    println!(
        "\nDone. {} docs in {:.2}s  ({:.0} docs/s overall)",
        total_docs,
        total_elapsed,
        total_docs as f64 / total_elapsed
    );

    // ── 4. Spot-check a few known keys ────────────────────────────────────────
    let spot_keys = ["stress_000000", "stress_001000", "stress_050000", "stress_099999"];
    println!("\nSpot-checking {} keys…", spot_keys.len());
    for key in &spot_keys {
        let resp: Value = client
            .post(format!("{}/get", base_url))
            .header("Authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "collection": "stress",
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