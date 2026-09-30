#!/usr/bin/env bash
# bootstrap.sh — silently install build prerequisites for SupportOS++, then build and launch.
# Safe to re-run.
#
# Supports: macOS (arm64, x86_64) and Linux (x86_64, aarch64).
# Windows: use bootstrap.ps1 instead.

set -euo pipefail

echo "==> SupportOS++ bootstrap (macOS / Linux)"

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

# --- 2. OS-specific system dependencies ------------------------------------
OS="$(uname -s)"
case "$OS" in
  Darwin)
    # Tauri 2 on macOS needs Xcode Command Line Tools.
    if ! xcode-select -p >/dev/null 2>&1; then
      echo "==> Installing Xcode Command Line Tools…"
      xcode-select --install || true
    fi
    ;;

  Linux)
    # Detect the distro family.
    if command -v apt-get >/dev/null 2>&1; then
      echo "==> Installing build deps via apt-get (Ubuntu/Debian)…"
      sudo apt-get update -y
      sudo apt-get install -y \
        libwebkit2gtk-4.1-dev libssl-dev libgtk-3-dev libayatana-appindicator3-dev \
        librsvg2-dev build-essential curl wget file pkg-config
    elif command -v dnf >/dev/null 2>&1; then
      echo "==> Installing build deps via dnf (Fedora)…"
      sudo dnf install -y \
        webkit2gtk4.1-devel openssl-devel gtk3-devel libappindicator-gtk3-devel \
        librsvg2-devel gcc gcc-c++ curl wget file pkgconfig
    elif command -v pacman >/dev/null 2>&1; then
      echo "==> Installing build deps via pacman (Arch)…"
      sudo pacman -S --noconfirm \
        webkit2gtk-4.1 openssl gtk3 libayatana-appindicator librsvg base-devel curl wget file pkgconf
    else
      echo "WARN: unsupported Linux distro. Tauri 2 needs: webkit2gtk-4.1, openssl, gtk3, librsvg." >&2
      echo "      Install them with your package manager, then re-run bootstrap.sh." >&2
    fi
    ;;

  *)
    echo "ERROR: unsupported OS ($OS). Use bootstrap.ps1 on Windows." >&2
    exit 1
    ;;
esac

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
