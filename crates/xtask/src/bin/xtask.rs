//! `cargo xtask` — the single developer entry point for SupportOS++.
//!
//! Subcommands (the cargo equivalents of the reference's package.json
//! scripts — dev/test/lint/package + the black-box audit):
//!   - `dev`        Run the Tauri app in dev mode (Tauri + Leptos trunk serve)
//!   - `trunk-serve` Internal: serve the Leptos UI on 127.0.0.1:1420 (called by tauri.conf.json beforeDevCommand)
//!   - `trunk-build` Internal: build the Leptos UI into ../ui/dist (called by tauri.conf.json beforeBuildCommand)
//!   - `test`       Run all unit + integration tests across the workspace
//!   - `lint`       rustfmt --check + clippy -D warnings
//!   - `package`    Build installers for the host OS (tauri build)
//!   - `audit`      Run the black-box audit binary against a packaged app

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
    /// Black-box audit of a packaged app (reference scripts/audit-phase1.mjs counterpart).
    Audit {
        /// Path to a packaged app to audit.
        #[arg(long)]
        app: String,
    },
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
        Cmd::Audit { app } => run_audit(&app),
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

fn run_audit(app: &str) -> anyhow::Result<()> {
    // The audit binary lives in the same crate; re-run it as a child process.
    // Using the build artefact avoids re-compilation.
    let exe = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("could not locate current exe: {e}"))?;
    let dir = exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("exe has no parent dir"))?;
    let audit_path = dir.join("audit");
    if !audit_path.exists() {
        eprintln!("audit: binary not built; run `cargo build -p supportos-plusplus-xtask --bin audit` first");
        std::process::exit(1);
    }
    let status = Command::new(&audit_path).arg("--app").arg(app).status()?;
    anyhow::ensure!(status.success(), "audit binary exited with non-zero status");
    Ok(())
}
