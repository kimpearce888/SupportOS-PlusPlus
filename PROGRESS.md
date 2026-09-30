# PROGRESS.md — SupportOS++

> Instruction file: `AGENTS.md` (top of repo).
> Repository is the only memory between sessions. Every session may be your last.

## Current state

| Field | Value |
|---|---|
| Instruction file | `AGENTS.md` |
| Current milestone | M1 — Foundation |
| Current task ID | M1-T07 (next up) — M1-T02 partial ✅, M1-T03 ✅, M1-T04 partial ✅, M1-T06 ✅ done this session |
| Last completed task | M1-T03 (SQLite first migration + runner) + M1-T06 (foundation: error/logging/config) + M1-T02 partial (Tauri config A0 verification) + M1-T04 partial (typed settings store with bool/i64/JSON + first-run flag) |
| Last commit hash | `c3b8c48` (c3b8c48880f56d0b6c4b9dbca2b5c464eb014c46) — M1-T03/M1-T06 done, M1-T02/M1-T04 partial pushed to `main` |
| Last updated | Session 3 |

## Next 3 tasks

1. **M1-T07**: Common UI components (loading / empty / error states), layout shell, theming tokens. Pure Leptos work; doesn't strictly need the Tauri shell to be linked locally — can be developed against `trunk serve` standalone.
2. **M1-T08**: Leptos routing scaffold + first route (`/`) with the empty dashboard placeholder.
3. **M1-T05**: Job queue — already at IMPLEMENTED status with the julianday fix; M1-T05 just needs the final "execute" handler shape (a small trait + a registration mechanism) to close out.

## Parity counts by status (honest, A3)

| Status | Count |
|---|---|
| DISCOVERED | 8 canonical counts + 13 surface-area rows + per-milestone high-level rows (reproducible via `cargo xtask discover`) |
| SPECIFIED | 5 (Tauri shell launch verification on CI, Leptos UI scaffold, theming, installers, Qdrant spike) |
| IMPLEMENTED | 6 (xtask discover, SQLite foundation + first migration + runner, job queue, settings store with typed bool/i64/JSON, error/logging/config foundation, Tauri config A0 verification) |
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

Session 3:
- **D-015**: Migrations are a single source-of-truth `&[Migration]` const array in `crates/core/src/migrations.rs`. Each migration is forward-only, versioned, applied in order, never edited after release. New migrations append at the end with the next version number.
- **D-016**: `cargo xtask verify-config` statically parses `tauri.conf.json` and asserts the spec amendment A0 mandates (`productName = "SupportOS++"`, `identifier = "com.supportos.plusplus"`, `window[0].title = "SupportOS++"`, all 6 bundle targets present). Runs in CI on every push, before clippy, without needing GTK/WebKit2GTK system deps.
- **D-017**: Typed settings store helpers (`get_bool`, `set_bool`, `get_i64`, `set_i64`, `get_json`, `set_json`) wrap the string-only `application_settings` table so callers get type-safe reads/writes with proper `Error::Config` validation on parse failure.

(See `docs/DECISIONS.md` for the full list D-001..D-017.)

## Resume protocol for next session

1. Read `AGENTS.md` → this file → `TASKS.md`.
2. `git status` + `git log --oneline -20` + `cargo xtask lint && cargo xtask test` (skipping the Tauri shell crate if GTK deps aren't installed locally; CI verifies the full workspace).
3. Confirm `tauri-cli` and `trunk` are installed (install if missing: `cargo install tauri-cli --version '^2.0' --locked --no-default-features && cargo install trunk --locked`).
4. Announce `Resuming at M1/M1-T07. Last commit: <hash>. Next: common UI components + Leptos routing scaffold.`
5. Continue from the first unchecked task in `TASKS.md`.
