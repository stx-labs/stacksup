//! Network definitions: everything that varies between Stacks networks (burnchain endpoint, chain
//! id, epochs, seeded balances, bootstrap peers) lives in a TOML definition file, not in code.
//!
//! Standard networks ship embedded in the binary from the repo's `networks/` directory, publishing
//! a new testnet means adding a file there. A `network` value that names no embedded definition is
//! resolved as a file path (or `networks/<name>.toml`) relative to the stacks.toml, so users can
//! define custom networks without a tool release.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// Standard definitions compiled into the binary.
const EMBEDDED: &[(&str, &str)] = &[
    ("mainnet", include_str!("../../networks/mainnet.toml")),
    ("testnet", include_str!("../../networks/testnet.toml")),
];

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct NetworkDef {
    pub name: String,
    pub chain_id: u32,
    /// Path segment under archive.hiro.so; absent when no Hiro archives exist for this network.
    pub hiro_archive_path: Option<String>,
    /// The network name signer-sidekick expects in SIDEKICK_NETWORK (e.g. testnet is
    /// "pox5-testnet" there); absent when sidekick has no profile for this network.
    pub sidekick_network: Option<String>,
    /// Hiro-hosted indexed Stacks API for this network; the sidekick default when no local
    /// stacks-api is enabled and no explicit URL is configured.
    pub hiro_api_url: Option<String>,
    pub bitcoind: BitcoindNet,
    pub node: NodeNet,
    #[serde(default, rename = "ustx_balance")]
    pub ustx_balances: Vec<UstxBalance>,
    #[serde(default)]
    pub epochs: Vec<Epoch>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct BitcoindNet {
    /// Whether this network can run its own bitcoind. False for networks following a hosted
    /// burnchain (a fresh local chain can't join it).
    pub allow_managed: bool,
    /// bitcoind -chain flag ("main", "test", "regtest")
    pub chain: String,
    pub rpc_port: u16,
    pub p2p_port: u16,
    /// Hosted burnchain endpoint used when [bitcoind] is disabled.
    pub default_host: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct NodeNet {
    /// stacks-node [burnchain] mode; also the node's on-disk chainstate subdirectory name.
    pub burnchain_mode: String,
    pub bootstrap_node: Option<String>,
    pub pox_prepare_length: Option<u32>,
    pub pox_reward_length: Option<u32>,
    /// Verbatim TOML appended to the rendered [node] section.
    pub node_extra: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UstxBalance {
    pub address: String,
    pub amount: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Epoch {
    pub epoch_name: String,
    pub start_height: u64,
}

/// Resolve a `network = "..."` value: embedded name first, then a custom definition file relative
/// to the directory holding stacks.toml.
pub fn load(name: &str, config_dir: &Path) -> Result<NetworkDef> {
    if let Some((_, raw)) = EMBEDDED.iter().find(|(n, _)| *n == name) {
        return parse(raw).with_context(|| format!("embedded network definition `{name}`"));
    }

    // Custom definitions resolve relative to stacks.toml only, an absolute path would silently
    // bypass config_dir (Path::join discards the base).
    if Path::new(name).is_absolute() {
        bail!(
            "network `{name}`: custom definition paths must be relative to the \
             directory containing stacks.toml"
        );
    }
    let candidates = [
        config_dir.join(name),
        config_dir.join("networks").join(format!("{name}.toml")),
    ];
    for path in &candidates {
        if path.is_file() {
            let raw = std::fs::read_to_string(path)?;
            return parse(&raw).with_context(|| format!("network definition {}", path.display()));
        }
    }

    bail!(
        "unknown network `{name}`. Built-in networks: {}; or point at a custom \
         definition file (`network = \"my-net.toml\"`, resolved next to stacks.toml)",
        EMBEDDED
            .iter()
            .map(|(n, _)| *n)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn parse(raw: &str) -> Result<NetworkDef> {
    let def: NetworkDef = toml::from_str(raw)?;
    // Missing fields are already serde errors (nothing here is defaulted); these guards catch the
    // empty/zero values serde accepts.
    if def.name.is_empty() || def.node.burnchain_mode.is_empty() {
        bail!("network definition must set `name` and [node] burnchain_mode");
    }
    if def.bitcoind.chain.is_empty() {
        bail!("network definition must set a non-empty [bitcoind] chain");
    }
    if def.bitcoind.rpc_port == 0 || def.bitcoind.p2p_port == 0 {
        bail!("network definition must set non-zero [bitcoind] rpc_port and p2p_port");
    }
    Ok(def)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_definitions_parse() {
        let mainnet = load("mainnet", Path::new(".")).unwrap();
        assert_eq!(mainnet.chain_id, 0x00000001);
        assert!(mainnet.bitcoind.allow_managed);
        assert_eq!(mainnet.node.burnchain_mode, "mainnet");
        assert!(mainnet.epochs.is_empty());

        let testnet = load("testnet", Path::new(".")).unwrap();
        assert_eq!(testnet.chain_id, 0x80000000);
        assert!(!testnet.bitcoind.allow_managed);
        assert_eq!(
            testnet.bitcoind.default_host.as_deref(),
            Some("bitcoin.regtest.hiro.so")
        );
        assert_eq!(testnet.node.burnchain_mode, "krypton");
        assert_eq!(testnet.ustx_balances.len(), 12);
        assert_eq!(testnet.epochs.len(), 14);
        assert!(
            testnet
                .node
                .node_extra
                .as_deref()
                .unwrap()
                .contains("pox_5_sbtc_contract")
        );
    }

    #[test]
    fn unknown_network_lists_builtins() {
        let err = load("nope", Path::new("/nonexistent"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("mainnet, testnet"));
    }

    #[test]
    fn absolute_custom_paths_are_rejected() {
        let err = load("/etc/evil.toml", Path::new("/tmp"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("must be relative"));
    }

    #[test]
    fn zero_ports_and_empty_chain_are_rejected() {
        let base = "name = \"x\"\nchain_id = 1\n[bitcoind]\nallow_managed = true\nchain = \"{chain}\"\nrpc_port = {rpc}\np2p_port = 8333\n[node]\nburnchain_mode = \"mainnet\"\n";
        let bad_port = base.replace("{chain}", "main").replace("{rpc}", "0");
        assert!(
            parse(&bad_port)
                .unwrap_err()
                .to_string()
                .contains("non-zero")
        );
        let bad_chain = base.replace("{chain}", "").replace("{rpc}", "8332");
        assert!(
            parse(&bad_chain)
                .unwrap_err()
                .to_string()
                .contains("non-empty")
        );
        let good = base.replace("{chain}", "main").replace("{rpc}", "8332");
        assert!(parse(&good).is_ok());
    }

    #[test]
    fn custom_definition_file_resolves() {
        let dir = std::env::temp_dir().join(format!("stacksup-net-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("networks")).unwrap();
        std::fs::write(
            dir.join("networks/testnet2.toml"),
            "name = \"testnet2\"\nchain_id = 0x80000002\n[bitcoind]\nallow_managed = false\nchain = \"test\"\nrpc_port = 18443\np2p_port = 18444\ndefault_host = \"btc.testnet2.example\"\n[node]\nburnchain_mode = \"krypton\"\n",
        )
        .unwrap();
        let def = load("testnet2", &dir).unwrap();
        assert_eq!(def.chain_id, 0x80000002);
        assert_eq!(
            def.bitcoind.default_host.as_deref(),
            Some("btc.testnet2.example")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
