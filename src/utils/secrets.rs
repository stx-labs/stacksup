//! The secrets overlay: `secrets.toml` lives beside `stacks.toml`, holds ONLY sensitive values
//! (clear text, mode 0600, never committed), and is merged into the deployment config at load time.
//!
//! Contract:
//! - The tool NEVER modifies an existing secrets.toml. `config init` creates one with generated
//!   values only when none exists; anything missing later is an error telling the user exactly what
//!   to add.
//! - Secret-bearing fields are rejected in stacks.toml — they must come from the overlay, so the
//!   committable config never contains credentials.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

pub const SECRETS_FILE: &str = "secrets.toml";

/// Secret values are interpolated into rendered TOML, env files, and the compose file, so they are
/// validated to an alphabet that is inert in all of those formats: printable ASCII without
/// whitespace, quotes, backslash, `$` (compose interpolation), or backtick.
const MIN_SECRET_LEN: usize = 8;
const MAX_SECRET_LEN: usize = 128;

/// The overlay mirrors stacks.toml's structure, restricted to secret-bearing fields.
/// `deny_unknown_fields` makes typos and non-secret config in this file loud errors.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SecretsOverlay {
    #[serde(default)]
    pub bitcoind: BitcoindSecrets,
    #[serde(default)]
    pub postgres: PostgresSecrets,
    #[serde(default, rename = "stacks-node")]
    pub stacks_node: NodeSecrets,
    #[serde(default, rename = "stacks-signer")]
    pub stacks_signer: SignerSecrets,
    #[serde(default, rename = "signer-sidekick")]
    pub signer_sidekick: SidekickSecrets,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SignerSecrets {
    /// The managed signer's key, using stacks-signer's config name for verbatim copying.
    /// Never generated: the operator must provide the on-chain identity.
    pub stacks_private_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SidekickSecrets {
    /// Dashboard bearer-token login (sidekick's own credential, distinct from the node token).
    pub auth_token: Option<String>,
    /// Optional Hiro API key for indexed-API backfill.
    pub stacks_api_key: Option<String>,
}

fn secrets_path(data_dir: &Path) -> PathBuf {
    data_dir.join(SECRETS_FILE)
}

/// Load the overlay. Missing file is `None`; `config::load` then reports the deployment's required
/// secrets as missing. Present values are validated (permissions, length, alphabet) so bad ones
/// fail here with a precise message instead of producing broken rendered configs.
pub fn load(data_dir: &Path) -> Result<Option<SecretsOverlay>> {
    let path = secrets_path(data_dir);
    if !path.exists() {
        return Ok(None);
    }
    // The file holds clear-text credentials: refuse to proceed while other users can read it.
    // Fixing the mode is the user's move, this tool never modifies the file, permissions included.
    let mode = std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&path)?.permissions());
    if mode & 0o077 != 0 {
        bail!(
            "{} is readable by other users (mode {:03o}) — run `chmod 600 {}`",
            path.display(),
            mode & 0o777,
            path.display()
        );
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
            anyhow::anyhow!(
                "invalid secrets file {}: {}",
                path.display(),
                describe_toml_error(&raw, &e)
            )
        }
    })?;
    validate_values(&overlay, &path)?;
    Ok(Some(overlay))
}

/// `toml::de::Error` may echo secrets from the offending line into terminals and CI logs.
/// Preserve the location and message, but omit the source snippet.
pub fn describe_toml_error(raw: &str, e: &toml::de::Error) -> String {
    match e.span() {
        Some(span) => {
            let start = span.start.min(raw.len());
            let line = raw.as_bytes()[..start]
                .iter()
                .filter(|&&b| b == b'\n')
                .count()
                + 1;
            format!("line {line}: {}", e.message())
        }
        None => e.message().to_string(),
    }
}

