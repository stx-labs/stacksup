//! Per-service constants: images, ports, compose names.

use crate::config::{Deployment, ServiceMode};

// Image repositories and the default tag used when a service section has no `version`.
const REPO_BITCOIND: (&str, &str) = ("bitcoin/bitcoin", "latest");
const REPO_STACKS_NODE: (&str, &str) = ("ghcr.io/stacks-network/stacks-core", "latest");
const REPO_STACKS_SIGNER: (&str, &str) = ("ghcr.io/stacks-network/stacks-signer", "latest");
const REPO_STACKS_API: (&str, &str) = ("hirosystems/stacks-blockchain-api", "latest");
const REPO_STACKS_MESH_API: (&str, &str) = ("ghcr.io/stx-labs/stacks-mesh-api", "latest");
const REPO_POSTGRES: (&str, &str) = ("postgres", "latest");

fn image(
    (default_repo, default_tag): (&str, &str),
    image_override: Option<&str>,
    version: Option<&str>,
) -> String {
    match image_override {
        // Full ref with its own tag: used verbatim (validation rejects a
        // conflicting `version`).
        Some(full) if crate::utils::versions::has_explicit_tag(full) => full.to_string(),
        // Repository override: tag still comes from `version`/the default.
        Some(repo) => format!("{repo}:{}", version.unwrap_or(default_tag)),
        None => format!("{default_repo}:{}", version.unwrap_or(default_tag)),
    }
}

pub fn bitcoind_image(deployment: &Deployment) -> String {
    image(
        REPO_BITCOIND,
        deployment.bitcoind.image.as_deref(),
        deployment.bitcoind.version.as_deref(),
    )
}

pub fn stacks_node_image(deployment: &Deployment) -> String {
    image(
        REPO_STACKS_NODE,
        deployment.stacks_node.image.as_deref(),
        deployment.stacks_node.version.as_deref(),
    )
}

pub fn stacks_signer_image(deployment: &Deployment) -> String {
    image(
        REPO_STACKS_SIGNER,
        deployment.stacks_signer.image.as_deref(),
        deployment.stacks_signer.version.as_deref(),
    )
}

pub fn stacks_api_image(deployment: &Deployment) -> String {
    image(
        REPO_STACKS_API,
        deployment.stacks_api.image.as_deref(),
        deployment.stacks_api.version.as_deref(),
    )
}

pub fn stacks_mesh_api_image(deployment: &Deployment) -> String {
    image(
        REPO_STACKS_MESH_API,
        deployment.stacks_mesh_api.image.as_deref(),
        deployment.stacks_mesh_api.version.as_deref(),
    )
}

pub fn postgres_image(deployment: &Deployment) -> String {
    image(
        REPO_POSTGRES,
        deployment.postgres.image.as_deref(),
        deployment.postgres.version.as_deref(),
    )
}

pub const NODE_RPC_PORT: u16 = 20443;
pub const NODE_P2P_PORT: u16 = 20444;
pub const API_PORT: u16 = 3999;
pub const API_EVENT_PORT: u16 = 3700;
pub const MESH_API_PORT: u16 = 3998;
pub const SIGNER_ENDPOINT_PORT: u16 = 30000;
pub const POSTGRES_PORT: u16 = 5432;

/// bitcoind ports: per-stack override, else the network definition's default.
pub fn bitcoind_rpc_port(deployment: &Deployment) -> u16 {
    deployment
        .bitcoind
        .rpc_port
        .unwrap_or(deployment.net.bitcoind.rpc_port)
}

pub fn bitcoind_p2p_port(deployment: &Deployment) -> u16 {
    deployment
        .bitcoind
        .p2p_port
        .unwrap_or(deployment.net.bitcoind.p2p_port)
}

/// Hostname of a service as seen by *managed* services: the compose DNS name
/// when managed, the user-supplied host when external.
pub fn bitcoind_host(deployment: &Deployment) -> Option<String> {
    match deployment.bitcoind.mode {
        ServiceMode::Enabled => Some("bitcoind".into()),
        ServiceMode::External => deployment.bitcoind.host.clone(),
        ServiceMode::Disabled => None,
    }
}

pub fn node_rpc_host(deployment: &Deployment) -> Option<String> {
    match deployment.stacks_node.mode {
        ServiceMode::Enabled => Some("stacks-node".into()),
        ServiceMode::External => deployment.stacks_node.rpc_host.clone(),
        ServiceMode::Disabled => None,
    }
}

pub fn node_rpc_port(deployment: &Deployment) -> u16 {
    deployment.stacks_node.rpc_port.unwrap_or(NODE_RPC_PORT)
}

pub fn postgres_host(deployment: &Deployment) -> Option<String> {
    match deployment.postgres.mode {
        ServiceMode::Enabled => Some("postgres".into()),
        ServiceMode::External => deployment.postgres.host.clone(),
        ServiceMode::Disabled => None,
    }
}

pub fn postgres_port(deployment: &Deployment) -> u16 {
    deployment.postgres.port.unwrap_or(POSTGRES_PORT)
}

/// All services with their mode, for status/doctor display.
pub fn roster(deployment: &Deployment) -> Vec<(&'static str, ServiceMode)> {
    vec![
        ("bitcoind", deployment.bitcoind.mode),
        ("stacks-node", deployment.stacks_node.mode),
        ("stacks-signer", deployment.stacks_signer.mode),
        ("stacks-api", deployment.stacks_api.mode),
        ("stacks-mesh-api", deployment.stacks_mesh_api.mode),
        ("postgres", deployment.postgres.mode),
    ]
}

