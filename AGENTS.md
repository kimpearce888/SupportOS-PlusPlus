# AGENTS.md — SupportOS++

> This is the project instruction file. The agent loads it automatically at the start of every session.
> File name recorded at the top of `PROGRESS.md`.

## Goal

Independent Rust/Tauri 2 desktop reimplementation of the existing `supportos` TS app. Local-first: no telemetry, no cloud AI, no data egress. Network traffic only to Help Scout, local AI providers, and user-configured connectors.

## Naming (A0)

- Display name everywhere (window title, installer product name, README, docs): **SupportOS++**
- GitHub repo slug: `SupportOS-PlusPlus`
- ASCII identifier (bundle id, crate names, package names, file names, log/db file names): `supportos-plusplus`
  - Example Tauri bundle id: `com.supportos.plusplus`
- Any packaging format that cannot handle `++` in the product name falls back to the ASCII name; record in `docs/DECISIONS.md`.

## Code quality (A12) — 8 lines

1. Less code first: prefer a crate, std, iterators, `?`, derive macros.
2. Never write the same logic twice — extract on the third occurrence.
3. One source of truth per closed vocabulary (single enum/catalog drives DB, validation, UI, tests).
4. No speculative code: no unused params, dead code, commented-out code, "might-need-later" hooks.
5. Type system makes wrong states impossible: enums, newtype IDs, typed `Result`, no `unwrap`/`expect` in prod paths.
6. Pure logic separate from I/O; small cohesive files; one responsibility each.
7. Tests written with the code; CI green before commit; rustfmt + clippy `-D warnings`.
8. Review own diff for duplication/dead code/long functions/unclear names; refactor in a separate small commit.

## Precedence

1. The AMENDMENTS in `docs/MASTER-SPEC.md` override the master spec where they conflict.
2. The master spec overrides everything else in this prompt.
3. Where the spec and the reference repo's actual code disagree on facts, the reference code wins; record in `docs/DEVIATIONS.md`.

## Non-negotiable rules

- Tauri 2 desktop app for Linux only (x86_64); packages: .deb + .AppImage only. Rust backend, Rust/WASM frontend (Leptos). No hand-written JS/TS anywhere (tooling-generated glue is OK).
- No Node, Python, Docker, PowerShell, or separate Qdrant process. (E2E/differential tooling is Rust, under crates/xtask.) SQLite bundled (WAL, FTS5). Everything ships inside the installer.
- Local-first: no telemetry, no cloud AI, no data egress. Network = Help Scout + local AI + user connectors only.
- AI is advisory. Auto-customer-reply is permanently OFF. "Unknown" is a legitimate answer.
- Secrets never reach the UI; always redacted in reads. Never commit credentials/tokens/user data.
- Never fake work: no dead controls, no mocks in real mode (Fake provider is for demo/tests only), no UI-only features without a working Rust core.

## Session start checklist (in order, no skipping)

1. Read this file, then `PROGRESS.md`, then `PARITY.md` (canonical audit record).
2. If `PROGRESS.md` does not exist → session 1: state detection, discovery (A7/A8/A9), write matrix + notes, start Milestone 1. Otherwise RESUME: do not redo discovery, do not re-scaffold.
3. Verify reality: `git status`, `git log --oneline -20`, run `cargo xtask test`. Commit or finish uncommitted work. If `PROGRESS.md` and git disagree, git wins.
4. Read only the master-spec sections and original-notes relevant to the current milestone.
5. Announce: `Resuming at <milestone>/<task>. Last commit: <hash>. Next: <task>.` Then continue.

## Build / test / lint commands

```bash
cargo xtask dev             # run the app in dev mode (Tauri + Leptos trunk)
cargo xtask test             # run all unit + integration tests across the workspace
cargo xtask lint             # rustfmt --check + clippy -D warnings
cargo xtask package          # build installers for the host OS
cargo xtask discover         # regenerate docs/original-notes/* from a local reference checkout
cargo xtask verify-config    # verify tauri.conf.json meets spec amendment A0 (no system deps needed)
cargo xtask audit            # black-box audit binary (port of reference audit-phase1)
```

Bootstrap script: `./bootstrap.sh` (Linux) installs prerequisites silently, then builds and launches. Safe to re-run.

## Communication rules

If something is ambiguous, choose the option closest to the reference's documented behavior, record it in `docs/DECISIONS.md`, and continue. Ask the owner only for BLOCKED items, deviations needing approval, missing credentials/certificates, and milestone sign-off.

## Legal

MIT license. Help Scout is a trademark of Help Scout, Inc. SupportOS++ is an independent, open-source integration and is not affiliated with or endorsed by Help Scout.
