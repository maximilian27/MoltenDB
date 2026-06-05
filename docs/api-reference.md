## HTTP API

All endpoints except `POST /login` require an `Authorization: Bearer <token>` header. Every endpoint also enforces **scopes** — the token must carry the appropriate `action:collection:key` scope or the request is rejected with `403 Forbidden`.  
All endpoints return a consistent JSON envelope with a `statusCode` field:

```json
{ "statusCode": 200, "count": 5, "status": "ok" }
```
```json
{ "statusCode": 400, "error": "Unknown property: 'foo'. Check the API docs..." }
```
```json
{ "statusCode": 404, "error": "No documents found" }
```

### Authentication

```http
POST /login
Content-Type: application/json

{ "username": "myuser", "password": "str0ng-p4ssw0rd" }
```

Returns `{ "token": "<jwt>" }`. The root token carries `*:*:*` scope (full access).

### Delegate a scoped token

The root user can mint narrow-permission JWTs for clients. Only the root user can call this endpoint.

```http
POST /auth/delegate
Authorization: Bearer <root-token>
Content-Type: application/json

{
  "client_id": "laptop-service",
  "scopes": ["read:laptops:*", "write:laptops:*"],
  "ttl_secs": 3600
}
```

Returns `{ "token": "<scoped-jwt>", "client_id": "laptop-service", "scopes": [...] }`.

**Scope format:** `action:collection:document_key`

| Scope | Meaning |
|---|---|
| `read:laptops:lp1` | Read only document `lp1` in `laptops` |
| `read:laptops:*` | Read any document in `laptops` |
| `write:laptops:*` | Write any document in `laptops` |
| `delete:laptops:*` | Delete any document in `laptops` |
| `read:*:*` | Read any document in any collection |
| `*:*:*` | Full admin — root only |

### Insert / Upsert

```http
POST /set
Content-Type: application/json
Authorization: Bearer <token>

{
  "collection": "laptops",
  "data": {
    "lp1": { "brand": "Lenovo", "model": "ThinkPad X1 Carbon", "price": 1499, "in_stock": true }
  }
}
```

Pass `data` as an **array** to auto-generate UUIDv7 keys:

```json
{ "collection": "laptops", "data": [{ "brand": "HP", "model": "Spectre x360", "price": 1599 }] }
```

Returns `{ "statusCode": 200, "status": "ok", "count": 1 }`.

Every document automatically receives the following engine-managed fields — clients cannot set any field whose name starts with `_`:

| Field | Description |
|---|---|
| `_key` | The document's own key (injected on read, never stored) |
| `_v` | Version counter — incremented on every write by the engine. Always starts at `1` for new documents. |
| `_seq` | Monotonic insertion sequence number — strictly increasing within a collection. Assigned at first insert and preserved on overwrites. Used for FIFO eviction when `maxSize` is set. **Opt-in** — only returned when explicitly listed in `fields`. |
| `_createdAt` | ISO-8601 timestamp set once at first insert, never overwritten. **Opt-in** — only returned when explicitly listed in `fields`. |
| `_modifiedAt` | ISO-8601 timestamp updated on every write. **Opt-in** — only returned when explicitly listed in `fields`. |
| `_expiresAt` | ISO-8601 timestamp when the **collection** expires. This is a **virtual field** — never stored inside documents. **Opt-in** — only returned when explicitly listed in `fields` (only relevant for TTL collections). |

Attempting to insert or update a document that contains any field starting with `_` (except `_v` on update) returns `400 Bad Request`.

**`_key` and `_v` are always present in every response** — they are protocol primitives and cannot be suppressed by `fields` or `excludedFields`.

`_seq`, `_createdAt`, `_modifiedAt`, and `_expiresAt` are **opt-in** — they are never returned unless explicitly listed in a `fields` projection:
```json
{ "collection": "laptops", "fields": ["brand", "price", "_createdAt", "_modifiedAt"] }
```

### TTL (Time-to-Live)

MoltenDB supports **collection-level TTL** — an entire collection expires and is dropped automatically after a configurable idle period. TTL is set via `/schema` (no JSON schema required) or inline on `/set`:

```json
POST /schema
{ "collection": "cache", "ttl": 300 }
```

```json
POST /set
{ "collection": "cache", "data": { "k": { "value": 1 } }, "ttl": 300 }
```

