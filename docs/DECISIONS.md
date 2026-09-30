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

## D-013 — Compare timestamps via `julianday()`, never lexically (KNOWN PITFALLS enforcement)

- Date: Session 2
- Status: ADOPTED
- Context: KNOWN PITFALLS in `docs/MASTER-SPEC.md` explicitly forbids comparing ISO-8601 timestamps against SQLite `datetime('now')` strings lexically. The session-1 jobs crate had a real flaky-test failure (~1 in 5 runs) caused by exactly this: SQLite's `strftime('%fZ','now')` produces 3 fractional digits while chrono's `%f` produces 9, so `"…123Z"` was lexically greater than `"…123456789Z"` and the `available_at <= ?` predicate silently failed.
- Decision: every SQL predicate that compares two ISO-8601 timestamps in the SupportOS++ codebase MUST use `julianday(col) <= julianday('now')` (or `unixepoch()`), never a direct string comparison. Writes still store ISO-8601 strings (SQLite-friendly, debuggable), but reads compare via the numeric conversion. Documented in `crates/core/src/jobs.rs::claim_next` with the warning inline.
- Why: removes the format-mismatch class of bugs entirely; aligns with spec mandate.
- Verification: `cargo test -p supportos-plusplus-core --lib` ran 10× consecutively in session 2; all 10 green. Before the fix, ~1 in 5 runs failed.

## D-014 — `cargo xtask discover` writes machine-readable `inventory.json`

- Date: Session 2
- Status: ADOPTED
- Context: A7 requires the discovery xtask to extract inventories and keep `docs/PARITY-MATRIX.md` reproducible.
- Decision: `cargo xtask discover --reference <path>` produces three outputs:
  1. `docs/original-notes/inventory.json` — the full inventory (surfaces, canonical counts, vocabulary values).
  2. A human-readable summary printed to stdout (the canonical-count cross-check table).
  3. An exit code that is non-zero if any of the 10 canonical counts differs from the spec, so CI catches reference drift.
- Why: makes reference drift detectable in CI without requiring a human to read the matrix; the JSON is the audit trail.
- AC for M1-T01 (per `TASKS.md`): "output diffs to zero against session-1 manual pass" — verified: 10/10 canonical counts match the session-1 manual pass and the spec.

## D-015 — Migrations live as a single `&[Migration]` const array

- Date: Session 3
- Status: ADOPTED
- Context: A12 mandates "Database changes only through forward migrations; keep configuration and constants in one place".
- Decision: All application migrations live in `crates/core/src/migrations.rs` as `pub const MIGRATIONS: &[Migration]`. Each `Migration` has `version`, `label`, `sql`. Versions are contiguous starting at 1. A migration is never edited after release; new ones append at the end with the next version number. `migrations::run_all` is idempotent — migrations already in `_migrations` are skipped.
- Why: one source of truth for the schema; trivially auditable (`git log crates/core/src/migrations.rs`); the version-contiguity test catches accidental re-ordering.
- Verification: 5 new unit tests in `migrations::tests` cover version contiguity, fresh-DB apply, idempotency, find-by-version, latest_version.

## D-016 — `cargo xtask verify-config` statically asserts A0 in CI

- Date: Session 3
- Status: ADOPTED
- Context: A0 mandates `productName = "SupportOS++"`, `identifier = "com.supportos.plusplus"`, and all 6 installer targets. The local dev sandbox can't link the Tauri shell (no GTK/WebKit2GTK), so we needed a way to verify A0 without running the app.
- Decision: New xtask subcommand `verify-config` parses `crates/app/src-tauri/tauri.conf.json` with `serde` and asserts every A0 mandate. Returns a violations list; exits 1 on any violation. Runs in CI on every push, before clippy, on every OS (no system deps needed).
- Why: catches accidental A0 regressions (e.g. someone renames the product or drops a bundle target) before the more expensive clippy + build steps; works on every OS without GUI libs.
- Verification: 7 unit tests cover the spec-compliant case + 5 violation cases (wrong product, wrong bundle id, wrong window title, missing bundle target, empty windows list) + 1 integration test that asserts the real `tauri.conf.json` in the repo passes.

## D-017 — Typed settings store helpers (bool / i64 / JSON) on top of the string-only table

- Date: Session 3
- Status: ADOPTED
- Context: A12 mandates "type system makes wrong states impossible". The `application_settings` table is `TEXT` key/value, so callers reading typed values had to parse strings themselves, with no validation.
- Decision: Add typed helpers in `crates/core/src/settings.rs`:
  - `get_bool(conn, key, default)` / `set_bool(conn, key, value)` — stored as `"true"`/`"false"` strings; invalid values produce `Error::Config` with a clear message.
  - `get_i64(conn, key, default)` / `set_i64(conn, key, value)` — stored as decimal strings; same error pattern.
  - `get_json<T>(conn, key)` / `set_json<T>(conn, key, value)` — for typed config structs.
  - `first_run_done(conn)` / `mark_first_run_done(conn)` — single-row `app_state` table from migration 1.
- Why: every caller gets type safety + a single error path. No `unwrap`/`expect` in callers; parse failures are typed `Error::Config` with the key name in the message.
- Verification: 7 new tests covering round-trips, defaults, invalid-value errors, JSON round-trip, and the first-run flag lifecycle.

## D-018 — Extract `supportos-plusplus-catalog` as a WASM-safe crate

