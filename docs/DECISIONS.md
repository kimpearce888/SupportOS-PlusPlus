# DECISIONS.md — SupportOS++

> All design decisions, with rationale. Append-only. Each decision has an ID, date, status, and "why".
> Statuses: PROPOSED · ADOPTED · SUPERSEDED · REJECTED.

## D-001 — Use Leptos for the WASM UI

- Date: Session 1
- Status: ADOPTED
- Context: The master spec says "Rust/WASM frontend (Leptos preferred; justify the choice in docs/DECISIONS.md)".
- Decision: Use **Leptos** with the `csr` (client-side rendering) feature for the WASM frontend, served as static assets by Tauri.
- Why:
  - Idiomatic Rust: signals instead of vdom diffing; no hand-written JS/TS anywhere (spec hard rule).
  - Server-functions map cleanly to Tauri `invoke` commands — no separate HTTP API for the UI.
  - Stable, well-maintained, broad community; ships with `trunk` for builds.
  - Hydration mode is available later if we ever serve HTML for SEO (we won't — local app — but it's free optionality).
- Alternatives considered:
  - **Dioxus**: equally viable; chose Leptos for stronger signal story and tighter fit with Tauri 2 examples.
  - **Yew**: older hooks model; would have worked but Leptos's fine-grained reactivity is a better fit for a complex, frequently-updating dashboard UI.
  - **Sycamore**: smaller community; same reactivity model as Leptos but less ecosystem.

## D-002 — Single embedded loopback listener for webhook + OAuth only

- Date: Session 1
- Status: ADOPTED
- Context: A2 of the spec bans a localhost backend, but Help Scout webhooks and the OAuth redirect callback need an inbound HTTP listener.
- Decision: Exactly **one** embedded HTTP listener bound to `127.0.0.1` (port chosen at startup, persisted in settings). It serves only two routes: `/oauth/callback` and `/webhooks/helpscout`. Everything else uses Tauri IPC commands and events. No local web API for the UI, no server mode, no separate CLI.
- Why: minimal attack surface; Host-header validation + rate limiting + timing-safe HMAC + single-use OAuth state + persist-first dedup is enough; satisfies the spec letter and intent.
- Implementation notes:
  - Use `axum` (tokio, mature, no surprises) behind a small `loopback` module in `crates/core`.
  - Only the `loopback` module may import `axum`; everything else goes through Tauri commands.

## D-003 — Pin `qdrant-edge` exact version; never auto-bump

- Date: Session 1 (placeholder — actual pin recorded in M1-T11)
- Status: PROPOSED (will become ADOPTED when M1-T11 records the version)
- Context: A4 mandates the `qdrant-edge` Rust crate, embedded, behind our `VectorStore` trait.
- Decision: Pin an **exact** version (no `^`, no `~`) in `Cargo.toml`. Document the pin and the rationale in `docs/architecture/VECTORSTORE.md`. CI dependency-audit job (`cargo audit`) must not bump it without a re-spike.
- Why: pre-1.0 crate; minor bumps can break behaviour silently; spec requires evidence per platform before any bump.

## D-004 — SQLite is the single source of truth; vectors are derived

- Date: Session 1
- Status: ADOPTED
- Context: A4 last paragraph + master spec.
- Decision: SQLite (rusqlite, bundled, WAL+FTS5) is authoritative. Qdrant Edge collections are derived from SQLite text and rebuildable from it. No Qdrant URL/API key anywhere in the UI or settings.
- Why: matches the spec; keeps the data story simple (one backup format = one `.sosync` file); vectors can be re-embedded when the model changes.

## D-005 — `cargo xtask` is the single entry point

- Date: Session 1
- Status: ADOPTED
- Context: Spec bans Node/Python; needs a dev/test/lint/package entry point.
- Decision: A single `cargo xtask` binary with subcommands: `dev`, `test`, `lint`, `package`, `discover`, `audit`. No `npm` scripts, no shell-script glue beyond `bootstrap.sh` / `bootstrap.ps1` (which only install prerequisites and then call `cargo xtask`).
- Why: trivially satisfies "no Node" rule; one language; reproducible on all OSes; integrates with CI without extra dependencies.

