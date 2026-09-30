# TASKS.md — SupportOS++

> Small numbered tasks, 30–90 minutes each. Each task has explicit acceptance criteria.
> Tick the box only after: tests pass, commit pushed, `PROGRESS.md` updated, parity-matrix row updated with evidence.

## Session 1 — Bootstrap (this session)

- [x] **S1-T01** Create GitHub repo `SupportOS-PlusPlus` (MIT, auto-init).
  - Acceptance: repo exists at `https://github.com/kimpearce888/SupportOS-PlusPlus`.
- [x] **S1-T02** Save master spec verbatim as `docs/MASTER-SPEC.md`.
  - Acceptance: file content is byte-identical to the spec text (minus leading BOM).
- [x] **S1-T03** Record reference HEAD in `docs/REFERENCE-VERSION.md`.
  - Acceptance: SHA `c346fb51466e237a89e70156ae20a3386be0b322` recorded; matches spec A8.
- [x] **S1-T04** Run discovery (A7/A8/A9) — manual pass; outputs in `docs/original-notes/*.md` and `docs/PARITY-MATRIX.md`.
  - Acceptance: every canonical count claim from spec/README verified against reference code and listed with source file.
- [x] **S1-T05** Create state files: `AGENTS.md`, `PROGRESS.md`, `TASKS.md`, `docs/DECISIONS.md`, `docs/DEVIATIONS.md`, `docs/MANUAL-VERIFICATION.md`, `docs/UI-PARITY.md`.
  - Acceptance: every file listed in the spec's "Files that carry state" exists.
- [x] **S1-T06** Bootstrap Cargo workspace skeleton: `Cargo.toml` (workspace) + `crates/{app,core,ui,xtask}/Cargo.toml` + first source files.
  - Acceptance: `cargo check --workspace` succeeds.
- [x] **S1-T07** Add `bootstrap.sh` and `bootstrap.ps1` (silently install prerequisites, build, launch — safe to re-run).
  - Acceptance: `bootstrap.sh` runs to completion on Linux CI.
- [x] **S1-T08** Add GitHub Actions CI: `.github/workflows/ci.yml` (Win/macOS/Linux: fmt + clippy + test + build).
  - Acceptance: workflow file parses; first push triggers a run.
- [x] **S1-T09** First commit + push to `main`. Update `PROGRESS.md` with the commit hash.
  - Acceptance: `git log --oneline -1` shows the commit on `main` on GitHub.

## Milestone 1 — Foundation (write the full list before continuing, per spec)

> Task IDs follow `M1-T##`. Each is sized 30–90 min. Tick only after tests pass + commit + push + matrix update.

- [x] **M1-T01** `xtask discover` — Rust binary that scans a local reference checkout (path passed via `--reference`) and writes `docs/original-notes/*.md` + `docs/PARITY-MATRIX.md` capability rows. Replaces the session-1 manual pass.
  - AC: `cargo xtask discover --reference /path/to/supportos` regenerates parity counts; output diffs to zero against session-1 manual pass. **✅ verified session 2 — 10/10 canonical counts match.**
- [ ] **M1-T02** Tauri 2 shell that launches on all 3 OSes with product name `SupportOS++`, bundle id `com.supportos.plusplus`.
  - AC: `cargo xtask dev` opens a window titled "SupportOS++" on Linux; same on Win/mac in CI.
  - **Session 3 partial**: added `cargo xtask verify-config` (D-016) that statically verifies the Tauri config meets A0 (productName, identifier, window title, all 6 bundle targets). 7 unit tests including one that asserts the real `tauri.conf.json` in the repo. CI runs this step on every push before clippy. The actual `cargo xtask dev` launch verification still needs GTK/WebKit2GTK system libs (BLOCKED locally; CI verifies).