**How it works:**
- The expiry clock resets to `now + ttl_secs` at the end of **every insert batch** — so the clock measures idle time since the last write, not time since schema registration.
- On expiry the **entire collection is dropped** in one O(1) `delete_collection` call — no per-document iteration.
- `_expiresAt` is a **virtual field** — never stored inside documents. It is computed from the collection TTL map and injected into every response when the collection has a TTL.
- TTL is **immutable by design** — once set, the TTL value cannot be changed without dropping and recreating the collection. This prevents silent retroactive changes to existing data.
- `/update` calls do **not** reset the expiry clock — only `/set` (insert) does.

> **Design decision — sliding-window expiry:** The TTL clock resets on every insert, not on every access. This means a collection that receives a steady stream of writes will never expire — it only drops after `ttl_secs` of complete write inactivity. This makes MoltenDB TTL ideal for **ephemeral caches, analytics buffers, and temporary working sets** where the collection as a whole should outlive active use. It is **not** designed for per-document expiry use cases such as OTPs, password-reset tokens, or session invalidation — for those, store your own `expires_at` field in the document and use `POST /delete` with a `where` clause to clean up expired entries.

**Eviction strategy:**
- **Lazy eviction on read** — if the collection has expired, reads return `404` immediately without scanning any documents.
- **Background sweep** (server only) — an event-driven min-heap with one entry per collection wakes exactly when the next collection expires and drops it. Zero CPU usage when no TTL collections exist.
- **WASM** — lazy eviction only (no background thread in the browser).

**Example — cache collection that expires 5 minutes after the last insert:**

```json
POST /schema
{ "collection": "hot_cache", "ttl": 300 }
```

```json
POST /set
{
  "collection": "hot_cache",
  "data": {
    "item_1": { "value": 42 },
    "item_2": { "value": 99 }
  }
}
```

Response includes `_expiresAt` on every document:

```json
[
  { "_key": "item_1", "value": 42, "_expiresAt": "2026-05-15T08:00:00Z", "_v": 1, ... },
  { "_key": "item_2", "value": 99, "_expiresAt": "2026-05-15T08:00:00Z", "_v": 1, ... }
]
```

### Capped Collections (`maxSize`)

Collections can be capped to a maximum document count. When the collection exceeds `maxSize` after an insert batch, the **oldest documents** (lowest `_seq`) are evicted automatically — keeping exactly `maxSize` documents at all times.

Set via `/schema` (no JSON schema required) or inline on `/set`:

```json
POST /schema
{ "collection": "recent_events", "maxSize": 100 }
```

```json
POST /set
{ "collection": "top5_scores", "maxSize": 5, "data": { "s1": { "score": 9800 } } }
```

- Eviction is **FIFO** — the document with the lowest `_seq` is always evicted first.
- Overwrites preserve the original `_seq`, so a document's position in the eviction queue is fixed at first insert.
- `maxSize` is reported in `POST /stats` and `GET /stats` responses.
- `maxSize` can be combined with `ttl` on the same collection.

**Example — manual cleanup pattern for per-document expiry (e.g. password resets):**

```json
POST /set
{
  "collection": "password_resets",
  "data": {
    "token_abc": { "userId": "u1", "email": "a@b.com", "expires_at": 1747240200000 }
  }
}
```

```json
POST /delete
{
  "collection": "password_resets",
  "where": { "expires_at": { "$lt": 1747240200000 } }
}
```

### Query

```http
POST /get
Content-Type: application/json
Authorization: Bearer <token>

{
  "collection": "laptops",
  "where": { "brand": { "$in": ["Apple", "Dell"] }, "in_stock": true },
  "fields": ["brand", "model", "price"],
  "count": 10,
  "offset": 0
}
```

**All query properties:**

