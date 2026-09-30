# MANUAL-VERIFICATION.md — SupportOS++

> Step-by-step checklist the owner runs on a clean machine.
> Per spec A5: rows that depend on this checklist stay PACKAGED until the owner records results here.
> Per spec A3: a row may be marked VERIFIED only with recorded evidence (test name + packaged-app check).

## How to use this file

1. Download the latest release asset for your OS from `https://github.com/kimpearce888/SupportOS-PlusPlus/releases`.
2. Install on a clean machine (no prior SupportOS++ install; fresh user profile).
3. Walk the checklist below, top to bottom.
4. For each row, write `✅ verified <date>` or `❌ failed <date>: <one-line symptom>` next to the row.
5. When a row is verified, the matching capability row in `docs/PARITY-MATRIX.md` may be promoted to VERIFIED.

## Pre-flight

| # | Check | Result |
|---|---|---|
| 0.1 | Downloaded installer matches the OS (`.msi`/`.exe` on Windows, `.dmg` on macOS, `.deb`/`.rpm`/`.AppImage` on Linux) | |
| 0.2 | SHA-256 of the downloaded file matches the value in the GitHub release notes | |
| 0.3 | OS is on the supported matrix (see README "Supported OS matrix") | |

## Install

| # | Step | Expected | Result |
|---|---|---|---|
| 1.1 | Double-click the installer | Installer launches; window title says "SupportOS++" (or ASCII `supportos-plusplus` for the package file name only, per D-011) | |
| 1.2 | Accept the license | License text is MIT; "Help Scout is a trademark of Help Scout, Inc. SupportOS++ is an independent, open-source integration and is not affiliated with or endorsed by Help Scout." appears | |
| 1.3 | Choose install location | Default location is per-OS convention (no system dir on macOS/Linux) | |
| 1.4 | Click Install | Installs without error; no admin prompt on macOS/Linux; WebView2 bootstrapper silently on Windows if missing | |
| 1.5 | Finish | "Launch SupportOS++" checkbox present | |

## First launch

| # | Step | Expected | Result |
|---|---|---|---|
| 2.1 | Launch the app | Window opens; title is `SupportOS++` | |
| 2.2 | First-run onboarding appears | Offers "Try the 2-minute demo mode" — no credentials required | |
| 2.3 | Click "Try demo mode" | App loads a simulated mailbox; sample conversations visible | |
| 2.4 | Quit the app | Quits cleanly; no orphan process; no error dialog | |
| 2.5 | Relaunch | Demo data persists from the previous session | |

## Sync (real Help Scout credentials, NOT demo mode)

> Owner only. Real Help Scout credentials must be entered inside the running app only. Never commit credentials.

| # | Step | Expected | Result |
|---|---|---|---|
| 3.1 | Open Settings → Help Scout | OAuth client ID/secret fields visible; values are masked after entry | |
| 3.2 | Click "Connect" | Browser opens Help Scout OAuth page; app's loopback listener receives the callback at `127.0.0.1:<port>/oauth/callback` | |
| 3.3 | Authorize in Help Scout | App returns to foreground; status shows "Connected as <name>" | |
| 3.4 | Trigger first sync | Incremental sync runs (≈5-minute cycle); conversations appear | |
| 3.5 | Wait one cycle | New conversations since the last sync appear without manual refresh | |

## Search

| # | Step | Expected | Result |
|---|---|---|---|
| 4.1 | Open the universal search | Command palette opens (Cmd/Ctrl+K) | |
| 4.2 | Type a known term | Results appear with relevance ranking; FTS works | |
| 4.3 | Open a result | The conversation/customer/doc opens | |

## Backup and restore

| # | Step | Expected | Result |
|---|---|---|---|
| 5.1 | Settings → Backup → Create | File picker prompts; default name `supportos-plusplus-<ts>.sosync` | |
| 5.2 | Save the file | `.sosync` file written; size > 0; magic header present | |
| 5.3 | Settings → Restore → pick the file | Verify-first import runs; safety backup of the current DB is created automatically; atomic swap on success | |
| 5.4 | Restart the app | All data is present; checksum matches | |

## Update

| # | Step | Expected | Result |
|---|---|---|---|
| 6.1 | With a newer release available, open Settings → About | "Update available" notice appears with version diff | |
| 6.2 | Click "Download and install" | Download progress visible; verified against the published SHA-256 | |
| 6.3 | Restart when prompted | App relaunches into the new version; data intact | |

## Uninstall

| # | Step | Expected | Result |
|---|---|---|---|
| 7.1 | Uninstall via the OS | App removed from installed programs list | |
| 7.2 | Confirm the data folder | Per-OS user profile data folder remains by default (so reinstall preserves data); a "Remove all data" option exists in the uninstaller (Windows) | |
| 7.3 | Reinstall | App launches as if first run; offers demo mode | |

## Optional: local AI providers

| # | Step | Expected | Result |
|---|---|---|---|
| 8.1 | Start LM Studio (or Ollama) on the default port | App's AI Center auto-detects the provider | |
| 8.2 | Pick a chat model and an embedding model | "Test connection" passes; latency shown | |
| 8.3 | Without any local AI provider running | All non-AI features work; AI Center shows "No provider detected" honestly (no silent failure) | |

## Reporting results

After a run, append a section to this file:

```
## Run — <date> — <OS> <version> — installer <format> <version>

| Row | Result |
|-----|--------|
| 1.1 | ✅ verified 2026-09-30 |
| 1.2 | ✅ verified 2026-09-30 |
| 1.3 | ❌ failed 2026-09-30: window title shows "SupportOS" not "SupportOS++" |
...
```

Promote corresponding rows in `docs/PARITY-MATRIX.md` to VERIFIED only when there is recorded evidence here.
