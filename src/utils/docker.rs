//! Lifecycle: thin wrapper over `docker compose` for managed services. Observation (status/health)
//! will move to the Docker Engine API (bollard) later; compose owns orchestration either way.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use colored::Colorize;

use crate::config::render::compose_file;
use crate::config::{Deployment, ServiceMode};
use crate::utils::services::roster;

fn compose(data_dir: &Path) -> Command {
    let mut cmd = Command::new("docker");
    // No -p: the rendered compose file carries its project via `name:`, so every invocation (ours
    // or a bare `docker compose -f ...`) is scoped to this deployment.
    cmd.args(["compose", "-f"]).arg(compose_file(data_dir));
    cmd
}

/// Docker Engine server version, distinguishing "not installed" from "daemon not running" so the
/// operator gets the right fix.
pub fn daemon_version() -> Result<String> {
    match Command::new("docker")
        .args(["version", "--format", "{{.Server.Version}}"])
        .output()
    {
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
    match Command::new("docker")
        .args(["compose", "version", "--short"])
        .output()
    {
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

/// Refuse to start when this deployment's name is already in use by a compose project rendered from
/// a DIFFERENT directory, otherwise compose silently adopts (and replaces) the other deployment's
/// containers.
fn guard_project_collision(deployment: &Deployment, data_dir: &Path) -> Result<()> {
    let out = Command::new("docker")
        .args(["compose", "ls", "--all", "--format", "json"])
        .output();
    let Ok(out) = out else { return Ok(()) }; // best effort
    let listing = String::from_utf8_lossy(&out.stdout);
    if let Some(other) = project_conflict(&listing, deployment.project(), &compose_file(data_dir)) {
        bail!(
            "deployment name `{}` is already in use by a stack rendered from {other} — set a \
             distinct `name` in stacks.toml (compose would otherwise adopt that stack's containers)",
            deployment.project()
        );
    }
    Ok(())
}

/// One row of `docker compose ls --format json`.
#[derive(serde::Deserialize)]
struct ComposeLsEntry {
    #[serde(rename = "Name")]
    name: String,
    /// Comma-separated compose file paths.
    #[serde(rename = "ConfigFiles", default)]
    config_files: String,
}

/// Pure half of the collision check: does `ls_json` (docker compose ls output) contain `project`
/// with a config file other than ours?
fn project_conflict(ls_json: &str, project: &str, our_compose_file: &Path) -> Option<String> {
    let ours = our_compose_file
        .canonicalize()
        .unwrap_or_else(|_| our_compose_file.to_path_buf());
    let entries: Vec<ComposeLsEntry> = serde_json::from_str(ls_json).ok()?;
    for e in entries {
        if e.name != project {
            continue;
        }
        // Any path matching ours means it's us.
        let is_ours = e.config_files.split(',').map(str::trim).any(|f| {
            Path::new(f)
                .canonicalize()
                .map(|p| p == ours)
                .unwrap_or(f == ours.to_string_lossy())
        });
        if !is_ours && !e.config_files.is_empty() {
            return Some(e.config_files);
        }
    }
    None
}

/// A port conflicts only when a bind fails with AddrInUse on SOME family, an unsupported family
/// (e.g. no IPv6), or a permission error is not a conflict. Mirrors how docker publishes on both
/// stacks.
fn port_taken(port: u16) -> bool {
    ["0.0.0.0", "::"]
        .iter()
        .any(|ip| match std::net::TcpListener::bind((*ip, port)) {
            Ok(_) => false,
            Err(e) => e.kind() == std::io::ErrorKind::AddrInUse,
        })
}

/// Test-bind every host port the deployment is about to publish, skipping services that are already
/// running (their ports are legitimately ours). Catches port squatting BEFORE compose creates half
/// a stack.
fn guard_published_ports(
    deployment: &Deployment,
    data_dir: &Path,
    only_service: Option<&str>,
) -> Result<()> {
    let running = running_services(data_dir).unwrap_or_default();
    for (service, port) in crate::utils::services::published_ports(deployment) {
        if running.iter().any(|r| r == service) {
            continue;
        }
        if let Some(only) = only_service
            && only != service
        {
            continue;
        }
        if port_taken(port) {
            bail!(
                "host port {port} (published by {service}) is already in use — another deployment \
             or process owns it; set or raise `port_offset` in stacks.toml, or stop \
             whatever holds the port"
            );
        }
    }
    Ok(())
}

/// Names of this stack's currently running compose services. Best effort: `None` when docker or the
/// rendered compose file is unavailable.
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

/// Start a single compose service (used by restores that need only postgres).
pub fn compose_up_service(data_dir: &Path, service: &str) -> Result<()> {
    let mut cmd = compose(data_dir);
    cmd.args(["up", "-d", service]);
    run(cmd, "docker compose up")
}

/// Stop a single compose service.
pub fn compose_stop_service(data_dir: &Path, service: &str) -> Result<()> {
    let mut cmd = compose(data_dir);
    cmd.args(["stop", service]);
    run(cmd, "docker compose stop")
}

/// Run a compose subcommand and capture its stdout (stderr appended on failure instead of erroring,
/// support bundles want best-effort output).
pub(crate) fn compose_capture(data_dir: &Path, args: &[&str]) -> Result<String> {
    let mut cmd = compose(data_dir);
    cmd.args(args);
    let out = cmd.output().context("failed to run docker compose")?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        text.push_str("\n[command failed]\n");
        text.push_str(&String::from_utf8_lossy(&out.stderr));
    }
    Ok(text)
}

/// Docker + rendered-compose preflight shared by commands that need both.
pub(crate) fn preflight(data_dir: &Path) -> Result<()> {
    ensure_docker()?;
    if !compose_file(data_dir).exists() {
        bail!(
            "no rendered configs at {} — run `stacksup config render` first",
            compose_file(data_dir).display()
        );
    }
    Ok(())
}

fn run(mut cmd: Command, what: &str) -> Result<()> {
    let status = cmd
        .status()
        .with_context(|| format!("failed to run {what}"))?;
    if !status.success() {
        bail!("{what} exited with {status}");
    }
    Ok(())
}

/// A service name is only startable/stoppable if it's enabled in stacks.toml.
pub(crate) fn ensure_enabled(deployment: &Deployment, name: &str) -> Result<()> {
    match roster(deployment).iter().find(|(n, _)| *n == name) {
        Some((_, ServiceMode::Enabled)) => Ok(()),
        Some((_, mode)) => bail!(
            "{name} is `{}` in stacks.toml — only enabled services can be started/stopped here",
            match mode {
                ServiceMode::External => "external",
                _ => "disabled",
            }
        ),
        None => {
            let enabled: Vec<&str> = roster(deployment)
                .into_iter()
                .filter(|(_, m)| *m == ServiceMode::Enabled)
                .map(|(n, _)| n)
                .collect();
            bail!(
                "unknown service `{name}` — enabled services: {}",
                enabled.join(", ")
            )
        }
    }
}

pub fn start(deployment: &Deployment, data_dir: &Path, service: Option<&str>) -> Result<()> {
    ensure_docker()?;
    if !compose_file(data_dir).exists() {
        bail!(
            "no rendered configs at {} — run `stacksup config render` first",
            compose_file(data_dir).display()
        );
    }

    guard_project_collision(deployment, data_dir)?;
    guard_published_ports(deployment, data_dir, service)?;

    if let Some(name) = service {
        ensure_enabled(deployment, name)?;
        println!("Starting {name} (and its dependencies)...");
        let mut cmd = compose(data_dir);
        cmd.args(["up", "-d", name]);
        run(cmd, "docker compose up")?;
        println!(
            "\n{name} is starting. Follow along with `stacksup status` or `stacksup logs {name}`."
        );
        return Ok(());
    }

    let managed: Vec<_> = roster(deployment)
        .into_iter()
        .filter(|(_, m)| *m == ServiceMode::Enabled)
        .collect();
    if managed.is_empty() {
        bail!("no services are set to mode = \"enabled\" in stacks.toml — nothing to start");
    }

    println!(
        "Starting {} managed service(s) on {}...",
        managed.len(),
        deployment.network
    );
    for (name, mode) in roster(deployment) {
        if mode == ServiceMode::External {
            println!(
                "{}",
                format!("  {name}: external — not managed by this tool").dimmed()
            );
        }
    }

    let mut cmd = compose(data_dir);
    cmd.args(["up", "-d", "--remove-orphans"]);
    run(cmd, "docker compose up")?;

    println!("\nStack is starting. Follow along with `stacksup status` or `stacksup logs`.");
    Ok(())
}

pub fn pull(deployment: &Deployment, data_dir: &Path) -> Result<()> {
    ensure_docker()?;
    let enabled: Vec<_> = roster(deployment)
        .into_iter()
        .filter(|(_, m)| *m == ServiceMode::Enabled)
        .collect();
    if enabled.is_empty() {
        bail!("no services are set to mode = \"enabled\" in stacks.toml — nothing to pull");
    }
    println!("Pulling images for {} enabled service(s):", enabled.len());
    for (name, _) in &enabled {
        println!("  {name}");
    }
    println!();
    let mut cmd = compose(data_dir);
    cmd.arg("pull");
    run(cmd, "docker compose pull")?;
    println!(
        "\n{} images up to date — restart with `stacksup stop && stacksup start` to run them",
        "✓".green()
    );
    Ok(())
}

pub fn stop(
    deployment: &Deployment,
    data_dir: &Path,
    service: Option<&str>,
    destroy: bool,
) -> Result<()> {
    ensure_docker()?;

    if let Some(name) = service {
        ensure_enabled(deployment, name)?;
        // Single service: `compose stop` halts just that container, leaving the rest of the stack
        // (and the network) running.
        let mut cmd = compose(data_dir);
        cmd.args(["stop", name]);
        run(cmd, "docker compose stop")?;
        println!("{name} stopped. `stacksup start {name}` to bring it back.");
        return Ok(());
    }

    if destroy {
        // `down` removes containers (and their logs) and the network. It only ever touches the
        // compose project; external services and their data are outside this tool's blast radius by
        // construction.
        let mut cmd = compose(data_dir);
        cmd.arg("down");
        run(cmd, "docker compose down")?;
    } else {
        // Halt containers but keep them (and their logs) so issues can still be inspected/exported
        // after stopping. `--destroy` for full cleanup.
        let mut cmd = compose(data_dir);
        cmd.arg("stop");
        run(cmd, "docker compose stop")?;
        println!(
            "{}",
            "Containers kept (logs still available via `stacksup logs`); use `stacksup stop --destroy` to remove them."
                .dimmed()
        );
    }
    for (name, mode) in roster(deployment) {
        if mode == ServiceMode::External {
            println!(
                "{}",
                format!("  {name}: external — left untouched").dimmed()
            );
        }
    }
    Ok(())
}

/// Restart = stop + up. Deliberately not `docker compose restart`, which reuses the existing
/// container: going through `up` means a re-rendered config or freshly pulled image takes effect on
/// restart.
pub fn restart(deployment: &Deployment, data_dir: &Path, service: Option<&str>) -> Result<()> {
    preflight(data_dir)?;

    if let Some(name) = service {
        ensure_enabled(deployment, name)?;
        println!("Restarting {name}...");
        let mut cmd = compose(data_dir);
        cmd.args(["stop", name]);
        run(cmd, "docker compose stop")?;
        let mut cmd = compose(data_dir);
        cmd.args(["up", "-d", name]);
        run(cmd, "docker compose up")?;
        println!("\n{name} restarted. Follow along with `stacksup logs {name}`.");
        return Ok(());
    }

    let enabled: Vec<_> = roster(deployment)
        .into_iter()
        .filter(|(_, m)| *m == ServiceMode::Enabled)
        .collect();
    if enabled.is_empty() {
        bail!("no services are set to mode = \"enabled\" in stacks.toml — nothing to restart");
    }
    println!("Restarting {} service(s)...", enabled.len());
    let mut cmd = compose(data_dir);
    cmd.arg("stop");
    run(cmd, "docker compose stop")?;
    let mut cmd = compose(data_dir);
    cmd.args(["up", "-d", "--remove-orphans"]);
    run(cmd, "docker compose up")?;
    println!("\nStack restarted. Follow along with `stacksup status` or `stacksup logs`.");
    Ok(())
}

pub fn status(deployment: &Deployment, data_dir: &Path) -> Result<()> {
    ensure_docker()?;
    println!("network: {}\n", deployment.network);
    for (name, mode) in roster(deployment) {
        match mode {
            ServiceMode::External => println!("  {name}: external"),
            ServiceMode::Disabled => println!("  {name}: disabled"),
            ServiceMode::Enabled => {} // shown by compose ps below
        }
    }
    println!();
    // TODO(hackathon): replace with bollard — health states, sync progress
    // (bitcoind headers, node tip vs peers, API ingest lag), --watch TUI.
    let mut cmd = compose(data_dir);
    cmd.arg("ps");
    run(cmd, "docker compose ps")
}

pub fn logs(_stack: &Deployment, data_dir: &Path, service: Option<&str>) -> Result<()> {
    ensure_docker()?;
    let mut cmd = compose(data_dir);
    cmd.args(["logs", "--follow", "--tail", "100"]);
    if let Some(s) = service {
        cmd.arg(s);
    }
    run(cmd, "docker compose logs")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deployment(toml_str: &str) -> Deployment {
        crate::config::test_deployment(toml_str)
    }

    #[test]
    fn compose_targets_project_and_rendered_file() {
        let cmd = compose(Path::new("/data"));
        assert_eq!(cmd.get_program(), "docker");
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args[..2], ["compose", "-f"]);
        assert!(args[2].ends_with("rendered/docker-compose.yml"));
        assert!(args[2].starts_with("/data"));
    }

    #[test]
    fn project_conflict_flags_other_directories_only() {
        let ours = Path::new("/data/rendered/docker-compose.yml");
        let ls = r#"[
            {"Name": "stacks", "Status": "running(2)", "ConfigFiles": "/other/rendered/docker-compose.yml"},
            {"Name": "testnet-b", "Status": "running(1)", "ConfigFiles": "/data/rendered/docker-compose.yml"}
        ]"#;
        // same name, different directory -> conflict
        assert_eq!(
            project_conflict(ls, "stacks", ours).as_deref(),
            Some("/other/rendered/docker-compose.yml")
        );
        // same name, same file -> that's us, no conflict
        assert!(project_conflict(ls, "testnet-b", ours).is_none());
        // name not present -> no conflict
        assert!(project_conflict(ls, "mainnet-c", ours).is_none());
        // garbage json -> best effort, no conflict
        assert!(project_conflict("not json", "stacks", ours).is_none());
    }

    #[test]
    fn guard_published_ports_reports_taken_port() {
        // hold a port, then ask the guard about a deployment that publishes
        // it. Ephemeral ports are virtually always >= 32768, but re-roll to
        // guarantee `taken - POSTGRES_PORT` can't underflow anywhere.
        let (listener, taken) = loop {
            let l = std::net::TcpListener::bind(("0.0.0.0", 0)).unwrap();
            let p = l.local_addr().unwrap().port();
            if p >= crate::utils::services::POSTGRES_PORT {
                break (l, p);
            }
        };
        let mut d = deployment("network = \"testnet\"\n[postgres]\nmode = \"enabled\"");
        // shift postgres (5432) onto the taken port
        d.port_offset = taken - crate::utils::services::POSTGRES_PORT;
        // data_dir without a compose file -> no services considered running
        let err = guard_published_ports(&d, Path::new("/nonexistent"), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains(&format!("host port {taken}")), "got: {err}");
        assert!(err.contains("port_offset"));
        // a free port passes
        drop(listener);
        assert!(guard_published_ports(&d, Path::new("/nonexistent"), None).is_ok());
    }

    #[test]
    fn ensure_enabled_accepts_enabled_services() {
        let s = deployment("network = \"testnet\"\n[postgres]\nmode = \"enabled\"");
        assert!(ensure_enabled(&s, "postgres").is_ok());
    }

    #[test]
    fn ensure_enabled_rejects_external_and_disabled() {
        let s = deployment("network = \"testnet\"\n[postgres]\nmode = \"external\"\nhost = \"pg\"");
        let err = ensure_enabled(&s, "postgres").unwrap_err().to_string();
        assert!(err.contains("external"));
        let err = ensure_enabled(&s, "stacks-node").unwrap_err().to_string();
        assert!(err.contains("disabled"));
    }

    #[test]
    fn ensure_enabled_unknown_lists_enabled_services() {
        let s = deployment("network = \"testnet\"\n[postgres]\nmode = \"enabled\"");
        let err = ensure_enabled(&s, "nonsense").unwrap_err().to_string();
        assert!(err.contains("unknown service `nonsense`"));
        assert!(err.contains("postgres"));
    }
}
