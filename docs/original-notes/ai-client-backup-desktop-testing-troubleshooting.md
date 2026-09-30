# Original Notes — AI-SETUP, CLIENT-INTELLIGENCE, BACKUP-RESTORE, DESKTOP, TESTING, TROUBLESHOOTING

> Condensed from the reference repo's `docs/{AI-SETUP,CLIENT-INTELLIGENCE,BACKUP-RESTORE,DESKTOP,TESTING,TROUBLESHOOTING}.md`.
> One file per area per A7; this file consolidates the smaller ones.

---

## AI-SETUP

### Providers
- **LM Studio** (default): OpenAI-compatible API at `http://127.0.0.1:1234/v1`. Two endpoints: chat completions and embeddings.
- **Ollama**: native API at `http://127.0.0.1:11434`.
- **Generic**: any OpenAI-compatible endpoint the user configures.
- SupportOS++ wraps all three behind a `LocalAiProvider` trait (D-008) — only the adapter knows which one.

### Models
- Two roles: chat (reasoning) and embedding (vectors). User picks one of each.
- Embedding dim is read from the model's first response; the VectorStore collection is created with that dim.

### Caching
- AI output cached by `hash(input_text) + prompt_version` in the `ai_runs` table.
- Re-embed only when content hash changes; failed embedding retries capped (KNOWN PITFALLS).

### Bounds (constants.ts)
- `COPILOT_MAX_TOOL_ROUNDS = 5`
- `COPILOT_MAX_TOOL_CALLS = 8`
- `COPILOT_TOOL_RESULT_MAX_CHARS = 4000`

### Honesty
- "Unknown" is a legitimate AI answer; never fabricated.
- AI attributes never overwrite Help Scout source data.
- Customer-reply sending is permanently OFF.

---

## CLIENT-INTELLIGENCE (M7)

### Interaction engine
- Maintains per-customer interaction signals: response style preferences, question count, technical familiarity, frustration cues, escalation intent.
- Deterministic by default (zero AI): observable from message metadata.
- AI layer fills AI-designated slots only when LM Studio is enabled.

### Evidence
- Every AI-derived attribute carries an evidence excerpt + thread reference.
- Preference requires ≥3 observations before counting as a pattern (`INTERACTION_MIN_OBSERVATIONS_FOR_PREFERENCE = 3`).
- Recency weighting half-life: 90 days (`INTERACTION_RECENCY_HALF_LIFE_DAYS`).
- Change significance threshold: 0.34.

### Issue Radar / known issues / clusters
- Known issue: a customer-facing problem with a name, status, and links.
- Issue cluster: a cluster of similar conversations (vector similarity + lexical overlap).
- Issue spike: cluster with abnormal recent growth — surfaces in Notification Center.
- Incident: a known issue that has been promoted to incident status (5 statuses, 4 severities, 3 sources).

---

## BACKUP-RESTORE (A6, M10)

### Format (reference `.supportos` — SupportOS++ uses its own `.sosync`)
- AES-256-GCM symmetric cipher.
- scrypt-derived key from a user passphrase.
- Authenticated header: magic bytes + version + salt + nonce + ciphertext + GCM tag.
- Versioned; SupportOS++ writes its own version `sosync_v1` (no compat with reference).

### Import (verify-first)
1. Read header; verify magic + version.
2. Decrypt with passphrase; verify GCM tag.
3. If tag fails: abort, do not touch the existing DB.
4. If tag passes: take a safety backup of the current DB to `supportos-plusplus-pre-restore-<ts>.sosync`.
5. Atomic swap: write the new DB to a temp file, then `rename` over the live DB file (atomic on POSIX, near-atomic on Windows).

### Export
- Stream SQLite tables → JSON → gzip → AES-256-GCM encrypt → write to user-chosen path.
- Default file name: `supportos-plusplus-<ts>.sosync`.

---

## DESKTOP (A0, M1)

### Tauri 2 bundle config (reference)
- `productName`: `SupportOS` → SupportOS++ uses `SupportOS++`.
- `identifier`: `com.supportos.local` → SupportOS++ uses `com.supportos.plusplus`.
- `bundle.targets`: `["msi", "nsis", "dmg", "appimage"]` → SupportOS++ adds `deb` and `rpm` (Linux is mandatory).
- Windows: `webviewInstallMode: embedBootstrapper` (handles missing WebView2 silently).
- macOS: `minimumSystemVersion: "10.15"`.
- Data folder: per-OS user profile (NOT in the install dir; survives uninstall).

### Data folder locations (SupportOS++ planned)
- Windows: `%APPDATA%\supportos-plusplus\`
- macOS: `~/Library/Application Support/supportos-plusplus/`
- Linux: `$XDG_DATA_HOME/supportos-plusplus/` (defaults to `~/.local/share/supportos-plusplus/`)

### Single instance
- Reference uses `tauri-plugin-single-instance`; SupportOS++ does the same.

### WebView2
- Windows: bundle the offline bootstrapper so installs work without internet.
- macOS/Linux: system WebKit/GTK — no installer needed.

---

## TESTING (M11 + every milestone)

### Reference's testing surface
- Unit tests in each TS module.
- Integration tests under `tests/` — including:
  - DST matrices (23h, 25h, 24.5h, +5:45).
  - Response-state SQL/Rust equivalence.
  - Event derivation + dedup.
  - View compilation with injection-shaped values.
  - SSRF matrix (loopback, private, link-local, allow-list).
  - Webhook HMAC + dedup.
  - Write-pipeline behavior.
  - Job recovery (enqueue → claim → execute → dead-letter).
  - Upgrade-in-place migrations.
  - VectorStore contract (spec §92).
  - Crash recovery.
  - Performance guards (synthetic 2,000-conversation dataset).
- Black-box audit binary: `scripts/audit-phase1.mjs` (~80 KB).

### SupportOS++ testing
- Same coverage matrix, all in Rust.
- `cargo xtask test` runs all unit + integration tests across the workspace.
- `cargo xtask audit` runs the ported black-box audit binary against a packaged app.
- CI on Win/macOS/Linux: fmt, clippy `-D warnings`, tests, WASM build, Tauri build, headless demo-mode boot.

---

## TROUBLESHOOTING (for users; condensed)

### Common symptoms + fixes (port to SupportOS++ user-facing docs as needed)
- **App won't launch on Windows**: missing WebView2 → fixed by the embedded bootstrapper.
- **OAuth redirect fails**: the loopback port is taken; the app picks the next free port and persists it.
- **Sync stuck**: check `sync_cursors` for the last checkpoint; resume from there with the 10-minute overlap window.
- **Webhook not receiving**: confirm Help Scout can reach the user's machine; "Webhook push" screen shows the registered URL and state.
- **AI Center "No provider detected"**: start LM Studio or Ollama and ensure the default port is open.
- **DB locked**: WAL mode + busy_timeout should prevent this; if it happens, restart the app.
- **Restore failed with "GCM tag mismatch"**: passphrase is wrong, or file is corrupt; the existing DB is untouched.
