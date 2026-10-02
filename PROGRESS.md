# PROGRESS.md — SupportOS++

> Instruction file: `AGENTS.md` (top of repo).
> Repository is the only memory between sessions. Every session may be your last.

## Current state

| Field | Value |
|---|---|
| Instruction file | `AGENTS.md` |
| Current milestone | M12 — Packaging verification + UI wiring + E2E (IN PROGRESS — all priorities done; awaiting owner sign-off) |
| Current task ID | (none — all M12 priorities complete; 24 UI pages, 49+ IPC commands, smoke-install all-pass, real WebDriver E2E passing) |
| Last completed task | M12-P4: Real WebDriver E2E — tauri-driver + per-page control clicking + text reports |
| Last commit hash | `914edfd` — STEP 1b: Complete Qdrant Edge adapter — sparse, snapshot, restore, filter translation |
| Last updated | Session 39 — SCOPE changed to Linux-only; STEP 1a (OPEN-ITEMS) + STEP 1b (complete Qdrant adapter) done |

## Next 3 tasks

1. **STEP 2**: Write PAGE-EVIDENCE.md + upgrade E2E to click every control, submit forms, test states.
2. **STEP 3**: Real-mode checks (Help Scout stand-in, AI providers, backup/restore/recovery).
3. **STEP 4-6**: Independent audit, clean build, docs + release.

## Resume protocol for next session

1. Read `AGENTS.md` → this file → `TASKS.md` → `docs/FINAL-PARITY-AUDIT.md`.
2. `git status` + `git log --oneline -10` + `CARGO_INCREMENTAL=0 cargo test -p supportos-plusplus-catalog -p supportos-plusplus-core -p supportos-plusplus-ui -p supportos-plusplus-xtask --all-targets`.
3. Check CI status: `cargo xtask ci-status` or query the GitHub API.
4. If the owner says "continue" → address any remaining items or start a new milestone.
5. The project is at IMPLEMENTED + TESTED + PACKAGED status; VERIFIED requires owner action (manual verification on clean machines).

## Parity counts by status (honest, A3)

| Status | Count |
|---|---|
| DISCOVERED | 8 canonical counts + 13 surface-area rows + per-milestone high-level rows (reproducible via `cargo xtask discover`) |
| SPECIFIED | 1 (Tauri shell launch verification on CI ✅ done) |
| IMPLEMENTED | 15 (all M1 foundation capabilities + M12: self_check, inbox, customers, qdrant adapter, 24 UI pages with 49+ IPC commands) |
| TESTED | 2 (CI green run on main — commit `481695f` — verifies fmt + clippy + tests + WASM + Tauri build on Win/macOS/Linux; M12: smoke-install CI verifies installers on all 6 platforms; real WebDriver E2E navigates all 24 pages) |
| PACKAGED | 1 (M12: Nightly produces installers for DEB, RPM, AppImage, MSI, NSIS, DMG on 4 OS targets; smoke-install verifies each installs + launches + self-check runs + DB initializes) |
| VERIFIED | 0 (requires owner-run `docs/MANUAL-VERIFICATION.md` checklist on a clean machine) |

**The project is NOT complete and is NOT at 100% parity.** Do not claim otherwise.

## Known issues

- M1-T10 (installer signing): BLOCKED on owner certificates. All 6 installer formats build and install correctly (verified by smoke-install CI), but they are unsigned.
- M1-T11 (Qdrant Edge spike): ✅ DONE (M12). The adapter compiles in CI via the `qdrant-build` smoke-install job.
- M5-T02 (Qdrant Edge adapter): ✅ DONE (M12). Dense-vector subset implemented; sparse/snapshot/restore return errors honestly (DEV-002).
- DEV-004 (Linux arm64): NOT SUPPORTED — owner decision. Linux arm64 users must build from source.
- The self-check reports honestly whether the `qdrant` cargo feature is enabled (currently OFF by default — the InMemoryVectorStore is the production adapter for now; see DEV-005).

## M12 summary (honest)

### What was done

