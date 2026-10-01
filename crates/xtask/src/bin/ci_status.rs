//! CI status checker — queries GitHub Actions API for run status.
//!
//! Usage: `cargo xtask ci-status [branch]`
//! Default branch: main
//!
//! Requires GH_TOKEN environment variable (or reads from ~/.git-credentials).

use anyhow::Result;
use serde::Deserialize;

pub fn run(branch: &str) -> Result<()> {
    let token = get_token()?;
    let runs = get_runs(&token, branch)?;
    if runs.is_empty() {
        println!("No CI runs found on branch '{branch}'.");
        return Ok(());
    }
    let latest = &runs[0];
    println!("Latest run on '{branch}':");
    println!("  ID:         {}", latest.id);
    println!("  Status:     {}", latest.status);
    println!(
        "  Conclusion: {}",
        latest.conclusion.as_deref().unwrap_or("(none)")
    );
    println!("  SHA:        {}", &latest.head_sha[..7]);
    println!(
        "  Message:    {}",
        latest
            .head_commit
            .message
            .lines()
            .next()
            .unwrap_or("(none)")
    );
    println!("  URL:        {}", latest.html_url);
    println!();

    // Get jobs for this run.
    let jobs = get_jobs(&token, latest.id)?;
    println!("Jobs:");
    for job in &jobs {
        let conclusion = job.conclusion.as_deref().unwrap_or("(in progress)");
        println!("  {:50} | {:12} | {}", job.name, job.status, conclusion);
        // Print failed steps.
        for step in &job.steps {
            if step.conclusion.as_deref() == Some("failure") {
                println!("    ❌ FAIL: {}", step.name);
            }
        }
    }

    Ok(())
}

fn get_token() -> Result<String> {
    // Try env var first.
    if let Ok(token) = std::env::var("GH_TOKEN").or_else(|_| std::env::var("GITHUB_TOKEN")) {
        if !token.is_empty() {
            return Ok(token);
        }
    }
    // Try ~/.git-credentials.
    let home = std::env::var("HOME").unwrap_or_default();
    let creds_path = format!("{home}/.git-credentials");
    if let Ok(content) = std::fs::read_to_string(&creds_path) {
        for line in content.lines() {
            // Format: https://username:token@host
            if let Some((_, rest)) = line.split_once("://") {
                if let Some((_, token_host)) = rest.split_once(':') {
                    if let Some((token, _)) = token_host.split_once('@') {
                        if token.starts_with("ghp_") || token.starts_with("github_pat_") {
                            return Ok(token.to_string());
                        }
                    }
                }
            }
        }
    }
    anyhow::bail!("No GitHub token found. Set GH_TOKEN or configure ~/.git-credentials.")
}

fn get_runs(token: &str, branch: &str) -> Result<Vec<WorkflowRun>> {
    let url = format!(
        "https://api.github.com/repos/kimpearce888/SupportOS-PlusPlus/actions/runs?branch={branch}&per_page=5"
    );
    let output = std::process::Command::new("curl")
        .args(["-s", "-H", &format!("Authorization: token {token}"), &url])
        .output()?;
    let json: RunsResponse = serde_json::from_slice(&output.stdout)?;
    Ok(json.workflow_runs)
}

fn get_jobs(token: &str, run_id: u64) -> Result<Vec<Job>> {
    let url = format!(
        "https://api.github.com/repos/kimpearce888/SupportOS-PlusPlus/actions/runs/{run_id}/jobs"
    );
    let output = std::process::Command::new("curl")
        .args(["-s", "-H", &format!("Authorization: token {token}"), &url])
        .output()?;
    let json: JobsResponse = serde_json::from_slice(&output.stdout)?;
    Ok(json.jobs)
}

#[derive(Deserialize)]
struct RunsResponse {
    workflow_runs: Vec<WorkflowRun>,
}

#[derive(Deserialize)]
struct WorkflowRun {
    id: u64,
    status: String,
    conclusion: Option<String>,
    head_sha: String,
    html_url: String,
    head_commit: HeadCommit,
}

#[derive(Deserialize)]
struct HeadCommit {
    message: String,
}

#[derive(Deserialize)]
struct JobsResponse {
    jobs: Vec<Job>,
}

#[derive(Deserialize)]
struct Job {
    name: String,
    status: String,
    conclusion: Option<String>,
    steps: Vec<Step>,
}

#[derive(Deserialize)]
struct Step {
    name: String,
    conclusion: Option<String>,
}

#[allow(dead_code)]
fn main() -> anyhow::Result<()> {
    let branch = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "main".to_string());
    run(&branch)
}
