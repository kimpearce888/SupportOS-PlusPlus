# SupportOS++

**Local-first, AI-powered support operating system for Help Scout.** An independent Rust/Tauri 2 desktop reimplementation of [`supportos`](https://github.com/kimpearce888/supportos). No telemetry, no cloud AI, no data egress.

---

### Install (non-developers)

1. Go to [Releases](https://github.com/kimpearce888/SupportOS-PlusPlus/releases).
2. Download the asset for your OS (`.msi` or `.exe` on Windows, `.dmg` on macOS, `.deb` / `.rpm` / `.AppImage` on Linux).
3. Install. Launch **SupportOS++**. On first run, click "Try the 2-minute demo mode" — no credentials needed.

> M1 foundation builds will appear under Releases once `cargo xtask package` runs green on all three OSes in CI. Until then this repo is developer-only.

### Develop

```bash
./bootstrap.sh        # macOS/Linux — installs Rust + Tauri CLI + trunk, then builds and launches
# or
./bootstrap.ps1       # Windows
```

Day-to-day, from the repo root:

```bash
cargo xtask dev       # run the app in dev mode (Tauri + Leptos trunk)
cargo xtask test      # all unit + integration tests
cargo xtask lint      # rustfmt --check + clippy -D warnings
cargo xtask package    # build installers for the host OS
```

### Supported OS matrix

| OS | Versions | Architectures |
|---|---|---|
| Windows | 10, 11 | x86_64 |
| macOS | 10.15+ | x86_64 (Intel), aarch64 (Apple Silicon) |
| Linux (Ubuntu) | 22.04, 24.04 | x86_64 |
| Linux (Fedora) | 40+ | x86_64 |
| Linux (generic) | any distro with glibc ≥ 2.31 + WebKit2GTK 4.1 | x86_64 |

AppImage is the universal fallback for unsupported distros. Linux arm64 is
not supported; users on arm64 Linux must build from source.

### Stack

- **Tauri 2** desktop shell — Windows, macOS, Linux from one Rust codebase.
- **Rust backend** — SQLite (bundled, WAL + FTS5), embedded Qdrant Edge for vectors, embedded loopback HTTP listener for Help Scout webhooks + OAuth only.
- **Leptos/WASM frontend** — pure Rust; no hand-written JavaScript or TypeScript anywhere.
- **Local AI** — LM Studio, Ollama, or any OpenAI-compatible provider, all optional. The app works fully without them.

### License

MIT. Help Scout is a trademark of Help Scout, Inc. SupportOS++ is an independent, open-source integration and is not affiliated with or endorsed by Help Scout.

### Documentation

- [`docs/MASTER-SPEC.md`](docs/MASTER-SPEC.md) — the authoritative spec.
- [`docs/PARITY-MATRIX.md`](docs/PARITY-MATRIX.md) — parity status against the reference.
- [`docs/REFERENCE-VERSION.md`](docs/REFERENCE-VERSION.md) — last inspected reference HEAD.
- [`docs/DECISIONS.md`](docs/DECISIONS.md) — design decisions.
- [`docs/MANUAL-VERIFICATION.md`](docs/MANUAL-VERIFICATION.md) — owner-run checklist.
- [`docs/original-notes/`](docs/original-notes/) — condensed notes from the reference repo.
- [`AGENTS.md`](AGENTS.md) — the agent instruction file.
- [`PROGRESS.md`](PROGRESS.md) — current milestone + task.
- [`TASKS.md`](TASKS.md) — task list per milestone.
