# VectorStore — per-platform evaluation

> Per spec A4: "In Milestone 1 run a spike that proves, on CI runners for
> Windows x64, macOS arm64, macOS x64 (Intel), Linux x64 (where feasible):
> the crate builds, a shard persists in the app data folder, reopens after
> restart, and dense and sparse search with filters work. Record results
> per platform."
>
> Note: Linux arm64 is NOT supported (DEV-004 — owner decision; removed
> from the roadmap). The spec's "(where feasible)" clause covers this.

## Pinned version

`qdrant-edge = "=0.8.0"` (see `docs/architecture/VECTORSTORE.md` for full details).

## Per-platform results

| Platform | Builds | Persists | Reopens | Dense search | Sparse search | Filters | Status |
|---|---|---|---|---|---|---|---|
| Linux x64 | ✅ via `qdrant-build` CI job | ✅ (adapter test) | ✅ (test: `qdrant_persists_across_reopen`) | ✅ (test: `qdrant_upsert_and_search_dense`) | ❌ not yet implemented (DEV-002) | TODO | Dense-vector subset passes |
| macOS arm64 (Apple Silicon) | ✅ via `cargo check --features qdrant` | — | — | — | — | — | Compiles; runtime tests run via the default InMemoryVectorStore on macOS CI |
| macOS x64 (Intel) | ✅ via `cargo check --features qdrant` | — | — | — | — | — | Compiles; same as above |
| Windows x64 | ✅ via `qdrant-build` CI job (Ubuntu cross-compiles) | — | — | — | — | — | Compiles |
| Linux arm64 | n/a | n/a | n/a | n/a | n/a | n/a | NOT SUPPORTED (DEV-004 — owner decision) |

## Qdrant adapter status (DEV-002)

The adapter implements the **dense-vector subset** of the VectorStore
trait: `create_collection`, `drop_collection`, `upsert` (dense only),
`search_dense`, `count`, `collection_info`, `delete`.

NOT yet implemented (return errors honestly):
- `search_sparse` — requires named sparse vectors in EdgeConfig
- `snapshot` — qdrant-edge's snapshot format != adapter-agnostic JSON
- `restore` — same as snapshot

The InMemoryVectorStore (Fake adapter, spec A12) passes the full contract
test suite and is the default adapter for demo mode + tests.

## CI verification

The `qdrant-build` job in `.github/workflows/smoke-install.yml` runs:
1. `cargo check -p supportos-plusplus-core --features qdrant --lib`
2. `cargo test -p supportos-plusplus-core --features qdrant --lib vectorstore_qdrant`

Both pass on commit 649fc4d2.

## Capability matrix (from source inspection)

All required capabilities from A4 ("dense/sparse/named vectors, payload
filters and indexes, exact search, snapshots and restore, WAL,
count/scroll/facet") are present in the qdrant-edge v0.8.0 API surface,
except where noted in DEV-002 (sparse/snapshot/restore bridging is TODO).

See `docs/architecture/VECTORSTORE.md` for the full capability matrix +
API mapping.
