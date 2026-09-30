GOAL
Continue or create the SupportOS++ project (GitHub repo "SupportOS-PlusPlus" under my account): an independent Rust/Tauri desktop reimplementation of https://github.com/kimpearce888/supportos. I am attaching the MASTER DEVELOPMENT SPECIFICATION. It is the authoritative scope. In your first session save it verbatim as docs/MASTER-SPEC.md and commit it. If it is not attached and docs/MASTER-SPEC.md does not exist, STOP and ask me for it. In later sessions read it from the repo, section by section as needed, never from memory.


PRECEDENCE
1. The AMENDMENTS below override the master spec where they conflict.
2. The master spec overrides everything else in this prompt.
3. Where the spec and the reference repo's actual code disagree on facts (counts, names, vocabularies), the reference code wins. Record it in docs/DEVIATIONS.md.


STATE DETECTION (first thing, every session)
If the repo already contains SupportOS++ work (including work created under the earlier names "SupportOS+" or "SupportOS-Plus"), do NOT restart: audit what exists against the spec, record honest statuses in docs/PARITY-MATRIX.md, and continue. If the repo does not exist, create it with the gh CLI. Then follow the SESSION START CHECKLIST.


NON-NEGOTIABLE RULES
- Tauri 2 desktop app for Windows, macOS and Linux. Rust backend. Rust/WASM frontend (Leptos preferred; justify the choice in docs/DECISIONS.md). No hand-written JavaScript or TypeScript anywhere (app, scripts, CI, tests); tooling-generated glue is acceptable.
- No Node, Python, Docker, or separate Qdrant. SQLite bundled (WAL, FTS5). Everything the app needs at runtime ships inside the installer.
- Local-first: no telemetry, no cloud AI, no data egress. Network traffic only to Help Scout, local AI providers, and user-configured connectors.
- AI is always advisory. Automatic customer-reply sending is permanently OFF. "Unknown" is a legitimate answer; never fabricate values. Derived facts stay derived where the reference derives them.
- Secrets never reach the UI layer and are always redacted in reads. Never commit credentials, tokens or user data. Real Help Scout credentials are entered by me inside the running app only; every automated test uses the Fake provider and can never send a real message.
- Never fake work: no dead controls, no mocked operations in real mode (the Fake provider exists only for demo mode and tests), no UI-only features without a working Rust core.


AMENDMENTS


A0. Naming: the product name is "SupportOS++". Use "SupportOS++" for the app title, window title, installer product name, README and docs. GitHub repository names cannot contain "+", so the repo slug is "SupportOS-PlusPlus". For anything that must be a safe identifier (Tauri bundle identifier, crate and package names, data folder name, installer file names, log and database file names, URL slugs) use the ASCII form "supportos-plusplus" (for example bundle id com.supportos.plusplus). The attached master spec and any earlier notes use the old names "SupportOS+" and "SupportOS-Plus"; read every occurrence as "SupportOS++" and "SupportOS-PlusPlus" respectively. Verify that the installers (MSI, NSIS, DMG, DEB, RPM, AppImage) build, install and uninstall correctly with the "++" display name, and that the data folder is created correctly on all three OSes. If any packaging format cannot handle "++" in the product name, use the ASCII name for that format's file names and record it in docs/DECISIONS.md.


A1. UI language: the UI is English only. Do not build UI localization (no i18n framework, locale files, language switcher or RTL layouts). Ticket translation (spec section 65) IS included as a SupportOS feature. Customer text in any language must be stored, searched and displayed correctly (UTF-8, Unicode-safe FTS5, broad font fallback).


A2. Loopback listener exception: the spec bans a localhost backend, but Help Scout webhooks and the OAuth redirect callback need an inbound listener. Provide exactly ONE small embedded listener bound to 127.0.0.1 for those two purposes only (Host-header validation, rate limiting, timing-safe HMAC-SHA1 webhook verification, persist-first with deduplication, single-use OAuth state). Everything else uses Tauri IPC commands and events. No local web API for the UI, no server mode, no separate CLI: maintenance (backup, restore, migrations, integrity checks) happens inside the app.