- Date: Session 4
- Status: ADOPTED
- Context: The UI crate (`crates/ui`) needs the catalog enums (closed vocabularies) so the UI has type-safe access. The original `crates/core` has `tokio`, `rusqlite`, `axum`, `aes-gcm`, etc. — none of which compile to `wasm32-unknown-unknown`. The UI couldn't share the same source of truth.
- Decision: Extract `crates/catalog` as a tiny crate with only `serde` as a dependency (no I/O). It compiles to both native and WASM. The UI depends on the catalog crate directly. The core crate re-exports the catalog via `pub mod catalog { pub use spp_catalog::*; }` so existing `spp_core::catalog::*` call sites keep working.
- Why: one source of truth per spec A12; type-safe in both UI and core; the WASM build of the UI crate now succeeds (`cargo check -p supportos-plusplus-ui --target wasm32-unknown-unknown` is green); CI can verify the WASM build on every push.
- Verification: 15 catalog unit tests (moved from the core crate); all green. UI crate compiles for both native and WASM targets.

## D-019 — `ViewState` enum + `<StateView>` component

- Date: Session 4
- Status: ADOPTED
- Context: KNOWN PITFALLS in `docs/MASTER-SPEC.md` mandates "every view has loading, empty, and error states". The previous dashboard view had none.
- Decision: A single `ViewState` enum with four variants — `Loading`, `Empty { message }`, `Error { message, retry }`, `Loaded` — makes wrong states impossible by construction (an error with no message is unrepresentable). The `<StateView state=... children=...>` component renders the right placeholder for the current state. A `<Button>` with Primary/Ghost styles completes the common-component set.
- Why: one source of truth for the three states; type system enforces the spec rule; every future view inherits the pattern for free.
- Verification: 5 unit tests for `ViewState` constructors + clone + debug-repr + retry-callback execution.

## D-020 — Leptos 0.6 with stable Rust (no `nightly` feature)

- Date: Session 4
- Status: ADOPTED
- Context: Session 1 set the Leptos dep with the `nightly` feature. Session 4 found that `server_fn_macro` (a transitive dep) requires nightly when that feature is on — the WASM CI build was impossible.
- Decision: Use Leptos 0.6 with the `csr` feature only, on stable Rust. CSR (client-side rendering) is enough for our use case; we don't need server functions (the Tauri shell is the backend). Removed the `nightly` feature from `crates/ui/Cargo.toml`.
- Why: aligns with the `rust-toolchain.toml` (`channel = "stable"`); unblocks WASM CI; no functional loss.
- Verification: UI crate now compiles on stable Rust for both native and `wasm32-unknown-unknown` targets; 12 UI unit tests pass.

## D-021 — Job queue: JobHandler trait + JobRegistry + Runner (closes M1-T05)

- Date: Session 5
- Status: ADOPTED
- Context: KNOWN PITFALLS mandates "Job claim loops must be tested end to end (enqueue, claim, execute), not by calling components directly." Sessions 1–4 built the storage layer (`enqueue`/`claim_next`/`complete`/`fail`) but not the execute layer. M1-T05 was the only remaining M1 task explicitly requiring end-to-end testing.
- Decision: New `crates/core/src/runner.rs` module:
  - `JobHandler` trait: `fn handle(&self, payload: &str) -> HandlerOutcome`. `Send + Sync` so the registry can be shared across Tokio workers.
  - `JobRegistry`: `Clone`, backed by `Arc<HashMap<String, Arc<dyn JobHandler>>>`. Immutable `register` returns a new registry. `get` returns `Option<Arc<dyn JobHandler>>` for unknown kinds (the runner then fails with a clear message — never silently skips).
  - `Runner`: owns `&mut Connection` + `&JobRegistry`. `run_until_idle(max_iterations)` runs the claim loop until either no more jobs are available or `max_iterations` is reached (the safety bound prevents livelock from a runaway enqueue source).
  - `HandlerOutcome` enum: `Success` or `Failure { message }`. Wrong combinations impossible — the runner maps these to `jobs::complete` or `jobs::fail`.
  - `RunSummary`: processed/succeeded/failed/dead-lettered counts. `Display` impl for human-readable logs.
- Why: closes the spec mandate; the trait abstraction means M2 sync handlers, M5 embedding handlers, etc. each register one kind with one line of code.
- Verification: 10 new tests including end-to-end enqueue→claim→execute→success, payload pass-through verification, unknown-kind failure with clear message, always-fail→dead-letter, fail-then-succeed retry path. 5 consecutive runs all stable.

## D-022 — `cargo xtask audit` is a separate binary sharing `spp_xtask` lib

- Date: Session 5
- Status: ADOPTED
- Context: M1-T14 requires porting the reference's `audit-phase1.mjs` (1100+ lines of HTTP probing). The full port lands check-by-check per milestone; M1 needed the scaffold + at least one real check.
- Decision: `crates/xtask` becomes a lib + 2 binaries:
  - `src/lib.rs` (new): exposes `discover` + `verify_config` modules.
  - `src/bin/xtask.rs`: the developer entry point (dev/test/lint/package/discover/verify-config/audit). Unchanged behavior; `cargo xtask audit --app PATH` shells out to the `audit` binary.
  - `src/bin/audit.rs` (new): the black-box audit binary. Takes `--app PATH`, runs all available checks, outputs JSON (default) or text.
  - `src/bin/checks/`: each check is its own module. M1 ships `path_exists` (critical if missing, info otherwise) + `config_a0` (reuses `spp_xtask::verify_config` — one source of truth per A12).
- Why: avoids code duplication between the two binaries; the `Check` struct + `ALL` const make adding new checks trivial (one entry per milestone); the JSON output shape matches the reference so the owner's existing audit tooling is reusable.
- Verification: 8 new audit tests; smoke test against the workspace's real `tauri.conf.json` produces 2 info findings.

