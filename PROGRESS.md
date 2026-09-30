# PROGRESS.md — SupportOS++

> Instruction file: `AGENTS.md` (top of repo).
> Repository is the only memory between sessions. Every session may be your last.

## Current state

| Field | Value |
|---|---|
| Instruction file | `AGENTS.md` |
| Current milestone | M1 — Foundation |
| Current task ID | M1-T13 (next up) — M1-T05 ✅, M1-T14 ✅ done this session |
| Last completed task | M1-T05 (job queue closed out with JobHandler trait + JobRegistry + Runner + end-to-end integration tests) + M1-T14 (cargo xtask audit binary with checks::path_exists + checks::config_a0) |
| Last commit hash | `f570511` (f570511c61946570c1c079e4b8fc8a039e5ec04c) — M1-T05/M1-T14 done pushed to `main` |
| Last updated | Session 5 |

## Next 3 tasks

1. **M1-T13**: First-run onboarding stub — the foundation is in place (`app_state.first_run_done` flag from M001, `settings::first_run_done()`/`mark_first_run_done()` helpers, `<StateView>` component pattern). M1-T13 wires them together: Tauri shell setup hook reads `first_run_done`; if false, the UI shows a 2-minute demo-mode offer overlay. Pure-Rust work — no new external deps.
2. **M1-T09**: CI matrix runs on Win/macOS/Linux with `rustfmt --check`, `clippy -D warnings`, `cargo test`, `cargo build --release`, `trunk build`, headless demo-mode boot. The CI workflow file is already in place; M1-T09 is about confirming a green run on `main` and adding any missing pieces.
3. **M1-T15**: M1 milestone close — every M1 task ticked, CI green on all 3 OSes, tag `milestone-1-done`, report parity counts + deviations + BLOCKED, STOP, wait for owner sign-off.

## Parity counts by status (honest, A3)

| Status | Count |
|---|---|
| DISCOVERED | 8 canonical counts + 13 surface-area rows + per-milestone high-level rows (reproducible via `cargo xtask discover`) |
| SPECIFIED | 3 (Tauri shell launch verification on CI, installers, Qdrant spike) |
| IMPLEMENTED | 11 (xtask discover, SQLite foundation + first migration + runner, job queue with JobHandler/JobRegistry/Runner, settings store with typed bool/i64/JSON, error/logging/config foundation, Tauri config A0 verification, catalog crate (WASM-safe, single source of truth), common UI components + theming tokens, Leptos Router scaffold with 3 routes, xtask audit binary with checks catalog) |
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

Session 1: D-001 through D-005.
Session 2: D-013 (julianday), D-014 (inventory.json).
Session 3: D-015 (migrations const array), D-016 (verify-config xtask), D-017 (typed settings helpers).
Session 4: D-018 (catalog crate extraction), D-019 (ViewState enum + StateView), D-020 (stable Rust, no Leptos nightly).

Session 5:
- **D-021**: `JobHandler` trait + `JobRegistry` + `Runner` close out the job queue per KNOWN PITFALLS. The trait is `Send + Sync` so the registry can be shared across Tokio workers; the registry is `Clone` (backed by `Arc<HashMap>`); the runner has a `max_iterations` bound so a runaway enqueue source can't livelock a single `run_until_idle` call. End-to-end tests cover enqueue → claim → execute → complete, retry-then-succeed, and always-fail → dead-letter.
- **D-022**: `cargo xtask audit` is a separate binary (`crates/xtask/src/bin/audit.rs`) sharing a `spp_xtask` lib with the `xtask` binary. Reuses the workspace's `verify_config` module for the `config_a0` check (one source of truth per A12). The reference's 1100-line `audit-phase1.mjs` will be ported check-by-check as the matching milestone lands (M2 sync, M3 saved-view injection, M9 campaigns).

(See `docs/DECISIONS.md` for the full list D-001..D-022.)

## Resume protocol for next session

1. Read `AGENTS.md` → this file → `TASKS.md`.
2. `git status` + `git log --oneline -20` + `cargo xtask lint && cargo xtask test` (skipping the Tauri shell crate if GTK deps aren't installed locally; CI verifies the full workspace).
3. Confirm `tauri-cli` and `trunk` are installed (install if missing: `cargo install tauri-cli --version '^2.0' --locked --no-default-features && cargo install trunk --locked`).
4. Announce `Resuming at M1/M1-T13. Last commit: <hash>. Next: first-run onboarding stub (foundation already in place: app_state.first_run_done + settings::first_run_done + StateView component).`
5. Continue from the first unchecked task in `TASKS.md`.