A3. Honest parity statuses: use DISCOVERED, SPECIFIED, IMPLEMENTED, TESTED, PACKAGED, VERIFIED. A row is VERIFIED only with recorded evidence (test name plus a packaged-app check). You may NOT declare the project complete or "100% parity"; report counts per status and list every gap. Every deviation from the spec or reference goes to docs/DEVIATIONS.md with reason and impact and stays "pending owner approval" until I approve it. Never approve your own deviations, never silently narrow scope. If a requirement is blocked by a technical limit, STOP that item, record it as BLOCKED, and report it to me. Do not substitute or fake it.


A4. Qdrant Edge (decision is final, do not reopen):
 - Use the `qdrant-edge` Rust crate, embedded and in-process, behind the SupportOS++ VectorStore abstraction; only the adapter module may import it.
 - The crate is pre-1.0 (0.7.x at last check). Verify the current version, pin the EXACT version, and record it in docs/architecture/VECTORSTORE.md and VECTORSTORE-EVALUATION.md. Never let routine dependency updates change it.
 - In Milestone 1 run a spike that proves, on CI runners for Windows x64, macOS arm64, macOS x64 (Intel), Linux x64 and Linux arm64 (where feasible): the crate builds, a shard persists in the app data folder, reopens after restart, and dense and sparse search with filters work. Record results per platform.
 - Verify every capability required by spec sections 16 to 37 exists in the pinned version (dense/sparse/named vectors, payload filters and indexes, exact search, snapshots and restore, WAL, count/scroll/facet). For anything missing: document it, implement it at the SupportOS++ layer only if honest and reasonable, otherwise mark BLOCKED and report. If the crate fails on a required platform, STOP and report; do not swap engines.
 - SQLite stays authoritative; vectors are derived and rebuildable from source text. No Qdrant URL or API-key settings anywhere in the UI.


A5. Testing and verification honesty:
 - You cannot use clean physical machines. Use fresh CI runners for install-launch-smoke tests on Windows, macOS and Linux, plus automated UI end-to-end tests where the platform tooling supports them (verify what Tauri supports per OS; do not assume). All test code in Rust.
 - Create docs/MANUAL-VERIFICATION.md: a step-by-step checklist I will run on clean machines (install, launch, first run, demo mode, sync, search, backup/restore, update, uninstall, optional LM Studio/Ollama). Rows that depend on it stay PACKAGED until I record results.
 - Signing and notarization need my certificates and accounts. Build the pipeline so signing plugs in when I provide secrets; until then produce unsigned builds and document the OS warnings honestly.


A6. Compatibility: the SupportOS++ database schema and .sosync file are its own. Use the same cryptographic approach as the reference (AES-256-GCM, scrypt-derived key, authenticated header, verify-first import, safety backup, atomic swap) in your own documented, versioned format. Compatibility with the original app's database or bundles is not required.


A7. Discovery method: write a Rust xtask that reads a local checkout of the reference repo (outside this repo, never committed) and extracts inventories: API routes, tables, migrations, settings keys, environment variables, closed vocabularies (condition kinds, Operations Center tiles, notification types, metrics, dimensions, attribute keys, graph node kinds, Copilot tools), UI pages and routes, scripts. Build docs/PARITY-MATRIX.md at capability level, grouped by milestone. Cross-check against the reference CHANGELOG counts (for example 16 Operations Center tiles, 15 notification types, 22 saved-view condition kinds per the README, 14 activity fields x 15 date modes, 21 report metrics x 14 dimensions, 14 AI attribute keys, 12 graph node kinds, 22 Copilot tools) but treat the code as truth and note discrepancies. Also read docs/ARCHITECTURE.md, DECISIONS.md (60 decisions), API-INTEGRATION.md, AI-SETUP.md, CLIENT-INTELLIGENCE.md, DESKTOP.md, BACKUP-RESTORE.md, TESTING.md, TROUBLESHOOTING.md and the full CHANGELOG.md (660 lines; the most detailed behavior spec). Write condensed notes to docs/original-notes/ (one file per area) so the reference never has to be re-read in full.


A8. Reference version: in the first session and before each milestone, fetch the reference repo's current HEAD, record it in docs/REFERENCE-VERSION.md (last inspected HEAD: c346fb51466e237a89e70156ae20a3386be0b322), and if it changed, list new or changed capabilities in the matrix before continuing.


