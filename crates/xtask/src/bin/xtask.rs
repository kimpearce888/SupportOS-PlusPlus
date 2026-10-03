//! `cargo xtask` — the single developer entry point for SupportOS++.
//!
//! Subcommands (the cargo equivalents of the reference's package.json
//! scripts — dev/test/lint/package):
//!   - `dev`        Run the Tauri app in dev mode (Tauri + Leptos trunk serve)
//!   - `trunk-serve` Internal: serve the Leptos UI on 127.0.0.1:1420 (called by tauri.conf.json beforeDevCommand)
//!   - `trunk-build` Internal: build the Leptos UI into ../ui/dist (called by tauri.conf.json beforeBuildCommand)
//!   - `test`       Run all unit + integration tests across the workspace
//!   - `lint`       rustfmt --check + clippy -D warnings
//!   - `package`    Build installers for the host OS (tauri build)

use std::process::Command;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "xtask", version, about = "SupportOS++ developer entry point", long_about = None)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the Tauri app in dev mode.
    Dev,
    /// Internal: serve the Leptos UI for the Tauri dev shell.
    TrunkServe,
    /// Internal: build the Leptos UI into ../ui/dist.
    TrunkBuild,
    /// Run all unit + integration tests across the workspace.
    Test,
    /// Run rustfmt --check + clippy -D warnings.
    Lint,
    /// Build installers for the host OS via Tauri.
    Package,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Dev => run_dev(),
        Cmd::TrunkServe => run_trunk("serve"),
        Cmd::TrunkBuild => run_trunk("build"),
        Cmd::Test => run_tests(),
        Cmd::Lint => run_lint(),
        Cmd::Package => run_package(),
    }
}

fn workspace_root() -> std::path::PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR")
        .unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").to_string());
    let manifest_dir = std::path::PathBuf::from(manifest);
    // crates/xtask → workspace root is two levels up.
    manifest_dir
        .ancestors()
        .nth(2)
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap())
}

fn run_dev() -> anyhow::Result<()> {
    // tauri dev runs the beforeDevCommand (cargo xtask trunk-serve) and the Tauri shell.
    let tauri_dir = workspace_root()
        .join("crates")
        .join("app")
        .join("src-tauri");
    let status = Command::new("cargo")
        .arg("tauri")
        .arg("dev")
        .current_dir(tauri_dir)
        .status()?;
    anyhow::ensure!(status.success(), "cargo tauri dev failed");
    Ok(())
}

fn run_trunk(mode: &str) -> anyhow::Result<()> {
    let ui_dir = workspace_root().join("crates").join("ui");
    let status = Command::new("trunk")
        .arg(mode)
        .current_dir(ui_dir)
        .status()?;
    anyhow::ensure!(status.success(), "trunk {mode} failed");
    Ok(())
}

fn run_tests() -> anyhow::Result<()> {
    let root = workspace_root();
    let status = Command::new("cargo")
        .args(["test", "--workspace", "--all-targets"])
        .current_dir(&root)
        .status()?;
    anyhow::ensure!(status.success(), "cargo test failed");
    Ok(())
}

fn run_lint() -> anyhow::Result<()> {
    let root = workspace_root();

    // rustfmt --check across the workspace.
    let fmt_status = Command::new("cargo")
        .args(["fmt", "--all", "--", "--check"])
        .current_dir(&root)
        .status()?;
    anyhow::ensure!(fmt_status.success(), "rustfmt --check failed");

    // clippy -D warnings across the workspace.
    let clippy_status = Command::new("cargo")
        .args([
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ])
        .current_dir(&root)
        .status()?;
    anyhow::ensure!(clippy_status.success(), "clippy -D warnings failed");

    Ok(())
}

fn run_package() -> anyhow::Result<()> {
    let tauri_dir = workspace_root()
        .join("crates")
        .join("app")
        .join("src-tauri");
    let status = Command::new("cargo")
        .arg("tauri")
        .arg("build")
        .current_dir(tauri_dir)
        .status()?;
    anyhow::ensure!(status.success(), "cargo tauri build failed");
    Ok(())
}
