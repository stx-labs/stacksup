//! `stacksup config check` answers "why isn't my stack working?".
//!
//! Config coherence is already enforced at load time (config::validate); this module checks the
//! *live* side: can every consumer actually reach its producers, and are they on the chain we think
//! they're on?

use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use anyhow::{Result, bail};
use colored::Colorize;

use crate::config::{Deployment, ServiceMode};
use crate::utils::services::*;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

struct Report {
    failures: u32,
}

impl Report {
    fn ok(&mut self, msg: &str) {
        println!("  {} {msg}", "✓".green());
    }
    fn fail(&mut self, msg: &str) {
        self.failures += 1;
        println!("{}", format!("  ✗ {msg}").red());
    }
    fn skip(&mut self, msg: &str) {
        println!("{}", format!("  - {msg}").dimmed());
    }
}

pub fn run(deployment: &Deployment) -> Result<()> {
    let mut r = Report { failures: 0 };

    println!("config");
    // load() already failed hard on errors; getting here means the config is coherent.
    r.ok("stacks.toml is valid and cross-service invariants hold");

    println!("\ndocker");
    if roster(deployment)
        .iter()
        .any(|(_, m)| *m == ServiceMode::Enabled)
    {
        match crate::utils::docker::daemon_version() {
            Ok(v) => r.ok(&format!("docker daemon reachable (server {v})")),
            Err(e) => r.fail(&e.to_string()),
        }
        match crate::utils::docker::compose_version() {
            Ok(v) => r.ok(&format!("docker compose plugin installed ({v})")),
            Err(e) => r.fail(&e.to_string()),
        }
    } else {
        r.skip("no managed services — docker not required");
    }

    println!("\nconnectivity");
    // External services are checked from the host at their configured port. Managed services are
    // probed via 127.0.0.1 at their published (offset) port, reachable there whether the mapping
    // binds loopback or all interfaces.
    let probe_port = |mode: ServiceMode, base: u16| match mode {
        ServiceMode::Enabled => deployment.published(base),
        _ => base,
    };
    check_tcp(
        &mut r,
        "bitcoind rpc",
        external_or_local(
            deployment.bitcoind.mode,
            deployment.bitcoind.host.as_deref(),
        ),
        probe_port(deployment.bitcoind.mode, bitcoind_rpc_port(deployment)),
    );
    check_tcp(
        &mut r,
        "stacks-node rpc",
        external_or_local(
            deployment.stacks_node.mode,
            deployment.stacks_node.rpc_host.as_deref(),
        ),
        probe_port(deployment.stacks_node.mode, node_rpc_port(deployment)),
    );
    check_tcp(
        &mut r,
        "postgres",
        external_or_local(
            deployment.postgres.mode,
            deployment.postgres.host.as_deref(),
        ),
        probe_port(deployment.postgres.mode, postgres_port(deployment)),
    );
    check_tcp(
        &mut r,
        "stacks-api",
        external_or_local(
            deployment.stacks_api.mode,
            deployment.stacks_api.host.as_deref(),
        ),
        probe_port(
            deployment.stacks_api.mode,
            deployment.stacks_api.port.unwrap_or(API_PORT),
        ),
    );

    println!("\nchain");
    check_node_info(&mut r, deployment);

    if r.failures > 0 {
        println!();
        bail!("config check found {} problem(s)", r.failures);
    }
    println!("\n{}", "All checks passed.".green());
    Ok(())
}

/// Where to reach a service from the host: its configured host when external, localhost when
/// enabled (published ports), None when disabled.
fn external_or_local(mode: ServiceMode, host: Option<&str>) -> Option<String> {
    match mode {
        ServiceMode::Enabled => Some("127.0.0.1".into()),
        ServiceMode::External => host.map(str::to_owned),
        ServiceMode::Disabled => None,
    }
}

fn check_tcp(r: &mut Report, label: &str, host: Option<String>, port: u16) {
    let Some(host) = host else {
        r.skip(&format!("{label}: disabled"));
        return;
    };
    let addr = format!("{host}:{port}");
    match addr.to_socket_addrs().ok().and_then(|mut a| a.next()) {
        Some(sock) => match TcpStream::connect_timeout(&sock, CONNECT_TIMEOUT) {
            Ok(_) => r.ok(&format!("{label}: reachable at {addr}")),
            Err(e) => r.fail(&format!(
                "{label}: cannot connect to {addr} ({e}) — is it running?"
            )),
        },
        None => r.fail(&format!("{label}: cannot resolve {addr}")),
    }
}

fn check_node_info(r: &mut Report, deployment: &Deployment) {
    let host = match deployment.stacks_node.mode {
        ServiceMode::Enabled => "127.0.0.1".to_string(),
        ServiceMode::External => match &deployment.stacks_node.rpc_host {
            Some(h) => h.clone(),
            None => return r.skip("stacks-node /v2/info: no rpc_host configured"),
        },
        ServiceMode::Disabled => return r.skip("stacks-node /v2/info: node is disabled"),
    };
    let port = match deployment.stacks_node.mode {
        // published (offset) port when we run the node; configured as-is when external
        ServiceMode::Enabled => deployment.published(node_rpc_port(deployment)),
        _ => node_rpc_port(deployment),
    };
    let url = format!("http://{host}:{port}/v2/info");
    match ureq::get(&url).timeout(CONNECT_TIMEOUT).call() {
        Ok(resp) => match resp.into_json::<serde_json::Value>() {
            Ok(info) => {
                let tip = info["stacks_tip_height"].as_u64().unwrap_or(0);
                let burn = info["burn_block_height"].as_u64().unwrap_or(0);
                r.ok(&format!(
                    "stacks-node /v2/info: stacks tip {tip}, burn height {burn}"
                ));
            }
            Err(e) => r.fail(&format!("stacks-node /v2/info: invalid response ({e})")),
        },
        Err(e) => r.fail(&format!(
            "stacks-node /v2/info: {e}. Node down or still booting?"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_or_local_follows_mode() {
        assert_eq!(
            external_or_local(ServiceMode::Enabled, None),
            Some("127.0.0.1".into())
        );
        assert_eq!(
            external_or_local(ServiceMode::External, Some("10.0.0.5")),
            Some("10.0.0.5".into())
        );
        assert_eq!(external_or_local(ServiceMode::External, None), None);
        assert_eq!(
            external_or_local(ServiceMode::Disabled, Some("ignored")),
            None
        );
    }
}