A9. Webhook reachability: in discovery, find exactly how the reference obtains and configures the public callback URL for Help Scout webhooks (settings, environment variables, docs, the register/delete flow) and record it in docs/original-notes/sync.md. Reproduce that behavior: incremental polling (the reference uses roughly a 5-minute cycle) is always the baseline and must work with no webhook configured; webhook push is optional, and the user supplies the reachable address. Do NOT bundle, install or start any third-party tunnel or relay, and never route event payloads through a third party. The "Webhook push" screen (in Sync Health) must explain in plain language what address Help Scout needs, show the state (not configured, registered, receiving, error), support register and delete as Tauri commands, and never claim real-time updates are active when they are not. Restart-safe: unprocessed persisted webhook events are drained on boot.


A10. Demo-mode tools: the reference exposes ways to push a simulated webhook event, a simulated CSAT rating and a simulated incoming customer message through the REAL pipeline (HMAC, dedup, job, sync, live update). Reproduce these as clearly labeled actions available only in demo mode, calling the same code paths as production. Also show the Copilot's read-only tool allowlist in the AI Center for transparency.


A11. Visual reference: use the reference repo's README screenshots (docs/screenshots/) and demo GIF as the visual and layout reference. Record a screen-by-screen comparison in docs/UI-PARITY.md. Match structure, labels, states and behavior; do not copy CSS, markup or code.


A12. Code quality: write clean, minimal, change-friendly Rust, so nothing has to be rewritten later and any change that does come is cheap.


Write less code:
 - Before writing anything, ask whether the same result can be reached with less code. If it can, write the shorter version. Prefer a well-maintained crate, the standard library, iterator chains, the `?` operator, derive macros (serde, thiserror and similar) and existing helpers over hand-written loops, manual parsing and boilerplate.
 - Never write the same logic twice. If a pattern appears a second time, note it; the third time, extract it. Keep one source of truth for each fact, especially closed vocabularies (condition kinds, tiles, notification types, metrics, states), which should be a single Rust enum or catalog that the database, validation, UI and tests all derive from.
 - Do not write speculative code: no unused parameters, no "might need later" hooks, no dead code, no commented-out code. Delete what is no longer used.
 - Shorter must not mean cryptic. Choose the clearest short form: descriptive names, small functions with one job, early returns, no deep nesting, no clever tricks a new reader would have to decode.


Build it right the first time:
 - Design the boundary before the code. Put each concern behind a small trait or module with a narrow public surface (for example the VectorStore, LocalAIProvider, HelpScoutProvider and the shared service layer), so changing an internal detail never touches callers. Keep business rules in the Rust core, never in the UI.
 - Use the type system to make wrong states impossible: enums instead of strings and flags, newtypes for IDs, `Result` with typed errors instead of panics or sentinel values, and no `unwrap` or `expect` in production paths.
 - Keep pure logic (calculations, compilers, state machines) separate from I/O (database, network, files), so it can be tested and changed without mocking the world.
 - Add a shared foundation once, in Milestone 1 (error type, logging, config, database access pattern, job runner, common UI components and layout), and make every later milestone use it instead of inventing its own.
 - Do not over-engineer either: no premature abstraction, generic frameworks or plugin systems for things that occur once. Add an abstraction when the second real use appears, or when a boundary is explicitly required by the spec.


Make later changes painless:
 - Every behavior is covered by tests written at the same time as the code, so a future refactor can be done safely and quickly. Prefer tests that check behavior over tests that check implementation details.
 - Keep modules and files small and cohesive. Split a file once it holds more than one responsibility; keep public APIs stable and documented with a short doc comment saying what and why.
 - Database changes only through forward migrations; keep configuration and constants in one place, not scattered as magic numbers or strings.
 - Follow one consistent style across the project (rustfmt, clippy with warnings denied, one naming convention, one error-handling pattern, one way of doing async work). Write the conventions in the instruction file and follow them.
 - Before each commit, review your own diff for duplication, dead code, over-long functions and unclear names, and fix them in the same commit. If a task needs a refactor, do it as a separate small commit with tests passing before and after; never leave a mess with a plan to clean up later.


