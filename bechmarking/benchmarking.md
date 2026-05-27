# 📊 Benchmarking & Performance

To evaluate the core engine's performance under realistic workloads, a dedicated desktop benchmarking application was developed using Tauri and Rust. The complete source code and pre-compiled binaries are available in the [moltendb-benchmark-app](https://github.com/maximilian27/moltendb-benchmark-app) repository.

---

## 🖥️ Test Environment

All benchmarks were executed on the primary development hardware with the following specifications:

| Component        | Details                                          |
|------------------|--------------------------------------------------|
| **Processor**    | Intel® Core™ i9-14900HX (2.20 GHz, 24 Cores / 32 Threads) |
| **Memory**       | 32 GB DDR5 RAM                                   |
| **Operating System** | Windows 11                                   |

---

## ⚡ Performance Metrics

MoltenDB achieves exceptional speeds even when managing very large collections, demonstrating highly efficient resource utilization and minimal system impact.

---

### 📦 Bulk Ingestion (5,000,000 Documents)

| Mode             | Throughput                  | Total Insertion Time |
|------------------|-----------------------------|----------------------|
| **In-Memory**    | Over 85,289 docs/second     | ~58.6 seconds        |
| **Async WAL**    | Over 81,129 docs/second     | ~61.6 seconds        |

> **Efficiency Note:** Activating the Write-Ahead Log (WAL) for full crash resilience introduces an incredibly low performance penalty of only **~5.1%**, proving the efficiency of the underlying async group-commit architecture.

---

### 🔍 Query & Retrieval Latency

| Query Type                  | Details                                                                                                      |
|-----------------------------|--------------------------------------------------------------------------------------------------------------|
| **Key-Value Lookup**        | Fetching a single document directly by its unique key is achieved in **~0.04 ms**.                          |
| **Complex Queries (5M Dataset)** | Multi-field filtering, logical evaluations, and on-the-fly sorting stay comfortably under **1000 ms** (e.g., complex criteria queries average **~387 ms** for a 100-document slice). |
| **Smaller Collections**     | Latency scales down linearly, delivering **sub-millisecond** query execution on typical operational datasets. |