| Property | Type | Description                                                                                                                                                 |
|---|---|-------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `collection` | string | **Required.** The collection to query.                                                                                                                      |
| `keys` | string \| string[] | Fetch one or more documents by key. Returns the document directly for a single string; returns an array for an array of keys.                               |
| `where` | object | Filter documents. All conditions at the top level are ANDed together.                                                                                       |
| `fields` | string[] | **Fine-grained field projection.** Return only these fields. Dot-notation selects nested fields. Mutually exclusive with `excludedFields`.                  |
| `excludedFields` | string[] | Return everything *except* these fields. Mutually exclusive with `fields`.                                                                                  |
| `joins` | object[] | Cross-collection joins. Each element is `{ "<name>": { "from": "<collection>", "on": "<foreign_key_field>", "fields": [...] } }`.                           |
| `sort` | object[] | Sort results. Each spec is `{ "field": "<name>", "order": "asc" \| "desc" }`. Multiple specs applied in priority order.                                     |
| `count` | number | Maximum number of results to return (applied after filtering and sorting). **Defaults to `100` if not supplied. Values above `1000` return a `400` error.** |
| `offset` | number | Number of results to skip (for stable pagination, applied after sorting).                                                                                   |

> **Response shape:** All multi-document queries return a **JSON array** where each element includes a `_key` field with the document ID. The only exception is a single-key lookup (`"keys": "lp2"`) which returns the document directly.

**Supported `where` operators:**

| Operator | Aliases | Description |
|---|---|---|
| `$eq` | `$equals` | Exact equality |
| `$ne` | `$notEquals` | Not equal |
| `$gt` | `$greaterThan` | Greater than (numeric) |
| `$gte` | | Greater than or equal |
| `$lt` | `$lessThan` | Less than (numeric) |
| `$lte` | | Less than or equal |
| `$contains` | `$ct` | Substring check (string, **case-insensitive**) or membership check (array) |
| `$in` | `$oneOf` | Field value is one of a list (string comparison is **case-insensitive**) |
| `$nin` | `$notIn` | Field value is not in a list |
| `$or` | | At least one of the sub-conditions must match (array of `where`-style objects) |
| `$and` | | All sub-conditions must match (array of `where`-style objects) |

**Query examples:**

// WHERE with multiple conditions (all must match — implicit AND)
```json
{ "collection": "laptops", "where": { "brand": "Apple", "in_stock": true } }
```
// Fine-grained field projection
```json
{ "collection": "laptops", "fields": ["brand", "model", "price"] }
```
// Deep nested field selection
```json
{ "collection": "laptops", "fields": ["brand", "specs.cpu.ghz", "specs.weight_kg"] }
```
// Field exclusion
```json
{ "collection": "laptops", "excludedFields": ["memory_id", "display_id"] }
```
// Sort by price descending, then brand ascending
```json
{ "collection": "laptops", "sort": [{ "field": "price", "order": "desc" }, { "field": "brand", "order": "asc" }] }
```
// Pagination — second page of 3
```json
{ "collection": "laptops", "sort": [{ "field": "price", "order": "asc" }], "offset": 3, "count": 3 }
```
// $in — brand is one of a list
```json
{ "collection": "laptops", "where": { "brand": { "$in": ["Apple", "Dell", "Razer"] } } }
```
// $contains on an array field
```json
{ "collection": "laptops", "where": { "tags": { "$contains": "gaming" } } }
```
// $or — match documents where brand is Apple OR price is below 1000
```json
{ "collection": "laptops", "where": { "$or": [{ "brand": "Apple" }, { "price": { "$lt": 1000 } }] } }
```
// $and — match documents where brand is Apple AND price is below 2000
```json
{ "collection": "laptops", "where": { "$and": [{ "brand": "Apple" }, { "price": { "$lt": 2000 } }] } }
```

### Cross-collection join

```http
POST /get
Content-Type: application/json
Authorization: Bearer <token>

{
  "collection": "laptops",
  "fields": ["brand", "model", "price"],
  "joins": [
    {  
      "ram": { 
        "from": "memory", 
        "on": "memory_id", 
        "fields": ["capacity_gb", "type"] 
      }
    },
    { 
      "screen": { 
        "from": "display",
        "on": "display_id", 
        "fields": ["size_inch", "panel", "refresh_hz"]
      }
    }
  ]
}
```

The `on` field is read from the parent document using dot-notation and used to look up a document in the target collection. The result is embedded under the alias key. `fields` is optional — omit it to return the full joined document.

> **Note:** Joins are resolved at **query time** — the joined data is fetched live on every request. For a snapshot embedded at **insert time**, use `extends` (see below).

### Inline reference embedding (`extends`)

The `extends` key embeds data from another collection directly into the stored document at insert time — no join needed on reads.

