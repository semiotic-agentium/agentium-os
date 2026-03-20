# baml-rt-hash & baml-rt-repository — Design Document

Two new crates that give Agentium OS a **content-addressable package repository** with lineage tracking, versioning, and search.

- **`baml-rt-hash`** — standalone canonical hashing (SHA-256, length-delimited sections, deterministic JSON key ordering)
- **`baml-rt-repository`** — hybrid FS + SQLite repository: publish, fork, search, lineage DAG, fitness scoring, tagging, and an Axum HTTP API

---

## Design Intent

The repository is the **memory layer** for an agent development and selection (ADAS) loop. Agents are treated as immutable, content-addressed artifacts. Every mutation (manual edit, LLM-driven rewrite, automated fork) produces a new entry linked to its predecessors via typed lineage edges. This enables:

1. **Deterministic identity** — same authored source ⇒ same hash, regardless of build artifacts
2. **Provenance** — full derivation history via fork/influence DAG
3. **Selection pressure** — fitness scores and tags let meta-agents query "best agent for X"
4. **Distribution** — pull by hash (exact) or name@version (human-friendly)

---

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                    HTTP API (Axum)                       │
│  /publish  /fork  /search  /lineage  /entries  /agents  │
└────────────────────────┬────────────────────────────────┘
                         │
              ┌──────────▼──────────┐
              │  RepositoryService  │  ← orchestrates all stores
              └──┬──────┬──────┬───┘
                 │      │      │
     ┌───────────▼┐ ┌───▼────┐ ┌▼──────────┐
     │ BlobStore  │ │MetaData│ │ LineageStore│
     │ (FS)      │ │Store   │ │ + Search   │
     │           │ │(SQLite)│ │ (SQLite)   │
     └───────────┘ └────────┘ └────────────┘
```

### Storage Split (deliberate)

| Concern | Backend | Why |
|---------|---------|-----|
| tar.gz blobs | Filesystem (sharded: `ab/cdef…tar.gz`) | Large, opaque, no indexing needed |
| Metadata, versions, fitness, tags | SQLite | Small, structured, needs indexed queries |
| Lineage DAG | SQLite (recursive CTEs for ancestry) | Graph traversal benefits from SQL |
| Full-text search | SQLite FTS5 | Built-in, no external dependency |

---

## Canonical Hash (baml-rt-hash)

```
SHA-256(
  section("manifest", canonical_json(manifest.json))
  ‖ for each .ts file sorted by path:
      section("ts", path ‖ '\0' ‖ content)
  ‖ for each .baml file sorted by path:
      section("baml", path ‖ '\0' ‖ content)
)

section(tag, data) = tag_len:u32le ‖ tag ‖ data_len:u64le ‖ data
```

Only **authored source** is hashed — runtime artifacts (d.ts, tsconfig, compiled JS, baml_client/) are excluded. Two packages with identical authored source always produce identical hashes. The hash is a standalone crate so other crates (builder, runner) can compute hashes without depending on the full repository.

### Why a separate crate?

The builder needs to stamp packages with their content hash at build time. The runner needs to verify hashes on load. Neither should depend on SQLite, Axum, or the full repository service. `baml-rt-hash` has only three dependencies: `sha2`, `serde_json`, `thiserror`.

---

## Lineage DAG

```
         ┌─────────┐
         │ agent-v1 │  (Original)
         │ gen=0    │
         └────┬─────┘
              │ Fork
         ┌────▼─────┐
         │ agent-v2 │  (Forked)
         │ gen=1    │
         └────┬─────┘
         ╱    │ Fork
   Influence  │
   ╱     ┌────▼─────┐
