# baml-rt-hash & baml-rt-repository — Design Document

Two crates that give Agentium OS a **content-addressable package repository** with lineage tracking, versioning, and search.

- **`baml-rt-hash`** — standalone canonical hashing (SHA-256 over a length-delimited section format with deterministic JSON key ordering). Depends only on `sha2`, `serde_json`, `thiserror`.
- **`baml-rt-repository`** — SurrealDB-backed repository: publish, fork, search, lineage DAG, tagging, and an Axum HTTP API.

---

## Design Intent

The repository is the **memory layer** for an agent development and selection (ADAS) loop. Agents are treated as immutable, content-addressed artefacts. Every mutation (manual edit, LLM-driven rewrite, automated fork) produces a new entry linked to its predecessors via typed lineage edges. This enables:

1. **Deterministic identity** — same authored source ⇒ same hash, regardless of build artefacts.
2. **Provenance** — full derivation history via fork/influence DAG.
3. **Selection pressure** — tags and lineage relationships let meta-agents query "what was derived from this?" or "what influenced that?".
4. **Distribution** — pull by hash (exact) or `name@version` (human-friendly).

---

## Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                        HTTP API (Axum)                            │
│  /agents  /entries  /search  /lineage  /blobs  /fork  /publish*  │
└──────────────────────────────┬───────────────────────────────────┘
                               │
                    ┌──────────▼───────────┐
                    │   RepositoryService  │  ← orchestrates publish, fork,
                    └──┬──────┬──────┬──┬──┘    search, lineage, tagging
                       │      │      │  │
              ┌────────▼──────▼──────▼──▼────────┐
              │           SurrealStore           │
              │  (single embedded SurrealDB Db)  │
              │  implements all four traits      │
              └──────────────────────────────────┘
```

`*` `/publish` is wired by default but can be omitted from the public router (see [Router composition](#router-composition)).

### Single Backend, Trait-Separated Surface

The repository keeps four storage traits separated at the type level:

| Trait | Concern |
|---|---|
| `BlobStore` | Distributable tar.gz packages keyed by content hash |
| `MetadataStore` | Entry/version metadata, tag CRUD, version-ref resolution |
| `LineageStore` | Edge recording and DAG traversal |
| `SearchStore` | Structured queries with text, tag, capability, tool, generation, and lineage filters |

A single concrete backend, `SurrealStore`, implements all four. The traits exist for testability and so that future deployments can swap individual concerns without rewriting the service layer.

---

## Canonical Hash (baml-rt-hash)

```
SHA-256(
  section("manifest", canonical_json(manifest.json))
  ‖ for each .ts file sorted by path:
      section("ts", path ‖ content)
  ‖ for each .baml file sorted by path:
      section("baml", path ‖ content)
)

section(tag, data) = tag_len:u32le ‖ tag ‖ data_len:u64le ‖ data
```

Only **authored source** is hashed — runtime artefacts (`*.d.ts`, `tsconfig.json`, compiled JS, `baml_client/`) are excluded. Two packages with identical authored source always produce identical hashes.

The hash is a standalone crate so other crates (the builder, the runner, future ADAS tooling) can compute hashes without depending on SurrealDB or Axum.

---

## Storage Layout

`SurrealStore` opens an embedded SurrealDB instance under namespace `baml`, database `repository`. Two open modes:

- **Persistent** — `SurrealStore::open(path)` uses the `SurrealKv` engine; data is stored on disk under the given path.
- **In-memory** — `SurrealStore::open_in_memory()` uses the `Mem` engine; suitable for tests and short-lived processes.

Schema is initialised idempotently on every open via `DEFINE TABLE IF NOT EXISTS` / `DEFINE FIELD IF NOT EXISTS` / `DEFINE INDEX IF NOT EXISTS` statements.

### Tables

```surql
DEFINE TABLE entries SCHEMAFULL;
DEFINE FIELD hash                       ON entries TYPE string;
DEFINE FIELD agent_name                 ON entries TYPE string;
DEFINE FIELD version                    ON entries TYPE int;
DEFINE FIELD generation                 ON entries TYPE int;
DEFINE FIELD parentage_json             ON entries TYPE string;     -- Parentage as JSON
DEFINE FIELD source_json                ON entries TYPE string;     -- SourceBundle as JSON
DEFINE FIELD change_rationale           ON entries TYPE string;
DEFINE FIELD created_at                 ON entries TYPE string;
DEFINE FIELD manifest_description       ON entries TYPE option<string>;
DEFINE FIELD manifest_tools_json        ON entries TYPE string;     -- Vec<String>
DEFINE FIELD manifest_capabilities_json ON entries TYPE string;     -- Vec<String>
DEFINE FIELD manifest_text              ON entries TYPE string;     -- searchable manifest projection
DEFINE FIELD source_text                ON entries TYPE string;     -- searchable source projection
DEFINE INDEX idx_entries_hash         ON entries FIELDS hash UNIQUE;
DEFINE INDEX idx_entries_name_version ON entries FIELDS agent_name, version UNIQUE;

