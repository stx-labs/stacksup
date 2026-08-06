//! Per-service constants: images, ports, compose names.

use crate::config::{Network, ServiceMode, Stack};

// Image repositories and the default tag used when a service section has no `version`.
const REPO_BITCOIND: (&str, &str) = ("bitcoin/bitcoin", "latest");
const REPO_STACKS_NODE: (&str, &str) = ("ghcr.io/stacks-network/stacks-core", "latest");
const REPO_STACKS_SIGNER: (&str, &str) = ("ghcr.io/stacks-network/stacks-signer", "latest");
const REPO_STACKS_API: (&str, &str) = ("hirosystems/stacks-blockchain-api", "latest");
const REPO_STACKS_MESH_API: (&str, &str) = ("ghcr.io/stx-labs/stacks-mesh-api", "latest");
const REPO_POSTGRES: (&str, &str) = ("postgres", "latest");

fn image((repo, default_tag): (&str, &str), version: Option<&str>) -> String {
    format!("{repo}:{}", version.unwrap_or(default_tag))
}

pub fn bitcoind_image(stack: &Stack) -> String {
    image(REPO_BITCOIND, stack.bitcoind.version.as_deref())
}

pub fn stacks_node_image(stack: &Stack) -> String {
    image(REPO_STACKS_NODE, stack.stacks_node.version.as_deref())
}

pub fn stacks_signer_image(stack: &Stack) -> String {
    image(REPO_STACKS_SIGNER, stack.stacks_signer.version.as_deref())
}

pub fn stacks_api_image(stack: &Stack) -> String {
    image(REPO_STACKS_API, stack.stacks_api.version.as_deref())
}

pub fn stacks_mesh_api_image(stack: &Stack) -> String {
    image(REPO_STACKS_MESH_API, stack.stacks_mesh_api.version.as_deref())
}

pub fn postgres_image(stack: &Stack) -> String {
    image(REPO_POSTGRES, stack.postgres.version.as_deref())
}

pub const NODE_RPC_PORT: u16 = 20443;
pub const NODE_P2P_PORT: u16 = 20444;
pub const API_PORT: u16 = 3999;
pub const API_EVENT_PORT: u16 = 3700;
pub const MESH_API_PORT: u16 = 3998;
pub const SIGNER_ENDPOINT_PORT: u16 = 30000;
pub const POSTGRES_PORT: u16 = 5432;

pub fn bitcoind_rpc_port(network: Network) -> u16 {
    match network {
        Network::Mainnet => 8332,
        _ => 18332,
    }
}

pub fn bitcoind_p2p_port(network: Network) -> u16 {
    match network {
        Network::Mainnet => 8333,
        _ => 18333,
    }
}

/// Hostname of a service as seen by *managed* services: the compose DNS name
/// when managed, the user-supplied host when external.
pub fn bitcoind_host(stack: &Stack) -> Option<String> {
    match stack.bitcoind.mode {
        ServiceMode::Managed => Some("bitcoind".into()),
        ServiceMode::External => stack.bitcoind.host.clone(),
        ServiceMode::Off => None,
    }
}

pub fn node_rpc_host(stack: &Stack) -> Option<String> {
    match stack.stacks_node.mode {
        ServiceMode::Managed => Some("stacks-node".into()),
        ServiceMode::External => stack.stacks_node.rpc_host.clone(),
        ServiceMode::Off => None,
    }
}

pub fn node_rpc_port(stack: &Stack) -> u16 {
    stack.stacks_node.rpc_port.unwrap_or(NODE_RPC_PORT)
}

pub fn postgres_host(stack: &Stack) -> Option<String> {
    match stack.postgres.mode {
        ServiceMode::Managed => Some("postgres".into()),
        ServiceMode::External => stack.postgres.host.clone(),
        ServiceMode::Off => None,
    }
}

pub fn postgres_port(stack: &Stack) -> u16 {
    stack.postgres.port.unwrap_or(POSTGRES_PORT)
}

/// All services with their mode, for status/doctor display.
pub fn roster(stack: &Stack) -> Vec<(&'static str, ServiceMode)> {
    vec![
        ("bitcoind", stack.bitcoind.mode),
        ("stacks-node", stack.stacks_node.mode),
        ("stacks-signer", stack.stacks_signer.mode),
        ("stacks-api", stack.stacks_api.mode),
        ("stacks-mesh-api", stack.stacks_mesh_api.mode),
        ("postgres", stack.postgres.mode),
    ]
}
