# PROGRESS.md — SupportOS++ session/progress state

Concise resume-state for the multi-session strict parity audit.
Canonical audit record: **PARITY.md** (F-IDs + evidence).

## Baseline

- Reference: https://github.com/kimpearce888/supportos — HEAD `c346fb5`
  (TypeScript v2.2.1, 310 routes, 16 migrations, 672 tests, 26 UI routes)
- Port: https://github.com/kimpearce888/SupportOS-PlusPlus — audit started
  at HEAD `ac80872`

## Current phase

**Phase 3–4** (repository cleanup landed; execution audit + repair in
progress). Overall flow: Phase 0 freeze → Phase 1 reference inventory →
Phase 2 port inventory → PARITY.md F-IDs → Phase 3 cleanup → Phase 4
execution audit → Phase 5 differential → Phase 6 UI parity → Phase 7 fix
everything → final proof (rebuild, packages, cleanliness scan, verdict).

## Completed (this audit)

- Phase 0: baseline frozen (both HEADs recorded above; clean trees).
- Phase 1: full reference inventory (see `docs/original-notes/` and the
  audit inventories behind PARITY.md): 310 routes / 16 migrations / 132
  tables (8 FTS5) / 672 tests / 26 UI routes / 16 notification kinds /
  16 ops tiles / 22 AI tools / 7 SSE event types.
- Phase 2: port inventory recalculated: 205 routes registered (58 stubs) +
  33 unregistered handlers; ~47 tables + 2 FTS5; 941 tests; UI 7 routes
  wired, 17 page modules dead; SSE 3 event types; vector store unwired in
  production.
- PARITY.md rewritten from scratch: 157 F-IDs with statuses recalculated
  from source (old PARITY.md contained false claims and was replaced).
- Phase 3 cleanup (Linux-only, deb+AppImage only, Rust-native):
  - Deleted: `bootstrap.ps1`, `icons/icon.ico`, `icons/icon.icns`, all
    7 Python scripts (E2E driver ported to Rust: `xtask --bin e2e`),
    `docs/UI-PARITY.md`, `docs/audit/UI-GAP.md`, `docs/FINAL-PARITY-AUDIT.md`.
  - `tauri.conf.json` targets → `["deb","appimage"]`; macOS/windows bundle
    blocks removed.
  - `verify_config.rs`: REQUIRED = deb+appimage, added FORBIDDEN list
    (rpm/msi/nsis/dmg) + lockstep tests.
  - `config.rs`: Linux-only data dir (windows/macos cfg branches removed).
  - App crate: `not(target_os="macos")` gates removed.
  - All 5 CI workflows: Linux-only, RPM jobs/refs removed, Python steps
    removed, bundle-format assertion added, `branches:` verified `[main]`.
  - `bootstrap.sh`: Darwin/dnf branches removed.
  - Unused `hyper` dependency removed; `.gitattributes` ico/icns rules
    removed.
  - README/AGENTS/DEVIATIONS rewritten to match reality (24-page/RPM/
    test-count/audit-findings claims corrected; DEV-002 stale claim fixed
    — adapter IS complete).
  - TASKS.md replaced with a concise bounded tracker.

## Remaining (top blockers — see PARITY.md for all 157 F-IDs)

1. F-021 webhook route 500 on valid HMAC (column drift) + F-019 base64
   signature + F-024 pipeline wiring.
2. F-006 real HTTP status codes (port returns 200 + `_status`).
3. F-014–F-018 SSE format (`event:` names, 7 events, UI client URL broken).
4. F-027–F-035 sync engine wiring (real provider, OAuth, registration).
5. F-041 .sosync byte-compat with reference (scrypt N, magic, layout).
6. F-044 HTML sanitizer; F-045 SSRF DNS-resolution parity.
7. F-051 EXTRA AI providers (Ollama/Generic) — remove per no-scope-expansion.
8. F-059 vector store production wiring + F-061 hybrid search over HTTP.
9. F-077 inbox missing ~19 routes; F-083 SLA business-minutes; F-084
   computed reports; F-086 CSV export.
10. UI: F-109–F-139 (17 dead pages to wire, keyboard shortcuts, URL state,
    theme, toasts, withGlobalTauri/IPC fix).
11. F-144–F-149 packaging verification (deb install/launch, AppImage) —
    environment-limited without root; see PARITY.md notes.

## Last verification (Session A, 2026-10-02)

- `cargo fmt --all` — clean.
- `cargo clippy` -D warnings — clean on core, ui, xtask, catalog, app
  (app via local sysroot: PKG_CONFIG_PATH + LIBRARY_PATH).
- `cargo test --workspace --exclude supportos-plusplus-app` — 906 passed,
  0 failed (788 core / 71 ui / 21 catalog / 26 xtask). The app crate's 11
  tests passed earlier under the sysroot (excluded in the final run only to
  fit the disk-constrained sandbox; GTK deps cost ~3 GB).
- Live execution audit (example http_server, ports 3457-3461): health,
  system/db, webhook (5 scenarios), SSE (hello + named events + : ping +
  conversation event on mutation), prefs (15 types + 422s), dashboard,
  status codes (404/422/200/429), DNS-rebinding guard.
- Environment limitation: no root/sudo in the audit sandbox — Tauri GUI
  build verified via a user-space sysroot extracted from distro debs;
  .deb/.AppImage packaging + install smoke still to be executed (CI has
  the jobs; local verification pending an environment with more disk).

## Session log

- Session A (2026-10-02): phases 0–4 complete; Phase 7 fixes landed in 6
  commits (PARITY rewrite, Linux-only cleanup, webhook pipeline, SSE wire
  format, SQL drift + prefs, status codes, EXTRA provider removal). 24
  F-IDs advanced to MATCH with execution evidence. Next session: T9 sync
  engine wiring (real HelpScout provider + OAuth + job runner + boot
  drain), then T10 .sosync byte-compat, T11 sanitizer/SSRF, T13-T14 AI +
  vector wiring, T15+ inbox routes, T20 UI pages.
