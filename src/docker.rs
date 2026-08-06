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

/// Docker Engine server version, distinguishing "not installed" from
/// "daemon not running" so the operator gets the right fix.
pub fn daemon_version() -> Result<String> {
    match Command::new("docker").args(["version", "--format", "{{.Server.Version}}"]).output() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!(
            "docker is not installed (or not on PATH) — see https://docs.docker.com/get-docker/"
        ),
        Err(e) => bail!("could not run docker: {e}"),
        Ok(o) if !o.status.success() => bail!(
            "docker is installed but the daemon is not reachable — is Docker running? ({})",
            String::from_utf8_lossy(&o.stderr).trim()
        ),
        Ok(o) => Ok(String::from_utf8_lossy(&o.stdout).trim().to_string()),
    }
}

/// Compose v2 plugin version ("docker compose" is a separate install from the engine).
pub fn compose_version() -> Result<String> {
    match Command::new("docker").args(["compose", "version", "--short"]).output() {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).trim().to_string()),
        _ => bail!(
            "the docker compose plugin is missing — see https://docs.docker.com/compose/install/"
        ),
    }
}

/// Gate for every command that touches docker: CLI present, daemon up, compose installed.
fn ensure_docker() -> Result<()> {
    daemon_version()?;
    compose_version()?;
    Ok(())
}

/// Names of this stack's currently running compose services. Best effort:
/// `None` when docker or the rendered compose file is unavailable.
pub fn running_services(data_dir: &Path) -> Option<Vec<String>> {
    if !compose_file(data_dir).exists() {
        return None;
    }
    let mut cmd = compose(data_dir);
    cmd.args(["ps", "--services", "--status", "running"]);
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_owned)
            .filter(|s| !s.is_empty())
            .collect(),
    )
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