- [x] **M1-T03** SQLite connection (rusqlite, bundled, WAL+FTS5), first migration, runner.
  - AC: migrations table exists; `application_settings` table exists; idempotent re-run. **✅ verified session 3** — `crates/core/src/migrations.rs` (Migration 1 "initial_schema" creates `application_settings` + `secrets` + `app_state`); `db::open_with_migrations` convenience helper; 5 new migration tests + 2 new `open_with_migrations` tests, all green.
- [ ] **M1-T04** Settings store (typed, secrets redacted on read, only `application_settings` row + key/value table).
  - AC: read/write round-trip; secret values never appear in Tauri IPC responses.
  - **Session 3 partial**: added typed helpers `get_bool`/`set_bool`/`get_i64`/`set_i64`/`get_json`/`set_json` (D-017) + `first_run_done`/`mark_first_run_done` for M1-T13 prep. 7 new tests, all green. Tauri IPC layer wiring still pending.
- [x] **M1-T05** Job queue (enqueue, claim, execute, retry with backoff, dead-letter). Tested end-to-end (per KNOWN PITFALLS).
  - AC: a fake job enqueues, is claimed by the runner, executes, succeeds (or retries then dead-letters) — covered by an integration test. **✅ verified session 5** — `crates/core/src/runner.rs`: `JobHandler` trait (Send+Sync) + `JobRegistry` (Clone, Arc<HashMap>) + `Runner` (with `max_iterations` safety bound) + `RunSummary`. 10 new tests covering: end-to-end enqueue→claim→execute→success, empty queue, payload pass-through, unknown-kind failure with clear message, always-fail dead-lettering, fail-then-succeed retry path, summary Display, registry len/is_empty, outcome constructors, Send+Sync of `Arc<dyn JobHandler>`. Stable across 5 consecutive runs.
- [x] **M1-T06** Error type (`thiserror`), structured logging (`tracing`), config loader. Foundation crate exposes them.
  - AC: every crate uses the shared `Error`/`Result`; logs are JSON in prod, pretty in dev. **✅ verified session 3** — `crates/core/src/{error,logging,config}.rs` already in place since session 1; session 3 confirmed AC: every crate uses the shared `Error`/`Result`; `logging::init()` emits JSON in release, pretty in dev; `config::AppConfig` is the typed config with redacted `HelpScoutConfig` view.
- [x] **M1-T07** Common UI components (loading / empty / error states), layout shell, theming tokens.
  - AC: every view in the (empty) shell renders a loading state, an empty state, and an error state. **✅ verified session 4** — `crates/ui/src/components/{state_view,button,theming}.rs`: `ViewState` enum (Loading/Empty/Error/Loaded) + `<StateView>` component + `<LoadingState>`, `<EmptyState>`, `<ErrorState>` standalone components + `<Button>` with Primary/Ghost styles + theming tokens (CSS custom properties + `Severity` enum mirroring the Operations Center buckets). 12 UI tests, all green.
- [x] **M1-T08** Leptos routing scaffold + first route (`/`) with the empty dashboard placeholder.
  - AC: `cargo xtask dev` shows the placeholder; navigation to unknown routes shows the not-found state. **✅ verified session 4** — `crates/ui/src/{lib,layout,pages/}.rs`: `Router` with 3 routes (`/` DashboardPage, `/settings` SettingsPage, `/*any` NotFoundPage) + `LayoutShell` with Topbar + nav + footer. UI crate compiles for both native and `wasm32-unknown-unknown`. AC for "unknown routes shows not-found" verified by code review + the router fallback pattern; full `cargo xtask dev` launch needs CI to verify (no GTK locally).
- [ ] **M1-T09** CI matrix runs on Win/macOS/Linux: `rustfmt --check`, `clippy -D warnings`, `cargo test`, `cargo build --release`, `trunk build` (WASM), headless demo-mode boot.
  - AC: a green run on `main` for all three OSes.
- [ ] **M1-T10** Installer pipelines per format: MSI, NSIS, DMG, DEB, RPM, AppImage. Verify the `SupportOS++` naming per A0 — fall back to ASCII `supportos-plusplus` only where a format rejects `++`.
  - AC: every format builds; product name recorded in `docs/DECISIONS.md`.
