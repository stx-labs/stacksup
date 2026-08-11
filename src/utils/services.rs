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
    image(
        REPO_STACKS_MESH_API,
        stack.stacks_mesh_api.version.as_deref(),
    )
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

// Non-mainnet networks are bitcoin *regtest* (krypton testnet follows the
// Hiro-hosted regtest), hence the 18443/18444 regtest ports.
pub fn bitcoind_rpc_port(network: Network) -> u16 {
    match network {
        Network::Mainnet => 8332,
        _ => 18443,
    }
}

pub fn bitcoind_p2p_port(network: Network) -> u16 {
    match network {
        Network::Mainnet => 8333,
        _ => 18444,
    }
}

/// Hostname of a service as seen by *managed* services: the compose DNS name
/// when managed, the user-supplied host when external.
pub fn bitcoind_host(stack: &Stack) -> Option<String> {
    match stack.bitcoind.mode {
        ServiceMode::Enabled => Some("bitcoind".into()),
        ServiceMode::External => stack.bitcoind.host.clone(),
        ServiceMode::Disabled => None,
    }
}

pub fn node_rpc_host(stack: &Stack) -> Option<String> {
    match stack.stacks_node.mode {
        ServiceMode::Enabled => Some("stacks-node".into()),
        ServiceMode::External => stack.stacks_node.rpc_host.clone(),
        ServiceMode::Disabled => None,
    }
}

pub fn node_rpc_port(stack: &Stack) -> u16 {
    stack.stacks_node.rpc_port.unwrap_or(NODE_RPC_PORT)
}

pub fn postgres_host(stack: &Stack) -> Option<String> {
    match stack.postgres.mode {
        ServiceMode::Enabled => Some("postgres".into()),
        ServiceMode::External => stack.postgres.host.clone(),
        ServiceMode::Disabled => None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Stack;

    fn stack(toml_str: &str) -> Stack {
        toml::from_str(toml_str).expect("test stack.toml should parse")
    }

    #[test]
    fn image_uses_default_tag_when_version_unset() {
        let s = stack("network = \"testnet\"");
        assert!(stacks_node_image(&s).ends_with(":latest"));
        assert!(postgres_image(&s).starts_with("postgres:"));
    }

    #[test]
    fn image_uses_configured_version() {
        let s = stack(
            "network = \"testnet\"\n[stacks-node]\nversion = \"4.0.1\"\n[postgres]\nversion = \"17\"",
        );
        assert!(stacks_node_image(&s).ends_with(":4.0.1"));
        assert_eq!(postgres_image(&s), "postgres:17");
    }

    #[test]
    fn bitcoind_ports_per_network() {
        assert_eq!(bitcoind_rpc_port(Network::Mainnet), 8332);
        assert_eq!(bitcoind_p2p_port(Network::Mainnet), 8333);
        // non-mainnet is the Hiro-hosted regtest, hence regtest ports
        assert_eq!(bitcoind_rpc_port(Network::Testnet), 18443);
        assert_eq!(bitcoind_p2p_port(Network::Testnet), 18444);
    }

    #[test]
    fn hosts_follow_service_mode() {
        let s = stack(
            "network = \"mainnet\"\n[bitcoind]\nmode = \"enabled\"\n[stacks-node]\nmode = \"external\"\nrpc_host = \"10.0.1.6\"",
        );
        assert_eq!(bitcoind_host(&s), Some("bitcoind".into()));
        assert_eq!(node_rpc_host(&s), Some("10.0.1.6".into()));
        assert_eq!(postgres_host(&s), None); // disabled by default
    }

    #[test]
    fn port_overrides_apply() {
        let s = stack(
            "network = \"testnet\"\n[stacks-node]\nmode = \"external\"\nrpc_host = \"h\"\nrpc_port = 30443\n[postgres]\nmode = \"enabled\"",
        );
        assert_eq!(node_rpc_port(&s), 30443);
        assert_eq!(postgres_port(&s), POSTGRES_PORT);
    }

    #[test]
    fn roster_lists_all_services_with_modes() {
        use crate::config::ServiceMode;
        let s = stack("network = \"testnet\"\n[postgres]\nmode = \"enabled\"");
        let roster = roster(&s);
        assert_eq!(roster.len(), 6);
        let (name, mode) = roster.iter().find(|(n, _)| *n == "postgres").unwrap();
        assert_eq!(*name, "postgres");
        assert_eq!(*mode, ServiceMode::Enabled);
        assert!(roster.iter().any(|(n, _)| *n == "stacks-mesh-api"));
    }
}
