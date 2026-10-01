# DEVIATIONS.md — SupportOS++

> Every deviation from the spec or reference goes here with reason and impact.
> Status stays **pending owner approval** until the owner approves it.
> Never approve your own deviations. Never silently narrow scope.

## Format

Each entry:
- **ID**: `DEV-###`
- **Date**: when discovered
- **Where**: file / module / capability
- **Spec says**: quote or paraphrase
- **Reference does**: what the reference repo's code does
- **Deviation**: what SupportOS++ does differently
- **Reason**: why
- **Impact**: user-visible / technical
- **Status**: `pending owner approval` | `approved (date)` | `rejected (date)`

---

## DEV-001 — none yet

(No deviations in sessions 1–11. The SupportOS++ skeleton matched the spec exactly.)

---

## DEV-002 — Qdrant Edge adapter: minimal dense-vector subset only

- **Date**: Session 37 (M12)
- **Where**: `crates/core/src/vectorstore_qdrant.rs` (new, behind `qdrant` cargo feature)
- **Spec says**: A4 — "Use the `qdrant-edge` Rust crate, embedded and in-process, behind the SupportOS++ VectorStore abstraction; only the adapter module may import it." A4 also requires "Verify every capability required by spec sections 16 to 37 exists in the pinned version (dense/sparse/named vectors, payload filters and indexes, exact search, snapshots and restore, WAL, count/scroll/facet)."
- **Reference does**: the reference repo's spec assumed `qdrant-edge` exists and is feature-complete.
- **Deviation**: The QdrantEdgeVectorStore adapter implements only the dense-vector subset of the VectorStore trait:
  - ✅ Implemented: `create_collection`, `drop_collection`, `upsert` (dense only), `delete`, `search_dense`, `count`, `collection_info`.
  - ❌ NOT implemented: `search_sparse` (returns an error), `snapshot` (returns an error), `restore` (returns an error).
- **Reason**:
  1. The `qdrant-edge = "=0.8.0"` crate (published Aug 2026) is real but pulls in 123 direct + 453 transitive deps; building it adds ~5 minutes to CI and >2GB of disk usage. Default builds keep the feature OFF; a dedicated CI job (`smoke-install.yml → qdrant-build`) verifies the feature compiles.
  2. The sparse-vector API in qdrant-edge requires named sparse vectors configured in `EdgeConfig` at collection-creation time; bridging our `SparseVector` type to qdrant-edge's named sparse vectors requires schema changes that are TODO.
  3. The snapshot/restore API in qdrant-edge uses its own binary format, not the adapter-agnostic JSON `CollectionSnapshot` that the spec requires; bridging is TODO.
- **Impact**:
  - User-visible: hybrid search (dense + sparse) falls back to the `InMemoryVectorStore` (Fake adapter, spec A12) when the qdrant feature is off. Production deployments that need persistent vectors must build with `--features qdrant` AND accept that sparse search + snapshot/restore are not yet available.
  - Technical: the InMemoryVectorStore passes the full contract test suite (`vectorstore_contract::run_contract_tests`); the QdrantEdgeVectorStore passes only the dense subset.
- **Status**: pending owner approval

---

## DEV-003 — macOS app-crate tests excluded; smoke-install is the alternative

- **Date**: Session 35 (originally documented), Session 37 (M12 honest re-investigation)
- **Where**: `.github/workflows/ci.yml` — `cargo test --workspace --all-targets --exclude supportos-plusplus-app` on macOS
- **Spec says**: A11 — CI matrix green on all 3 OSes.
- **Reference does**: n/a (the reference repo is the original TypeScript `supportos` app; it does not constrain Rust test strategy).
- **Deviation**: The Tauri shell crate (`supportos-plusplus-app`) is excluded from `cargo test` on macOS only.
- **Reason**:
  1. The `tauri::generate_context!()` macro on macOS generates a `_EMBED_INFO_PLIST` symbol used by the macOS linker to embed the `Info.plist` into the binary.
  2. The app crate's `Cargo.toml` declares `crate-type = ["staticlib", "cdylib", "rlib"]` (standard Tauri template for mobile compatibility). When `cargo test` builds the crate for testing, the symbol is generated once per crate-type, causing a "duplicate symbol `_EMBED_INFO_PLIST`" link error.
  3. The duplicate-symbol link error is in the test-binary link step, NOT in the actual app build. The app build itself succeeds (verified by Nightly workflow on macOS).
  4. **The smoke-install workflow (M12 PRIORITY 1) launches the actual built macOS DMG app and verifies the self-check runs** — this is more authoritative than unit tests of the Tauri shell crate would be.
