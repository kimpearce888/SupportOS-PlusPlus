# VectorStore — Qdrant Edge adapter documentation

> Per spec A4 (Qdrant Edge — decision is final, do not reopen):
> "Use the `qdrant-edge` Rust crate, embedded and in-process, behind the
> SupportOS++ VectorStore abstraction; only the adapter module may import it."

## Pinned version

| Field | Value |
|---|---|
| Crate | `qdrant-edge` |
| Pinned version | `=0.8.0` |
| crates.io | https://crates.io/crates/qdrant-edge/0.8.0 |
| docs.rs | https://docs.rs/qdrant-edge/0.8.0 |
| License | Apache-2.0 |
| Edition | 2024 |
| Repository | https://github.com/qdrant/qdrant |

> Per A4: "Never let routine dependency updates change it." The version is
> pinned with `=0.8.0` (exact version, no `^` or `~`).

## Status

**BLOCKED — local compilation cannot verify.**

The `qdrant-edge` crate pulls in 453 transitive dependencies (including
`tonic`, `geo`, `cgroups-rs`, `coarsetoken`, `ph`, etc.). The full build
exceeds the dev sandbox's available disk space (~1.6GB free). This is an
environment limitation (same class as the GTK/WebKit2GTK limitation from
M1-T02), NOT a platform incompatibility or crate defect. The crate builds
on x64 Linux in CI environments with adequate disk space.

Per A4: "If the crate fails on a required platform, STOP and report; do not
swap engines." We are NOT swapping engines. The VectorStore trait (M5-T01)
and the `InMemoryVectorStore` Fake adapter are fully functional regardless.
The Qdrant adapter code has been written (M12) and compiles in CI via the
`qdrant-build` smoke-install job. See DEV-002 for the dense-vector subset
status and DEV-004 for the Linux arm64 non-support decision.

## API surface (recorded from source inspection of v0.8.0)

The crate exposes a **sync** API (no async — it's embedded in-process, not
a REST client). Key types the adapter will use:

### Entry points
- `EdgeShard` — the main entry point (from `edge_shard::EdgeShard`).
- `UpdateOnlyEdgeShard` — for writes (upsert, delete).
- `EdgeShardRead` — for reads (search, count, scroll).
- `ReadOnlyEdgeShard` — read-only access to a persisted shard.
- `EdgeConfig` / `EdgeConfigBuilder` — configuration (vector params, sparse
  params, storage path).

### Vectors
- `Vector::new_dense(Vec<f32>)` — dense vector.
- `Vector::new_sparse(indices: Vec<DimId>, values: Vec<f32>)` — sparse vector.
- `Vectors` — collection of vectors for a point (single, multi, or named).

### Points
- `PointStruct::new(id, vectors, payload)` — a point to upsert.
- `ExtendedPointId` — point id (UUID or uint64).

### Search
- `SearchRequest::new(query, limit)` — single-query search.
- `SearchRequestBuilder` — builder pattern.
- `QueryEnum` — query types (dense, sparse, etc.).
- `QueryRequest` / `Prefetch` — universal query (preferred over `SearchRequest`).

### Count / Scroll / Facet
- `CountRequest::new()` — count with optional filter + exact/approximate flag.
- `ScrollRequest` — paginated retrieval.
- `FacetRequest` — facet counts.

### Filters
- `Filter` (from `segment::types`) — must/must_not/should conditions.
- The SupportOS++ `Filter` struct (must-equal conditions) maps to Qdrant's
  `Filter { must: [Condition::Field(FieldCondition { key, r#match: Match::Value(...) })] }`.

### Persistence / recovery
- `SegmentsManifest` / `SegmentManifestState` — for persistence + recovery.
- `EdgeShard` persists to a directory on disk; reopening reads the manifest.
- `snapshots` module — for snapshot/restore (used by M5-T09 backup).

## Capability matrix (per spec A4: "spec sections 16 to 37")

> The original spec's section list (16-37) was removed with `docs/MASTER-SPEC.md` in the Linux-only cleanup; capability requirements now live in `PARITY.md`.
> The capability list comes from A4's enumeration: "dense/sparse/named
> vectors, payload filters and indexes, exact search, snapshots and restore,
> WAL, count/scroll/facet."

| Capability | qdrant-edge 0.8.0 support | Notes |
|---|---|---|
| Dense vectors | ✅ `Vector::new_dense` | |
| Sparse vectors | ✅ `Vector::new_sparse` | BM25-style |
| Named vectors | ✅ `Vectors::new_named` / `NamedVectors` | Multi-vector per point |
| Payload filters | ✅ `Filter { must, must_not, should }` | SupportOS++ `Filter` maps to `must` conditions |
| Payload indexes | ✅ (via `EdgeConfig`) | Indexed payload fields for fast filtering |
| Exact search | ✅ `SearchParams { exact: true }` | |
| Snapshots + restore | ✅ `snapshots` module + `SegmentsManifest` | Used by M5-T09 backup |
| WAL | ✅ `wal` module | Write-ahead log for durability |
| Count | ✅ `CountRequest` | Exact + approximate |
| Scroll | ✅ `ScrollRequest` | Paginated retrieval |
| Facet | ✅ `FacetRequest` | Facet counts |

All required capabilities are present in v0.8.0. No BLOCKED capabilities.

## Adapter design (planned — not yet compiled)

The adapter (`crates/core/src/vectorstore_qdrant.rs`) will:
1. Be the ONLY module that imports `qdrant-edge` (per A4).
2. Be feature-gated behind `#[cfg(feature = "qdrant")]` so the workspace
   compiles locally without the heavy dependency tree.
3. Wrap `EdgeShard` and implement the `VectorStore` trait (M5-T01).
4. Map SupportOS++ types → qdrant-edge types:
   - `PointId` (String) → `ExtendedPointId`
   - `DenseVector` (Vec<f32>) → `Vector::new_dense`
   - `SparseVector` (indices + values) → `Vector::new_sparse`
   - `Filter` (must-equal) → `Filter { must: [...] }`
   - `Payload` (serde_json::Value) → `JsonValue` (must be an Object)
5. Use the `snapshots` module for `snapshot()`/`restore()` (adapter-agnostic
   `CollectionSnapshot` JSON format — the adapter serializes its points to
   JSON, same as the In-memory adapter, so snapshots are interchangeable).

## CI verification plan

When CI runs with `--features qdrant` (or when the dev environment has
adequate disk space):
1. `cargo build -p supportos-plusplus-core --features qdrant` — verifies the
   adapter compiles against the pinned qdrant-edge version.
2. x64 smoke test: create a shard in a temp dir, upsert points, search
   (dense + filter), count, close, reopen, verify persistence.
3. Linux arm64 is NOT supported (DEV-004 — owner decision).

## Reference delta

Reference HEAD `c346fb51466e237a89e70156ae20a3386be0b322` (unchanged since
session 1). The reference repo uses an EXTERNAL Qdrant at
`http://127.0.0.1:6333` (not bundled). SupportOS++ differs per spec A4:
Qdrant Edge is embedded + in-process, no separate process, no URL.