## D-006 — `com.supportos.plusplus` bundle identifier

- Date: Session 1
- Status: ADOPTED
- Context: A0 mandates ASCII identifier `supportos-plusplus` for bundle id, crate names, file names.
- Decision: Tauri bundle id `com.supportos.plusplus`. Crate names: `supportos-plusplus-app`, `supportos-plusplus-core`, `supportos-plusplus-ui`, `supportos-plusplus-xtask`.
- Why: satisfies A0; reverse-DNS convention for Apple; safe on all platforms.

## D-007 — UI is English only (A1)

- Date: Session 1
- Status: ADOPTED
- Context: A1.
- Decision: No i18n framework, no locale files, no language switcher, no RTL. Ticket translation (spec §65) IS supported and uses the local AI provider; customer text in any language is stored/searched/displayed UTF-8 safe with broad font fallback.
- Why: spec mandate; reduces surface area; keeps the UI simple.

## D-008 — Status vocabulary uses an enum, not strings

- Date: Session 1
- Status: ADOPTED (principle; per-vocabulary enums implemented as M2/M3 lands)
- Context: A12 + KNOWN PITFALLS.
- Decision: Every closed vocabulary (condition kinds, tiles, notification types, metrics, dimensions, attribute keys, graph node kinds, Copilot tools, response states, incident statuses, etc.) is a single Rust `enum` in `crates/core::catalog`. DB columns store the enum's discriminant as TEXT; validation, FTS, UI, and tests all derive from the same enum.
- Why: one source of truth; impossible to write an unknown value; type system catches missing cases at compile time.

## D-009 — Database format is its own (A6); no compatibility with the reference

- Date: Session 1
- Status: ADOPTED
- Context: A6.
- Decision: SupportOS++ has its own SQLite schema and `.sosync` bundle format. Same crypto approach as the reference (AES-256-GCM, scrypt-derived key, authenticated header, verify-first import, safety backup, atomic swap) but our own documented versioned format. Compatibility with the original app's database or bundles is not required.
- Why: spec mandate; we are a reimplementation, not a fork.

## D-010 — Reference counts verified against code, not README

- Date: Session 1
- Status: ADOPTED
- Context: A7 + PRECEDENCE §3.
- Decision: Every count claim in the spec/README was re-derived from the reference source code in session 1. All 8 canonical counts match. Any future discrepancy goes to `docs/DEVIATIONS.md`.
- Why: spec mandates "code wins".

## D-011 — Installer naming fallback rule

- Date: Session 1
- Status: PROPOSED (confirmed when M1-T10 runs)
- Context: A0 last paragraph.
- Decision: Try `SupportOS++` as the product name in every installer format (MSI, NSIS, DMG, DEB, RPM, AppImage). For any format that rejects `+` in the product name (likely DEB/RPM package names — Debian policy restricts package names to `[a-z0-9+.-]` but `+` is unusual), fall back to ASCII `supportos-plusplus` for that format's **file name** only. The in-app product name stays `SupportOS++`. Every fallback is recorded here in M1-T10.
- Why: satisfies A0 letter and intent; keeps the user-facing name consistent everywhere it can be.

## D-012 — CI uses native GitHub Actions runners, not containers

- Date: Session 1
- Status: ADOPTED
- Context: A5.
- Decision: Three CI jobs — `windows-latest`, `macos-latest`, `ubuntu-22.04` (and `ubuntu-24.04` for the Linux matrix per INSTALL AND PACKAGING). Each job runs the same matrix: `rustfmt --check`, `clippy -D warnings`, `cargo test`, `cargo build --release`, `trunk build` (WASM), and a headless demo-mode boot smoke test.
- Why: matches A5 ("fresh CI runners"); avoids cross-compilation complexity for the WASM target.
