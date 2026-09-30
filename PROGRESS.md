# PROGRESS.md — SupportOS++

> Instruction file: `AGENTS.md` (top of repo).
> Repository is the only memory between sessions. Every session may be your last.

## Current state

| Field | Value |
|---|---|
| Instruction file | `AGENTS.md` |
| Current milestone | M1 — Foundation |
| Current task ID | M1-T05 (next up) — M1-T07 ✅, M1-T08 ✅ done this session; new `catalog` crate (D-018) extracted |
| Last completed task | M1-T07 (common UI components: StateView, LoadingState, EmptyState, ErrorState, Button, theming tokens) + M1-T08 (Leptos Router scaffold with /, /settings, /*any not-found) + extracted `supportos-plusplus-catalog` crate as the WASM-safe single source of truth (D-018) |
| Last commit hash | `a4261e8` (a4261e8) — M1-T07/M1-T08 done, catalog crate extracted pushed to `main` |
| Last updated | Session 4 |

## Next 3 tasks

1. **M1-T05**: Job queue — already at IMPLEMENTED status with the julianday fix (D-013); M1-T05 closes it out by adding a `JobHandler` trait + a small registry + an end-to-end integration test that exercises the full claim → execute → complete cycle. Pure Rust in `crates/core`.
2. **M1-T13**: First-run onboarding stub — the foundation is in place (`app_state.first_run_done` flag from M001, `settings::first_run_done()`/`mark_first_run_done()` helpers). M1-T13 wires it into the Tauri shell's setup hook + a UI overlay.
3. **M1-T14**: `cargo xtask audit` — port the reference's `scripts/audit-phase1.mjs` to a Rust binary that runs against a packaged app and reports a JSON findings list. Independent of GUI libs.

## Parity counts by status (honest, A3)

| Status | Count |
|---|---|
| DISCOVERED | 8 canonical counts + 13 surface-area rows + per-milestone high-level rows (reproducible via `cargo xtask discover`) |
| SPECIFIED | 4 (Tauri shell launch verification on CI, installers, Qdrant spike, demo-mode boot) |
| IMPLEMENTED | 9 (xtask discover, SQLite foundation + first migration + runner, job queue, settings store with typed bool/i64/JSON, error/logging/config foundation, Tauri config A0 verification, catalog crate (WASM-safe, single source of truth), common UI components + theming tokens, Leptos Router scaffold with 3 routes) |
| TESTED | 0 (foundation tested at unit level; no milestone complete yet) |
| PACKAGED | 0 |
| VERIFIED | 0 |

**The project is NOT complete and is NOT at 100% parity.** Do not claim otherwise.

## Known issues

- Tauri CLI and `trunk` CLI were not fully built at end of session 1 (long Rust compile). The Cargo workspace + `tauri-cli` as a dev-dependency means `cargo xtask dev` will work once the toolchain is available; until then, the M1-T02 build verification step is BLOCKED-on-toolchain locally. **The CI workflow on ubuntu-22.04 installs the GTK/WebKit2GTK system deps via apt-get and verifies the full workspace build.**
- The local dev sandbox in session 2 has no sudo, so the GTK/WebKit2GTK system libraries cannot be installed locally. M1-T02 (Tauri shell launch) cannot be verified locally; CI must verify. This is a tooling issue, not a spec deviation.
- GitHub Personal Access Token was supplied by the owner in plaintext in chat (session 1). It is stored ONLY in `~/.git-credentials` on the dev machine (never in the repo). The owner has been advised to revoke and rotate it.
- Session 1's `jobs::claim_next` had a flaky-test bug (~1 in 5 failures) caused by lexical ISO-8601 comparison (KNOWN PITFALLS). **Fixed in session 2 (D-013)** — `claim_next` now compares via `julianday()`. Verified stable over 10 consecutive test runs.

## Decisions this session

Session 1 (recorded above): D-001 through D-005.
Session 2: D-013 (julianday), D-014 (inventory.json).
Session 3: D-015 (migrations const array), D-016 (verify-config xtask), D-017 (typed settings helpers).

Session 4:
- **D-018**: Extracted `supportos-plusplus-catalog` crate — WASM-safe (no I/O deps) — as the single source of truth for closed vocabularies. The UI crate depends on it directly (not on `core`, which has tokio/rusqlite/etc. that don't compile to WASM). `core` re-exports the catalog so native callers can keep writing `spp_core::catalog::*`.
- **D-019**: `ViewState` enum + `<StateView>` component — the canonical implementation of the KNOWN PITFALLS rule ("every view has loading, empty, and error states"). Wrong states are impossible by construction; no caller can render an error without a message or a loading state with results.
- **D-020**: Leptos 0.6 with stable Rust (no `nightly` feature). Removed `nightly` feature flag from the Leptos dep — the `server_fn_macro` crate requires nightly when that feature is on, which broke CI. CSR-only + stable Rust is enough for our use case.

(See `docs/DECISIONS.md` for the full list D-001..D-020.)

## Resume protocol for next session

1. Read `AGENTS.md` → this file → `TASKS.md`.
2. `git status` + `git log --oneline -20` + `cargo xtask lint && cargo xtask test` (skipping the Tauri shell crate if GTK deps aren't installed locally; CI verifies the full workspace).
3. Confirm `tauri-cli` and `trunk` are installed (install if missing: `cargo install tauri-cli --version '^2.0' --locked --no-default-features && cargo install trunk --locked`).
4. Announce `Resuming at M1/M1-T05. Last commit: <hash>. Next: close out job queue with JobHandler trait + integration test.`
5. Continue from the first unchecked task in `TASKS.md`.
