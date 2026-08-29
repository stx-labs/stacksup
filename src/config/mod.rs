//! The `stacks.toml` schema and its validation rules.
//!
//! Every service uses the same tri-state:
//!   - `enabled`:  rendered into the compose file, lifecycle owned by this tool
//!   - `external`: not run by us, but wired into every enabled service's config,
//!     health-checked by `doctor`/`status`, and never touched by `down`
//!   - `disabled`: absent; anything that requires it fails validation

pub mod check;
pub mod network;
pub mod render;

use std::path::Path;

use anyhow::{Context, Result, bail};
use colored::Colorize;
use serde::{Deserialize, Serialize};

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
/// The deployment configuration. This is the root of the configuration and contains all the
/// services and their configurations.
pub struct Deployment {
    /// Deployment name: becomes the compose project name and the container name prefix, so several
    /// deployments can share a machine. Lowercase alphanumerics and dashes. The default keeps the
    /// historical single- deployment naming (project `stacks`, containers `stacks-node`, ...).
    #[serde(default = "default_deployment_name")]
    pub name: String,
    /// Added to every published HOST port (container-internal ports never change), so a second
    /// deployment can run beside the first: `port_offset = 100` publishes the node RPC on 20543,
    /// the API on 4099, postgres on 5532, and so on.
    #[serde(default)]
    pub port_offset: u16,
    /// Network name: a built-in definition (mainnet, testnet) or a custom definition file resolved
    /// relative to stacks.toml.
    pub network: String,
    /// The resolved definition; populated by `load()` after parsing.
    #[serde(skip)]
    pub net: network::NetworkDef,
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
    #[serde(default, rename = "signer-sidekick")]
    pub signer_sidekick: SignerSidekick,
    #[serde(default)]
    pub postgres: Postgres,
}

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Bitcoind {
    #[serde(default)]
    pub mode: ServiceMode,
    /// Docker image override: a repository (`myorg/stacks-node`) that keeps using `version`/the
    /// default tag, or a full ref with its own tag (`myorg/stacks-node:4.0.1`, mutually exclusive
    /// with `version`).
    pub image: Option<String>,
    /// Docker image tag for the managed container (defaults to a pinned tag)
    pub version: Option<String>,
    /// Required when mode = "external"
    pub host: Option<String>,
    pub rpc_port: Option<u16>,
    pub p2p_port: Option<u16>,
    /// SECRET: set in secrets.toml ([bitcoind] rpc_user / rpc_password).
    pub rpc_user: Option<String>,
    pub rpc_password: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct StacksNode {
    #[serde(default)]
    pub mode: ServiceMode,
    /// Docker image override: a repository (`myorg/stacks-node`) that keeps using `version`/the
    /// default tag, or a full ref with its own tag (`myorg/stacks-node:4.0.1`, mutually exclusive
    /// with `version`).
    pub image: Option<String>,
    /// Docker image tag for the managed container (defaults to a pinned tag)
    pub version: Option<String>,
    #[serde(default)]
    pub role: NodeRole,
    /// Required when mode = "external"
    pub rpc_host: Option<String>,
    pub rpc_port: Option<u16>,
    /// The node's `connection_options.auth_token`. SECRET: set in secrets.toml ([stacks-node]
    /// auth_token), never here. For an external node it must match what your node runs with.
    pub auth_token: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct StacksSigner {
    #[serde(default)]
    pub mode: ServiceMode,
    /// Docker image override: a repository (`myorg/stacks-node`) that keeps using `version`/the
    /// default tag, or a full ref with its own tag (`myorg/stacks-node:4.0.1`, mutually exclusive
    /// with `version`).
    pub image: Option<String>,
    /// Docker image tag for the managed container (defaults to a pinned tag)
    pub version: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct StacksApi {
    #[serde(default)]
    pub mode: ServiceMode,
    /// Docker image override: a repository (`myorg/stacks-node`) that keeps using `version`/the
    /// default tag, or a full ref with its own tag (`myorg/stacks-node:4.0.1`, mutually exclusive
    /// with `version`).
    pub image: Option<String>,
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
    /// Docker image override: a repository (`myorg/stacks-node`) that keeps using `version`/the
    /// default tag, or a full ref with its own tag (`myorg/stacks-node:4.0.1`, mutually exclusive
    /// with `version`).
    pub image: Option<String>,
    /// Docker image tag for the managed container (defaults to a pinned tag)
    pub version: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
/// signer-sidekick (github.com/stx-labs/signer-sidekick): PoX-5 operations dashboard for the
/// deployment's signer and pool. Optional; requires a node and a signer.
pub struct SignerSidekick {
    #[serde(default)]
    pub mode: ServiceMode,
    /// Docker image override: a repository that keeps using `version`/the default tag, or a full
    /// ref with its own tag (mutually exclusive with `version`).
    pub image: Option<String>,
    /// Release version (e.g. "2.0.0"); becomes image tag v<version>.
    pub version: Option<String>,
    /// The deployed PoX-5 signer-manager contract this deployment's signer registers through
    /// (SP....contract-name). Required when enabled.
    pub manager_principal: Option<String>,
    /// "observe" (default) or "operator-run" (sidekick signs reward calls with the gas wallet it
    /// generates in its own Settings UI; enable deliberately).
    pub engine_mode: Option<String>,
    /// Indexed Stacks API. Defaults to the managed stacks-api when enabled, else the network's
    /// Hiro API.
    pub stacks_api_url: Option<String>,
    /// SECRET: set in secrets.toml ([signer-sidekick] auth_token), never here.
    pub auth_token: Option<String>,
    /// SECRET: optional Hiro API key, set in secrets.toml ([signer-sidekick] stacks_api_key).
    pub stacks_api_key: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Postgres {
    #[serde(default)]
    pub mode: ServiceMode,
    /// Docker image override: a repository (`myorg/stacks-node`) that keeps using `version`/the
    /// default tag, or a full ref with its own tag (`myorg/stacks-node:4.0.1`, mutually exclusive
    /// with `version`).
    pub image: Option<String>,
    /// Docker image tag for the managed container (defaults to a pinned tag)
    pub version: Option<String>,
    /// Required when mode = "external"
    pub host: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
    /// SECRET: set in secrets.toml ([postgres] password), never here.
    pub password: Option<String>,
}

/// The default deployment name: compose project `stacks` and the historical container names.
/// Render-time naming branches on this same constant.
pub const DEFAULT_PROJECT: &str = "stacks";

fn default_deployment_name() -> String {
    DEFAULT_PROJECT.into()
}

/// `ADDRESS.contract-name`: a c32 Stacks address (starts with S, uppercase alphanumerics) plus a
/// lowercase contract name.
fn is_contract_principal(p: &str) -> bool {
    let Some((addr, name)) = p.split_once('.') else {
        return false;
    };
    (28..=41).contains(&addr.len())
        && addr.starts_with('S')
        && addr
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && !name.is_empty()
        && name.len() <= 40
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

impl Deployment {
    /// The docker compose project (embedded in the rendered compose file as its top-level `name:`),
    /// and the container-name prefix.
    pub fn project(&self) -> &str {
        &self.name
    }

    /// A service's host-published port: the base shifted by `port_offset`. Validation bounds the
    /// offset, but guard the addition anyway so an unvalidated path can never wrap around the u16
    /// range silently.
    pub fn published(&self, base: u16) -> u16 {
        base.checked_add(self.port_offset).unwrap_or_else(|| {
            panic!(
                "port {base} + port_offset {} exceeds 65535. Lower port_offset in stacks.toml",
                self.port_offset
            )
        })
    }

    /// Secret-bearing fields that must NOT be set in stacks.toml.
    fn config_secret_leaks(&self) -> Vec<(&'static str, &'static str)> {
        let mut leaks = Vec::new();
        if self.bitcoind.rpc_user.is_some() {
            leaks.push(("bitcoind", "rpc_user"));
        }
        if self.bitcoind.rpc_password.is_some() {
            leaks.push(("bitcoind", "rpc_password"));
        }
        if self.postgres.password.is_some() {
            leaks.push(("postgres", "password"));
        }
        if self.stacks_node.auth_token.is_some() {
            leaks.push(("stacks-node", "auth_token"));
        }
        if self.signer_sidekick.auth_token.is_some() {
            leaks.push(("signer-sidekick", "auth_token"));
        }
        if self.signer_sidekick.stacks_api_key.is_some() {
            leaks.push(("signer-sidekick", "stacks_api_key"));
        }
        leaks
    }

    /// Fill secret fields from the secrets.toml overlay.
    fn merge_secrets(&mut self, overlay: crate::utils::secrets::SecretsOverlay) {
        self.bitcoind.rpc_user = overlay.bitcoind.rpc_user;
        self.bitcoind.rpc_password = overlay.bitcoind.rpc_password;
        self.postgres.password = overlay.postgres.password;
        self.stacks_node.auth_token = overlay.stacks_node.auth_token;
        self.signer_sidekick.auth_token = overlay.signer_sidekick.auth_token;
        self.signer_sidekick.stacks_api_key = overlay.signer_sidekick.stacks_api_key;
    }

    /// Secrets that MUST be present given the enabled services. The tool never writes secrets.toml
    /// itself. Missing values are the user's to add, so the error carries a paste-ready snippet.
    fn check_required_secrets(&self, config_dir: &Path) -> Result<()> {
        let mut missing: Vec<(&str, &str)> = Vec::new();
        if self.postgres.mode != ServiceMode::Disabled && self.postgres.password.is_none() {
            missing.push(("postgres", "password"));
        }
        if self.bitcoind.mode == ServiceMode::Enabled {
            if self.bitcoind.rpc_user.is_none() {
                missing.push(("bitcoind", "rpc_user"));
            }
            if self.bitcoind.rpc_password.is_none() {
                missing.push(("bitcoind", "rpc_password"));
            }
        }
        let node_needs_token = self.stacks_node.mode == ServiceMode::Enabled
            || (self.stacks_node.mode == ServiceMode::External
                && (self.stacks_signer.mode == ServiceMode::Enabled
                    || self.stacks_mesh_api.mode == ServiceMode::Enabled));
        if node_needs_token && self.stacks_node.auth_token.is_none() {
            missing.push(("stacks-node", "auth_token"));
        }
        if self.signer_sidekick.mode == ServiceMode::Enabled
            && self.signer_sidekick.auth_token.is_none()
        {
            missing.push(("signer-sidekick", "auth_token"));
        }
        if !missing.is_empty() {
            return Err(crate::utils::secrets::missing_secrets_error(
                config_dir, &missing,
            ));
        }
        Ok(())
    }

    /// Cross-service validation: the rules that make invalid stacks fail at `up` time instead of
    /// becoming runtime mysteries.
    pub fn validate(&self) -> (Vec<String>, Vec<String>) {
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        // The name becomes a compose project and container-name prefix; keep
        // it to the safe intersection of docker's naming rules.
        if self.name.is_empty()
            || self.name.len() > 32
            || !self
                .name
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            || !self
                .name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            errors.push(format!(
                "name `{}` is invalid: 1-32 lowercase letters, digits, or dashes, starting with a letter or digit",
                self.name
            ));
        }
        // Highest published base port is the node p2p (20444); keep the shifted ports inside the
        // u16 range with room to spare.
        if self.port_offset > 40000 {
            errors.push(format!(
                "port_offset {} is too large (max 40000)",
                self.port_offset
            ));
        }
        // A node needs a burnchain source: its own/external bitcoind, or the network's hosted
        // default endpoint.
        if self.stacks_node.mode == ServiceMode::Enabled
            && self.bitcoind.mode == ServiceMode::Disabled
            && self.net.bitcoind.default_host.is_none()
        {
            errors.push(format!(
                "a {} stacks-node requires bitcoind; set [bitcoind] mode = \"enabled\" or \"external\"",
                self.network
            ));
        }
        if self.bitcoind.mode == ServiceMode::Enabled && !self.net.bitcoind.allow_managed {
            errors.push(format!(
                "network `{}` follows a hosted burnchain ({}) — a locally managed bitcoind \
                 cannot join it; set [bitcoind] mode = \"disabled\" (hosted default) or \"external\"",
                self.network,
                self.net.bitcoind.default_host.as_deref().unwrap_or("hosted"),
            ));
        }

        // `image` with an explicit tag and `version` are two sources of truth for the same tag,
        // reject the ambiguity.
        for (name, image, version) in [
            ("bitcoind", &self.bitcoind.image, &self.bitcoind.version),
            (
                "stacks-node",
                &self.stacks_node.image,
                &self.stacks_node.version,
            ),
            (
                "stacks-signer",
                &self.stacks_signer.image,
                &self.stacks_signer.version,
            ),
            (
                "stacks-api",
                &self.stacks_api.image,
                &self.stacks_api.version,
            ),
            (
                "stacks-mesh-api",
                &self.stacks_mesh_api.image,
                &self.stacks_mesh_api.version,
            ),
            ("postgres", &self.postgres.image, &self.postgres.version),
        ] {
            if let (Some(image), Some(_)) = (image, version)
                && crate::utils::versions::has_explicit_tag(image)
            {
                errors.push(format!(
                    "[{name}] `image` already pins a tag (`{image}`). Remove `version` or drop the tag from `image`"
                ));
            }
        }

        if self.bitcoind.mode == ServiceMode::External && self.bitcoind.host.is_none() {
            errors.push("[bitcoind] mode = \"external\" requires `host`".into());
        }
        if self.bitcoind.mode == ServiceMode::External
            && self.bitcoind.rpc_user.is_some() != self.bitcoind.rpc_password.is_some()
        {
            errors.push("[bitcoind] rpc_user and rpc_password must be set together".into());
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
                    warnings.push(
                        "signer is managed but the node is external: apply `rendered/apply-to-your-node.toml` \
                         to your node (stacker = true, matching auth_password, signer events_observer). \
                         Note: the signer's event endpoint (port 30000) is not published on this host — \
                         an off-host node cannot push to it; run the node on this machine or expose the \
                         port yourself (compose override)"
                            .into(),
                    );
                }
                _ => {}
            }
        }

        // signer-sidekick monitors THIS deployment's signer through the node RPC and a deployed
        // signer-manager contract; both ends must exist and the manager must be named.
        match self.signer_sidekick.mode {
            ServiceMode::External => errors.push(
                "[signer-sidekick] mode = \"external\" is not supported — run it here (\"enabled\") or disable it"
                    .into(),
            ),
            ServiceMode::Enabled => {
                if self.stacks_node.mode == ServiceMode::Disabled {
                    errors.push(
                        "[signer-sidekick] requires a stacks-node; set [stacks-node] mode = \"enabled\" or \"external\""
                            .into(),
                    );
                }
                if self.stacks_signer.mode == ServiceMode::Disabled {
                    errors.push(
                        "[signer-sidekick] monitors this deployment's signer; set [stacks-signer] mode = \"enabled\" or \"external\""
                            .into(),
                    );
                }
                match self.signer_sidekick.manager_principal.as_deref() {
                    None => errors.push(
                        "[signer-sidekick] requires manager_principal — the deployed PoX-5 signer-manager contract (e.g. SP2ABC....signer-manager)"
                            .into(),
                    ),
                    Some(p) if !is_contract_principal(p) => errors.push(format!(
                        "[signer-sidekick] manager_principal `{p}` is not a contract principal (expected ADDRESS.contract-name)"
                    )),
                    _ => {}
                }
                if let Some(mode) = self.signer_sidekick.engine_mode.as_deref()
                    && !matches!(mode, "observe" | "operator-run")
                {
                    errors.push(format!(
                        "[signer-sidekick] engine_mode `{mode}` is invalid: \"observe\" or \"operator-run\""
                    ));
                }
                if self.net.sidekick_network.is_none() {
                    errors.push(format!(
                        "network `{}` has no signer-sidekick profile (its definition lacks `sidekick_network`)",
                        self.network
                    ));
                }
            }
            ServiceMode::Disabled => {}
        }

        // Reversed (push) edges: the node's config must name its observers. When the node is
        // external we can't write that config, only emit it.
        if self.stacks_node.mode == ServiceMode::External
            && self.stacks_api.mode == ServiceMode::Enabled
        {
            warnings.push(
                "stacks-api is managed but the node is external: add the [[events_observer]] block \
                 from `rendered/apply-to-your-node.toml` to your node config, then verify with `stacksup config check`. \
                 Note: the API's event port (3700) is not published on this host, an off-host node \
                 cannot push to it; run the node on this machine or expose the port yourself (compose override)"
                    .into(),
            );
        }

        (errors, warnings)
    }
}

pub fn load(path: &Path) -> Result<Deployment> {
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
    let mut deployment: Deployment =
        toml::from_str(&raw).with_context(|| format!("invalid config in {}", path.display()))?;
    let config_dir = path.parent().unwrap_or(Path::new("."));
    deployment.net = network::load(&deployment.network, config_dir)?;

    // Secrets never belong in the committable config; they come from the secrets.toml overlay
    // beside it.
    let leaked = deployment.config_secret_leaks();
    if !leaked.is_empty() {
        for (section, field) in &leaked {
            eprintln!(
                "{}",
                format!(
                    "error: [{section}] {field} is a secret. Remove it from {} and set it in secrets.toml instead",
                    path.display()
                )
                .red()
            );
        }
        bail!("secrets found in {}", path.display());
    }
    if let Some(overlay) = crate::utils::secrets::load(config_dir)? {
        deployment.merge_secrets(overlay);
    }
    deployment.check_required_secrets(config_dir)?;

    let (errors, warnings) = deployment.validate();
    for w in &warnings {
        eprintln!("{}", format!("warning: {w}").yellow());
    }
    if !errors.is_empty() {
        for e in &errors {
            eprintln!("{}", format!("error: {e}").red());
        }
        bail!("{} found {} config error(s)", path.display(), errors.len());
    }
    Ok(deployment)
}

pub fn init(force: bool) -> Result<()> {
    let path = Path::new("stacks.toml");
    if path.exists() && !force {
        bail!("stacks.toml already exists (use --force to overwrite)");
    }
    std::fs::write(path, DEFAULT_STACK_TOML)?;
    // secrets.toml lives beside stacks.toml. Generated ONLY when missing, an existing file is the
    // user's and is never touched, even with --force.
    crate::utils::secrets::generate_if_missing(Path::new("."))?;
    println!("Wrote stacks.toml — edit it, then run `stacksup start`.");
    Ok(())
}

const DEFAULT_STACK_TOML: &str = r#"# stacksup config
# Everything under rendered/ is generated from this file; edit here, not there.
#
# Every service has a `mode`:
#   "enabled"    run and managed by this tool (docker compose)
#   "external"   you run it elsewhere; we wire configs to it and health-check it
#   "disabled"   not part of this stack
#
# Managed services also take:
#   `version`    the docker image tag to run (omit for this tool's default)
#   `image`      a custom docker image: a repository that keeps using
#                `version`/the default tag, or a full ref with its own tag
#                (then omit `version`)

network = "testnet" # mainnet | testnet | custom network definition file

# To run several deployments on one machine, give each a distinct name
# (compose project + container prefix) and shift its published host ports:
# name = "testnet-b"
# port_offset = 100

[bitcoind]
# Only needed on mainnet. Testnet (krypton) follows the Hiro-hosted bitcoin
# regtest.
mode = "disabled"
# version = "29"
# For mode = "external":
# host = "10.0.1.5"
# rpc_port = 18332
# p2p_port = 18333
# Credentials go in secrets.toml ([bitcoind] rpc_user / rpc_password).

[stacks-node]
mode = "enabled"
role = "follower" # follower | signer-host (required when running a signer)
# version = "3.2.0.0.1"
# For mode = "external":
# rpc_host = "10.0.1.6"
# rpc_port = 20443
# The auth token goes in secrets.toml ([stacks-node] auth_token).

[stacks-signer]
mode = "disabled"
# version = "3.2.0.0.1.0"

[stacks-api]
mode = "enabled"
# version = "8.1.0"

[stacks-mesh-api]
mode = "disabled"

# PoX-5 operations dashboard for this deployment's signer and pool
# (github.com/stx-labs/signer-sidekick). Requires a node and a signer.
[signer-sidekick]
mode = "disabled"
# manager_principal = "SP....signer-manager"  # your deployed PoX-5 signer-manager (required)
# engine_mode = "observe"                     # or "operator-run" (deliberate opt-in)
# The dashboard auth token goes in secrets.toml ([signer-sidekick] auth_token).

[postgres]
mode = "enabled"
# version = "17"
# For mode = "external":
# host = "pg.internal"
# port = 5432
# user = "stacks"
# password = "..."
"#;

/// Test helper: parse a stacks.toml string and resolve its (built-in) network.
#[cfg(test)]
pub fn test_deployment(toml_str: &str) -> Deployment {
    let mut d: Deployment = toml::from_str(toml_str).expect("test stacks.toml should parse");
    d.net = network::load(&d.network, Path::new(".")).expect("built-in network");
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deployment(toml_str: &str) -> Deployment {
        test_deployment(toml_str)
    }

    fn errors(toml_str: &str) -> Vec<String> {
        deployment(toml_str).validate().0
    }

    fn warnings(toml_str: &str) -> Vec<String> {
        deployment(toml_str).validate().1
    }

    #[test]
    fn init_template_is_valid_and_clean() {
        let s = test_deployment(DEFAULT_STACK_TOML);
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
        assert!(e.iter().any(|m| m.contains("hosted burnchain")));
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
    fn image_with_tag_conflicts_with_version() {
        let e = errors(
            "network = \"testnet\"\n[postgres]\nmode = \"enabled\"\nimage = \"myorg/pg:17\"\nversion = \"17\"",
        );
        assert!(e.iter().any(|m| m.contains("already pins a tag")));
        // repo-only image + version is fine; full ref alone is fine
        assert!(errors("network = \"testnet\"\n[postgres]\nmode = \"enabled\"\nimage = \"myorg/pg\"\nversion = \"17\"").is_empty());
        assert!(
            errors(
                "network = \"testnet\"\n[postgres]\nmode = \"enabled\"\nimage = \"myorg/pg:17\""
            )
            .is_empty()
        );
    }

    #[test]
    fn external_bitcoind_credentials_must_come_in_pairs() {
        let base = "network = \"mainnet\"\n[bitcoind]\nmode = \"external\"\nhost = \"h\"";
        let e = errors(&format!("{base}\nrpc_user = \"u\""));
        assert!(e.iter().any(|m| m.contains("set together")));
        let e = errors(&format!("{base}\nrpc_password = \"p\""));
        assert!(e.iter().any(|m| m.contains("set together")));
        assert!(errors(&format!("{base}\nrpc_user = \"u\"\nrpc_password = \"p\"")).is_empty());
        assert!(errors(base).is_empty());
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
    fn signer_with_external_node_warns_about_apply_snippet() {
        let w = warnings(
            "network = \"testnet\"\n[stacks-node]\nmode = \"external\"\nrpc_host = \"h\"\n[stacks-signer]\nmode = \"enabled\"",
        );
        assert!(w.iter().any(|m| m.contains("apply-to-your-node")));
    }

    #[test]
    fn external_node_with_managed_receivers_warns_about_unpublished_ports() {
        let w = warnings(
            "network = \"testnet\"\n[stacks-node]\nmode = \"external\"\nrpc_host = \"h\"\n[stacks-api]\nmode = \"enabled\"\n[postgres]\nmode = \"enabled\"",
        );
        assert!(
            w.iter().any(|m| m.contains("event port (3700)")),
            "got: {w:?}"
        );
        let w = warnings(
            "network = \"testnet\"\n[stacks-node]\nmode = \"external\"\nrpc_host = \"h\"\n[stacks-signer]\nmode = \"enabled\"",
        );
        assert!(
            w.iter().any(|m| m.contains("event endpoint (port 30000)")),
            "got: {w:?}"
        );
    }

    #[test]
    fn deployment_name_and_offset_are_validated() {
        let e = errors("name = \"Bad_Name\"\nnetwork = \"testnet\"");
        assert!(e.iter().any(|m| m.contains("name `Bad_Name` is invalid")));
        let e = errors("name = \"-leading\"\nnetwork = \"testnet\"");
        assert!(e.iter().any(|m| m.contains("is invalid")));
        let e = errors("port_offset = 50000\nnetwork = \"testnet\"");
        assert!(e.iter().any(|m| m.contains("port_offset")));
        assert!(
            errors("name = \"testnet-b\"\nport_offset = 100\nnetwork = \"testnet\"").is_empty()
        );
    }

    #[test]
    fn sidekick_requires_signer_node_and_manager() {
        let base = "network = \"testnet\"\n[signer-sidekick]\nmode = \"enabled\"";
        let e = errors(base);
        assert!(
            e.iter().any(|m| m.contains("requires a stacks-node")),
            "got: {e:?}"
        );
        assert!(
            e.iter()
                .any(|m| m.contains("monitors this deployment's signer"))
        );
        assert!(e.iter().any(|m| m.contains("requires manager_principal")));

        let bad_principal = "network = \"testnet\"\n[stacks-node]\nmode = \"enabled\"\nrole = \"signer-host\"\n[stacks-signer]\nmode = \"enabled\"\n[signer-sidekick]\nmode = \"enabled\"\nmanager_principal = \"not-a-principal\"";
        assert!(
            errors(bad_principal)
                .iter()
                .any(|m| m.contains("not a contract principal"))
        );

        let bad_engine = "network = \"testnet\"\n[stacks-node]\nmode = \"enabled\"\nrole = \"signer-host\"\n[stacks-signer]\nmode = \"enabled\"\n[signer-sidekick]\nmode = \"enabled\"\nmanager_principal = \"SP000000000000000000002Q6VF78.signer-manager\"\nengine_mode = \"autopilot\"";
        assert!(errors(bad_engine).iter().any(|m| m.contains("engine_mode")));

        let external = "network = \"testnet\"\n[signer-sidekick]\nmode = \"external\"\nmanager_principal = \"SP000000000000000000002Q6VF78.signer-manager\"";
        assert!(errors(external).iter().any(|m| m.contains("not supported")));

        let ok = "network = \"testnet\"\n[stacks-node]\nmode = \"enabled\"\nrole = \"signer-host\"\n[stacks-signer]\nmode = \"enabled\"\n[signer-sidekick]\nmode = \"enabled\"\nmanager_principal = \"SP000000000000000000002Q6VF78.signer-manager\"";
        assert!(errors(ok).is_empty(), "got: {:?}", errors(ok));
    }

    #[test]
    fn sidekick_auth_token_is_required_and_leak_checked() {
        let d = deployment(
            "network = \"testnet\"\n[stacks-node]\nmode = \"enabled\"\nrole = \"signer-host\"\n[stacks-signer]\nmode = \"enabled\"\n[signer-sidekick]\nmode = \"enabled\"\nmanager_principal = \"SP000000000000000000002Q6VF78.signer-manager\"",
        );
        let err = d
            .check_required_secrets(Path::new("."))
            .unwrap_err()
            .to_string();
        assert!(err.contains("[signer-sidekick]\nauth_token"), "got: {err}");

        let mut leaky = deployment("network = \"testnet\"");
        leaky.signer_sidekick.auth_token = Some("x".into());
        leaky.signer_sidekick.stacks_api_key = Some("y".into());
        let leaks = leaky.config_secret_leaks();
        assert!(leaks.contains(&("signer-sidekick", "auth_token")));
        assert!(leaks.contains(&("signer-sidekick", "stacks_api_key")));
    }

    #[test]
    fn secrets_in_stacks_toml_are_detected_as_leaks() {
        let d = deployment(
            "network = \"mainnet\"\n[bitcoind]\nmode = \"enabled\"\nrpc_password = \"p\"\n[postgres]\nmode = \"enabled\"\npassword = \"x\"\n[stacks-node]\nmode = \"enabled\"\nauth_token = \"t\"",
        );
        let leaks = d.config_secret_leaks();
        assert!(leaks.contains(&("bitcoind", "rpc_password")));
        assert!(leaks.contains(&("postgres", "password")));
        assert!(leaks.contains(&("stacks-node", "auth_token")));
        // a clean config has no leaks
        assert!(
            deployment("network = \"testnet\"\n[stacks-node]\nmode = \"enabled\"")
                .config_secret_leaks()
                .is_empty()
        );
    }

    #[test]
    fn required_secrets_depend_on_enabled_services() {
        // enabled node + postgres, empty overlay -> both reported missing
        let d = deployment(
            "network = \"testnet\"\n[stacks-node]\nmode = \"enabled\"\n[postgres]\nmode = \"enabled\"",
        );
        let err = d
            .check_required_secrets(Path::new("."))
            .unwrap_err()
            .to_string();
        assert!(err.contains("[postgres]\npassword"));
        assert!(err.contains("[stacks-node]\nauth_token"));

        // external node pushing to a managed signer still needs the token
        let d = deployment(
            "network = \"testnet\"\n[stacks-node]\nmode = \"external\"\nrpc_host = \"h\"\n[stacks-signer]\nmode = \"enabled\"",
        );
        let err = d
            .check_required_secrets(Path::new("."))
            .unwrap_err()
            .to_string();
        assert!(err.contains("[stacks-node]\nauth_token"));

        // merged overlay satisfies the requirements
        let mut d = deployment(
            "network = \"testnet\"\n[stacks-node]\nmode = \"enabled\"\n[postgres]\nmode = \"enabled\"",
        );
        d.merge_secrets(
            toml::from_str("[postgres]\npassword = \"p\"\n[stacks-node]\nauth_token = \"t\"")
                .unwrap(),
        );
        assert!(d.check_required_secrets(Path::new(".")).is_ok());
    }
}
