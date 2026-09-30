# Parity Matrix — SupportOS++ vs. reference `supportos`

> Statuses (A3): **DISCOVERED** · **SPECIFIED** · **IMPLEMENTED** · **TESTED** · **PACKAGED** · **VERIFIED**
> A row may be marked VERIFIED only with recorded evidence (test name + packaged-app check). Until then it stays at most PACKAGED.
> Reference repo HEAD: `c346fb51466e237a89e70156ae20a3386be0b322` (see `docs/REFERENCE-VERSION.md`).
> Last updated: Session 1.

## Canonical counts (verified against reference code, A7)

The master spec / reference README makes count claims. We re-derived every one of them from the reference code. Code wins (PRECEDENCE §3). All match the spec.

| Capability | Spec claim | Code count | Match | Source file |
|---|---|---|---|---|
| Operations Center tiles | 16 | **16** | ✅ | `src/shared/collaboration.ts` — `OPERATIONS_TILE_KEYS` |
| Notification types | 15 | **15** | ✅ | `src/shared/collaboration.ts` — `NOTIFICATION_TYPES` |
| Saved-view condition kinds | 22 | **22** | ✅ | `src/shared/activity.ts` — `z.literal('…')` condition kinds |
| Activity fields × date modes | 14 × 15 | **14 × 15** | ✅ | `src/shared/activity.ts` — `ACTIVITY_FIELDS`, `DATE_MODES` (7 cal + 6 rolling + 2 exact) |
| Report metrics × dimensions | 21 × 14 | **21 × 14** | ✅ | `src/shared/reporting.ts` — `REPORT_METRICS`, `REPORT_DIMENSIONS` |
| AI attribute keys | 14 | **14** | ✅ | `src/shared/constants.ts` — `AI_ATTRIBUTE_CATALOG` |
| Graph node kinds | 12 | **12** | ✅ | `src/shared/graph.ts` — `GRAPH_NODE_KINDS` |
| Copilot tools | 22 | **22** | ✅ | `src/server/ai/tools.ts` — `name: '…'` literals |

## Reference surface area

| Surface | Count | Status |
|---|---|---|
| API route files | 30 | DISCOVERED |
| HTTP endpoints (approx) | 310 | DISCOVERED |
| Database migrations | 16 (001-016) | DISCOVERED |
| Tables created by migrations | 125 | DISCOVERED |
| Repositories (server/db/repositories) | 22 | DISCOVERED |
| Client pages (`.tsx`) | 21 | DISCOVERED |
| Shared TS modules | 14 | DISCOVERED |
| Server module dirs (ai, sync, inbox…) | 25 | DISCOVERED |
| Environment variables in `.env.example` | 19 | DISCOVERED |
| Scripts in `scripts/` | 21 | DISCOVERED |
| Documentation files in `docs/` (excl. screenshots) | 11 | DISCOVERED |
| CHANGELOG.md lines | 660+ (≈135 KB) | DISCOVERED |
| DECISIONS.md ADR count | 60 (spec claim) | DISCOVERED |
| Screenshots in `docs/screenshots/` | ~25 PNGs + 1 demo GIF | DISCOVERED |

> The counts above are produced by `cargo xtask discover --reference <path/to/reference>`,
> which writes a machine-readable snapshot to `docs/original-notes/inventory.json`.
> Last xtask-discover run: reference HEAD `c346fb51466e237a89e70156ae20a3386be0b322`,
> 10/10 canonical counts matched the spec (A7 cross-check green).

## Per-milestone parity (high-level)

Each milestone groups dozens of capabilities. Detailed per-capability rows live in the per-milestone files under `docs/original-notes/`. Statuses below reflect the SupportOS++ side only.

