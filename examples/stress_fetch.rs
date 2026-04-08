/// Concurrent fetch stress test — fires 10 000 simultaneous GET requests
/// against a live MoltenDB server and reports latency percentiles + throughput.
///
/// Usage:
///   cargo run --example stress_fetch
///
/// Environment variables (all optional):
///   MOLTENDB_URL        — base URL,          default https://localhost:1538
///   MOLTENDB_USER       — username,           default admin
///   MOLTENDB_PASS       — password,           default admin123
///   MOLTENDB_TOKEN      — skip login, use JWT directly
///   STRESS_CONCURRENCY  — parallel requests,  default 10000
///   STRESS_COLLECTION   — collection name,    default stress
///
/// Run `cargo run --example generate_stress_data` and
/// `cargo run --example stress_insert` first so the data exists on the server.
use std::sync::Arc;
use std::time::{Duration, Instant};

use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::Client;
use serde_json::Value;
use tokio::sync::Semaphore;

fn env(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn bar(n: u64) -> ProgressBar {
    let pb = ProgressBar::new(n);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.cyan} [{elapsed_precise}] [{bar:45.cyan/blue}] {pos}/{len} ({percent}%) — {msg}",
        )
        .unwrap()
        .progress_chars("█▉▊▋▌▍▎▏ "),
    );
    pb.enable_steady_tick(Duration::from_millis(80));
    pb
}

