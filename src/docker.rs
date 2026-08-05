//! Lifecycle: thin wrapper over `docker compose` for managed services.
//! Observation (status/health) will move to the Docker Engine API (bollard)
//! later; compose owns orchestration either way.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::config::{ServiceMode, Stack};
use crate::render::{COMPOSE_PROJECT, compose_file};
use crate::services::roster;

fn compose(data_dir: &Path) -> Command {
    let mut cmd = Command::new("docker");
    cmd.args(["compose", "-p", COMPOSE_PROJECT, "-f"]).arg(compose_file(data_dir));
    cmd
}

fn ensure_docker() -> Result<()> {
    let ok = Command::new("docker")
        .args(["version", "--format", "{{.Server.Version}}"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        bail!("docker daemon is not reachable — is Docker running?");
    }
    Ok(())
}

fn run(mut cmd: Command, what: &str) -> Result<()> {
    let status = cmd.status().with_context(|| format!("failed to run {what}"))?;
    if !status.success() {
        bail!("{what} exited with {status}");
    }
    Ok(())
}

pub fn up(stack: &Stack, data_dir: &Path) -> Result<()> {
    ensure_docker()?;

    let managed: Vec<_> =
        roster(stack).into_iter().filter(|(_, m)| *m == ServiceMode::Managed).collect();
    if managed.is_empty() {
        bail!("no services are set to mode = \"managed\" in stacks.toml — nothing to start");
    }

    println!("Starting {} managed service(s) on {}...", managed.len(), stack.network);
    for (name, mode) in roster(stack) {
        if mode == ServiceMode::External {
            println!("  {name}: external — not managed by this tool");
        }
    }

    let mut cmd = compose(data_dir);
    cmd.args(["up", "-d", "--remove-orphans"]);
    run(cmd, "docker compose up")?;

    println!("\nStack is starting. Follow along with `stacks status` or `stacks logs`.");
    Ok(())
}

pub fn down(stack: &Stack, data_dir: &Path) -> Result<()> {
    ensure_docker()?;
    // `down` only ever touches the compose project; external services and
    // their data are outside this tool's blast radius by construction.
    let mut cmd = compose(data_dir);
    cmd.arg("down");
    run(cmd, "docker compose down")?;
    for (name, mode) in roster(stack) {
        if mode == ServiceMode::External {
            println!("  {name}: external — left untouched");
        }
    }
    Ok(())
}

pub fn status(stack: &Stack, data_dir: &Path) -> Result<()> {
    ensure_docker()?;
    println!("network: {}\n", stack.network);
    for (name, mode) in roster(stack) {
        match mode {
            ServiceMode::External => println!("  {name}: external"),
            ServiceMode::Off => println!("  {name}: off"),
            ServiceMode::Managed => {} // shown by compose ps below
        }
    }
    println!();
    // TODO(hackathon): replace with bollard — health states, sync progress
    // (bitcoind headers, node tip vs peers, API ingest lag), --watch TUI.
    let mut cmd = compose(data_dir);
    cmd.arg("ps");
    run(cmd, "docker compose ps")
}

pub fn logs(_stack: &Stack, data_dir: &Path, service: Option<&str>) -> Result<()> {
    ensure_docker()?;
    let mut cmd = compose(data_dir);
    cmd.args(["logs", "--follow", "--tail", "100"]);
    if let Some(s) = service {
        cmd.arg(s);
    }
    run(cmd, "docker compose logs")
}
