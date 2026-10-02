#!/usr/bin/env bash
# bootstrap.sh — silently install build prerequisites for SupportOS++, then build and launch.
# Safe to re-run.
#
# SupportOS++ is Linux-only (x86_64/aarch64) per DEV-006.
# Supported packages: .deb and .AppImage only.

set -euo pipefail

echo "==> SupportOS++ bootstrap (Linux)"

# --- 1. Rust toolchain ----------------------------------------------------
if ! command -v cargo >/dev/null 2>&1; then
  echo "==> Installing Rust (stable) via rustup…"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile default
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
else
  echo "==> Rust already installed: $(cargo --version)"
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env" 2>/dev/null || true
fi

# Ensure the wasm32 target is installed (Leptos/WASM frontend).
rustup target add wasm32-unknown-unknown 2>/dev/null || true

# --- 2. Linux system dependencies -----------------------------------------
if ! command -v apt-get >/dev/null 2>&1; then
  echo "ERROR: this bootstrap supports apt-based Linux (Ubuntu/Debian)." >&2
  echo "Tauri 2 needs: webkit2gtk-4.1, openssl, gtk3, librsvg." >&2
  echo "Install them with your package manager, then re-run bootstrap.sh." >&2
  exit 1
fi

echo "==> Installing build deps via apt-get (Ubuntu/Debian)…"
sudo apt-get update -y
sudo apt-get install -y \
  libwebkit2gtk-4.1-dev libssl-dev libgtk-3-dev libayatana-appindicator3-dev \
  librsvg2-dev build-essential curl wget file pkg-config

# --- 3. Tauri CLI + trunk (Leptos build tool) -----------------------------
if ! command -v tauri >/dev/null 2>&1; then
  echo "==> Installing Tauri CLI 2.x…"
  cargo install tauri-cli --version "^2.0" --locked --no-default-features
else
  echo "==> Tauri CLI already installed: $(tauri --version)"
fi

if ! command -v trunk >/dev/null 2>&1; then
  echo "==> Installing trunk (Leptos build tool)…"
  cargo install trunk --locked
else
  echo "==> trunk already installed."
fi

# --- 4. Build + launch ----------------------------------------------------
echo "==> Building SupportOS++…"
cargo xtask lint
cargo xtask test
cargo xtask dev
