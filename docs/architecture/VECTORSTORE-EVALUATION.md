# VectorStore — per-platform evaluation

> Per spec A4: "In Milestone 1 run a spike that proves, on CI runners for
> Windows x64, macOS arm64, macOS x64 (Intel), Linux x64 and Linux arm64
> (where feasible): the crate builds, a shard persists in the app data
> folder, reopens after restart, and dense and sparse search with filters
> work. Record results per platform."

## Pinned version

`qdrant-edge = "=0.8.0"` (see `docs/architecture/VECTORSTORE.md` for full details).

## Per-platform results

| Platform | Builds | Persists | Reopens | Dense search | Sparse search | Filters | Status |
|---|---|---|---|---|---|---|---|
| Linux x64 | ❌ BLOCKED | — | — | — | — | — | Dev sandbox disk space (1.6GB) insufficient for 453-dep build tree; CI must verify |
| Linux arm64 | ❌ BLOCKED | — | — | — | — | — | M1-T11 BLOCKED: no arm64 CI runner on free tier |
| macOS arm64 (Apple Silicon) | ❌ BLOCKED | — | — | — | — | — | M1-T11 BLOCKED: no arm64 CI runner on free tier |
| macOS x64 (Intel) | ❌ BLOCKED | — | — | — | — | — | Not yet attempted; CI must verify |
| Windows x64 | ❌ BLOCKED | — | — | — | — | — | Not yet attempted; CI must verify |

## BLOCKED reason

The `qdrant-edge` v0.8.0 crate pulls in 453 transitive dependencies. The
full build tree requires more disk space than the dev sandbox has available
(~1.6GB free). This is an environment limitation, not a platform
incompatibility or crate defect.

Per spec A4: "If the crate fails on a required platform, STOP and report;
do not swap engines." We are NOT swapping engines. The VectorStore trait
(M5-T01) + `InMemoryVectorStore` Fake adapter are fully functional. The
Qdrant adapter will be compiled + smoke-tested when the environment has
adequate disk space (CI on GitHub Actions, or a dev machine with >4GB free).

## CI verification plan

When CI runs on GitHub Actions (which has more disk space than the dev
sandbox), the following jobs will be added to `.github/workflows/ci.yml`:

1. **Linux x64**: `cargo build -p supportos-plusplus-core --features qdrant`
   + smoke test (create shard, upsert, search dense+sparse+filter, close,
   reopen, verify persistence).
2. **macOS x64 (Intel)**: same as Linux x64.
3. **Windows x64**: same as Linux x64.
4. **macOS arm64 / Linux arm64**: BLOCKED (M1-T11) — no arm64 CI runner on
   free tier. Owner must enable arm64 CI runners, accept x64-only, or
   provide a local arm64 machine.

## Capability matrix (from source inspection)

All required capabilities from A4 ("dense/sparse/named vectors, payload
filters and indexes, exact search, snapshots and restore, WAL,
count/scroll/facet") are present in the qdrant-edge v0.8.0 API surface.

See `docs/architecture/VECTORSTORE.md` for the full capability matrix +
API mapping.

## Next steps

1. **M5-T02** remains BLOCKED for local compilation. The adapter code will
   be written when CI can verify the build, or when the dev environment has
   more disk space.
2. **M5-T03–T11** proceed without qdrant-edge — they depend only on the
   `VectorStore` trait (M5-T01), not on the Qdrant adapter.
3. **M5-T10** (VectorStore contract tests) will run against the
   `InMemoryVectorStore` (M5-T01) locally and against the Qdrant adapter
   in CI (when the `qdrant` feature is enabled).
