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

const CHAIN_ID_MAINNET: u32 = 0x00000001;
const CHAIN_ID_TESTNET: u32 = 0x80000000;

/// Standard definitions compiled into the binary.
const EMBEDDED: &[(&str, &str)] = &[
    ("mainnet", include_str!("../../networks/mainnet.toml")),
    ("testnet", include_str!("../../networks/testnet.toml")),
    (
        "staking-testnet",
        include_str!("../../networks/staking-testnet.toml"),
    ),
];

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct NetworkDef {
    pub name: String,
    pub chain_id: u32,
    /// Path segment under archive.hiro.so; absent when no Hiro archives exist for this network.
    pub hiro_archive_path: Option<String>,
    /// Filename prefix of the node archives under that path (`<prefix>-stacks-blockchain-*`);
    /// defaults to `hiro_archive_path`.
    pub hiro_archive_prefix: Option<String>,
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
    /// Custom signet challenge (hex script, bitcoind's -signetchallenge); only with chain "signet".
    pub signet_challenge: Option<String>,
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

impl NetworkDef {
    /// Stacks network family: address/transaction versions and the network name the signer and
    /// sidekick are configured with. Only mainnet runs on chain id 1; every other id is a testnet
    /// (e.g. staking-testnet's 0x00000500).
    pub fn family(&self) -> &'static str {
        if self.chain_id == CHAIN_ID_MAINNET {
            "mainnet"
        } else {
            "testnet"
        }
    }

    /// True when clients configured by family alone would assume a different chain id, so the
    /// id must be passed to them explicitly.
    pub fn has_custom_chain_id(&self) -> bool {
        !matches!(self.chain_id, CHAIN_ID_MAINNET | CHAIN_ID_TESTNET)
    }

    pub fn hiro_archive_prefix(&self) -> Option<&str> {
        self.hiro_archive_prefix
            .as_deref()
            .or(self.hiro_archive_path.as_deref())
    }
}

/// (name, node-archive filename prefix) of every built-in network that publishes archives.
pub fn builtin_archive_prefixes() -> Vec<(&'static str, String)> {
    EMBEDDED
        .iter()
        .filter_map(|(name, raw)| {
            let def = parse(raw).ok()?;
            Some((*name, def.hiro_archive_prefix()?.to_string()))
        })
        .collect()
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
    // Signet on one side only means a managed bitcoind and the node follow different burnchains.
    if (def.bitcoind.chain == "signet") != (def.node.burnchain_mode == "signet") {
        bail!(
            "[bitcoind] chain = \"signet\" and [node] burnchain_mode = \"signet\" must be set \
             together"
        );
    }
    if let Some(challenge) = &def.bitcoind.signet_challenge {
        // stacks-node rejects signet_challenge outside signet mode; bitcoind needs -chain=signet.
        if def.bitcoind.chain != "signet" {
            bail!(
                "[bitcoind] signet_challenge requires [bitcoind] chain = \"signet\" and \
                 [node] burnchain_mode = \"signet\""
            );
        }
        if challenge.is_empty()
            || challenge.len() % 2 != 0
            || !challenge.chars().all(|c| c.is_ascii_hexdigit())
        {
            bail!("[bitcoind] signet_challenge must be a hex script");
        }
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
    fn staking_testnet_definition_parses() {
        let net = load("staking-testnet", Path::new(".")).unwrap();
        assert_eq!(net.family(), "testnet");
        assert_eq!(net.chain_id, 0x00000500);
        assert!(net.has_custom_chain_id());
        assert!(!net.bitcoind.allow_managed);
        assert_eq!(net.bitcoind.chain, "signet");
        assert_eq!(
            (net.bitcoind.rpc_port, net.bitcoind.p2p_port),
            (38332, 38333)
        );
        assert_eq!(
            net.bitcoind.signet_challenge.as_deref(),
            Some("0014511787db620184f553cfa06bb2a6607497f09f4a")
        );
        assert_eq!(net.node.burnchain_mode, "signet");
        assert_eq!(net.hiro_archive_path.as_deref(), Some("staking-testnet"));
        assert_eq!(net.hiro_archive_prefix(), Some("signet"));
        // the live chain's 17-entry genesis; the signet build owns epochs and PoX lengths
        assert_eq!(net.ustx_balances.len(), 17);
        assert!(net.epochs.is_empty());
        assert!(net.node.pox_prepare_length.is_none() && net.node.pox_reward_length.is_none());

        // the standard networks keep their implicit family and default chain ids
        let testnet = load("testnet", Path::new(".")).unwrap();
        assert_eq!(testnet.family(), "testnet");
        assert!(!testnet.has_custom_chain_id());
        assert_eq!(testnet.hiro_archive_prefix(), Some("testnet"));
        assert!(
            !load("mainnet", Path::new("."))
                .unwrap()
                .has_custom_chain_id()
        );
    }

    #[test]
    fn signet_challenge_is_validated() {
        let base = "name = \"x\"\nchain_id = 0x80000000\n[bitcoind]\nallow_managed = false\nchain = \"{chain}\"\nrpc_port = 38332\np2p_port = 38333\n{challenge}\n[node]\nburnchain_mode = \"{mode}\"\n";
        let def = |chain: &str, mode: &str, challenge: &str| {
            base.replace("{chain}", chain)
                .replace("{mode}", mode)
                .replace("{challenge}", challenge)
        };
        assert!(parse(&def("signet", "signet", "")).is_ok()); // public signet
        assert!(parse(&def("signet", "signet", "signet_challenge = \"51\"")).is_ok());
        let err = parse(&def("test", "krypton", "signet_challenge = \"51\""))
            .unwrap_err()
            .to_string();
        assert!(err.contains("requires"));
        // signet on one side only, with or without a challenge
        for (chain, mode) in [("test", "signet"), ("signet", "krypton")] {
            let err = parse(&def(chain, mode, "")).unwrap_err().to_string();
            assert!(
                err.contains("must be set together"),
                "{chain}/{mode}: {err}"
            );
        }
        let err = parse(&def("signet", "signet", "signet_challenge = \"5g\""))
            .unwrap_err()
            .to_string();
        assert!(err.contains("hex"));
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