KNOWN PITFALLS (bugs the reference had to fix; do not repeat)
- Never compare ISO-8601 timestamps against SQLite datetime('now') strings lexically; store one format or compare via julianday/unixepoch. Job claim loops must be tested end to end (enqueue, claim, execute), not by calling components directly.
- Calendar-day boundaries use date-only arithmetic converted per zone; test spring-forward (23h), fall-back (25h), Lord Howe (24.5h), Kathmandu (+5:45).
- Escape LIKE wildcards; quote FTS5 queries safely; cap query length.
- Bound every potentially large query, disclose bounds, add indexes plus EXPLAIN QUERY PLAN and performance tests on a synthetic 2,000-conversation dataset.
- User input never becomes SQL identifiers; conditions and metrics come from closed catalogs; cap condition-tree depth and node counts.
- Re-embed only when the content hash changes; cap failed embedding retries.
- Dedup keys on every derived event so rebuilds and re-syncs are idempotent.
- Reads have no side effects; rebuilds only via explicit commands.
- Reset composer and draft state per conversation so a draft can never reach another customer.
- Escape closes only the topmost dialog; sequence stale async responses; every view has loading, empty and error states.
- Campaign recipients that exhaust retries must fail, never livelock; "retry failed" resets the attempt budget.
- Status write and closed_at stamp in one transaction; timeouts on sends become "unknown" and are reconciled before any retry.
- Notification sweep cursors must not initialize until the first sync settles (no first-run notification spam).
- Anchor .gitignore patterns so docs/screenshots are never swallowed.


INSTALL AND PACKAGING (Linux is mandatory)
- GitHub Actions on native runners builds and publishes on version tags: Windows MSI and NSIS (WebView2 handled automatically; evaluate the offline installer), macOS DMG (architectures per the release matrix), Linux DEB, RPM and AppImage. Everything needed at runtime is bundled. Data lives in the user profile.
- Also provide bootstrap.sh and bootstrap.ps1 that silently install build prerequisites, then build and launch, and a cargo xtask for dev, test, lint and package. Safe to re-run.
- Document the exact supported Linux matrix. Do not claim universal Linux support.
- LM Studio and Ollama are optional, never bundled: auto-detect, list models, select, test. The app works fully without them. First run offers the 2-minute demo mode with no credentials.
- README opens with a 3-line install section for non-developers.


TESTING
Independent Rust test suite (never copy reference tests): unit, integration and end-to-end, including DST matrices, response-state SQL/Rust equivalence, event derivation and dedup, view compilation with injection-shaped values, SSRF matrix, webhook HMAC and dedup, write-pipeline behavior, job recovery, upgrade-in-place migrations, VectorStore contract tests (spec section 92), crash recovery, performance guards. Port the idea of the reference's black-box audit as a Rust binary. CI on all three OSes: fmt, clippy with warnings denied, tests, WASM build, Tauri build, headless demo-mode boot.


DEFINITION OF DONE FOR A TASK
Code plus tests pass, no dead controls, no mocks in real mode, error/empty/loading states present, docs updated, parity-matrix row updated with evidence, committed and pushed. Also: no duplicated logic, no dead code, no unwrap/expect in production paths, functions small and single-purpose, and the diff reviewed for anything that could be shorter or clearer. Work in small bounded tasks with explicit acceptance criteria and review your own diff before each commit.


WORKFLOW AND RESUME PROTOCOL
You have NO memory between sessions. The repository is your only memory. Every session may be your last.


Files that carry state (create and commit in the first session):
1. Project instruction file (under 60 lines): determine which filename YOUR tool loads automatically and create exactly that file (AGENTS.md if unsure); no duplicates, no tool-specific copies. Contents: goal, naming rule (A0), code-quality rules (A12) in 8 lines, precedence, non-negotiable rules, the SESSION START CHECKLIST, build/test/lint commands. Record the filename at the top of PROGRESS.md.
2. PROGRESS.md: instruction filename; current milestone; current task ID; last completed task and commit hash; next 3 tasks; known issues; decisions this session; parity counts by status; last updated.
3. TASKS.md: small numbered tasks (M3-T01...), 30 to 90 minutes each, with acceptance criteria. Write the full task list for a milestone before starting it.
4. docs/MASTER-SPEC.md, PARITY-MATRIX.md, REFERENCE-VERSION.md, DEVIATIONS.md, DECISIONS.md, MANUAL-VERIFICATION.md, UI-PARITY.md, original-notes/.