/// Every (service, host port) this deployment publishes when started —
/// the base ports shifted by `port_offset`. Used by the start preflight to
/// test-bind before compose does.
pub fn published_ports(deployment: &Deployment) -> Vec<(&'static str, u16)> {
    let mut ports = Vec::new();
    if deployment.bitcoind.mode == ServiceMode::Enabled {
        ports.push((
            "bitcoind",
            deployment.published(bitcoind_rpc_port(deployment)),
        ));
        ports.push((
            "bitcoind",
            deployment.published(bitcoind_p2p_port(deployment)),
        ));
    }
    if deployment.stacks_node.mode == ServiceMode::Enabled {
        ports.push(("stacks-node", deployment.published(NODE_RPC_PORT)));
        ports.push(("stacks-node", deployment.published(NODE_P2P_PORT)));
    }
    if deployment.stacks_api.mode == ServiceMode::Enabled {
        ports.push(("stacks-api", deployment.published(API_PORT)));
    }
    if deployment.stacks_mesh_api.mode == ServiceMode::Enabled {
        ports.push(("stacks-mesh-api", deployment.published(MESH_API_PORT)));
    }
    if deployment.postgres.mode == ServiceMode::Enabled {
        ports.push(("postgres", deployment.published(POSTGRES_PORT)));
    }
    ports
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Deployment;

    fn deployment(toml_str: &str) -> Deployment {
        crate::config::test_deployment(toml_str)
    }

    #[test]
    fn image_uses_default_tag_when_version_unset() {
        let s = deployment("network = \"testnet\"");
        assert!(stacks_node_image(&s).ends_with(":latest"));
        assert!(postgres_image(&s).starts_with("postgres:"));
    }

    #[test]
    fn image_override_resolution() {
        // repo-only override + version -> override repo with version tag
        let d =
            deployment("network = \"testnet\"\n[postgres]\nimage = \"myorg/pg\"\nversion = \"17\"");
        assert_eq!(postgres_image(&d), "myorg/pg:17");
        // repo-only override without version -> default tag
        let d = deployment("network = \"testnet\"\n[postgres]\nimage = \"myorg/pg\"");
        assert!(postgres_image(&d).starts_with("myorg/pg:"));
        // full ref -> verbatim, incl. private registries with ports
        let d = deployment(
            "network = \"testnet\"\n[stacks-node]\nimage = \"localhost:5000/core:4.0.1\"",
        );
        assert_eq!(stacks_node_image(&d), "localhost:5000/core:4.0.1");
        // no override -> defaults unchanged
        let d = deployment("network = \"testnet\"");
        assert!(!stacks_node_image(&d).starts_with("myorg"));
    }

    #[test]
    fn image_uses_configured_version() {
        let s = deployment(
            "network = \"testnet\"\n[stacks-node]\nversion = \"4.0.1\"\n[postgres]\nversion = \"17\"",
        );
        assert!(stacks_node_image(&s).ends_with(":4.0.1"));
        assert_eq!(postgres_image(&s), "postgres:17");
    }

    #[test]
    fn bitcoind_ports_come_from_network_def() {
        let mainnet = deployment("network = \"mainnet\"");
        assert_eq!(bitcoind_rpc_port(&mainnet), 8332);
        assert_eq!(bitcoind_p2p_port(&mainnet), 8333);
        // testnet follows the Hiro-hosted regtest, hence regtest ports
        let testnet = deployment("network = \"testnet\"");
        assert_eq!(bitcoind_rpc_port(&testnet), 18443);
        assert_eq!(bitcoind_p2p_port(&testnet), 18444);
        // per-stack override wins
        let custom =
            deployment("network = \"mainnet\"\n[bitcoind]\nmode = \"enabled\"\nrpc_port = 9999");
        assert_eq!(bitcoind_rpc_port(&custom), 9999);
    }

    #[test]
    fn hosts_follow_service_mode() {
        let s = deployment(
            "network = \"mainnet\"\n[bitcoind]\nmode = \"enabled\"\n[stacks-node]\nmode = \"external\"\nrpc_host = \"10.0.1.6\"",
        );
        assert_eq!(bitcoind_host(&s), Some("bitcoind".into()));
        assert_eq!(node_rpc_host(&s), Some("10.0.1.6".into()));
        assert_eq!(postgres_host(&s), None); // disabled by default
    }

    #[test]
    fn port_overrides_apply() {
        let s = deployment(
            "network = \"testnet\"\n[stacks-node]\nmode = \"external\"\nrpc_host = \"h\"\nrpc_port = 30443\n[postgres]\nmode = \"enabled\"",
        );
        assert_eq!(node_rpc_port(&s), 30443);
        assert_eq!(postgres_port(&s), POSTGRES_PORT);
    }

    #[test]
    fn roster_lists_all_services_with_modes() {
        use crate::config::ServiceMode;
        let s = deployment("network = \"testnet\"\n[postgres]\nmode = \"enabled\"");
        let roster = roster(&s);
        assert_eq!(roster.len(), 6);
        let (name, mode) = roster.iter().find(|(n, _)| *n == "postgres").unwrap();
        assert_eq!(*name, "postgres");
        assert_eq!(*mode, ServiceMode::Enabled);
        assert!(roster.iter().any(|(n, _)| *n == "stacks-mesh-api"));
    }
}
