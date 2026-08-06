//! `stacks doctor` — answers "why isn't my stack working?".
//!
//! Config coherence is already enforced at load time (config::validate); this
//! module checks the *live* side: can every consumer actually reach its
//! producers, and are they on the chain we think they're on?

use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use anyhow::{Result, bail};

use crate::config::{ServiceMode, Stack};
use crate::services::*;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

struct Report {
    failures: u32,
}

impl Report {
    fn ok(&mut self, msg: &str) {
        println!("  ✓ {msg}");
    }
    fn fail(&mut self, msg: &str) {
        self.failures += 1;
        println!("  ✗ {msg}");
    }
    fn skip(&mut self, msg: &str) {
        println!("  - {msg}");
    }
}

pub fn run(stack: &Stack) -> Result<()> {
    let mut r = Report { failures: 0 };

    println!("config");
    // load() already failed hard on errors; getting here means the config is coherent.
    r.ok("stacks.toml is valid and cross-service invariants hold");

    println!("\ndocker");
    if roster(stack).iter().any(|(_, m)| *m == ServiceMode::Managed) {
        match crate::docker::daemon_version() {
            Ok(v) => r.ok(&format!("docker daemon reachable (server {v})")),
            Err(e) => r.fail(&e.to_string()),
        }
        match crate::docker::compose_version() {
            Ok(v) => r.ok(&format!("docker compose plugin installed ({v})")),
            Err(e) => r.fail(&e.to_string()),
        }
    } else {
        r.skip("no managed services — docker not required");
    }

    println!("\nconnectivity");
    // External services are checked from the host. Managed services publish
    // their ports on localhost, so they are checkable the same way once up.
    check_tcp(&mut r, "bitcoind rpc", external_or_local(stack.bitcoind.mode, stack.bitcoind.host.as_deref()), stack.bitcoind.rpc_port.unwrap_or(bitcoind_rpc_port(stack.network)));
    check_tcp(&mut r, "stacks-node rpc", external_or_local(stack.stacks_node.mode, stack.stacks_node.rpc_host.as_deref()), node_rpc_port(stack));
    check_tcp(&mut r, "postgres", external_or_local(stack.postgres.mode, stack.postgres.host.as_deref()), postgres_port(stack));
    check_tcp(&mut r, "stacks-api", external_or_local(stack.stacks_api.mode, stack.stacks_api.host.as_deref()), stack.stacks_api.port.unwrap_or(API_PORT));

    println!("\nchain");
    check_node_info(&mut r, stack);
    // TODO(hackathon): the checks that catch the silent failure modes —
    //  - bitcoind getblockchaininfo: chain matches stacks.toml network
    //  - API /extended chain tip vs node /v2/info tip (event stream actually flowing)
    //  - node /v3/health difference_from_max_peer (sync lag vs peers)

    if r.failures > 0 {
        println!();
        bail!("doctor found {} problem(s)", r.failures);
    }
    println!("\nAll checks passed.");
    Ok(())
}

/// Where to reach a service from the host: its configured host when external,
/// localhost when managed (published ports), None when off.
fn external_or_local(mode: ServiceMode, host: Option<&str>) -> Option<String> {
    match mode {
        ServiceMode::Managed => Some("127.0.0.1".into()),
        ServiceMode::External => host.map(str::to_owned),
        ServiceMode::Off => None,
    }
}

fn check_tcp(r: &mut Report, label: &str, host: Option<String>, port: u16) {
    let Some(host) = host else {
        r.skip(&format!("{label}: off"));
        return;
    };
    let addr = format!("{host}:{port}");
    match addr.to_socket_addrs().ok().and_then(|mut a| a.next()) {
        Some(sock) => match TcpStream::connect_timeout(&sock, CONNECT_TIMEOUT) {
            Ok(_) => r.ok(&format!("{label}: reachable at {addr}")),
            Err(e) => r.fail(&format!("{label}: cannot connect to {addr} ({e}) — is it running?")),
        },
        None => r.fail(&format!("{label}: cannot resolve {addr}")),
    }
}

fn check_node_info(r: &mut Report, stack: &Stack) {
    let host = match stack.stacks_node.mode {
        ServiceMode::Managed => "127.0.0.1".to_string(),
        ServiceMode::External => match &stack.stacks_node.rpc_host {
            Some(h) => h.clone(),
            None => return r.skip("stacks-node /v2/info: no rpc_host configured"),
        },
        ServiceMode::Off => return r.skip("stacks-node /v2/info: node is off"),
    };
    let url = format!("http://{host}:{}/v2/info", node_rpc_port(stack));
    match ureq::get(&url).timeout(CONNECT_TIMEOUT).call() {
        Ok(resp) => match resp.into_json::<serde_json::Value>() {
            Ok(info) => {
                let tip = info["stacks_tip_height"].as_u64().unwrap_or(0);
                let burn = info["burn_block_height"].as_u64().unwrap_or(0);
                r.ok(&format!("stacks-node /v2/info: stacks tip {tip}, burn height {burn}"));
            }
            Err(e) => r.fail(&format!("stacks-node /v2/info: invalid response ({e})")),
        },
        Err(e) => r.fail(&format!("stacks-node /v2/info: {e} — node down or still booting?")),
    }
}
