//! The `stacks.toml` schema and its validation rules.
//!
//! Every service uses the same tri-state:
//!   - `enabled`:  rendered into the compose file, lifecycle owned by this tool
//!   - `external`: not run by us, but wired into every enabled service's config,
//!     health-checked by `doctor`/`status`, and never touched by `down`
//!   - `disabled`: absent; anything that requires it fails validation

pub mod check;
pub mod render;

use std::fmt;
use std::path::Path;

use anyhow::{Context, Result, bail};
use colored::Colorize;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    Mainnet,
    Testnet,
}

impl fmt::Display for Network {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Network::Mainnet => write!(f, "mainnet"),
            Network::Testnet => write!(f, "testnet"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ServiceMode {
    Enabled,
    External,
    #[default]
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum NodeRole {
    #[default]
    Follower,
    /// Follower with `stacker = true`, required when running a signer
    SignerHost,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stack {
    pub network: Network,
    #[serde(default)]
    pub bitcoind: Bitcoind,
    #[serde(default, rename = "stacks-node")]
    pub stacks_node: StacksNode,
    #[serde(default, rename = "stacks-signer")]
    pub stacks_signer: StacksSigner,
    #[serde(default, rename = "stacks-api")]
    pub stacks_api: StacksApi,
    #[serde(default, rename = "stacks-mesh-api")]
    pub stacks_mesh_api: StacksMeshApi,
    #[serde(default)]
    pub postgres: Postgres,
}

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Bitcoind {
    #[serde(default)]
    pub mode: ServiceMode,
    /// Docker image tag for the managed container (defaults to a pinned tag)
    pub version: Option<String>,
    /// Required when mode = "external"
    pub host: Option<String>,
    pub rpc_port: Option<u16>,
    pub p2p_port: Option<u16>,
    pub rpc_user: Option<String>,
    pub rpc_password: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct StacksNode {
    #[serde(default)]
    pub mode: ServiceMode,
    /// Docker image tag for the managed container (defaults to a pinned tag)
    pub version: Option<String>,
    #[serde(default)]
    pub role: NodeRole,
    /// Required when mode = "external"
    pub rpc_host: Option<String>,
    pub rpc_port: Option<u16>,
    /// The node's `connection_options.auth_token`. Only configurable when
    /// mode = "external" (it must match what your node runs with); managed
    /// nodes always use a tool-managed token.
    pub auth_token: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct StacksSigner {
    #[serde(default)]
    pub mode: ServiceMode,
    /// Docker image tag for the managed container (defaults to a pinned tag)
    pub version: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct StacksApi {
    #[serde(default)]
    pub mode: ServiceMode,
    /// Docker image tag for the managed container (defaults to a pinned tag)
    pub version: Option<String>,
    /// Required when mode = "external": where the API serves HTTP
    pub host: Option<String>,
    pub port: Option<u16>,
    /// Where the API's event server listens, reachable *from the node*
    pub event_host: Option<String>,
    pub event_port: Option<u16>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct StacksMeshApi {
    #[serde(default)]
    pub mode: ServiceMode,
    /// Docker image tag for the managed container (defaults to a pinned tag)
    pub version: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Postgres {
    #[serde(default)]
    pub mode: ServiceMode,
    /// Docker image tag for the managed container (defaults to a pinned tag)
    pub version: Option<String>,
    /// Required when mode = "external"
    pub host: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub password: Option<String>,
}

impl Stack {
    /// Cross-service validation: the rules that make invalid stacks fail at
    /// `up` time instead of becoming runtime mysteries.
    pub fn validate(&self) -> (Vec<String>, Vec<String>) {
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        // Only a mainnet node needs its own bitcoind: krypton testnet follows
        // the Hiro-hosted bitcoin regtest.
        if self.network == Network::Mainnet
            && self.stacks_node.mode == ServiceMode::Enabled
            && self.bitcoind.mode == ServiceMode::Disabled
        {
            errors.push(
                "a mainnet stacks-node requires bitcoind; set [bitcoind] mode = \"enabled\" or \"external\""
                    .into(),
            );
        }
        if self.network == Network::Testnet && self.bitcoind.mode == ServiceMode::Enabled {
            errors.push(
                "testnet (krypton) follows the Hiro-hosted bitcoin regtest — a locally managed \
                 bitcoind cannot join it; set [bitcoind] mode = \"disabled\" (default endpoint) \
                 or \"external\" to point at another krypton regtest"
                    .into(),
            );
        }

        if self.bitcoind.mode == ServiceMode::External && self.bitcoind.host.is_none() {
            errors.push("[bitcoind] mode = \"external\" requires `host`".into());
        }
        if self.stacks_node.mode == ServiceMode::External && self.stacks_node.rpc_host.is_none() {
            errors.push("[stacks-node] mode = \"external\" requires `rpc_host`".into());
        }
        if self.postgres.mode == ServiceMode::External && self.postgres.host.is_none() {
            errors.push("[postgres] mode = \"external\" requires `host`".into());
        }

        // Both APIs need a node; only the blockchain API needs Postgres.
        for (name, mode) in [
            ("stacks-api", self.stacks_api.mode),
            ("stacks-mesh-api", self.stacks_mesh_api.mode),
        ] {
            if mode == ServiceMode::Enabled && self.stacks_node.mode == ServiceMode::Disabled {
                errors.push(format!(
                    "[{name}] requires a stacks-node; set [stacks-node] mode = \"enabled\" or \"external\""
                ));
            }
        }
        if self.stacks_api.mode == ServiceMode::Enabled
            && self.postgres.mode == ServiceMode::Disabled
        {
            errors.push(
                "[stacks-api] requires Postgres; set [postgres] mode = \"enabled\" or \"external\""
                    .into(),
            );
        }

        if self.stacks_signer.mode == ServiceMode::Enabled {
            match self.stacks_node.mode {
                ServiceMode::Disabled => errors.push(
                    "[stacks-signer] requires a stacks-node; set [stacks-node] mode = \"enabled\" or \"external\""
                        .into(),
                ),
                ServiceMode::Enabled if self.stacks_node.role != NodeRole::SignerHost => {
                    errors.push(
                        "a managed signer needs [stacks-node] role = \"signer-host\" (stacker = true)".into(),
                    )
                }
                ServiceMode::External => {
                    if self.stacks_node.auth_token.is_none() {
                        errors.push(
                            "a managed signer with an external node needs [stacks-node] auth_token \
                             (your node's `connection_options.auth_token`, so the signer can authenticate)"
                                .into(),
                        );
                    }
                    warnings.push(
                        "signer is managed but the node is external: apply `rendered/apply-to-your-node.toml` \
                         to your node (stacker = true, matching auth_password, signer events_observer)"
                            .into(),
                    );
                }
                _ => {}
            }
        }

        if self.stacks_node.mode != ServiceMode::External && self.stacks_node.auth_token.is_some() {
            warnings.push(
                "[stacks-node] auth_token is only used when mode = \"external\"; \
                 a managed node uses a tool-managed token"
                    .into(),
            );
        }

        // Reversed (push) edges: the node's config must name its observers. When
        // the node is external we can't write that config, only emit it.
        if self.stacks_node.mode == ServiceMode::External
            && self.stacks_api.mode == ServiceMode::Enabled
        {
            warnings.push(
                "stacks-api is managed but the node is external: add the [[events_observer]] block \
                 from `rendered/apply-to-your-node.toml` to your node config, then verify with `stacksup config check`"
                    .into(),
            );
        }

        (errors, warnings)
    }
}

pub fn load(path: &Path) -> Result<Stack> {
    // `--config` accepts either the file itself or a directory containing one.
    let path = if path.is_dir() {
        path.join("stacks.toml")
    } else {
        path.to_path_buf()
    };
    let raw = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "could not read {} (run `stacksup config init` to create one)",
            path.display()
        )
    })?;
    let stack: Stack =
        toml::from_str(&raw).with_context(|| format!("invalid config in {}", path.display()))?;

    let (errors, warnings) = stack.validate();
    for w in &warnings {
        eprintln!("{}", format!("warning: {w}").yellow());
    }
    if !errors.is_empty() {
        for e in &errors {
            eprintln!("{}", format!("error: {e}").red());
        }
        bail!("{} found {} config error(s)", path.display(), errors.len());
    }
    Ok(stack)
}

pub fn init(force: bool) -> Result<()> {
    let path = Path::new("stacks.toml");
    if path.exists() && !force {
        bail!("stacks.toml already exists (use --force to overwrite)");
    }
    std::fs::write(path, DEFAULT_STACK_TOML)?;
    println!("Wrote stacks.toml — edit it, then run `stacksup start`.");
    Ok(())
}

const DEFAULT_STACK_TOML: &str = r#"# stacksup config
# Everything under rendered/ is generated from this file; edit here, not there.
#
# Every service has a `mode`:
#   "enabled"  — run and managed by this tool (docker compose)
#   "external" — you run it elsewhere; we wire configs to it and health-check it
#   "disabled" — not part of this stack
#
# Managed services also take a `version` — the docker image tag to run.
# Omit it to use this tool's pinned default.

network = "testnet" # mainnet | testnet

[bitcoind]
# Only needed on mainnet. Testnet (krypton) follows the Hiro-hosted bitcoin
# regtest.
mode = "disabled"
# version = "29"
# For mode = "external":
# host = "10.0.1.5"
# rpc_port = 18332
# p2p_port = 18333
# rpc_user = "stacks"
# rpc_password = "..."

[stacks-node]
mode = "enabled"
role = "follower" # follower | signer-host (required when running a signer)
# version = "3.2.0.0.1"
# For mode = "external":
# rpc_host = "10.0.1.6"
# rpc_port = 20443
# auth_token = "..." # your node's connection_options.auth_token

[stacks-signer]
mode = "disabled"
# version = "3.2.0.0.1.0"

[stacks-api]
mode = "enabled"
# version = "8.1.0"

[stacks-mesh-api]
mode = "disabled"

[postgres]
mode = "enabled"
# version = "17"
# For mode = "external":
# host = "pg.internal"
# port = 5432
# user = "stacks"
# password = "..."
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(toml_str: &str) -> Stack {
        toml::from_str(toml_str).expect("test stack.toml should parse")
    }

    fn errors(toml_str: &str) -> Vec<String> {
        stack(toml_str).validate().0
    }

    fn warnings(toml_str: &str) -> Vec<String> {
        stack(toml_str).validate().1
    }

    #[test]
    fn init_template_is_valid_and_clean() {
        let s: Stack = toml::from_str(DEFAULT_STACK_TOML).expect("template must parse");
        let (errors, _) = s.validate();
        assert!(errors.is_empty(), "template has errors: {errors:?}");
    }

    #[test]
    fn mainnet_node_requires_bitcoind() {
        let e = errors("network = \"mainnet\"\n[stacks-node]\nmode = \"enabled\"");
        assert!(e.iter().any(|m| m.contains("requires bitcoind")));
    }

    #[test]
    fn testnet_rejects_managed_bitcoind() {
        let e = errors("network = \"testnet\"\n[bitcoind]\nmode = \"enabled\"");
        assert!(e.iter().any(|m| m.contains("Hiro-hosted bitcoin regtest")));
    }

    #[test]
    fn external_services_require_hosts() {
        let e = errors(
            "network = \"mainnet\"\n[bitcoind]\nmode = \"external\"\n[stacks-node]\nmode = \"external\"\n[postgres]\nmode = \"external\"",
        );
        assert!(
            e.iter()
                .any(|m| m.contains("[bitcoind]") && m.contains("`host`"))
        );
        assert!(e.iter().any(|m| m.contains("`rpc_host`")));
        assert!(
            e.iter()
                .any(|m| m.contains("[postgres]") && m.contains("`host`"))
        );
    }

    #[test]
    fn apis_require_node_and_api_requires_postgres() {
        let e = errors(
            "network = \"testnet\"\n[stacks-api]\nmode = \"enabled\"\n[stacks-mesh-api]\nmode = \"enabled\"",
        );
        assert!(
            e.iter()
                .any(|m| m.contains("[stacks-api] requires a stacks-node"))
        );
        assert!(
            e.iter()
                .any(|m| m.contains("[stacks-mesh-api] requires a stacks-node"))
        );
        assert!(
            e.iter()
                .any(|m| m.contains("[stacks-api] requires Postgres"))
        );
        // the mesh API deliberately does NOT require postgres
        assert!(
            !e.iter()
                .any(|m| m.contains("[stacks-mesh-api] requires Postgres"))
        );
    }

    #[test]
    fn managed_signer_needs_signer_host_role() {
        let e = errors(
            "network = \"testnet\"\n[stacks-node]\nmode = \"enabled\"\n[stacks-signer]\nmode = \"enabled\"",
        );
        assert!(e.iter().any(|m| m.contains("signer-host")));
    }

    #[test]
    fn signer_with_external_node_needs_auth_token() {
        let base = "network = \"testnet\"\n[stacks-node]\nmode = \"external\"\nrpc_host = \"h\"\n[stacks-signer]\nmode = \"enabled\"";
        let e = errors(base);
        assert!(e.iter().any(|m| m.contains("auth_token")));
        // with the token supplied it degrades to a warning, not an error
        let ok = "network = \"testnet\"\n[stacks-node]\nmode = \"external\"\nrpc_host = \"h\"\nauth_token = \"tok-1234\"\n[stacks-signer]\nmode = \"enabled\"";
        assert!(errors(ok).is_empty());
        assert!(
            warnings(ok)
                .iter()
                .any(|m| m.contains("apply-to-your-node"))
        );
    }

    #[test]
    fn auth_token_on_managed_node_warns() {
        let w = warnings(
            "network = \"mainnet\"\n[bitcoind]\nmode = \"enabled\"\n[stacks-node]\nmode = \"enabled\"\nauth_token = \"x\"",
        );
        assert!(
            w.iter()
                .any(|m| m.contains("only used when mode = \"external\""))
        );
    }
}
