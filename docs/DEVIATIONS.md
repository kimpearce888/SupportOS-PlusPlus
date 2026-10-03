# DEVIATIONS.md — SupportOS++

> Every deviation from the reference goes here with reason and impact.
> Status stays **pending owner approval** until the owner approves it.
> Never approve your own deviations. Never silently narrow scope.
> Canonical parity status lives in **PARITY.md** — this file records approved
> or pending *scope* deviations only, not per-feature parity gaps.
> IDs are stable forever; resolved/moot entries stay as tombstones so
> historical references (code comments, architecture docs) keep resolving.

## Format

Each entry:
- **ID**: `DEV-###`
- **Date**: when discovered
- **Where**: file / module / capability
- **Reference does**: what the reference repo's code does
- **Deviation**: what SupportOS++ does differently
- **Reason**: why
- **Impact**: user-visible / technical
- **Status**: `pending owner approval` | `approved (date)` | `rejected (date)` | `removed (…)`

---

## DEV-002 — Qdrant Edge adapter: default-off, pending owner enable decision

- **Date**: Session 37 (M12); re-verified Session B audit
- **Where**: `crates/core/src/vectorstore_qdrant.rs` (behind `qdrant` cargo feature)
- **Reference does**: the reference keeps Float32 embeddings in SQLite and treats
  its Qdrant REST integration as an optional accelerator (fail-soft); semantic
  search always works locally via a linear cosine scan over persisted vectors.
- **Deviation**: the port embeds `qdrant-edge = "=0.8.0"` behind a cargo feature
  that is OFF by default. A 2026-10 audit re-verification found `search_sparse`
  (vectorstore_qdrant.rs:292), `snapshot` (:360) and `restore` (:406) ARE
  implemented; the earlier "dense-only" claim was stale. The open deviation is
  that the feature is off by default and the semantic-search fallback that the
  reference provides (Float32-in-SQLite cosine scan) is not yet wired into the
  port's production search path (tracked as PARITY.md F-017).
- **Reason**:
  1. The `qdrant-edge = "=0.8.0"` crate pulls in 123 direct + 453 transitive
     deps; building it adds ~5 minutes to CI and >2GB of disk usage. Default
     builds keep the feature OFF; a dedicated CI job (`smoke-install.yml →
     qdrant-build`) verifies the feature compiles.
  2. The sparse-vector API in qdrant-edge requires named sparse vectors
     configured in `EdgeConfig` at collection-creation time; bridging our
     `SparseVector` type to qdrant-edge's named sparse vectors requires schema
     changes that are TODO.
  3. The snapshot/restore API in qdrant-edge uses its own binary format, not
     the adapter-agnostic JSON `CollectionSnapshot`; bridging is TODO.
- **Impact**:
  - User-visible: semantic search is reported as unavailable in default builds
    until F-017 lands the local cosine fallback (reference-equivalent
    behavior); building with `--features qdrant` enables persistent vectors.
  - Technical: the InMemoryVectorStore passes the full contract test suite
    (`vectorstore_contract::run_contract_tests`) but is demo/test-only;
    the QdrantEdgeVectorStore passes only the dense subset.
- **Status**: pending owner approval

---

## DEV-003 — REMOVED (was: macOS app-crate tests excluded)

- **Status**: removed — macOS support was dropped entirely (DEV-006) and the
  `not(target_os = "macos")` gates were deleted in the Linux-only cleanup.
  The Tauri 2 macOS `generate_context!` duplicate-symbol link error this
  entry documented no longer applies to any supported platform.

---

## DEV-004 — Linux arm64 not supported (removed)

- **Date**: Session 37 (M12), removed in session 38 per owner directive
- **Where**: README.md, CI matrix
- **Reference does**: n/a
- **Deviation**: Linux arm64 is NOT supported. The CI matrix is x86_64-only
  for Linux (Ubuntu 22.04 + 24.04). Linux arm64 users must build from
  source. The README documents this.
- **Reason**: The owner decided not to pursue arm64 CI runners. This is a
  permanent scope decision, not a blocker.
- **Impact**:
  - User-visible: Linux arm64 users must build from source.
  - Technical: none — the codebase compiles fine on arm64; only CI does not
    produce arm64 installers.
- **Status**: approved (owner decision — removed from the roadmap)

---

## DEV-005 — REMOVED (was: production vector store is InMemoryVectorStore)

- **Status**: removed — the claim was stale. InMemoryVectorStore now appears
  only in demo/test code; production builds wire no vector store at all by
  default (the `qdrant` feature is off and the search routes report semantic
  search unavailable). The remaining, accurate record of this situation is
  DEV-002 (feature default-off) and PARITY.md F-017 (semantic fallback gap).

---

## DEV-006 — Windows and macOS: excluded by owner

- **Date**: Session 39 (owner directive)
- **Where**: `.github/workflows/`, `tauri.conf.json`, `README.md`
- **Reference does**: n/a (the reference ships its own multi-platform matrix;
  the port's distribution scope is intentionally narrower)
- **Deviation**: Windows and macOS are excluded from CI, smoke-install, and
  nightly builds. Only Linux x86_64 is built and tested. There are no
  `#[cfg(target_os)]` gates on business logic and no Windows/macOS platform
  code remains in the repository; packaging + smoke-testing are Linux-only.
- **Reason**: Owner decision to focus on Linux for initial release
  (Linux-only scope: `.deb` + `.AppImage` only).
- **Impact**:
  - User-visible: No Windows .msi/.exe or macOS .dmg installers are
    produced. Windows/macOS users must build from source.
  - Technical: only packaging + smoke-testing are Linux-only.
- **Status**: approved (owner decision)