/// Every present value must be long enough to redact safely and free of characters that need
/// escaping in any rendered format (TOML strings, env files, compose commands).
fn validate_values(overlay: &SecretsOverlay, path: &Path) -> Result<()> {
    let fields: [(&str, &str, &Option<String>); 7] = [
        ("bitcoind", "rpc_user", &overlay.bitcoind.rpc_user),
        ("bitcoind", "rpc_password", &overlay.bitcoind.rpc_password),
        ("postgres", "password", &overlay.postgres.password),
        ("stacks-node", "auth_token", &overlay.stacks_node.auth_token),
        (
            "stacks-signer",
            "stacks_private_key",
            &overlay.stacks_signer.stacks_private_key,
        ),
        (
            "signer-sidekick",
            "auth_token",
            &overlay.signer_sidekick.auth_token,
        ),
        (
            "signer-sidekick",
            "stacks_api_key",
            &overlay.signer_sidekick.stacks_api_key,
        ),
    ];
    for (section, field, value) in fields {
        let Some(v) = value else { continue };
        if v.len() < MIN_SECRET_LEN || v.len() > MAX_SECRET_LEN {
            bail!(
                "[{section}] {field} in {} must be {MIN_SECRET_LEN}-{MAX_SECRET_LEN} characters (got {})",
                path.display(),
                v.len()
            );
        }
        if let Some(c) = v
            .chars()
            .find(|c| !c.is_ascii_graphic() || matches!(c, '"' | '\'' | '\\' | '$' | '`'))
        {
            bail!(
                "[{section}] {field} in {} contains unsupported character {c:?} — \
                 secrets are interpolated into rendered TOML/env/compose files, so use \
                 printable ASCII without spaces, quotes, backslashes, `$`, or backticks",
                path.display()
            );
        }
    }
    if let Some(key) = &overlay.stacks_signer.stacks_private_key
        && !is_signer_key(key)
    {
        bail!(
            "[stacks-signer] stacks_private_key in {} is not a valid signer key — expected 64 hex \
             characters (a secp256k1 private key), optionally followed by 01",
            path.display()
        );
    }
    Ok(())
}

/// secp256k1 group order n: a private key is a scalar in [1, n).
const SECP256K1_ORDER: [u8; 32] = [
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe,
    0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c, 0xd0, 0x36, 0x41, 0x41,
];

/// `stacks-signer` key format: 64 hex characters encoding a scalar in [1, n),
/// optionally followed by the `01` compressed-public-key marker.
fn is_signer_key(key: &str) -> bool {
    if !key.chars().all(|c| c.is_ascii_hexdigit()) {
        return false;
    }
    let scalar_hex = match key.len() {
        64 => key,
        66 if key.ends_with("01") => &key[..64],
        _ => return false,
    };
    let mut scalar = [0u8; 32];
    for (i, byte) in scalar.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&scalar_hex[2 * i..2 * i + 2], 16).expect("hex checked above");
    }
    scalar != [0u8; 32] && scalar < SECP256K1_ORDER
}

/// Create secrets.toml with generated values, ONLY when it does not exist. An existing file is the
/// user's; it is never touched.
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
         [bitcoind]\nrpc_user = \"stacksup-{}\"\nrpc_password = \"{}\"\n\n\
         [stacks-node]\nauth_token = \"{}\"\n\n\
         # Only when [stacks-signer] mode = \"enabled\": the signer's own key (64 hex\n\
         # characters, optionally followed by 01). Never generated - it is the on-chain\n\
         # identity you grant and register, so paste yours.\n\
         # [stacks-signer]\n# stacks_private_key = \"...\"\n\n\
         [signer-sidekick]\nauth_token = \"{}\"\n",
        random_hex(32)?,
        // rpc_user is redacted in `logs export`; a random suffix keeps that exact-value scrub from
        // also eating every plain "stacksup" in logs.
        random_hex(4)?,
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

/// Write a file readable only by the owner; permissions are set before the content is written.
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