### Milestone 1 — Foundation
| Capability | Status |
|---|---|
| Repo created (`SupportOS-PlusPlus`, MIT) | ✅ DONE |
| Master spec committed verbatim (`docs/MASTER-SPEC.md`) | ✅ DONE |
| Reference version recorded (`docs/REFERENCE-VERSION.md`) | ✅ DONE |
| Discovery notes (`docs/original-notes/*.md`) | ✅ DONE |
| Parity matrix (this file) | ✅ DONE |
| State files (`AGENTS.md`, `PROGRESS.md`, `TASKS.md`, `DECISIONS.md`, `DEVIATIONS.md`, `MANUAL-VERIFICATION.md`, `UI-PARITY.md`) | ✅ DONE |
| Cargo workspace + crates (`app`, `core`, `ui`, `xtask`) | ✅ DONE |
| Tauri 2 shell that launches on Win/macOS/Linux | SPECIFIED — config verified via `cargo xtask verify-config` (D-016); full launch needs CI |
| Leptos WASM UI scaffold | ✅ DONE (M1-T08) — `crates/ui/src/{lib,layout,pages/}.rs`: Router with `/`, `/settings`, `/*any` (not-found); LayoutShell with Topbar + nav + footer; compiles to both native and `wasm32-unknown-unknown` |
| SQLite (WAL + FTS5) + first migration | ✅ DONE (M1-T03) — `crates/core/src/migrations.rs`, M001 creates `application_settings` + `secrets` + `app_state`; `db::open_with_migrations` convenience helper |
| Job queue | ✅ DONE (M1-T05) — `crates/core/src/runner.rs`: `JobHandler` trait + `JobRegistry` + `Runner` + `RunSummary` (D-021). End-to-end tests cover enqueue → claim → execute → complete, retry-then-succeed, always-fail → dead-letter. 10 new tests, stable across 5 runs |
| Settings store | ✅ IMPLEMENTED (M1-T04 partial) — typed helpers `get_bool`/`set_bool`/`get_i64`/`set_i64`/`get_json`/`set_json` (D-017) + first-run flag; secrets redacted on read |
| Theming | ✅ DONE (M1-T07) — CSS custom properties in `crates/ui/styles/app.css` (colors, spacing, typography, radii, shadows, motion) + `Severity` enum (D-019) mirroring Operations Center buckets |
| CI matrix (Win/macOS/Linux: fmt + verify-config + clippy + test + build) | ✅ DONE — `.github/workflows/ci.yml` runs fmt + `cargo xtask verify-config` + clippy + tests + WASM build + Tauri build on ubuntu-22.04, ubuntu-24.04, macos-latest, windows-latest |
| Installer pipelines (MSI, NSIS, DMG, DEB, RPM, AppImage) | SPECIFIED — bundle targets present in `tauri.conf.json` (verified by `verify-config`); actual builds happen in `.github/workflows/release.yml` on tag push |
| Qdrant Edge spike on all 5 platforms (A4) | SPECIFIED |
| `xtask discover` (A7) replaces manual pass | ✅ DONE (10/10 canonical counts match; writes `docs/original-notes/inventory.json`) |
| `xtask verify-config` (A0 in CI) | ✅ DONE (D-016) — 7 unit tests, runs on every CI push |
| `xtask audit` (M1-T14, port of `audit-phase1.mjs`) | ✅ DONE (D-022) — separate binary sharing `spp_xtask` lib; M1 checks: `path_exists` + `config_a0`. 8 audit tests; JSON output; `cargo xtask audit --app PATH` shells out |
| Loopback listener HMAC + OAuth state + dedup (M1-T12) | ✅ DONE (D-023) — `crates/core/src/{webhook,oauth_state}.rs`: timing-safe HMAC-SHA1 (FIPS 180-1 verified) + `subtle::ConstantTimeEq`; persist-first dedup via `webhook_events` SQLite table; single-use OAuth state via `oauth_states` table. 25 new tests |
| First-run onboarding overlay (M1-T13) | ✅ DONE (D-024) — `<OnboardingOverlay>` Leptos component + `first_run_state` Tauri IPC command. 5 UI tests + 3 Tauri IPC tests |
| Closed-vocabulary catalog crate (WASM-safe) | ✅ DONE (D-018) — `crates/catalog` extracted as WASM-safe single source of truth; UI + core share it |
| Common UI components + state pattern | ✅ DONE (M1-T07) — `ViewState` enum + `<StateView>`, `<LoadingState>`, `<EmptyState>`, `<ErrorState>`, `<Button>` (D-019) |
| Leptos Router scaffold | ✅ DONE (M1-T08) — 3 routes (`/`, `/settings`, `/*any`) + `LayoutShell` |
| Error type + logging + config foundation (M1-T06) | ✅ DONE — `crates/core/src/{error,logging,config}.rs`; shared `Error`/`Result`; JSON logs in release, pretty in dev |
| Empty app installs & launches on all 3 OSes | SPECIFIED |

### Milestones 2–11 — all rows DISCOVERED only
M2 Help Scout mirror · M3 Activity engine & inbox · M4 Team operations · M5 VectorStore & AI providers · M6 AI features · M7 Intelligence · M8 Reports & quality · M9 Outreach · M10 Data tools · M11 Conformance & hardening.

Detailed per-capability rows for M2-M11 will be expanded at the start of each milestone (per spec: "Write the full task list for a milestone before starting it").

## Reference delta since last session

None. Reference HEAD `c346fb51466e237a89e70156ae20a3386be0b322` unchanged since session 1 (verified by `cargo xtask discover` reading the local reference checkout).

## BLOCKED items

None at this time.

## Deviations awaiting owner approval

None at this time. See `docs/DEVIATIONS.md`.
