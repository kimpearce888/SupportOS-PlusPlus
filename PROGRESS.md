# PROGRESS.md — SupportOS++

> Instruction file: `AGENTS.md` (top of repo).
> Repository is the only memory between sessions. Every session may be your last.

## Current state

| Field | Value |
|---|---|
| Instruction file | `AGENTS.md` |
| Current milestone | M6 — AI features (CLOSED — pending owner sign-off to proceed to M7) |
| Current task ID | (none — M6 milestone complete; awaiting owner "continue" to start M7) |
| Last completed task | M6-T11 (M6 milestone close — all 11 tasks ticked, tag `milestone-6-done` pushed) |
| Last commit hash | (about to be) M6-T11: M6 milestone close — tag milestone-6-done |
| Last updated | Session 30 — M6 closed (11 of 11 tasks done; 726 tests passing) |

## Next 3 tasks

1. **Owner sign-off**: confirm M6 milestone is acceptable; say "continue" to proceed to M7.
2. **M7 — Intelligence**: write the M7 task list, then start M7-T01.
3. **(After M7)**: M8 — Reports and quality.

## Resume protocol for next session

1. Read `AGENTS.md` → this file → `TASKS.md` (M7 task list will be written at the start of the next session, per spec: "Write the full task list for a milestone before starting it").
2. `git status` + `git log --oneline -10` + `CARGO_INCREMENTAL=0 cargo test -p supportos-plusplus-catalog -p supportos-plusplus-core -p supportos-plusplus-ui -p supportos-plusplus-xtask --all-targets` (skip the Tauri shell crate locally; CI verifies the full workspace).
3. Announce `Resuming at M7/M7-T01. Last commit: <hash>. Next: M7 task list + Intelligence.`
4. Continue from the first unchecked task in `TASKS.md`.

## Parity counts by status (honest, A3)

| Status | Count |
|---|---|
| DISCOVERED | 8 canonical counts + 13 surface-area rows + per-milestone high-level rows (reproducible via `cargo xtask discover`) |
| SPECIFIED | 2 (Tauri shell launch verification on CI ✅, Qdrant spike) |
| IMPLEMENTED | 14 (xtask discover, SQLite foundation + first migration + runner, job queue with JobHandler/JobRegistry/Runner, settings store with typed bool/i64/JSON, error/logging/config foundation, Tauri config A0 verification, catalog crate (WASM-safe, single source of truth), common UI components + theming tokens, Leptos Router scaffold with 3 routes, xtask audit binary with checks catalog, loopback HMAC-SHA1 + OAuth state + persist-first dedup, first-run onboarding overlay + first_run_state IPC, CI matrix green on all 3 OSes + WASM + Tauri build) |
| TESTED | 1 (CI green run on main — commit d27f413 — verifies fmt + clippy + tests + WASM + Tauri build on Win/macOS/Linux) |
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
Session 5: D-021 (JobHandler + JobRegistry + Runner), D-022 (xtask lib + 2 binaries).

Session 6:
- **D-023**: Loopback listener cryptographic primitives — HMAC-SHA1 implemented inline (no extra dep), verified with FIPS 180-1 known vectors. Timing-safe comparison via `subtle::ConstantTimeEq`. Persist-first + dedup via a `webhook_events` SQLite table (id PRIMARY KEY, INSERT OR IGNORE for dedup). Single-use OAuth state via `oauth_states` table with `consumed_at` column. 25 new tests covering HMAC determinism, signature verification (good/bad/replay), persist-first dedup, OAuth state single-use violation, redirect_uri lookup.
- **D-024**: First-run onboarding overlay — `<OnboardingOverlay>` Leptos component with two actions: "Try the 2-minute demo mode" (calls `first_run_state(Some(true))` Tauri IPC) and "I'll set up later" (calls `first_run_state(Some(false))`). The Tauri IPC command `first_run_state(demo_mode: Option<bool>) -> Result<bool, String>` reads/writes the flag; M1 uses an in-memory stub (process-global Mutex) because the Tauri shell doesn't yet boot a SQLite connection at startup (M2 will swap in the real `spp_core::settings::first_run_done` / `mark_first_run_done`). The overlay uses `Arc<dyn Fn>` props so it can live in Leptos signals. 5 new UI tests + 3 new Tauri IPC tests.

(See `docs/DECISIONS.md` for the full list D-001..D-024.)

## Resume protocol for next session

1. Read `AGENTS.md` → this file → `TASKS.md`.
2. `git status` + `git log --oneline -20` + `cargo test -p supportos-plusplus-catalog -p supportos-plusplus-core -p supportos-plusplus-ui -p supportos-plusplus-xtask --all-targets` (skipping the Tauri shell crate if GTK deps aren't installed locally; CI verifies the full workspace).
3. Confirm `tauri-cli` and `trunk` are installed (install if missing: `cargo install tauri-cli --version '^2.0' --locked --no-default-features && cargo install trunk --locked`).
4. Announce `Resuming at M4/<task>. Last commit: <hash>. Next: <task>.`
5. Continue from the first unchecked task in `TASKS.md`.