DEFINE TABLE tags SCHEMAFULL;
DEFINE FIELD entry_hash ON tags TYPE string;
DEFINE FIELD tag        ON tags TYPE string;
DEFINE INDEX idx_tag_unique ON tags FIELDS entry_hash, tag UNIQUE;
DEFINE INDEX idx_tag_lookup ON tags FIELDS tag;

DEFINE TABLE lineage_edges SCHEMAFULL;
DEFINE FIELD id          ON lineage_edges TYPE string;
DEFINE FIELD source_hash ON lineage_edges TYPE string;
DEFINE FIELD target_hash ON lineage_edges TYPE string;
DEFINE FIELD kind        ON lineage_edges TYPE string;              -- 'fork' | 'influence'
DEFINE FIELD description ON lineage_edges TYPE string;
DEFINE INDEX idx_edge_id     ON lineage_edges FIELDS id UNIQUE;
DEFINE INDEX idx_edge_source ON lineage_edges FIELDS source_hash;
DEFINE INDEX idx_edge_target ON lineage_edges FIELDS target_hash;

DEFINE TABLE blobs SCHEMAFULL;
DEFINE FIELD hash     ON blobs TYPE string;
DEFINE FIELD data_hex ON blobs TYPE string;                         -- hex-encoded tar.gz
DEFINE INDEX idx_blob_hash ON blobs FIELDS hash UNIQUE;
```

### Why a single SurrealDB backend?

- **One process, one storage primitive.** The runner already embeds SurrealDB for provenance; sharing the engine for repository state simplifies operations and removes a second persistence dependency.
- **Blobs live in the database.** Tar.gz bytes are stored as hex-encoded strings in the `blobs` table rather than on the host filesystem. Pull and deploy paths are pure database reads, which keeps the repository portable across container restarts and pod replacements without a separately mounted volume.
- **Search projections are precomputed.** Filterable fields (`manifest_tools_json`, `manifest_capabilities_json`) and the searchable text projections (`manifest_text`, `source_text`) are written at insert time so reads are simple field lookups.
- **DAG traversal happens in Rust.** Lineage queries load edges from `lineage_edges` and walk the graph in-process via BFS over an adjacency map. SurrealDB's native graph features are not currently used.

---

## Lineage DAG

```
         ┌──────────┐
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
|---|---|---|---|
| **Fork** | Hard derivation — "this was created by mutating that" | Single parent | Increments parent's generation |
| **Influence** | Soft reference — "this was informed by those" | Zero or more | New generation = max(influence generations) + 1 |

### Parentage (discriminated union)

```rust
enum Parentage {
    Original,                                      // no parent — first in lineage
    Forked     { parent: ContentHash, … },         // single fork parent
    Synthesized { influences: Vec<InfluenceRef> }, // one or more soft references
}
```

Fork and influence are structurally distinct so the graph can answer different questions: "what was directly derived?" vs "what references informed this design?".

### Traversal

`LineageStore` exposes `parents`, `children`, `ancestors(max_depth)`, `influenced_by`, and `subgraph(ancestor_depth)`. The `SurrealStore` implementation loads the full edge set, builds forward and reverse adjacency maps, and executes a depth-bounded BFS in Rust.

---

## Versioning

```
weather-agent@1  →  ContentHash("a1b2c3…")
weather-agent@2  →  ContentHash("d4e5f6…")
weather-agent@3  →  ContentHash("789abc…")
```