┌──────┐ │ agent-v3 │  (Synthesized: fork + influence)
│ref-v1├─▶ gen=2    │
└──────┘ └──────────┘
```

### Edge Kinds

| Kind | Meaning | Cardinality | Generation effect |
|------|---------|-------------|-------------------|
| **Fork** | Hard derivation — "this was created by mutating that" | Single parent | Increments parent's generation |
| **Influence** | Soft reference — "this was informed by those" | Zero or more | No effect |

### Parentage (discriminated union)

```rust
enum Parentage {
    Original,                              // no parent — first in lineage
    Forked { parent: ContentHash, … },     // single fork parent
    Synthesized { influences: Vec<…> },    // multiple soft references
}
```

Fork and influence are structurally distinct so the graph can answer different questions: "what was directly derived?" vs "what references informed this design?"

Ancestry traversal uses a recursive CTE in SQLite — walk `lineage_edges` upward to collect the full derivation chain for any entry.

---

## Versioning

```
weather-agent@1  →  ContentHash("a1b2c3…")
weather-agent@2  →  ContentHash("d4e5f6…")
weather-agent@3  →  ContentHash("789abc…")
```

- **Monotonic per-agent**: versions start at 1 and increment within a named lineage
- **`VersionRef = AgentName + Version`** — human-addressable (`weather-agent@2`)
- **`ContentHash`** — machine-addressable (exact, immutable)
- **Generation** tracks fork-depth from the lineage root (not the same as version)

---

## Search

```rust
SearchQuery {
    full_text: Option<String>,            // FTS5 over source content
    capabilities: Vec<String>,            // manifest.capabilities filter
    tools: Vec<String>,                   // manifest.tools filter
    tags: Vec<Tag>,                       // user/system tags
    fitness: Option<FitnessFilter>,       // min score in domain
    generation: Option<GenerationFilter>, // fork-depth range
    lineage: Option<ContentHash>,         // descendants of hash
    order: SearchOrder,                   // Relevance | Newest | HighestFitness
    limit: usize,
}
```

The query builder dynamically constructs parameterized SQL with JOINs only for the filters that are active. FTS5 searches over the concatenated source content of each entry (TypeScript + BAML).

---

## HTTP Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/agents` | List all agent names |
| `GET` | `/agents/{name}/versions` | List versions for an agent |
| `GET` | `/entries/hash/{hash}` | Retrieve entry by content hash |
| `GET` | `/entries/{name}/{version}` | Retrieve entry by name@version |
| `POST` | `/publish` | Publish a new entry (original, iteration, or influenced) |
| `POST` | `/fork` | Fork an existing entry into a new agent lineage |
| `POST` | `/search` | Search entries with filters |
| `GET` | `/lineage/{hash}` | Get lineage subgraph (ancestors + descendants) |
| `POST` | `/entries/{hash}/fitness` | Record a fitness score |
| `POST` | `/entries/{hash}/tags` | Add a tag |
| `DELETE` | `/entries/{hash}/tags` | Remove a tag |

All errors use RFC 7807 Problem Details via `http-api-problem`.

### Publish Flow

```
Client                    Service                  Stores
  │                         │                        │
  │  POST /publish          │                        │
  │  { source, origin }     │                        │
  │ ───────────────────────▶│                        │
  │                         │  compute_hash(source)  │
  │                         │───────┐                │
  │                         │◀──────┘ ContentHash    │
  │                         │                        │
  │                         │  resolve parentage     │
  │                         │  from origin           │
  │                         │───────┐                │
  │                         │◀──────┘ Parentage +    │
  │                         │         Generation     │
  │                         │                        │
  │                         │  assign next version   │
  │                         │───────────────────────▶│ MetadataStore
  │                         │                        │
  │                         │  store blob            │
  │                         │───────────────────────▶│ BlobStore
  │                         │                        │
  │                         │  record lineage edge   │
  │                         │───────────────────────▶│ LineageStore
  │                         │                        │
  │                         │  index for search      │
  │                         │───────────────────────▶│ SearchStore
  │                         │                        │
  │  { hash, version_ref }  │                        │
  │ ◀───────────────────────│                        │
```

---

## Crate Map

