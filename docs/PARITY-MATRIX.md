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
| Tauri 2 shell that launches on Win/macOS/Linux | SPECIFIED |
| Leptos WASM UI scaffold | SPECIFIED |
| SQLite (WAL + FTS5) + first migration | ✅ IMPLEMENTED (foundation; first app migration lands in M1-T03) |
| Job queue | ✅ IMPLEMENTED (claim/complete/fail/dead-letter; julianday-based, 35 tests stable × 10 runs) |
| Settings store | ✅ IMPLEMENTED (redacted reads, encrypted secrets table) |
| Theming | SPECIFIED |
| CI matrix (Win/macOS/Linux: fmt + clippy + test + build) | SPECIFIED |
| Installer pipelines (MSI, NSIS, DMG, DEB, RPM, AppImage) | SPECIFIED |
| Qdrant Edge spike on all 5 platforms (A4) | SPECIFIED |
| `xtask discover` (A7) replaces manual pass | ✅ DONE (10/10 canonical counts match; writes `docs/original-notes/inventory.json`) |
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
