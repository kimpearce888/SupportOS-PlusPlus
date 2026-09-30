# bootstrap.ps1 — silently install build prerequisites for SupportOS++, then build and launch.
# Safe to re-run.
#
# Supports: Windows 10/11 x64.
# macOS / Linux: use bootstrap.sh instead.

$ErrorActionPreference = "Stop"

Write-Host "==> SupportOS++ bootstrap (Windows)"

# --- 1. Rust toolchain ----------------------------------------------------
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Host "==> Installing Rust (stable) via rustup..."
    Invoke-WebRequest -Uri "https://win.rustup.rs/x86_64" -OutFile "$env:TEMP\rustup-init.exe"
    & "$env:TEMP\rustup-init.exe" -y --default-toolchain stable --profile default
    $env:Path += ";$env:USERPROFILE\.cargo\bin"
} else {
    Write-Host "==> Rust already installed: $(cargo --version)"
}

# Ensure the wasm32 target is installed (Leptos/WASM frontend).
rustup target add wasm32-unknown-unknown

# --- 2. Microsoft C++ Build Tools (required by rustc on Windows) ----------
$vsWhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
if (Test-Path $vsWhere) {
    $vsPath = & $vsWhere -latest -property installationPath 2>$null
    if ($vsPath) {
        Write-Host "==> Visual Studio Build Tools found at: $vsPath"
    } else {
        Write-Host "WARN: vswhere present but no VS installation detected. Install the 'Desktop development with C++' workload." 
    }
} else {
    Write-Host "==> Microsoft C++ Build Tools not found."
    Write-Host "    The Tauri 2 build on Windows requires the MSVC C++ build tools."
    Write-Host "    Install Visual Studio Build Tools 2022 with the 'Desktop development with C++' workload from:"
    Write-Host "    https://aka.ms/vs/17/release/vs_BuildTools.exe"
    Write-Host "    Then re-run bootstrap.ps1."
    exit 1
}

# WebView2 runtime: Tauri 2 bundles the embedBootstrapper, so no manual install is required.

# --- 3. Tauri CLI + trunk -------------------------------------------------
if (-not (Get-Command tauri -ErrorAction SilentlyContinue)) {
    Write-Host "==> Installing Tauri CLI 2.x..."
    cargo install tauri-cli --version "^2.0" --locked --no-default-features
} else {
    Write-Host "==> Tauri CLI already installed."
}

if (-not (Get-Command trunk -ErrorAction SilentlyContinue)) {
    Write-Host "==> Installing trunk (Leptos build tool)..."
    cargo install trunk --locked
} else {
    Write-Host "==> trunk already installed."
}

# --- 4. Build + launch ----------------------------------------------------
Write-Host "==> Building SupportOS++..."
cargo xtask lint
cargo xtask test
cargo xtask dev