- **Monotonic per-agent**: versions start at 1 and increment within a named lineage.
- **`VersionRef = AgentName + Version`** — human-addressable (`weather-agent@2`).
- **`ContentHash`** — machine-addressable (exact, immutable).
- **Generation** tracks fork-depth from the lineage root and is independent of version number.

### Repository-assigned versions are part of the hash input

`MetadataStore::insert_entry` is the canonical write path:

1. Look up the highest existing `version` for `agent_name`; the next version is `prev + 1` (or `1` for a new lineage).
2. Rewrite `manifest.version` to that next version (`SourceBundle::with_manifest_version`).
3. Compute the canonical content hash from the versioned source bundle.
4. Reject the insert with `RepositoryError::DuplicateHash` if another entry already carries this hash.
5. Persist the row, including the precomputed `manifest_text` / `source_text` search projections.

Hashing the version-stamped manifest means the published `ContentHash` is determined by the repository, not by whatever version string the client supplied. Builders can leave `manifest.version` blank (or stale) and trust the repository to make it canonical.

---

## Search

```rust
SearchQuery {
    text:         Option<FullTextTerm>,            // case-insensitive substring
    name:         Option<AgentName>,               // exact name match
    capabilities: Vec<CapabilityFilter>,           // manifest.capabilities (all required)
    tools:        Vec<ToolFilter>,                 // manifest.tools         (all required)
    tags:         Vec<TagFilter>,                  // entry tags             (all required)
    generation:   Option<GenerationFilter>,        // min/max fork-depth
    lineage:      Option<LineageFilter>,           // descendants/ancestors of a hash
    limit:        Option<usize>,
    order:        SearchOrder,                     // Newest | Oldest | Relevance
}
```

