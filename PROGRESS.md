# PROGRESS.md — SupportOS++

> Instruction file: `AGENTS.md` (top of repo).
> Repository is the only memory between sessions. Every session may be your last.

## Current state

| Field | Value |
|---|---|
| Instruction file | `AGENTS.md` |
| Current milestone | M1 — Foundation |
| Current task ID | M1-T02 (next up) — M1-T01 ✅ done this session |
| Last completed task | M1-T01 — `cargo xtask discover` (A7) implemented; 10/10 canonical counts match spec; writes `docs/original-notes/inventory.json` |
| Last commit hash | _(set after push — see `git log`)_ |
| Last updated | Session 2 |

## Next 3 tasks

1. **M1-T02**: Tauri 2 shell that launches on Win/macOS/Linux with the `SupportOS++` product name and `com.supportos.plusplus` bundle id. **BLOCKED locally** (no sudo → can't install GTK/WebKit2GTK); CI on ubuntu-22.04 will verify. Code is already in place from session 1; this task is primarily about installing the system deps and running `cargo xtask dev` to confirm the window opens.
2. **M1-T03**: SQLite first migration + runner (foundation already in `crates/core/src/db.rs`; M1-T03 will add the first *app* migration that creates the `application_settings` + `secrets` tables the `settings.rs` module already expects).
3. **M1-T04**: Settings store — already partially done; M1-T04 is to wire it into the Tauri IPC layer with redaction-on-read.

## Parity counts by status (honest, A3)

| Status | Count |
|---|---|
| DISCOVERED | 8 canonical counts + 13 surface-area rows + per-milestone high-level rows (now reproducible via `cargo xtask discover`) |
| SPECIFIED | 7 (M1 capabilities still pending: Tauri shell launch, Leptos UI scaffold, theming, CI matrix, installers, Qdrant spike, demo-mode boot) |
| IMPLEMENTED | 4 (xtask discover, SQLite foundation, job queue, settings store) |
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

Session 2:
- **D-013**: All timestamp comparisons in SQL go through `julianday()` (never lexical). Enforced by code review + clippy; documented in `crates/core/src/jobs.rs::claim_next`. Fixes a real flaky-test bug from session 1.
- **D-014**: `cargo xtask discover` writes a machine-readable `docs/original-notes/inventory.json` and exits non-zero if any canonical count differs from the spec. Makes reference drift detectable in CI.

(See `docs/DECISIONS.md` for the full list D-001..D-014.)

## Resume protocol for next session

1. Read `AGENTS.md` → this file → `TASKS.md`.
2. `git status` + `git log --oneline -20` + `cargo xtask lint && cargo xtask test` (skipping the Tauri shell crate if GTK deps aren't installed locally; CI verifies the full workspace).
3. Confirm `tauri-cli` and `trunk` are installed (install if missing: `cargo install tauri-cli --version '^2.0' --locked --no-default-features && cargo install trunk --locked`).
4. Announce `Resuming at M1/M1-T02. Last commit: <hash>. Next: Tauri 2 shell that launches.`
5. Continue from the first unchecked task in `TASKS.md`.