```http
POST /set
Content-Type: application/json
Authorization: Bearer <token>

{
  "collection": "laptops",
  "data": {
    "lp7": {
      "brand": "MSI",
      "model": "Titan GT77",
      "price": 3299,
      "extends": {
        "ram":    "memory.mem4",
        "screen": "display.dsp3"
      }
    }
  }
}
```

Each value in `extends` is a `"collection.key"` reference. The engine fetches the referenced document and embeds it under the alias key. The `extends` key itself is removed from the stored document.

**When to use `extends` vs `joins`:**

| | `extends` | `joins` |
|---|---|---|
| Resolved at | Insert time (once) | Query time (every request) |
| Data freshness | Snapshot — may become stale | Always live |
| Read cost | O(1) — data already embedded | O(1) per join per document |
| Use when | Data rarely changes, fast reads matter | Data changes frequently, freshness matters |

### Patch / merge

```http
POST /update
Content-Type: application/json
Authorization: Bearer <token>

{
  "collection": "laptops",
  "data": { "lp4": { "in_stock": true, "price": 1749 } }
}
```

Only the fields in `data` are changed. All other fields are preserved. `_v` is incremented automatically; `_createdAt` cannot be overwritten.

### Delete

```http
POST /delete
Content-Type: application/json
Authorization: Bearer <token>

{ "collection": "laptops", "keys": "lp6" }              // single key
{ "collection": "laptops", "keys": ["lp4", "lp5"] }     // batch
{ "collection": "laptops", "drop": true }               // drop entire collection
{ "collection": "laptops", "where": { "in_stock": { "$eq": false } } }  // bulk delete by filter
```

The `where` clause supports every filter operator available in `/get` — `$eq`, `$ne`, `$gt`, `$gte`, `$lt`, `$lte`, `$contains`, `$in`, `$nin`, `$and`, `$or`. An optional `count` property limits how many documents are deleted (**default `100`**, max `1000`). The response includes the count of deleted documents:

```json
{ "status": "ok", "deleted": 42 }
```

### Paginated collection fetch

```http
GET /collections/laptops?limit=100&offset=0
Authorization: Bearer <token>
```

Returns all documents in the collection, with optional pagination.

---

## Query Builder (JavaScript / TypeScript)

The `@moltendb-web/query` package provides a type-safe, chainable API that works with both the HTTP server and the WASM engine.

```bash
npm install @moltendb-web/query
```

```typescript
import { MoltenDBClient, WorkerTransport, HttpTransport } from '@moltendb-web/query';

// WASM (browser)
const client = new MoltenDBClient(new WorkerTransport(worker));

// HTTP server
const client = new MoltenDBClient(new HttpTransport('https://localhost:1538', token));

// GET — chainable query
const results = await client.collection('laptops')
  .get()
  .where({ brand: 'Apple', in_stock: true })
  .fields(['brand', 'model', 'price'])
  .joins([{ 
    screen: { 
      from: 'display', on: 'display_id', fields: ['panel', 'refresh_hz'] 
    }
  }])
  .sort([{ field: 'price', order: 'asc' }])
  .count(5)
  .exec();

// SET — insert / upsert
await client.collection('laptops')
  .set({ lp1: { brand: 'Lenovo', model: 'ThinkPad X1', price: 1499 } })
  .exec();

// UPDATE — partial patch
await client.collection('laptops')
  .update({ lp4: { price: 1749, in_stock: true } })
  .exec();

// DELETE
await client.collection('laptops').delete().keys('lp6').exec();
await client.collection('laptops').delete().drop().exec();
```

Each operation class only exposes the methods that are valid for that operation — invalid method chains are caught at compile time in TypeScript.

---

## WebSocket (Real-time Push)

The WebSocket endpoint is exclusively for **real-time push notifications**. All CRUD operations must go through the HTTP endpoints.

```
wss://localhost:1538/ws
```

**Protocol:**

1. The first message **must** be `{ "action": "AUTH", "token": "<jwt>" }`. The connection is closed immediately if authentication fails, with one of the following structured error codes:

   | `error` code | Cause |
   |---|---|
   | `invalid_message` | First frame was not valid JSON or not a text frame |
   | `invalid_action` | First message was not an `AUTH` action |
   | `missing_token` | `AUTH` frame had no `token` field |
   | `invalid_token` | JWT verification failed (expired, wrong secret, malformed) |
   | `token_revoked` | Token has been revoked via `DELETE /auth/tokens/:jti` |