`SearchStore::search` executes filters in-process: it loads candidate rows from `entries`, filters them through `manifest_text` / `source_text` for the optional text term, hydrates each surviving row into a `RepositoryEntryHeader`, applies the metadata filters, then orders and limits the result set. Lineage filters reuse the in-memory adjacency map described in [Traversal](#traversal).

`SearchOrder::Relevance` currently falls back to newest-first ordering.

---

## HTTP Endpoints

### Router composition

`baml-rt-repository` exposes four router builders so hosts can choose how `/publish` is wired:

| Builder | Routes | Intended use |
|---|---|---|
| `repository_router(service)` | All routes including `POST /publish` | Standalone repository deployments |
| `repository_router_without_publish(service)` | All routes except `POST /publish` | Hosts that orchestrate publish externally (e.g. the runner, which builds before storing) |
| `repository_read_router(service)` | Read-only routes (agents, versions, entries, lineage, blobs, search) | Mount unauthenticated for public discovery and pull |
| `repository_mutation_router(service)` | `POST /fork`, tag CRUD | Mount behind operator authentication |

The split read/mutation routers reflect how `baml-agent-runner` exposes the repository in cluster mode: the read router is reachable without credentials, and the mutation router (plus the runner's own `/deploy`) sits behind the runner token.

### Routes

| Method | Path | Description |
|---|---|---|
| `GET` | `/agents` | List all agent names |
| `GET` | `/agents/{name}/versions` | List versions for an agent |
| `GET` | `/entries` | List or filter entries by `name`/`name+version` query parameters |
| `GET` | `/entries/{hash}` | Retrieve an entry by content hash |
| `GET` | `/entries/{name}/{version}` | Retrieve an entry by `name@version` |
| `POST` | `/publish` | Publish a new entry (Original / Iteration / Influenced) |
| `POST` | `/fork` | Fork an existing entry into a new agent lineage |
| `POST` | `/search` | Run a structured search query |
| `GET` | `/lineage/{hash}?depth={n}` | Get a lineage subgraph (ancestors + direct descendants) |
| `GET` | `/blobs/{hash}` | Download the tar.gz blob for an entry |
| `POST` | `/entries/{hash}/tags` | Add a tag |
| `DELETE` | `/entries/{hash}/tags` | Remove a tag |

All errors use RFC 7807 Problem Details via `http-api-problem`.

### Publish Flow

```
Client                    RepositoryService              SurrealStore
  │                              │                              │
  │  POST /publish               │                              │
  │  { name, source, origin,     │                              │
  │    rationale }               │                              │
  │ ────────────────────────────▶│                              │
  │                              │  resolve parentage +         │
  │                              │  generation from origin      │
  │                              │  (lookup latest / influences)│
  │                              │─────────────────────────────▶│
  │                              │                              │
  │                              │  insert_entry(NewEntry):     │
  │                              │   - assign next version      │
  │                              │   - rewrite manifest.version │
  │                              │   - compute ContentHash      │
  │                              │   - reject on duplicate hash │
  │                              │   - persist row + tags +     │
  │                              │     search projections       │
  │                              │─────────────────────────────▶│
  │                              │◀─ RepositoryEntry            │
  │                              │                              │
  │                              │  record lineage edges        │
  │                              │  (using stored.hash as       │
  │                              │   target)                    │
  │                              │─────────────────────────────▶│
  │  { hash, version_ref,        │                              │
  │    generation }              │                              │
  │ ◀────────────────────────────│                              │
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
├── lineage.rs         LineageKind, Parentage, LineageEdge, EdgeDescription,
│                      InfluenceRef, AncestryNode, LineageSubgraph
├── entry.rs           SourceBundle, ManifestSource, RepositoryEntry,
│                      RepositoryEntryHeader, ChangeRationale, Tag, Timestamp
├── search.rs          SearchQuery, filter newtypes, LineageRelation, SearchOrder
├── commands.rs        PublishCommand, ForkCommand, PublishOrigin, PublishResult
├── error.rs           RepositoryError (StorageWrite/Read carry boxed sources)
├── package.rs         tar.gz / on-disk → SourceBundle extraction
├── storage.rs         BlobStore, MetadataStore, LineageStore, SearchStore traits
├── surreal_store.rs   SurrealStore — single backend implementing all four traits
├── service.rs         RepositoryService — publish, fork, search, lineage, tagging
├── http.rs            Request/response types, RFC 7807 mappings
├── handlers.rs        Axum handlers for each route
├── router.rs          repository_router, repository_router_without_publish,
│                      repository_read_router, repository_mutation_router
├── spans.rs           OTel span helpers
└── metrics.rs         OTel metric instruments
```

---

## Key Design Decisions

### Hash in its own crate
Builder and runner need to compute hashes without pulling in SurrealDB or Axum. `baml-rt-hash` depends only on `sha2`, `serde_json`, `thiserror`.

### Trait boundaries for storage
`BlobStore`, `MetadataStore`, `LineageStore`, `SearchStore` are async traits even though one struct implements all four. Keeping the seams visible documents the responsibilities and lets future deployments split or replace any single concern.

### Discriminated unions over flags
`LineageKind::{Fork, Influence}`, `Parentage::{Original, Forked, Synthesized}`, `PublishOrigin::{Original, Iteration, Influenced}` — invalid states are unrepresentable at the type level.

### Generation is computed, not declared
Fork increments the parent's generation; influence sets generation to `max(influence generations) + 1`. Original entries are at generation 0. The store never accepts a caller-supplied generation; it is always derived from parentage.

### Version is canonical, not advisory
The repository assigns the next monotonic version, rewrites `manifest.version` to that value, and hashes the result. The published `ContentHash` reflects the repository-assigned version, not anything the client wrote into the manifest beforehand.

### Searchable text is precomputed at insert
The `manifest_text` and `source_text` projections are built in `SurrealStore::insert_entry` from `manifest.{name, version, description, tools, capabilities, tags}` and the concatenated source files, then matched at query time with case-insensitive substring containment. This is intentionally simple; richer ranking (FTS, embeddings) can be layered later without changing the trait.

### Blobs in the database
Storing tar.gz bytes inside SurrealDB removes the need for a separately mounted blob volume and keeps pull semantics independent of host filesystem layout. Bytes are hex-encoded for storage and decoded on read.

---

## Future Work

The following capabilities appear in earlier design notes but are not implemented today:

- **Fitness scoring.** There is no fitness table, no score recording API, and no fitness filter in `SearchQuery`. ADAS-style "best agent for X" selection would need this added before it can be expressed.
- **Native graph traversal.** Lineage walks are done in Rust over loaded edges. SurrealDB's `RELATE` / graph query syntax is unused.
- **Full-text relevance ranking.** `SearchOrder::Relevance` currently falls back to newest-first. A real relevance score would need either FTS scoring or embedding-based similarity.