```
baml-rt-hash/
├── content_hash.rs    ContentHash newtype (validated SHA-256 hex-64)
├── hasher.rs          CanonicalHasher, HashInput, canonical_json()
└── lib.rs             Re-exports

baml-rt-repository/
├── ids.rs             AgentName, Version, Generation, VersionRef, LineageEdgeId
├── lineage.rs         LineageKind, Parentage, LineageEdge, AncestryNode, LineageSubgraph
├── entry.rs           SourceBundle, RepositoryEntry, RepositoryEntryHeader, FitnessScore, Tag
├── search.rs          SearchQuery, typed filter structs, SearchOrder
├── commands.rs        PublishCommand, ForkCommand, PublishOrigin, PublishResult
├── error.rs           RepositoryError (12 variants, maps to RFC 7807)
├── storage.rs         4 async traits: BlobStore, MetadataStore, LineageStore, SearchStore
├── fs_blob_store.rs   FsBlobStore — sharded filesystem implementation
├── sqlite_store.rs    SqliteStore — unified metadata/lineage/search (bundled SQLite)
├── service.rs         RepositoryService — orchestrator (publish, fork, search, fitness, tags)
├── http.rs            RFC 7807 mappings, request/response types
├── handlers.rs        11 Axum handlers
├── router.rs          repository_router() → Router
├── spans.rs           OTel span helpers (pre-wired)
└── metrics.rs         OTel metric instruments (pre-wired)
```

---

## Key Design Decisions

### Hash in its own crate
Builder and runner need to compute hashes without pulling in SQLite/Axum. `baml-rt-hash` depends only on `sha2`, `serde_json`, `thiserror`.

### Trait boundaries for storage
`BlobStore`, `MetadataStore`, `LineageStore`, `SearchStore` are async traits — swappable for cloud backends (S3, Postgres) later without changing the service layer.

### `spawn_blocking` for SQLite
rusqlite is synchronous; all DB calls go through `tokio::task::spawn_blocking` so they never block the async runtime. The connection is wrapped in `Arc<Mutex<Connection>>`.

### Discriminated unions over flags
`LineageKind::Fork | Influence`, `Parentage::Original | Forked | Synthesized`, `PublishOrigin::Original | Iteration | Influenced` — invalid states are unrepresentable at the type level.

### Generation is computed, not declared
Fork increments parent's generation; influence doesn't affect it. This ensures the generation counter accurately reflects structural fork-depth in the DAG.

### FTS5 over source content
Agents can be discovered by what their code does, not just metadata. The FTS index covers all TypeScript and BAML source files in the bundle.

### Sharded blob layout
Blobs are stored as `<root>/<first-2-hex-chars>/<remaining-62-chars>.tar.gz`. The two-character prefix sharding prevents any single directory from accumulating too many files.

---

## SQLite Schema

```sql
CREATE TABLE entries (
    hash            TEXT PRIMARY KEY,
    agent_name      TEXT NOT NULL,
    version         INTEGER NOT NULL,
    generation      INTEGER NOT NULL,
    parentage_json  TEXT NOT NULL,       -- JSON-encoded Parentage
    source_json     TEXT NOT NULL,       -- JSON-encoded SourceBundle
    description     TEXT NOT NULL,
    created_at      TEXT NOT NULL,       -- RFC 3339
    UNIQUE(agent_name, version)
);

CREATE TABLE fitness_scores (
    hash    TEXT NOT NULL REFERENCES entries(hash),
    domain  TEXT NOT NULL,
    score   REAL NOT NULL,
    PRIMARY KEY (hash, domain)
);

CREATE TABLE tags (
    hash  TEXT NOT NULL REFERENCES entries(hash),
    tag   TEXT NOT NULL,
    PRIMARY KEY (hash, tag)
);

CREATE TABLE lineage_edges (
    id          TEXT PRIMARY KEY,
    source_hash TEXT NOT NULL REFERENCES entries(hash),
    target_hash TEXT NOT NULL REFERENCES entries(hash),
    kind        TEXT NOT NULL,           -- 'Fork' | 'Influence'
    description TEXT NOT NULL
);

CREATE VIRTUAL TABLE entries_fts USING fts5(
    hash UNINDEXED,
    content
);
```

---

## Test Coverage

| Area | Status | Tests |
|------|--------|-------|
| `baml-rt-hash` | 12 passing | Determinism, ordering invariance, parse validation, serde roundtrip, insta snapshot |
| `FsBlobStore` | 2 passing | Put/get/delete roundtrip, shard directory creation |
| `SqliteStore` | Planned | Metadata CRUD, lineage traversal, search filters |
| `RepositoryService` | Planned | Publish → fork → search flow |
| HTTP handlers | Planned | `axum::test` / `tower::ServiceExt` |
| Property tests | Planned | Version monotonicity, lineage cycle detection |
