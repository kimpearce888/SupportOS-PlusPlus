# SupportOS++

**A local-first, AI-powered support operating system for Help Scout.**

SupportOS++ is a desktop app that mirrors your Help Scout conversations, customers, and reports locally — then layers AI analysis, automation, and intelligence on top. Everything stays on your machine. No telemetry, no cloud AI, no data egress.

---

## Install (3 steps)

1. Download the `.deb`, `.rpm`, or `.AppImage` from [Releases](https://github.com/kimpearce888/SupportOS-PlusPlus/releases).
2. Install it (`sudo apt install ./SupportOS++_*.deb` or `sudo dnf install ./SupportOS++-*.rpm` or just run the `.AppImage`).
3. Launch **SupportOS++**. On first run, click **"Try the 2-minute demo mode"** — no credentials needed.

Linux x86_64 only. Windows and macOS are excluded for now (the code is portable; see [DEV-006](docs/DEVIATIONS.md)).

---

## What's new

- **24 pages** with real data wiring — Dashboard, Inbox (3-pane with filters + saved views + reply/note/status/assign/bulk actions), Customer profiles + timeline, Operations Center, Notifications, Automation, AI Center, Reports (21×14 builder), Issue Radar, Incidents, Knowledge Gaps, Side Threads, Connectors, Custom Objects, Outreach (campaigns + segments + DNC), Search (FTS5), Backup, Support Graph, Support Health, Settings, Onboarding wizard, Command Palette, 404.
- **49+ IPC commands** connecting every UI control to real Rust core functions.
- **Startup self-check** — at boot, verifies database (28 migrations), FTS5, vector store, AI provider, loopback listener, and catalog conformance. Visible in logs + the Settings page.
- **Real WebDriver E2E** — CI drives the actual UI via tauri-driver, clicks every control on every page, checks for errors and placeholder data.
- **Smoke-install CI** — verifies each installer (DEB + RPM) actually installs, launches, runs the self-check, initializes the DB, and uninstalls correctly.
- **Qdrant Edge adapter** — complete VectorStore implementation (dense + sparse search, snapshot, restore, filter translation) behind a cargo feature flag.
- **786 core tests + 68 UI tests + 25 E2E pages + 3 smoke-install jobs** — all green.

---

## Why it was built

The reference `supportos` is a TypeScript web app that depends on a cloud database, cloud AI, and a server. We wanted a version that:

- **Stays local.** Your Help Scout data lives on your machine, not in a cloud database. Network traffic goes to Help Scout (for sync), your local AI provider (LM Studio or Ollama, both optional), and user-configured connectors — nothing else.
- **Works offline.** No internet? The app still works. Sync pauses; everything else continues.
- **Is auditable.** Every line is Rust. No `node_modules` black box. `cargo audit` checks for known vulnerabilities. The spec is committed verbatim in `docs/MASTER-SPEC.md`.

## Why Rust and Tauri

We chose Rust because it's fast, memory-safe, and has excellent SQLite + FTS5 support via `rusqlite` (bundled — no system SQLite dependency). The trade-off: slower compilation, and the Rust ecosystem for desktop UIs (Leptos/WASM) is less mature than React.

We chose Tauri 2 because it produces small native binaries with a system webview (WebKit2GTK on Linux), not a bundled Chromium. The trade-off: you need WebKit2GTK installed (most Linux distros have it), and the webview rendering can differ slightly from Chromium.

## How it works (simple terms)

1. **Sync.** SupportOS++ talks to the Help Scout API (OAuth 2.0), fetches conversations/customers/mailboxes, and stores them in a local SQLite database (with WAL + FTS5 for fast full-text search). Webhooks push updates in real-time when configured.
2. **Analyze.** The local AI (if configured — LM Studio at `127.0.0.1:1234` or Ollama at `127.0.0.1:11434`) reads conversations and generates summaries, suggested replies, customer attributes, and coaching tips. AI is advisory only — auto-customer-reply is permanently OFF.
3. **Organize.** The Operations Center shows 16 tiles of real-time metrics (active conversations, SLA breaches, automation approvals, etc.). The Issue Radar surfaces known issues, clusters, and incidents. Reports let you build any of 21 metrics × 14 dimensions with previous-period comparison.
4. **Backup.** Export the entire database as JSON (encrypted with AES-256-GCM + scrypt if you use `.sosync` format). Restore on any machine.

## Key decisions

| Decision | Why | Trade-off |
|---|---|---|
| Rust + Tauri 2 | Memory safety, small binaries, no Chromium | Slower compile; Leptos/WASM UI is less mature than React |
| SQLite (bundled, WAL, FTS5) | No external database; ACID; full-text search built-in | Not a distributed database (single machine) |
| Qdrant Edge behind a feature flag | Adds 400+ deps; InMemoryVectorStore works for demo/tests | Production vector search requires `--features qdrant` |
| Linux-only (for now) | Owner decision; CI matrix simplified | Windows/macOS users build from source (code is portable) |
| AI is advisory, never auto-reply | Spec rule: "Auto-customer-reply is permanently OFF" | Slower than fully automated; safer for support quality |
| No telemetry, no cloud AI | Privacy-first; all data stays local | No remote monitoring; no GPT-4/Claude integration |

## What's verified

- ✅ 786 core tests + 68 UI tests pass on every push.
- ✅ CI: fmt + clippy + tests + WASM build + Tauri build + cargo audit + clean-build check.
- ✅ E2E: WebDriver drives all 24 pages, clicks every control, checks for errors.
- ✅ Smoke-install: DEB + RPM install, launch, self-check, DB init, uninstall — all verified.
- ✅ Qdrant adapter: compiles + tests pass with `--features qdrant`.
- ✅ Independent audit: 0 Blocker, 0 Critical, 0 Major findings.

## What's NOT verified

- ❌ Installer signing (needs owner certificates — M1-T10).
- ❌ Real Help Scout API integration (needs real OAuth credentials — stand-in server verified the protocol shape).
- ❌ Real AI provider integration (needs running LM Studio or Ollama — stand-in server verified the protocol shape).
- ❌ Manual verification on a clean machine (owner action — see `docs/MANUAL-VERIFICATION.md`).

## Deviations awaiting owner approval

| ID | Description | Status |
|---|---|---|
| DEV-002 | Qdrant adapter: complete but behind `qdrant` feature flag | Resolved (STEP 1b) |
| DEV-003 | macOS app-crate tests excluded | Moot (DEV-006 removes macOS) |
| DEV-004 | Linux arm64 not supported | Approved (owner decision) |
| DEV-005 | Production builds use InMemoryVectorStore, not Qdrant Edge | Pending (depends on owner decision to enable `qdrant` feature) |
| DEV-006 | Windows and macOS excluded | Approved (owner decision) |

## Credits

Built by an AI agent (Claude) across 39 sessions, following the master spec in `docs/MASTER-SPEC.md`. The reference repo is [`supportos`](https://github.com/kimpearce888/supportos) (TypeScript).

## License

MIT. Help Scout is a trademark of Help Scout, Inc. SupportOS++ is an independent, open-source integration and is not affiliated with or endorsed by Help Scout.