SESSION START CHECKLIST (in order, no skipping)
1. Read the instruction file, then PROGRESS.md, then the current milestone in TASKS.md.
2. If PROGRESS.md does not exist: session 1. Run state detection, discovery (A7, A8, A9), write the matrix and notes, then start Milestone 1. Otherwise RESUME: do not redo discovery, do not re-create the repo, do not re-scaffold.
3. Verify reality: git status, git log --oneline -20, run build and tests. Commit or finish uncommitted work. If PROGRESS.md and git disagree, git wins; fix PROGRESS.md.
4. Read only the master-spec sections and original-notes relevant to the current milestone.
5. Announce: "Resuming at <milestone>/<task>. Last commit: <hash>. Next: <task>." Then continue.


CHECKPOINT RULES
- After EVERY task: run tests, commit with the task ID, push, tick TASKS.md, update PROGRESS.md.
- Before a long or risky task write "IN PROGRESS: <task>, approach: <one line>" to PROGRESS.md and commit.
- When the session is getting long, park the current task, fully update PROGRESS.md, commit, push, and stop cleanly.
- Never leave main broken; unfinished work goes on a named branch.
- At the end of each milestone: all its tasks ticked, CI green on all OSes, tag milestone-N-done, then STOP. Report parity counts by status, deviations awaiting my approval, BLOCKED items, and what the next session covers. Wait for me to say "continue".


MILESTONES
 1. Foundation: repo, Tauri 2 shell, Rust/WASM UI framework, SQLite and migrations, job queue, settings, theming, the shared foundation from A12 (error type, logging, config, database access pattern, job runner, common UI components), CI matrix (Windows, macOS, Linux), installer pipelines for all formats (verify the "SupportOS++" naming per A0), Qdrant Edge spike (A4), discovery outputs. The empty app must install and launch on all three OSes.
 2. Help Scout mirror: provider trait (Real and Fake), OAuth, rate-limited queue, checkpointed sync, Beacon chat, Docs mirror, ratings, webhook listener and Webhook push screen, live events, demo mode and demo tools (A10), first-run onboarding.
 3. Activity engine and inbox: events, derived timestamps, response states, filters, saved views, priority, ticket states, ticket operations, write-protection pipeline, lexical universal search, command palette.
 4. Team operations: Operations Center, workload and capacity, Notification Center, mentions, side threads, automation.
 5. VectorStore and AI providers: VectorStore abstraction and Qdrant Edge adapter (full contract), LocalAIProvider (LM Studio, Ollama, generic), embeddings, hybrid search, vector backup, migration and recovery.
 6. AI features: AI Center, analysis, attributes, Copilot, verified drafts, coaching, customer memory, translation, QA, suggestions.
 7. Intelligence: client interaction intelligence, Issue Radar, known issues and clusters, incidents and impact, SLA, knowledge, Docs, freshness and gaps.
 8. Reports and quality: dashboards, report builder, post-resolution QA, effectiveness, friction, support health, customer timeline, support graph.
 9. Outreach: segmentation, saved segments, campaigns, do-not-contact, monitoring.
10. Data tools: custom objects, connectors with SSRF guard, backup and restore, encrypted sync, settings.
11. Conformance and hardening: parity gate, reference-delta check, black-box audit, performance and crash-recovery tests, packaged-app verification, Linux matrix, UI-PARITY walkthrough, MANUAL-VERIFICATION handoff, docs/FINAL-PARITY-AUDIT.md (an honest report, not a completion claim).


COMMUNICATION
If something is ambiguous, choose the option closest to the reference's documented behavior, record it in docs/DECISIONS.md, and continue. Ask me only for BLOCKED items, deviations needing approval, missing credentials or certificates, and milestone sign-off.
Legal: MIT license, credit the original project, and include: "Help Scout is a trademark of Help Scout, Inc. SupportOS++ is an independent, open-source integration and is not affiliated with or endorsed by Help Scout."