#[tokio::main]
async fn main() {
    let base_url = env("MOLTENDB_URL", "https://localhost:1538");
    let user     = env("MOLTENDB_USER", "admin");
    let pass     = env("MOLTENDB_PASS", "admin123");
    let concurrency: usize = env("STRESS_CONCURRENCY", "100000")
        .parse()
        .expect("STRESS_CONCURRENCY must be a number");
    let collection = env("STRESS_COLLECTION", "stress");

    // ── Header ───────────────────────────────────────────────────────────────
    println!();
    println!("{}", "╔══════════════════════════════════════════════╗".bright_cyan());
    println!("{}", "║     🔥  MoltenDB  Stress Fetch  🔥           ║".bright_cyan());
    println!("{}", "╚══════════════════════════════════════════════╝".bright_cyan());
    println!();

    // ── 1. Build async client ────────────────────────────────────────────────
    let client = Client::builder()
        .danger_accept_invalid_certs(true)
        .pool_max_idle_per_host(512)
        .timeout(Duration::from_secs(30))
        .build()
        .expect("build HTTP client");

    // ── 2. Authenticate ──────────────────────────────────────────────────────
    let token = if let Ok(t) = std::env::var("MOLTENDB_TOKEN") {
        println!("  {} Using provided token.", "🔑".bright_yellow());
        t
    } else {
        print!("  {} Logging in as '{}'… ", "🔐".bright_yellow(), user.bright_white());
        let resp: Value = client
            .post(format!("{}/login", base_url))
            .json(&serde_json::json!({ "username": user, "password": pass }))
            .send()
            .await
            .expect("login request")
            .json()
            .await
            .expect("login response JSON");
        let t = resp["token"]
            .as_str()
            .expect("no token in login response")
            .to_string();
        println!("{}", "✅ OK".bright_green());
        t
    };

    // ── 3. Load keys ─────────────────────────────────────────────────────────
    let raw = std::fs::read_to_string("tests/stress_keys.json")
        .expect("tests/stress_keys.json not found — run generate_stress_data first");
    let all_keys: Vec<String> = serde_json::from_str(&raw).expect("parse stress_keys.json");
    let total_keys = all_keys.len();

    println!(
        "  {} Loaded {} keys  •  collection: {}  •  concurrency: {}",
        "📦".bright_yellow(),
        total_keys.to_string().bright_white(),
        collection.bright_white(),
        concurrency.to_string().bright_white(),
    );
    println!();

    // ── 4. Spawn tasks ───────────────────────────────────────────────────────
    let client     = Arc::new(client);
    let token      = Arc::new(token);
    let base_url   = Arc::new(base_url);
    let collection = Arc::new(collection);
    let all_keys   = Arc::new(all_keys);
    let sem        = Arc::new(Semaphore::new(4096));

    let pb = bar(concurrency as u64);
    pb.set_message("firing requests…");

    let overall_start = Instant::now();
    let mut handles   = Vec::with_capacity(concurrency);

    for i in 0..concurrency {
        let client     = Arc::clone(&client);
        let token      = Arc::clone(&token);
        let base_url   = Arc::clone(&base_url);
        let collection = Arc::clone(&collection);
        let all_keys   = Arc::clone(&all_keys);
        let sem        = Arc::clone(&sem);
        let pb_clone   = pb.clone();
        let key        = all_keys[i % total_keys].clone();

        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore closed");
            let t0      = Instant::now();

            let result = client
                .post(format!("{}/get", base_url))
                .header("Authorization", format!("Bearer {}", token))
                .json(&serde_json::json!({
                    "collection": *collection,
                    "keys": key
                }))
                .send()
                .await;

            let elapsed = t0.elapsed();
            pb_clone.inc(1);

            match result {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    let _ = resp.bytes().await;
                    (status, elapsed, None::<String>)
                }
                Err(e) => (0u16, elapsed, Some(e.to_string())),
            }
        }));
    }

    // ── 5. Collect results ───────────────────────────────────────────────────
    let mut latencies_ms: Vec<f64> = Vec::with_capacity(concurrency);
    let mut ok      = 0usize;
    let mut errors  = 0usize;
    let mut non_200 = 0usize;

    for handle in handles {
        match handle.await {
            Ok((status, elapsed, err)) => {
                latencies_ms.push(elapsed.as_secs_f64() * 1000.0);
                if err.is_some()    { errors  += 1; }
                else if status == 200 { ok    += 1; }
                else                  { non_200 += 1; }
            }
            Err(_) => errors += 1,
        }
    }

    let wall_secs = overall_start.elapsed().as_secs_f64();
    pb.finish_with_message(format!("done in {:.2}s", wall_secs));

    // ── 6. Percentiles ───────────────────────────────────────────────────────
    latencies_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = latencies_ms.len();

    let percentile = |p: f64| -> f64 {
        if n == 0 { return 0.0; }
        let idx = ((p / 100.0) * n as f64).ceil() as usize;
        latencies_ms[idx.min(n) - 1]
    };

    let mean: f64 = latencies_ms.iter().sum::<f64>() / n as f64;
    let min  = latencies_ms.first().copied().unwrap_or(0.0);
    let max  = latencies_ms.last().copied().unwrap_or(0.0);

    // colour helpers
    let ok_pct = ok as f64 / concurrency as f64 * 100.0;
    let ok_str = if ok_pct >= 99.0 {
        format!("{} ({:.1}%)", ok, ok_pct).bright_green().to_string()
    } else if ok_pct >= 90.0 {
        format!("{} ({:.1}%)", ok, ok_pct).yellow().to_string()
    } else {
        format!("{} ({:.1}%)", ok, ok_pct).bright_red().to_string()
    };

    let non200_str = if non_200 == 0 {
        "0".bright_green().to_string()
    } else {
        non_200.to_string().bright_red().to_string()
    };

    let err_str = if errors == 0 {
        "0".bright_green().to_string()
    } else {
        errors.to_string().bright_red().to_string()
    };

    let lat_color = |ms: f64| -> String {
        if ms < 50.0       { format!("{:>8.2} ms", ms).bright_green().to_string() }
        else if ms < 200.0 { format!("{:>8.2} ms", ms).yellow().to_string() }
        else               { format!("{:>8.2} ms", ms).bright_red().to_string() }
    };

    // ── 7. Report ────────────────────────────────────────────────────────────
    println!();
    println!("{}", "╔══════════════════════════════════════════════╗".bright_cyan());
    println!("{}", "║      MoltenDB Concurrent Fetch — Report      ║".bright_cyan());
    println!("{}", "╠══════════════════════════════════════════════╣".bright_cyan());
    println!(
        "{}  {:<22} {}",
        "║".bright_cyan(),
        "Total requests".bright_white(),
        format!("{:<20}", concurrency).bright_white()
    );
    println!(
        "{}  {:<22} {}",
        "║".bright_cyan(),
        "✅  200 OK".bright_white(),
        format!("{:<20}", ok_str)
    );
    println!(
        "{}  {:<22} {}",
        "║".bright_cyan(),
        "⚠️   Non-200".bright_white(),
        format!("{:<20}", non200_str)
    );
    println!(
        "{}  {:<22} {}",
        "║".bright_cyan(),
        "❌  Errors".bright_white(),
        format!("{:<20}", err_str)
    );
    println!(
        "{}  {:<22} {}",
        "║".bright_cyan(),
        "⏱   Wall time".bright_white(),
        format!("{:.3}s", wall_secs).bright_white()
    );
    println!(
        "{}  {:<22} {}",
        "║".bright_cyan(),
        "🚀  Throughput".bright_white(),
        format!("{:.0} req/s", concurrency as f64 / wall_secs).bright_white()
    );
    println!("{}", "╠══════════════════════════════════════════════╣".bright_cyan());
    println!("{}", "║  Latency breakdown                           ║".bright_cyan());
    println!("{}", "╠══════════════════════════════════════════════╣".bright_cyan());

    let rows: &[(&str, f64)] = &[
        ("min",   min),
        ("mean",  mean),
        ("p50",   percentile(50.0)),
        ("p75",   percentile(75.0)),
        ("p90",   percentile(90.0)),
        ("p95",   percentile(95.0)),
        ("p99",   percentile(99.0)),
        ("p99.9", percentile(99.9)),
        ("max",   max),
    ];

    for (label, val) in rows {
        println!(
            "{}  {:<10} {}",
            "║".bright_cyan(),
            label.bright_white(),
            lat_color(*val)
        );
    }

    println!("{}", "╚══════════════════════════════════════════════╝".bright_cyan());
    println!();
}