2. After authentication, the server pushes a change event on every write **for collections the token's scopes allow `read` access to**. Events for other collections are silently filtered out. Admin tokens (`*:*:*`) receive all events.
   ```json
   { "event": "change", "collection": "laptops", "key": "lp2", "new_v": 3 }
   ```
   ```json
   { "event": "change", "collection": "laptops", "key": "lp6", "new_v": null }
   ```
   ```json
   { "event": "change", "collection": "laptops", "key": "*",   "new_v": null }
   ```
   - `new_v` is the document's `_v` after the write, or `null` for deletes/drops
   - `key: "*"` means the entire collection was dropped
3. Clients fetch fresh data via HTTP after receiving a notification.

**Revocation on open connections:** If a token is revoked while a WebSocket connection is already open, the server will detect this within 30 seconds, send a `token_revoked` error, and close the connection.

See `src/ws_test/websocket-test.html` for an interactive tester.

---

## Collection Stats

Returns document counts per collection. Both `POST` and `GET` are supported. TTL-aware: expired collections report `count: 0` and `expired: true`.

```http
GET /stats
Authorization: Bearer <token>
```

```http
POST /stats
Content-Type: application/json
Authorization: Bearer <token>

{ "collection": "laptops" }
```

**All collections response:**
```json
{
  "collections": {
    "laptops": { "count": 42381 },
    "sessions": { "count": 1200, "expiresAt": "2026-05-15T15:00:00Z" },
    "expired_cache": { "count": 0, "expired": true, "expiresAt": "2026-05-15T07:00:00Z" }
  },
  "total": 43581
}
```

**Single collection response:**
```json
{ "collection": "laptops", "count": 42381 }
```

> **Note:** Counts are O(1) atomic reads from the in-memory DashMap — no document scanning. On TTL collections the count may include a small number of not-yet-evicted documents; expired collections are reported accurately as `count: 0`.

## Telemetry

### Health check

Public endpoint — no authentication required. Use it as a liveness probe in Docker / Kubernetes.

```http
GET /system/health
```

Response:
```json
{ "status": "ok", "message": "MoltenDB is running" }
```

### Metrics

Admin-only endpoint. Returns a structured snapshot of server uptime, process memory, host hardware, and live database internals. All values are raw integers — formatting is left to the client (MoltenDB Studio / dashboards).

```http
GET /system/metrics
Authorization: Bearer <admin-token>
```

Response:
```json
{
  "uptime_seconds": 14200,
  "process": {
    "memory_used_bytes": 20017152
  },
  "host": {
    "memory": {
      "total_bytes": 34070192128,
      "used_bytes": 17026154496,
      "free_bytes": 17044037632
    },
    "disks": [
      {
        "mount": "C:\\",
        "total_bytes": 1022645760000,
        "used_bytes": 616695963648,
        "available_bytes": 405949796352
      }
    ]
  },
  "database": {
    "hot_keys_count": 14523,
    "wal_size_bytes": 8450122,
    "storage_mode": "async"
  }
}
```

| Field | Description |
|---|---|
| `uptime_seconds` | Seconds since the server started |
| `process.memory_used_bytes` | RAM consumed by the MoltenDB process |
| `host.memory` | Total / used / free RAM on the host machine |
| `host.disks` | Per-disk total, used, and available bytes |
| `database.hot_keys_count` | Total number of documents currently held in RAM |
| `database.wal_size_bytes` | Current size of the WAL / storage file on disk |
| `database.storage_mode` | `async`, `sync`, or `in-memory` |

Returns `403 Forbidden` if the token does not have admin (`*:*:*`) scope.

---

## Configuration Reference

All options can be set via CLI flags or environment variables. CLI flags take priority.

> [!NOTE]
> **If you are running the `moltendb-server` binary, you can use all flags listed below.** The separation between "Networking/Auth" and "Database Engine" is only relevant for developers embedding `moltendb-core` as a library.

