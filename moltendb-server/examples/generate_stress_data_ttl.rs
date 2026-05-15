/// Generate 1,000,000 synthetic laptop-like documents for stress testing with TTL expiration.
/// TTL is collection-level -- register a default TTL on the target collection via POST /schema
/// before inserting this data so that all documents receive `_expiresAt` automatically.
/// Writes `tests/stress_data.json` (100 batches of 10,000) and
/// `tests/stress_keys.json` (flat list of all keys).
use serde_json::{json, Value};
use std::fs;

const BRANDS: &[&str] = &["Lenovo", "Apple", "Asus", "Dell", "Razer", "Framework", "HP", "Acer"];
const PANELS: &[&str] = &["IPS", "OLED", "Mini-LED", "VA", "TN"];
const TAGS: &[&[&str]] = &[
    &["business", "ultrabook", "lightweight"],
    &["creative", "professional", "macos"],
    &["gaming", "amd", "portable"],
    &["gaming", "windows", "rgb"],
    &["modular", "linux", "budget"],
    &["creative", "windows", "4k"],
    &["student", "budget", "lightweight"],
    &["workstation", "professional", "windows"],
];

fn synthetic_entry(i: usize) -> Value {
    let brand = BRANDS[i % BRANDS.len()];
    let model = format!("Model-{:05}", i);
    let price = 499 + (i % 3001) as u64;
    let in_stock = i % 3 != 0;
    let cores = 4 + (i % 13) as u64;
    let ghz = 2.0 + (i % 30) as f64 * 0.1;
    let battery = 40 + (i % 61) as u64;
    let weight = 1.0 + (i % 15) as f64 * 0.1;
    let panel = PANELS[i % PANELS.len()];
    let size = 13.0 + (i % 5) as f64 * 0.5;
    let refresh = [60u64, 90, 120, 144, 165][i % 5];
    let mem_gb = [8u64, 16, 32, 64][(i / 4) % 4];
    let tags: Vec<&str> = TAGS[i % TAGS.len()].to_vec();
    json!({
        "brand": brand,
        "model": model,
        "price": price,
        "in_stock": in_stock,
        "tags": tags,
        "specs": {
            "cpu": { "brand": brand, "cores": cores, "ghz": ghz },
            "battery_wh": battery,
            "weight_kg": weight
        },
        "display": {
            "size_inch": size,
            "panel": panel,
            "refresh_hz": refresh,
            "hdr": refresh >= 120
        },
        "memory": {
            "capacity_gb": mem_gb,
            "type": if mem_gb <= 16 { "LPDDR5" } else { "DDR5" }
        }
    })
}

fn main() {
    let total = 100_000usize;
    let batch_size = 10_000usize;
    let batches = total / batch_size;

    let mut all_batches: Vec<Value> = Vec::with_capacity(batches);
    let mut all_keys: Vec<String> = Vec::with_capacity(total);

    for b in 0..batches {
        let mut data = serde_json::Map::new();
        for j in 0..batch_size {
            let i = b * batch_size + j;
            let key = format!("stress_{:06}", i);
            all_keys.push(key.clone());
            data.insert(key, synthetic_entry(i));
        }
        all_batches.push(json!({
            "collection": "stress",
            "data": Value::Object(data)
        }));
    }

    fs::create_dir_all("tests").expect("create tests dir");
    fs::write(
        "tests/stress_data.json",
        serde_json::to_string_pretty(&all_batches).unwrap(),
    )
    .expect("write stress_data.json");

    fs::write(
        "tests/stress_keys.json",
        serde_json::to_string_pretty(&all_keys).unwrap(),
    )
    .expect("write stress_keys.json");

    println!("Generated {} entries across {} batches (TTL collection).", total, batches);
    println!("  Register a collection TTL first: POST /schema {{ \"collection\": \"stress\", \"ttl\": 60 }}");
    println!("  -> tests/stress_data.json");
    println!("  -> tests/stress_keys.json");
}