- **Impact**:
  - User-visible: none — the macOS app launches correctly (verified by smoke-install).
  - Technical: the 8 unit tests in `crates/app/src-tauri/src/lib.rs::tests` run on Linux + Windows but NOT on macOS. They verify: `ping_returns_pong`, `version_is_set`, `catalog_counts_match_spec`, `copilot_allowlist_has_22_tools`, `parity_gate_passes`, `first_run_state_reads_false_on_fresh_db`, `first_run_state_marks_done_on_write`, `open_db_with_all_migrations_applies_m003_through_m027`, `self_check_report_is_honest_on_fresh_db`.
- **Status**: pending owner approval — alternative verification via smoke-install macOS DMG launch.

---

## DEV-004 — Linux arm64 + macOS Intel: macOS Intel added, Linux arm64 not yet

- **Date**: Session 37 (M12)
- **Where**: `.github/workflows/nightly.yml` matrix
- **Spec says**: A4 — "spike that proves, on CI runners for Windows x64, macOS arm64, macOS x64 (Intel), Linux x64 and Linux arm64 (where feasible)..."
- **Reference does**: n/a
- **Deviation**:
  - ✅ macOS Intel (x86_64-apple-darwin) added via `macos-13` runner.
  - ❌ Linux arm64 NOT added (no free arm64 runner on GitHub Actions free tier).
- **Reason**: GitHub Actions free tier does not include Linux arm64 runners. Native arm64 Linux builds require either a self-hosted runner, GitHub's pay-tier arm64 runners, or QEMU-based cross-compilation (which is slow + unreliable for Tauri).
- **Impact**:
  - User-visible: Linux arm64 users must build from source.
  - Technical: the spec's per-platform Qdrant Edge spike for Linux arm64 is BLOCKED pending owner action (a CI runner or a local arm64 machine).
- **Status**: pending owner approval — owner must provide an arm64 CI runner or accept x64-only Linux builds.

---

## DEV-005 — Vector store in production builds is InMemoryVectorStore, not Qdrant Edge

- **Date**: Session 37 (M12)
- **Where**: `crates/app/src-tauri/src/lib.rs` (boot path)
- **Spec says**: A4 — Qdrant Edge is the production vector store.
- **Reference does**: n/a
- **Deviation**: The Tauri shell does NOT instantiate a QdrantEdgeVectorStore at boot. The InMemoryVectorStore (Fake adapter, spec A12) is the only adapter wired in. The `self_check` IPC command reports this honestly: `qdrant_feature_enabled: false` (when feature is off) and `adapter: "in_memory"`.
- **Reason**:
  1. The QdrantEdgeVectorStore adapter (DEV-002) only implements the dense-vector subset; sparse/snapshot/restore return errors.
  2. Wiring the adapter into the boot path requires choosing a persistence directory + lifecycle management; deferred until the adapter is feature-complete.
  3. The startup self-check (M12-P2) now reports the actual adapter kind to the UI + logs, so users can see the truth.
- **Impact**:
  - User-visible: hybrid search + vector-backed features work in-memory only; vectors are NOT persisted across restarts in the default build.
  - Technical: demo mode + tests pass; production deployments need `--features qdrant` + adapter completion (DEV-002).
- **Status**: pending owner approval