### Networking & Authentication (Server-only)
| Flag | Env var | Default | Description |
|---|---|---|---|
| `--cert` | `MOLTENDB_TLS_CERT` | `cert.pem` | TLS certificate |
| `--host` | `MOLTENDB_HOST` | `0.0.0.0` | IP address to bind to. Use `127.0.0.1` for localhost-only, `0.0.0.0` for all interfaces (required for Docker) |
| `--cors-origin` | `MOLTENDB_CORS_ORIGIN` | `*` ⚠️ | Allowed CORS origin(s) |
| `--jwt-secret` | `MOLTENDB_JWT_SECRET` | **REQUIRED** 🔥 | JWT signing secret |
| `--key` | `MOLTENDB_TLS_KEY` | `key.pem` | TLS private key |
| `--port` | `MOLTENDB_PORT` | `1538` | TCP port |
| `--root-password` | `MOLTENDB_ROOT_PASSWORD` | **REQUIRED** 🔥 | Root password |
| `--root-user` | `MOLTENDB_ROOT_USER` | **REQUIRED** 🔥 | Root username |
| `--debug` | `MOLTENDB_DEBUG` | `false` | Enable verbose debug logging |
| `--dev-mode` | `MOLTENDB_DEV_MODE` | `false` | Run over plain HTTP/WS instead of HTTPS/WSS. Ignores `--cert` and `--key`. ⚠️ NEVER use in production |

### Database Engine Flags (passed to `moltendb-core`)

| Flag | Env var | Default | Description |
|---|---|---|---|
| `--db-path` | `MOLTENDB_DB_PATH` | `my_database.log` | Log file path |
| `--disable-encryption` | `MOLTENDB_DISABLE_ENCRYPTION` | `false` | Store data as plain JSON |
| `--encryption-key` | `MOLTENDB_ENCRYPTION_KEY` | built-in default ⚠️ | At-rest encryption password |
| `--max-body-size` | `MOLTENDB_MAX_BODY_SIZE` | `10485760` | Maximum request body size in bytes |
| `--max-keys-per-request` | `MOLTENDB_MAX_KEYS_PER_REQUEST` | `1000` | Maximum number of keys allowed per JSON request |
| `--rate-limit-requests` | `MOLTENDB_RATE_LIMIT_REQS` | `100` | Max requests per IP per window |
| `--rate-limit-window` | `MOLTENDB_RATE_LIMIT_WINDOW` | `60` | Window size in seconds |
| `--in-memory` | `MOLTENDB_IN_MEMORY` | `false` | Run entirely in RAM — no WAL, no disk I/O. All data is lost on exit. Ideal for ephemeral caches and CI environments |
| `--write-mode` | `MOLTENDB_WRITE_MODE` | `async` | `async` or `sync` — controls flush behaviour for the single log file |

### 🔒 Security Considerations

Executing external scripts carries inherent risks. MoltenDB mitigates some of these by:
- **Positional Arguments:** The snapshot path is passed as a sanitized argument, not injected into a command string.
- **Explicit Paths:** On Windows, scripts in the current directory require the `./` prefix (e.g., `--post-backup-script "./my_hook.ps1"`).

#### Recommended Mitigations:
1. **Docker Isolation:** Run MoltenDB in a container to isolate the host filesystem and network. Use a minimal base image.
2. **Principle of Least Privilege:** Run the MoltenDB process under a dedicated service account with access only to its data directory. Ensure only the MoltenDB service user can read the hook script files.
3. **Absolute Paths:** Always use absolute paths for your scripts to avoid "command not found" errors or potential path hijacking.
4. **Sandboxing:** Use `seccomp` or `AppArmor`/`Selinux` on Linux to restrict the types of processes MoltenDB can spawn.
5. **Script Hardening:** Ensure your hook scripts have restricted permissions (e.g., `chmod 700`) and do not contain hardcoded secrets. Use environment variables for API keys.

⚠️ = insecure default, must be overridden in production. The server prints a warning at startup for each one that is not set.

🔥 = mandatory requirement. The server will not start if these are missing.

---

## Storage Modes

MoltenDB has three storage modes. Choose based on your durability requirements:

| Mode | Flag | Best for |
|---|---|---|
| `async` (default) | `--write-mode async` | Max throughput, up to 50 ms data loss on crash |
| `sync` | `--write-mode sync` | Zero data loss per write, lower throughput |
| `in-memory` | `--in-memory` | Ephemeral caches, CI, session stores |

### Async (default)

