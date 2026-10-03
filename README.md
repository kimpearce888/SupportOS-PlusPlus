# SupportOS++

**A local-first, AI-powered support operating system for Help Scout.**

SupportOS++ is a desktop app that mirrors your Help Scout conversations, customers, and reports locally — then layers AI analysis, automation, and intelligence on top. Everything stays on your machine. No telemetry, no cloud AI, no data egress.

---

## Install (3 steps)

1. Download the `.deb` or `.AppImage` from [Releases](https://github.com/kimpearce888/SupportOS-PlusPlus/releases).
2. Install it (`sudo apt install ./SupportOS++_*.deb`) or just run the `.AppImage`.
3. Launch **SupportOS++**. On first run, use demo mode — no credentials needed.

**Linux x86_64 only.** The only supported package formats are `.deb` and `.AppImage`. There are no Windows, macOS, or RPM packages, and no plans to add them.

---

## Status: parity audit in progress

SupportOS++ is a Rust/Tauri 2/Leptos port of the TypeScript reference
[`supportos`](https://github.com/kimpearce888/supportos). A strict
reference-parity audit is in progress — see **[PARITY.md](PARITY.md)** for
the canonical F-ID checklist (what matches, what is missing, what differs)
and PROGRESS.md for current session state. Claims on this page are limited
to what the audit has actually verified.

**What is real today:**

- Rust workspace: `core` (Axum HTTP server + SQLite/FTS5 business core),
  `ui` (Leptos/WASM), `app` (Tauri 2 shell), `catalog` (closed vocabularies),
  `xtask` (dev/test/lint/package/e2e tooling).
- Loopback HTTP server (127.0.0.1:3000) with mutation rate limiting,
  Host-header DNS-rebinding guard, and localhost-only CORS.
- SQLite (bundled, WAL, FTS5) persistence with boot-time migrations.
- SSE event bus (`/api/events`) wired to conversation/webhook/demo mutations.
- Demo mode with simulate-incoming / simulate-rating / simulate-webhook endpoints.
- 900+ cargo tests across the workspace.

**What is not yet at reference parity (see PARITY.md for the full list):**
the Leptos UI routes fewer pages than the reference, several HTTP routes are
stubbed pending wiring to the engines behind them, the real Help Scout
provider/OAuth flow and production vector search wiring are incomplete, and
`.sosync` bundles are not yet byte-compatible with the reference format.

---

## Why it was built

The reference `supportos` is a TypeScript web app that depends on a cloud database, cloud AI, and a server. We wanted a version that:

- **Stays local.** Your Help Scout data lives on your machine, not in a cloud database. Network traffic goes to Help Scout (for sync), your local AI provider (LM Studio, optional), and user-configured connectors — nothing else.
- **Works offline.** No internet? The app still works. Sync pauses; everything else continues.
- **Is auditable.** Every line is Rust. No `node_modules` black box. `cargo audit` checks for known vulnerabilities. The audit record is committed verbatim in `PARITY.md`.

## How it works (simple terms)

1. **Sync.** SupportOS++ talks to the Help Scout API (OAuth 2.0), fetches conversations/customers/mailboxes, and stores them in a local SQLite database (with WAL + FTS5 for fast full-text search). Webhooks push updates in real-time when configured.
2. **Analyze.** The local AI (if configured — LM Studio at `127.0.0.1:1234`) reads conversations and generates summaries, suggested replies, customer attributes, and coaching tips. AI is advisory only — auto-customer-reply is permanently OFF.
3. **Organize.** The Operations Center shows 16 tiles of real-time metrics. The Issue Radar surfaces known issues, clusters, and incidents. Reports support 21 metrics × 14 dimensions with previous-period comparison.
4. **Backup.** Export the entire database (encrypted with AES-256-GCM + scrypt in `.sosync` format). Restore on any machine.

## Key decisions

| Decision | Why | Trade-off |
|---|---|---|
| Rust + Tauri 2 | Memory safety, small binaries, no Chromium | Slower compile; Leptos/WASM UI is less mature than React |
| SQLite (bundled, WAL, FTS5) | No external database; ACID; full-text search built-in | Not a distributed database (single machine) |
| Linux-only, deb + AppImage only | Project scope decision | No Windows/macOS/RPM packages |
| AI is advisory, never auto-reply | Spec rule: "Auto-customer-reply is permanently OFF" | Slower than fully automated; safer for support quality |
| No telemetry, no cloud AI | Privacy-first; all data stays local | No remote monitoring; no GPT-4/Claude integration |

## Development

```bash
./bootstrap.sh        # Linux only: install Rust + system deps, build, launch
cargo xtask lint      # rustfmt --check + clippy -D warnings
cargo xtask test      # workspace tests
cargo xtask package   # tauri build (deb + AppImage)
```

CI (Linux runners only): fmt + clippy + tests + WASM build + Tauri deb/AppImage
bundle + package smoke tests + WebDriver E2E via tauri-driver + cargo audit.

## Credits

The reference repo is [`supportos`](https://github.com/kimpearce888/supportos) (TypeScript).

## License

MIT. Help Scout is a trademark of Help Scout, Inc. SupportOS++ is an independent, open-source integration and is not affiliated with or endorsed by Help Scout.
