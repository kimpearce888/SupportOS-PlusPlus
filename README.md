# SupportOS++

**The local-first, AI-powered support operating system for your Help Scout mailbox — as a Rust desktop app.**

SupportOS++ mirrors your Help Scout mailbox into a local SQLite database, then
layers a support workspace on top: fast local search, a real inbox, team
operations, and optional local AI. Your customer data never leaves your
machine: no telemetry, no cloud AI, no data egress. The server binds to
`127.0.0.1` only.

This repository is a Rust/Tauri 2 port of the TypeScript reference
[`supportos`](https://github.com/kimpearce888/supportos) (same features, API
contract, data and safety rules — see *Differences from the reference* below).

---

## What you get

- **A real support inbox** — conversation list, thread view, reply and note
  composer, status changes, assignment, saved views, bulk close
- **Universal search** — one query across conversations, threads, customers,
  knowledge, known issues, saved replies and docs, powered by SQLite FTS5;
  semantic ticket search via stored Float32 embeddings with a local cosine
  scan (no vector server needed), fused with Reciprocal Rank Fusion
- **Team operations** — Operations Center tiles, workload and capacity,
  notification center with preferences, side threads, automation rules with
  action-risk tiers
- **Support intelligence** — issue radar, incidents, SLA and business-hours
  reporting, a report builder (21 metrics × 14 dimensions), client
  interaction signals with a fixed behavioral vocabulary
- **Local AI, optional** — an interactive copilot with a read-only tool
  allowlist, evidence-backed draft replies, pre-send coaching and customer
  memory, all against LM Studio on your machine; every bit of it is advisory
- **Local-first plumbing** — sync engine with per-resource checkpoints and
  reconciliation, webhooks (HMAC-SHA1, persist-first, hash dedup),
  SSE live updates, backups and encrypted `.sosync` bundles

Help Scout remains the source of truth — SupportOS++ is the fast, private,
intelligent layer on top of it.

## Safety — trust by design

- Binds `127.0.0.1` only, with a Host-header DNS-rebinding guard and
  localhost-only CORS
- OAuth tokens and secrets never reach the webview/browser side
- Automatic customer-reply sending is permanently OFF; AI is advisory
- AI evaluation mode blocks every remote write
- Payment data, tokens and API keys are redacted before AI prompts and logs
- Untrusted ticket HTML is sanitized before render
- Write pipeline: validate → auth → fresh-read → merge → write → confirm →
  persist → audit; idempotent sends, never auto-retried

## Install

**Linux x86_64 only.** The only supported package formats are `.deb` and
`.AppImage` (built and smoke-tested in CI on Ubuntu 22.04 and 24.04). There
are no Windows, macOS or RPM packages.

1. Download the `.deb` or `.AppImage` from
   [Releases](https://github.com/kimpearce888/SupportOS-PlusPlus/releases).
2. Install it (`sudo apt install ./SupportOS++_*.deb`) or run the
   `.AppImage`.
3. Launch **SupportOS++**. On first run choose demo mode — no credentials
   needed.

## Build, run, test

Requires Rust (stable), the `wasm32-unknown-unknown` target, Trunk, and the
Tauri 2 Linux prerequisites (WebKit2GTK 4.1, GTK 3).

```bash
git clone https://github.com/kimpearce888/SupportOS-PlusPlus.git
cd SupportOS-PlusPlus

cargo xtask dev          # tauri dev (UI dev server + shell)
cargo xtask test         # cargo test --workspace
cargo xtask lint         # rustfmt --check + clippy -D warnings
cargo xtask package      # tauri build → deb + AppImage
cargo xtask audit --app PATH   # black-box audit of a packaged app
```

The HTTP API server (the Rust counterpart of the reference's Fastify server)
listens on `127.0.0.1:3000` and serves the webview and any localhost browser
client — webhooks, OAuth callbacks, SSE (`/api/events`) and the demo
endpoints included. Demo mode (`LOCAL_DEMO_MODE=true`) runs against a
simulated Help Scout provider with seeded sample data.

## Differences from the reference

Intentional, owner-approved differences — everything else aims to match the
reference exactly:

1. **Linux x86_64 only (D1).** The reference ships Windows/macOS/Linux
   installers; this port builds and verifies `.deb` + `.AppImage` on Linux
   only, and its code-signing/notarization work is not ported.
2. **No external vector server (D2 direction).** The reference can use a
   locally-run Qdrant server for semantic docs search; this port stores
   Float32 embeddings in SQLite and ranks them with an in-process cosine
   scan, so semantic search works with zero external services. Keyword
   (FTS5) search remains fully functional either way.

Language-forced substitutions (no behavioral difference intended): Rust
crates in place of npm packages, Leptos in place of React, serde in place of
Zod, Axum in place of Fastify, cargo in place of npm/Vitest.

## Repository layout

- `crates/core` — the HTTP API server + SQLite business core
- `crates/ui` — the Leptos/WASM client
- `crates/app` — the Tauri 2 desktop shell (thin launcher: boots the HTTP
  server and opens a window; no IPC)
- `crates/catalog` — closed vocabularies shared by core and UI
- `crates/xtask` — dev/test/lint/package/audit entry points

## License

[MIT](LICENSE)