| Priority | Description | Status |
|---|---|---|
| P1 | Smoke-install CI: install + launch + self-check + DB init + uninstall for all 6 installers | ✅ ALL 6 PASS |
| P2 | Startup self-check (6 subsystems: database, FTS5, vector_store, ai_provider, loopback, catalog_conformance) | ✅ DONE |
| P2 | Qdrant Edge adapter behind `qdrant` cargo feature (dense-vector subset) | ✅ DONE (compiles in CI) |
| P3a | AppImage target re-enabled | ✅ DONE |
| P3b | macOS _EMBED_INFO_PLIST documented (DEV-003); smoke-install macOS DMG is the alternative | ✅ DONE |
| P3c | macOS Intel (macos-13) added; Linux arm64 removed per owner (DEV-004) | ✅ DONE |
| P4 | Real WebDriver E2E: tauri-driver + per-page control clicking + text reports | ✅ DONE (passes on `481695f`) |
| P5 | 24 UI pages with real IPC wiring (49+ IPC commands) | ✅ DONE |

### UI pages built (24 total)

1. Dashboard (`dashboard_metrics`)
2. Inbox + Conversation detail + Context pane (8 IPC commands)
3. Operations Center (`operations_snapshot`)
4. Notifications (3 IPC commands)
5. Automation (4 IPC commands)
6. Sync Health (`sync_health_state`)
7. Customer profile + search (4 IPC commands)
8. AI Center (5 IPC commands)
9. Reports (`report_build`)
10. Issue Radar (`issue_radar_snapshot`)
11. Settings (`self_check`, `parity_gate_check`, `first_run_state`)
12. Support Health (`support_health`)
13. Incidents (`incidents_list`)
14. Knowledge Gaps (`knowledge_gaps_list`)
15. Side Threads (`side_threads_list`, `side_thread_messages`)
16. Connectors (`connectors_list`)
17. Custom Objects (`custom_object_types_list`, `custom_object_fields_list`)
18. Outreach (`segments_list`, `campaigns_list`, `dnc_list`)
19. Search (`universal_search`)
20. Backup (`backup_export`)
21. Support Graph (`graph_nodes_list`, `graph_neighbors`)
22. Onboarding wizard (`first_run_state`)
23. Command Palette (full-page version)
24. 404 / Not Found (existing)

### CI workflows (4)

| Workflow | What it does | Status |
|---|---|---|
| CI (`ci.yml`) | fmt + clippy + test on 4 OSes + WASM + Tauri build on 3 OSes | ✅ GREEN |
| E2E (`e2e.yml`) | Real WebDriver UI tests: navigate all 24 pages, click controls, capture text, write report | ✅ GREEN |
| Nightly (`nightly.yml`) | Build installers (DEB, RPM, AppImage, MSI, NSIS, DMG) for 4 OS targets + upload to nightly release | ✅ GREEN |
| Smoke Install (`smoke-install.yml`) | Download nightly artifacts, install, launch, self-check, DB init, uninstall; + qdrant-build job | ✅ ALL 6 PASS |

### Honest deviations (docs/DEVIATIONS.md)

| ID | Description | Status |
|---|---|---|
| DEV-002 | QdrantEdge adapter implements only dense-vector subset | pending owner approval |
| DEV-003 | macOS app-crate tests excluded; smoke-install is the alternative | pending owner approval |
| DEV-004 | Linux arm64 NOT supported | approved (owner decision) |
| DEV-005 | Production builds use InMemoryVectorStore, not Qdrant Edge | pending owner approval |

## Decisions this session

Session 1-5: D-001 through D-022.
Session 6: D-023 (loopback HMAC), D-024 (onboarding overlay).
Session 37-38 (M12):
- **D-025**: Startup self-check — 6 subsystems verified at boot, logged + exposed via `self_check` IPC.
- **D-026**: Qdrant adapter behind `qdrant` cargo feature — dense-vector subset only (DEV-002).
- **D-027**: Smoke-install CI — 6 jobs (Linux DEB+RPM, Windows MSI+NSIS, macOS DMG, qdrant-build).
- **D-028**: Real WebDriver E2E — tauri-driver + raw HTTP WebDriver protocol, no selenium dependency.

(See `docs/DECISIONS.md` for the full list D-001..D-028.)

## Resume protocol for next session

1. Read `AGENTS.md` → this file → `TASKS.md`.
2. `git status` + `git log --oneline -20` + `cargo test -p supportos-plusplus-catalog -p supportos-plusplus-core -p supportos-plusplus-ui -p supportos-plusplus-xtask --all-targets` (skipping the Tauri shell crate if GTK deps aren't installed locally; CI verifies the full workspace).
3. Check CI: query GitHub API for the latest run status on `main`.
4. If the owner says "continue" → address any remaining items (M1-T10 signing, manual verification) or start a new milestone.
5. The project is at IMPLEMENTED + TESTED + PACKAGED status; VERIFIED requires owner action.