Single append-only log file (`my_database.log`). Writes are buffered in memory and flushed to disk every **50 ms** — up to 50 ms of data can be lost on a hard crash. Highest write throughput. Call `POST /snapshot` to compact manually — a binary snapshot is written so the next startup only replays the delta, not the full log.

### Sync (`--write-mode sync`)

Same single-file layout as async, but every write blocks until the OS confirms the data is on disk. **Zero data loss on crash.** Lower throughput than async. Use this when losing even 50 ms of writes is unacceptable (financial records, audit logs).

### In-Memory (`--in-memory`)

Bypasses the WAL and all disk I/O entirely. All data lives exclusively in the RAM `DashMap` — no log file is created or written. This turns MoltenDB into a pure in-process cache with the full query engine (filters, joins, pub/sub) on top. Compaction and revocation-file persistence are automatically skipped. A startup warning is printed to make the ephemeral nature explicit.

> ⚠️ **All data is lost when the server exits.** Use this mode for ephemeral caches, session stores, CI test environments, or any scenario where durability is not required.

### Write modes summary
- **async** (default): writes are buffered in memory and flushed every 50 ms. Up to 50 ms of data loss on a hard crash. Highest throughput.
- **sync**: every write blocks until the OS confirms the data. Zero data loss on crash. Lower throughput.

---

## Snapshots, Compaction & Data Safety

### What happens during compaction

Compaction runs on demand when you call `POST /snapshot`. It:

1. Writes the complete current in-memory state to a **temp snapshot file** — the live snapshot is untouched at this point.
2. **Moves the existing snapshot** to `backup/<name>.snapshot.bin.<unix_timestamp>.bak` — the old snapshot is never deleted.
3. **Atomically renames** the temp file to the live snapshot — a single OS rename, so there is no window where neither file exists.
4. **Resets the live log to empty** — but all data is already captured in the new snapshot before this happens.

### Is any data lost during compaction?

**No.** The new snapshot is a full state dump — it contains every document that existed at compaction time, including documents first inserted many compactions ago. There is no snapshot chain to traverse; each snapshot is self-contained.

```
Compaction 1:  snapshot_1 = { doc_A, doc_B }
Compaction 2:  snapshot_2 = { doc_A, doc_B, doc_C }   ← doc_A still here
Compaction 3:  snapshot_3 = { doc_A, doc_B, doc_C, doc_D }  ← doc_A still here
```

Data is only gone if it was explicitly deleted or overwritten before the compaction ran.

### What the `backup/` folder contains

Every compaction moves the previous snapshot to `backup/` as a `.bak` file. These are point-in-time copies of the full database state. They are:
- **Not loaded at startup** — only the current snapshot is used.
- **Not pruned automatically** — they accumulate indefinitely. Clean them up manually or add a retention policy.
- Useful for **manual point-in-time recovery** via the `recover` CLI command.

### How large snapshots are loaded at startup

At startup, `stream_into_state` reads the snapshot file and applies each entry **directly into the `DashMap`** as it is read — there is no intermediate buffer. Peak RAM usage at startup is approximately **1× the snapshot file size** (just the DashMap being built).

The snapshot is a full state dump — it contains every document that existed at compaction time. On startup, only the delta (log lines written after the last snapshot) needs to be replayed.

---

## How the Log Works

MoltenDB uses an append-only log format — every insert, update, and delete is a new JSON line:

```json
{"cmd":"INSERT","collection":"laptops","key":"lp1","value":{"brand":"Lenovo","model":"ThinkPad X1 Carbon","price":1499,"_v":1,"createdAt":"2026-03-09T13:51:05Z","modifiedAt":"2026-03-09T13:51:05Z"}}
```
```json
{"cmd":"DELETE","collection":"laptops","key":"lp6","value":null}
```
```json
{"cmd":"DROP","collection":"laptops","key":"_","value":null}
```

With encryption enabled (the default), each line is an opaque `ENC` entry:

```json
{"cmd":"ENC","collection":"_","key":"_","value":"base64encodedciphertext..."}
```

On startup, the log is replayed top-to-bottom to rebuild the in-memory state. After compaction, only the current state is kept — dead entries are removed.

---

## Testing

```bash
# Run the full integration test suite (56 tests)
cargo test -p moltendb-server --test integration

# Run with verbose output
cargo test -p moltendb-server --test integration -- --nocapture

# Run the 100 000-entry stress test (insert + log replay verification)
cargo test -p moltendb-server --test stress -- --nocapture
```

