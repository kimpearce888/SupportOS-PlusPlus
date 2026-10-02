# ARTIFACTS.md — Installer Contents

> Lists the files included in each installer format.
> Generated: Session 39 (STEP 5).

## DEB (`.deb`)

The DEB package contains:

| Path | Description |
|---|---|
| `/usr/bin/supportos-plusplus` | The main binary (~6.8 MB, statically links the Rust core + SQLite + FTS5) |
| `/usr/share/icons/hicolor/32x32/apps/supportos-plusplus.png` | 32×32 app icon |
| `/usr/share/icons/hicolor/128x128/apps/supportos-plusplus.png` | 128×128 app icon |
| `/usr/share/icons/hicolor/256x256@2/apps/supportos-plusplus.png` | 256×256 @2x app icon |
| `/usr/share/applications/SupportOS++.desktop` | Desktop entry file |

**Package name**: `support-os` (derived from productName "SupportOS++" by Tauri's naming convention)
**Dependencies**: `libwebkit2gtk-4.1-0`, `libssl3`, `libwebkit2gtk-4.1-0`, `libgtk-3-0`
**Installed size**: ~6.7 MB


## AppImage

The AppImage is a single self-contained file that includes the binary + all
shared libraries + the icon + the .desktop file. It runs on any Linux distro
with glibc ≥ 2.31.

**Size**: ~80 MB (includes WebKit2GTK and all dependencies)

## What's NOT in the installers

- No Node.js, no npm, no Python runtime — the entire app is Rust.
- No separate SQLite binary — SQLite is compiled into the binary (bundled feature).
- No separate Qdrant binary — the InMemoryVectorStore is used by default; the
  Qdrant Edge adapter is behind a cargo feature flag (`qdrant`).
- No external config files — all config is in the SQLite DB.
- No telemetry, no cloud AI SDKs, no data egress libraries.