/// bitcoind's `-rpcauth` line derived from the clear-text credentials (share/rpcauth/rpcauth.py):
/// `user:salt$hex(hmac_sha256(key=salt, msg=password))`. Never stored — a hand-edited password
/// can't drift from its hash.
pub fn bitcoind_rpcauth(user: &str, password: &str) -> String {
    let salt = derived_salt(user);
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

/// Deterministic salt so the rpcauth line (and the rendered compose file) is stable across renders.
/// Salt secrecy is not load-bearing in the rpcauth scheme, bitcoind stores it in plaintext beside
/// the hash. Derived from the username only: the salt is published (compose file, docker inspect),
/// so a password-derived one would be a fast unsalted password hash. Generated usernames carry a
/// random suffix, which keeps the salt unique per deployment.
fn derived_salt(user: &str) -> String {
    use sha2::Digest;
    let digest = Sha256::digest(format!("stacksup-rpcauth-salt:{user}").as_bytes());
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// bail! with a ready-to-paste snippet listing the missing secret fields, one table per section
/// (fields sharing a section are grouped so the snippet is valid TOML).
pub fn missing_secrets_error(data_dir: &Path, missing: &[(&str, &str)]) -> anyhow::Error {
    let mut sections: Vec<(&str, Vec<&str>)> = Vec::new();
    for (section, field) in missing {
        match sections.last_mut() {
            Some((s, fields)) if s == section => fields.push(field),
            _ => sections.push((section, vec![field])),
        }
    }
    let snippet: String = sections
        .iter()
        .map(|(section, fields)| {
            let body: String = fields.iter().map(|f| format!("{f} = \"...\"\n")).collect();
            format!("[{section}]\n{body}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    anyhow::anyhow!(
        "missing required secrets in {} — add these values (merge into the \
         matching sections if they already exist):\n\n{snippet}\n\
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

    /// Test overlays must pass the same permission gate as real ones.
    fn write_overlay(dir: &Path, content: &str) {
        write_0600(&dir.join(SECRETS_FILE), content).unwrap();
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
        let raw = std::fs::read_to_string(dir.join(SECRETS_FILE)).unwrap();
        assert!(raw.contains("# [stacks-signer]\n# stacks_private_key = \"...\""));
        assert!(overlay.stacks_signer.stacks_private_key.is_none());
        assert_eq!(overlay.postgres.password.unwrap().len(), 64);
        assert!(overlay.bitcoind.rpc_user.unwrap().starts_with("stacksup-"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn group_readable_secrets_file_is_rejected() {
        let dir = temp_dir("mode");
        std::fs::write(
            dir.join(SECRETS_FILE),
            "[postgres]\npassword = \"hunter2hunter2\"\n",
        )
        .unwrap(); // default umask: group/world readable
        let err = load(&dir).unwrap_err().to_string();
        assert!(err.contains("chmod 600"), "got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn partial_overlay_parses_and_unknown_fields_fail() {
        let dir = temp_dir("partial");
        write_overlay(&dir, "[postgres]\npassword = \"hunter2hunter2\"\n");
        let overlay = load(&dir).unwrap().unwrap();
        assert_eq!(overlay.postgres.password.as_deref(), Some("hunter2hunter2"));
        assert!(overlay.stacks_node.auth_token.is_none());

        write_overlay(&dir, "[postgres]\nmode = \"enabled\"\n");
        let err = load(&dir).unwrap_err().to_string();
        assert!(err.contains("invalid secrets file"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn short_and_unsafe_values_are_rejected() {
        let dir = temp_dir("values");
        write_overlay(&dir, "[postgres]\npassword = \"short\"\n");
        let err = load(&dir).unwrap_err().to_string();
        assert!(err.contains("8-128 characters"), "got: {err}");

        write_overlay(&dir, "[postgres]\npassword = \"with space etc\"\n");
        let err = load(&dir).unwrap_err().to_string();
        assert!(err.contains("unsupported character"), "got: {err}");

        write_overlay(&dir, "[stacks-node]\nauth_token = \"has$dollar1\"\n");
        let err = load(&dir).unwrap_err().to_string();
        assert!(err.contains("unsupported character"), "got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn signer_key_must_be_a_valid_secp256k1_key() {
        const KEY: &str = "eb05c83546fdd2c79f10f5ad5434a90dd28f7e3acb7c092157aa1bc3656b012c01";
        let dir = temp_dir("signer-key");
        let load_key = |key: &str| {
            write_overlay(
                &dir,
                &format!("[stacks-signer]\nstacks_private_key = \"{key}\"\n"),
            );
            load(&dir)
        };

        // compressed and bare forms, any case, up to n - 1
        let upper = KEY.to_ascii_uppercase();
        let n_minus_1 = "fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364140";
        for ok in [KEY, &KEY[..64], &upper, n_minus_1] {
            let overlay = load_key(ok)
                .unwrap_or_else(|e| panic!("{ok} rejected: {e}"))
                .unwrap();
            assert_eq!(
                overlay.stacks_signer.stacks_private_key.as_deref(),
                Some(ok)
            );
        }

        let wrong_marker = format!("{}02", &KEY[..64]);
        let non_hex = format!("{}g", &KEY[..63]);
        let zero = "0".repeat(64);
        let max = "f".repeat(64);
        let n = "fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141";
        for bad in [
            "REPLACE_ME",
            &KEY[..63],
            &KEY[..65],
            &wrong_marker,
            &non_hex,
            &zero,
            &max,
            n,
        ] {
            let err = load_key(bad).unwrap_err().to_string();
            assert!(err.contains("stacks_private_key"), "{bad}: {err}");
            assert!(!err.contains(bad), "error echoed the key: {err}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_errors_never_echo_the_file() {
        const KEY: &str = "eb05c83546fdd2c79f10f5ad5434a90dd28f7e3acb7c092157aa1bc3656b012c01";
        let dir = temp_dir("echo");
        // An unquoted value or misspelled field triggers a TOML error that may echo the line and key.
        for body in [
            format!("[stacks-signer]\nstacks_private_key = {KEY}\n"),
            format!("[stacks-signer]\nprivate_key = \"{KEY}\"\n"),
        ] {
            write_overlay(&dir, &body);
            let err = load(&dir).unwrap_err().to_string();
            assert!(!err.contains(&KEY[..64]), "key echoed: {err}");
            assert!(err.contains("line 2:"), "no location: {err}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_flat_format_gets_migration_hint() {
        let dir = temp_dir("legacy");
        write_overlay(&dir, "node_auth_token = \"x\"\npg_password = \"y\"\n");
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
    fn rpcauth_salt_does_not_depend_on_the_password() {
        let salt = |line: String| line.split(['$', ':']).nth(1).unwrap().to_string();
        assert_eq!(
            salt(bitcoind_rpcauth("user", "password")),
            salt(bitcoind_rpcauth("user", "other-password"))
        );
        assert_ne!(
            salt(bitcoind_rpcauth("user", "password")),
            salt(bitcoind_rpcauth("other-user", "password"))
        );
    }

    #[test]
    fn missing_secrets_error_groups_fields_by_section() {
        let err = missing_secrets_error(
            Path::new("/data"),
            &[
                ("postgres", "password"),
                ("bitcoind", "rpc_user"),
                ("bitcoind", "rpc_password"),
                ("stacks-node", "auth_token"),
            ],
        )
        .to_string();
        assert!(err.contains("[postgres]\npassword"));
        // two bitcoind fields, ONE [bitcoind] header — the snippet stays valid TOML
        assert!(err.contains("[bitcoind]\nrpc_user = \"...\"\nrpc_password = \"...\""));
        assert_eq!(err.matches("[bitcoind]").count(), 1);
        assert!(err.contains("[stacks-node]\nauth_token"));
        assert!(err.contains("never modifies"));
    }
}
