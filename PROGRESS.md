# PROGRESS.md — SupportOS++

> Instruction file: `AGENTS.md` (top of repo).
> Repository is the only memory between sessions. Every session may be your last.

## Current state

| Field | Value |
|---|---|
| Instruction file | `AGENTS.md` |
| Current milestone | M1 — Foundation |
| Current task ID | M1-T01 |
| Last completed task | Session-1 setup (repo, master spec, discovery, state files, workspace skeleton) |
| Last commit hash | _(set after first push — see `git log`)_ |
| Last updated | Session 1 |

## Next 3 tasks

1. **M1-T01**: Implement `xtask discover` (A7) — replace the manual discovery pass with a Rust xtask that reads a local reference checkout and writes `docs/original-notes/*.md` + `docs/PARITY-MATRIX.md` capability rows. (The session-1 manual pass produced the first cut; the xtask will keep it reproducible.)
2. **M1-T02**: Stand up the Tauri 2 + Leptos shell that launches on Win/macOS/Linux with the `SupportOS++` product name and `com.supportos.plusplus` bundle id.
3. **M1-T03**: SQLite (WAL + FTS5) connection, first migration, and the migrations runner.

(Full M1 task list will be written to `TASKS.md` before M1 work continues — per spec "Write the full task list for a milestone before starting it.")

## Parity counts by status (honest, A3)

| Status | Count |
|---|---|
| DISCOVERED | 8 canonical counts + ~13 surface-area rows + per-milestone high-level rows |
| SPECIFIED | 12 (M1 capabilities listed in `docs/PARITY-MATRIX.md`) |
| IMPLEMENTED | 0 |
| TESTED | 0 |
| PACKAGED | 0 |
| VERIFIED | 0 |

**The project is NOT complete and is NOT at 100% parity.** Do not claim otherwise.

## Known issues

- Tauri CLI and `trunk` CLI were not fully built at end of session 1 (long Rust compile). The Cargo workspace + `tauri-cli` as a dev-dependency means `cargo xtask dev` will work once the toolchain is available; until then, the M1-T02 build verification step is BLOCKED-on-toolchain (a tooling issue, not a spec deviation).
- GitHub Personal Access Token was supplied by the owner in plaintext in the chat. It is stored ONLY in `~/.git-credentials` on the dev machine (never in the repo). The owner has been advised to revoke and rotate it.

## Decisions this session

- **D-001**: Use Leptos for the WASM UI (preferred by spec A12 / master spec; stable, idiomatic Rust, server-functions map cleanly to Tauri IPC). Recorded in `docs/DECISIONS.md`.
- **D-002**: Single embedded HTTP listener on `127.0.0.1` for webhook + OAuth redirect only (A2). All other UI/backend traffic via Tauri IPC.
- **D-003**: Pin `qdrant-edge` to whatever exact version is current at M1-T11 (Qdrant spike). The pin will be recorded in `docs/architecture/VECTORSTORE.md` and `Cargo.toml`; never auto-bumped.
- **D-004**: SQLite (rusqlite bundled, WAL+FTS5) is the single source of truth. Vectors are derived and rebuildable from SQLite text. No Qdrant URL/API key in the UI.
- **D-005**: `cargo xtask` is the single entry point for `dev`/`test`/`lint`/`package`/`discover`/`audit`. No `npm`/`node` scripts. Keeps the spec's "no Node" rule trivially true.

## Resume protocol for next session

1. Read `AGENTS.md` → this file → `TASKS.md`.
2. `git status` + `git log --oneline -20` + `cargo xtask lint && cargo xtask test`.
3. Confirm `tauri-cli` and `trunk` are installed (install if missing: `cargo install tauri-cli --version '^2.0' --locked --no-default-features && cargo install trunk --locked`).
4. Announce `Resuming at M1/M1-T01. Last commit: <hash>. Next: implement xtask discover.`
5. Continue from the first unchecked task in `TASKS.md`.
