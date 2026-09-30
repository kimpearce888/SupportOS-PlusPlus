# Reference Repo Version

The reference repository for SupportOS++ is:
**https://github.com/kimpearce888/supportos**

## Last inspected HEAD

| Field | Value |
|-------|-------|
| SHA | `c346fb51466e237a89e70156ae20a3386be0b322` |
| Author | kimpearce888 |
| Date | Mon Sep 28 14:44:32 2026 +0000 |
| Subject | docs: restructure README for readability — 14.9k words → 3.4k |
| Reference `package.json` version | `2.2.1` |
| Reference Tauri config productName | `SupportOS` |
| Reference Tauri config identifier | `com.supportos.local` |
| Bundle targets in reference | `msi`, `nsis`, `dmg`, `appimage` |

## Matches the spec?

Yes — the SHA recorded in spec amendment **A8** (`c346fb51466e237a89e70156ae20a3386be0b322`) is identical to the current HEAD. No new reference-side capabilities need to be added to the matrix for this session.

## How to re-check (next session)

```bash
git ls-remote https://github.com/kimpearce888/supportos.git HEAD
```

Compare against the SHA above. If it differs:

1. Fetch the new HEAD into a local checkout of the reference (NEVER inside this repo).
2. Run `cargo xtask discover --reference <path>` (when implemented) to rebuild the inventories.
3. Diff the new inventories against `docs/original-notes/*.md` and `docs/PARITY-MATRIX.md`.
4. List every new or changed capability under "Reference delta since last session" at the top of `docs/PARITY-MATRIX.md`, then continue milestone work.

## Reference repo at a glance (snapshot of structure on this HEAD)

```
supportos/
├─ .env.example             # 25 env vars (Help Scout OAuth, LM Studio, Qdrant URL, etc.)
├─ CHANGELOG.md             # ~135 KB; 660+ lines; authoritative behaviour spec
├─ README.md                # screenshots + canonical count claims
├─ docs/
│  ├─ ARCHITECTURE.md       # ~35 KB
│  ├─ DECISIONS.md          # ~23 KB (60 ADRs)
│  ├─ API-INTEGRATION.md
│  ├─ AI-SETUP.md
│  ├─ CLIENT-INTELLIGENCE.md
│  ├─ BACKUP-RESTORE.md
│  ├─ DESKTOP.md
│  ├─ TESTING.md            # ~50 KB
│  ├─ TROUBLESHOOTING.md
│  └─ screenshots/          # ~25 PNGs + demo.gif (UI parity reference, A11)
├─ src/                     # TS app (server + client + shared)
│  ├─ server/
│  │  ├─ routes/            # 32 route files, ~310 endpoints
│  │  ├─ database/
│  │  │  ├─ migrations/     # 16 migrations (001-016) + index.ts
│  │  │  └─ repositories/   # 23 repository files
│  │  ├─ ai/                # LM Studio provider, copilot tools, attributes
│  │  ├─ operations/        # Operations Center tiles, workload
│  │  ├─ notifications/     # sweep engine
│  │  └─ ...25 module dirs
│  ├─ client/pages/         # 21 .tsx pages (UI parity reference, A11)
│  ├─ shared/               # 14 files: activity, collaboration, graph, reporting, etc.
│  └─ types/
├─ src-tauri/               # Tauri 2 shell (thin: tauri + single-instance plugin)
├─ scripts/                 # release/build/audit scripts (mjs/ts)
└─ .github/workflows/       # ci.yml, desktop-release.yml
```

This snapshot was produced by `discover` (manual pass in session 1; to be replaced by the `xtask discover` command in M1-T01).