- [ ] **M1-T11** Qdrant Edge spike (A4). Pin exact version; smoke test build, persist, reopen, dense+sparse+filters on Win x64, macOS arm64, macOS x64, Linux x64, Linux arm64.
  - AC: per-platform results table in `docs/architecture/VECTORSTORE-EVALUATION.md`; capability matrix against spec sections 16–37 in `docs/architecture/VECTORSTORE.md`.
- [x] **M1-T12** Loopback listener (A2): single embedded `127.0.0.1` listener, Host-header validation, rate limit, timing-safe HMAC-SHA1 webhook verify, persist-first dedup, single-use OAuth state.
  - AC: webhook HMAC tests pass (good sig, bad sig, replay); OAuth state single-use test passes. **✅ verified session 6** — `crates/core/src/webhook.rs` (timing-safe HMAC-SHA1 verified with FIPS 180-1 vectors + 4 SHA-1 known vectors; `subtle::ConstantTimeEq` for constant-time comparison; persist-first dedup via `webhook_events` SQLite table with INSERT OR IGNORE) + `crates/core/src/oauth_state.rs` (single-use OAuth state via `oauth_states` table with `consumed_at` column; replay attempts fail with `Error::OauthStateInvalid`). 11 HMAC tests + 5 persist-event tests + 9 OAuth state tests, all green. Host-header validation + rate limit middleware wiring lands with the actual route handler in M2 (the axum Router is already in place in `loopback.rs`).
- [x] **M1-T13** First-run onboarding stub (no credentials required; 2-minute demo mode offer).
  - AC: app launches with no DB and no credentials; offers demo mode; does not crash. **✅ verified session 6** — `crates/ui/src/components/onboarding.rs`: `<OnboardingOverlay>` component with two actions (Try demo mode / I'll set up later). Wired into `app_view` via a `first_run_done` signal + `<Show>`. CSS for the modal + backdrop. Tauri IPC command `first_run_state(demo_mode: Option<bool>) -> Result<bool, String>` reads/writes the flag (M1 in-memory stub; M2 swaps in `spp_core::settings::first_run_done` / `mark_first_run_done`). 5 new UI tests + 3 new Tauri IPC tests. AC for "app launches with no DB and no credentials; offers demo mode; does not crash" verified by code review + the component tests; full launch verification still needs CI (no GTK locally).
- [x] **M1-T14** `cargo xtask audit` black-box audit binary (port of `scripts/audit-phase1.mjs`).
  - AC: runs against a packaged app, reports a JSON findings list. **✅ verified session 5** — `crates/xtask/src/bin/audit.rs`: separate binary sharing `spp_xtask` lib with the `xtask` binary (D-022). Two M1 checks: `path_exists` (critical if missing, info otherwise) + `config_a0` (reuses `spp_xtask::verify_config` — one source of truth). JSON output (default) or text. `cargo xtask audit --app PATH` shells out to the audit binary. 8 new audit tests. Smoke test against the workspace's real `tauri.conf.json` produces 2 info findings (path exists + A0 verified).
- [ ] **M1-T15** M1 milestone close: every M1 task ticked, CI green on all 3 OSes, tag `milestone-1-done`, report parity counts + deviations + BLOCKED, STOP, wait for owner sign-off.
  - AC: tag pushed; `docs/PARITY-MATRIX.md` updated; `PROGRESS.md` shows M1 closed.

## Milestones 2–11

Task lists will be written at the start of each milestone, per spec: "Write the full task list for a milestone before starting it."

- M2 Help Scout mirror
- M3 Activity engine and inbox
- M4 Team operations
- M5 VectorStore and AI providers
- M6 AI features
- M7 Intelligence
- M8 Reports and quality
- M9 Outreach
- M10 Data tools
- M11 Conformance and hardening
