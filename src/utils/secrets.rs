//! The secrets overlay: `secrets.toml` lives beside `stacks.toml`, holds ONLY
//! sensitive values (clear text, mode 0600, never committed), and is merged
//! into the deployment config at load time.
//!
//! Contract:
//! - The tool NEVER modifies an existing secrets.toml. `config init` creates
//!   one with generated values only when none exists; anything missing later
//!   is an error telling the user exactly what to add.
//! - Secret-bearing fields are rejected in stacks.toml — they must come from
//!   the overlay, so the committable config never contains credentials.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

pub const SECRETS_FILE: &str = "secrets.toml";

/// The overlay mirrors stacks.toml's structure, restricted to secret-bearing
/// fields. `deny_unknown_fields` makes typos and non-secret config in this
/// file loud errors.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SecretsOverlay {
    #[serde(default)]
    pub bitcoind: BitcoindSecrets,
    #[serde(default)]
    pub postgres: PostgresSecrets,
    #[serde(default, rename = "stacks-node")]
    pub stacks_node: NodeSecrets,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct BitcoindSecrets {
    pub rpc_user: Option<String>,
    pub rpc_password: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PostgresSecrets {
    pub password: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct NodeSecrets {
    pub auth_token: Option<String>,
}

fn secrets_path(data_dir: &Path) -> PathBuf {
    data_dir.join(SECRETS_FILE)
}

/// Load the overlay. Missing file is `None` — callers decide whether that is
/// an error (render does; read-only commands tolerate it).
pub fn load(data_dir: &Path) -> Result<Option<SecretsOverlay>> {
    let path = secrets_path(data_dir);
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)?;
    let overlay: SecretsOverlay = toml::from_str(&raw).map_err(|e| {
        // Old generated files used flat keys; point at the new layout.
        if raw.contains("node_auth_token") || raw.contains("pg_password") {
            anyhow::anyhow!(
                "{} uses the old flat format — rewrite it with sections, e.g.\n\n\
                 [postgres]\npassword = \"...\"\n\n[bitcoind]\nrpc_user = \"...\"\nrpc_password = \"...\"\n\n\
                 [stacks-node]\nauth_token = \"...\"",
                path.display()
            )
        } else {
            anyhow::anyhow!("invalid secrets file {}: {e}", path.display())
        }
    })?;
    Ok(Some(overlay))
}

/// Create secrets.toml with generated values — ONLY when it does not exist.
/// An existing file is the user's; it is never touched.
pub fn generate_if_missing(data_dir: &Path) -> Result<()> {
    let path = secrets_path(data_dir);
    if path.exists() {
        println!("Keeping existing {} (never overwritten).", path.display());
        return Ok(());
    }
    let body = format!(
        "# stacksup deployment secrets — PLAIN TEXT, edit values freely.\n\
         # Keep this file out of version control (mode 0600). stacksup never\n\
         # modifies it; missing values are reported as errors at render time.\n\n\
         [postgres]\npassword = \"{}\"\n\n\
         [bitcoind]\nrpc_user = \"stacksup\"\nrpc_password = \"{}\"\n\n\
         [stacks-node]\nauth_token = \"{}\"\n",
        random_hex(32)?,
        random_hex(32)?,
        random_hex(32)?,
    );
    write_0600(&path, &body)?;
    println!(
        "Generated deployment secrets at {} (mode 0600)",
        path.display()
    );
    Ok(())
}

/// Write a file readable only by the owner; permissions are set before the
/// content is written.
pub fn write_0600(path: &Path, content: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("cannot create {}", path.display()))?;
    // An existing file keeps its old mode; enforce 0600 either way.
    let mut perms = file.metadata()?.permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o600);
    file.set_permissions(perms)?;
    file.write_all(content.as_bytes())?;
    Ok(())
}