The test suite covers: SET, GET, field selection, WHERE (all 9 operators, case-insensitive string matching), sort, pagination, joins, update, delete, versioning, extends, validation, persistence, compaction, and concurrency (8 threads × 100 docs).

### Stress & Performance Tools

Three Rust example binaries are provided for real-world load testing against a live server:

```bash
# 1. Generate 100 000 synthetic documents (writes tests/stress_data.json + stress_keys.json)
cargo run -p moltendb-server --example generate_stress_data

# 2. Bulk-insert the dataset into the running server
cargo run -p moltendb-server --example stress_insert

# 3. Fire 10 000 concurrent fetch requests and print a latency report
cargo run -p moltendb-server --example stress_fetch

# Tune concurrency (default 10 000) and collection name via env vars
STRESS_CONCURRENCY=50000 STRESS_COLLECTION=stress cargo run -p moltendb-server --example stress_fetch
```

The fetch report includes min / mean / p50 / p75 / p90 / p95 / p99 / p99.9 / max latency and sustained throughput (req/s). In a typical local debug build, MoltenDB sustains **4 000–8 000 req/s** for pure in-memory reads.

---

## Horizontal Scaling

MoltenDB is currently a **single-node, embedded database**. Its state lives in `DashMap` in memory, backed by an append-only log on disk. There is no built-in concept of nodes, replication, or sharding.

### Single-node throughput

| Operation | Throughput | Bottleneck |
|---|---|---|
| Reads (`get`, `get_all`) | 100k–500k+ req/s | None — pure lock-free `DashMap` lookups |
| Writes (`insert`, `delete`, `update`) | 10k–50k req/s | Sequential log writer (one `Mutex`-guarded append) |

Reads are fully parallel and scale with CPU cores. Writes are bounded by disk I/O on the log writer.

### Scaling options

#### Option 1 — Read replicas (easiest, read-heavy workloads)

One **primary** node accepts all writes. One or more **replica** nodes tail the primary's log and replay entries via the same `apply_entry` path used at startup. Reads are distributed across replicas; writes always go to the primary.

MoltenDB already has most of the building blocks: the append-only log is the source of truth, `stream_into_state` / `apply_entry` already replay log entries into RAM state, and the WebSocket broadcast could be repurposed to stream log entries to replicas.

**What needs to be added:** a replication protocol (push log entries from primary → replicas), a `read_only` flag on replicas, and a load balancer to route reads to replicas and writes to the primary.

#### Option 2 — Sharding (write-heavy workloads)

Split collections across nodes — each node owns a subset of the data. Requires a shard map and a coordinator or client-side routing layer. Most complex option but gives true write scalability.

#### Option 3 — Active-active (high availability)

Multiple nodes accept writes independently and sync with each other. Requires conflict resolution. MoltenDB already has conflict detection logic (`_v` optimistic locking), but full multi-master is a significant undertaking.

### Recommended path

**Read replicas** are the most natural first step given the existing architecture. A single node with read replicas will scale very far before sharding becomes necessary — the single node already handles hundreds of thousands of reads per second.

---

## What's Next? (The Roadmap)

MoltenDB is currently in **RC Stage**. The core engine is stable, fast, and feature-rich.

### 1. Scaling & Ecosystem
- **Mobile Native Modules:** Compiling the exact same Rust core to run natively on iOS and Android (via FFI/JNI). This will bring blazing-fast, local-first embedded databases to React Native and Flutter.
- **Language Clients:** Official transport drivers for Python, Go, and Swift.
- **Data Portability:** Built-in, zero-friction utilities to export your entire database to standard JSON and CSV formats. No vendor lock-in.

### 2. Distributed Systems & Core
- **Robust Sync:** Two-way browser ↔ server delta sync with automatic conflict resolution (server-wins on `_v` collision).
- **Hardened Analytics:** The `COUNT/SUM/AVG/MIN/MAX` analytics engine exists in the codebase but is **currently under development and not ready for production use**. Expanding and rigorously testing it, accompanied by a comprehensive, interactive live demo, is a key roadmap item.

### 3. Security, Tooling & Polish
- **MoltenDB Studio (Premium):** A paid, official GUI dashboard to visually manage your databases, inspect collections, and execute queries without touching the CLI.

---