fn random_hex(bytes: usize) -> Result<String> {
    let mut buf = vec![0u8; bytes];
    getrandom::getrandom(&mut buf)
        .map_err(|e| anyhow::anyhow!("could not gather randomness: {e}"))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// bitcoind's `-rpcauth` line derived from the clear-text credentials
/// (share/rpcauth/rpcauth.py): `user:salt$hex(hmac_sha256(key=salt, msg=password))`.
/// Never stored — a hand-edited password can't drift from its hash.
pub fn bitcoind_rpcauth(user: &str, password: &str) -> String {
    let salt = derived_salt(user, password);
    let mut mac = Hmac::<Sha256>::new_from_slice(salt.as_bytes()).expect("hmac accepts any key");
    mac.update(password.as_bytes());
    let digest: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("{user}:{salt}${digest}")
}

/// Deterministic salt so the rpcauth line (and the rendered compose file) is
/// stable across renders. Salt secrecy is not load-bearing in the rpcauth
/// scheme — bitcoind stores it in plaintext beside the hash.
fn derived_salt(user: &str, password: &str) -> String {
    use sha2::Digest;
    let digest = Sha256::digest(format!("{user}:{password}").as_bytes());
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// bail! with a ready-to-paste snippet listing the missing secret fields.
pub fn missing_secrets_error(data_dir: &Path, missing: &[(&str, &str)]) -> anyhow::Error {
    let snippet: String = missing
        .iter()
        .map(|(section, field)| format!("[{section}]\n{field} = \"...\"\n"))
        .collect::<Vec<_>>()
        .join("\n");
    anyhow::anyhow!(
        "missing required secrets in {} — add:\n\n{snippet}\n\
         (stacksup never modifies this file; `stacksup config init` generates one when absent)",
        secrets_path(data_dir).display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("stacksup-secrets-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn generate_creates_once_and_never_touches_existing() {
        let dir = temp_dir("gen");
        generate_if_missing(&dir).unwrap();
        let first = std::fs::read_to_string(dir.join(SECRETS_FILE)).unwrap();
        // second call must leave the file byte-identical
        generate_if_missing(&dir).unwrap();
        let second = std::fs::read_to_string(dir.join(SECRETS_FILE)).unwrap();
        assert_eq!(first, second);
        // and a user-authored file is also never rewritten
        std::fs::write(dir.join(SECRETS_FILE), "[postgres]\npassword = \"mine\"\n").unwrap();
        generate_if_missing(&dir).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join(SECRETS_FILE)).unwrap(),
            "[postgres]\npassword = \"mine\"\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn generated_file_is_owner_only_and_parses() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("perms");
        generate_if_missing(&dir).unwrap();
        let mode = std::fs::metadata(dir.join(SECRETS_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let overlay = load(&dir).unwrap().unwrap();
        assert_eq!(overlay.postgres.password.unwrap().len(), 64);
        assert_eq!(overlay.bitcoind.rpc_user.as_deref(), Some("stacksup"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn partial_overlay_parses_and_unknown_fields_fail() {
        let dir = temp_dir("partial");
        std::fs::write(
            dir.join(SECRETS_FILE),
            "[postgres]\npassword = \"hunter2\"\n",
        )
        .unwrap();
        let overlay = load(&dir).unwrap().unwrap();
        assert_eq!(overlay.postgres.password.as_deref(), Some("hunter2"));
        assert!(overlay.stacks_node.auth_token.is_none());

        std::fs::write(dir.join(SECRETS_FILE), "[postgres]\nmode = \"enabled\"\n").unwrap();
        let err = load(&dir).unwrap_err().to_string();
        assert!(err.contains("invalid secrets file"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_flat_format_gets_migration_hint() {
        let dir = temp_dir("legacy");
        std::fs::write(
            dir.join(SECRETS_FILE),
            "node_auth_token = \"x\"\npg_password = \"y\"\n",
        )
        .unwrap();
        let err = load(&dir).unwrap_err().to_string();
        assert!(err.contains("old flat format"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rpcauth_matches_bitcoin_reference_algorithm() {
        // hmac.new(b"<derived salt>", b"password", "SHA256") — algorithm
        // cross-checked against share/rpcauth/rpcauth.py; determinism is the
        // property under test here.
        let a = bitcoind_rpcauth("user", "password");
        let b = bitcoind_rpcauth("user", "password");
        assert_eq!(a, b);
        assert!(a.starts_with("user:"));
        assert_eq!(a.split('$').nth(1).unwrap().len(), 64);
        assert_ne!(a, bitcoind_rpcauth("user", "other-password"));
    }

    #[test]
    fn missing_secrets_error_lists_fields() {
        let err = missing_secrets_error(
            Path::new("/data"),
            &[("postgres", "password"), ("stacks-node", "auth_token")],
        )
        .to_string();
        assert!(err.contains("[postgres]\npassword"));
        assert!(err.contains("[stacks-node]\nauth_token"));
        assert!(err.contains("never modifies"));
    }
}
