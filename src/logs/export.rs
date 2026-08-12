//! `stacksup logs export` — package logs (and diagnostic context) into a shareable file for
//! troubleshooting.
//!
//! Default output is a support bundle: per-service logs plus versions, ps, images, redacted
//! configs, and the chainstate/config check reports. All text passes through redaction (known
//! secret values + password/token/key lines) so the artifact is safe to hand to someone else —
//! though the final message still tells the user to review it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use colored::Colorize;

use crate::config::{Deployment, ServiceMode};
use crate::utils::services::roster;

pub struct Opts {
    pub service: Option<String>,
    pub since: String,
    pub logs_only: bool,
    pub out: Option<PathBuf>,
}

pub fn run(deployment: &Deployment, config_path: &Path, data_dir: &Path, opts: Opts) -> Result<()> {
    crate::utils::docker::preflight(data_dir)?;

    let services: Vec<String> = match &opts.service {
        Some(name) => {
            crate::utils::docker::ensure_enabled(deployment, name)?;
            vec![name.clone()]
        }
        None => roster(deployment)
            .into_iter()
            .filter(|(_, m)| *m == ServiceMode::Enabled)
            .map(|(n, _)| n.to_string())
            .collect(),
    };
    if services.is_empty() {
        bail!("no enabled services in stacks.toml — nothing to export");
    }

    // Containers that still exist (logs live inside them). `stacksup stop`
    // keeps them; `stacksup stop --destroy` removes them along with their logs.
    let existing: Vec<String> =
        crate::utils::docker::compose_capture(data_dir, &["ps", "-a", "--services"])?
            .lines()
            .map(str::to_owned)
            .collect();
    if !services.iter().any(|s| existing.contains(s)) {
        bail!(
            "no containers exist for {} — logs are removed by `stacksup stop --destroy`; \
             reproduce the issue, then export before destroying the stack",
            services.join(", ")
        );
    }

    let secrets = secret_values(deployment);
    let ts = timestamp();
    let network = deployment.network.as_str();

    // Single service + --logs-only: a plain .log file, no archive.
    if opts.logs_only && services.len() == 1 {
        let name = &services[0];
        let text = redact(&capture_logs(data_dir, name, &opts.since), &secrets);
        let out = opts
            .out
            .unwrap_or_else(|| PathBuf::from(format!("{name}-{ts}.log")));
        fs::write(&out, text)?;
        println!("Wrote {}", out.display());
        print_review_note();
        return Ok(());
    }

    // Everything else: a tar.gz bundle assembled in a temp dir.
    let tmp = data_dir.join(".export-tmp");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(tmp.join("logs"))?;

    println!("Collecting logs (--since {})...", opts.since);
    for name in &services {
        let text = if existing.contains(name) {
            capture_logs(data_dir, name, &opts.since)
        } else {
            "no container for this service (never started, or removed by `stacksup stop --destroy`)\n"
                .to_string()
        };
        fs::write(
            tmp.join("logs").join(format!("{name}.log")),
            redact(&text, &secrets),
        )?;
    }

    if !opts.logs_only {
        println!("Collecting diagnostics...");
        let meta = format!(
            "stacks-tool version: {}\nnetwork: {network}\ncreated: {ts}\ndocker: {}\ncompose: {}\nservices: {}\n",
            env!("CARGO_PKG_VERSION"),
            crate::utils::docker::daemon_version().unwrap_or_else(|e| format!("unavailable ({e})")),
            crate::utils::docker::compose_version()
                .unwrap_or_else(|e| format!("unavailable ({e})")),
            services.join(", "),
        );
        fs::write(tmp.join("meta.txt"), meta)?;
        fs::write(
            tmp.join("ps.txt"),
            redact(
                &crate::utils::docker::compose_capture(data_dir, &["ps", "-a"])?,
                &secrets,
            ),
        )?;
        fs::write(
            tmp.join("images.txt"),
            redact(
                &crate::utils::docker::compose_capture(data_dir, &["images"])?,
                &secrets,
            ),
        )?;

        // Config context: stacks.toml + everything in rendered/, redacted.
        fs::create_dir_all(tmp.join("config/rendered"))?;
        if let Ok(raw) = fs::read_to_string(config_path) {
            fs::write(tmp.join("config/stacks.toml"), redact(&raw, &secrets))?;
        }
        copy_redacted_tree(
            &data_dir.join("rendered"),
            &tmp.join("config/rendered"),
            &secrets,
        )?;

        // Best-effort reports via self-invocation: a broken stack is exactly
        // when these might fail, and a "failed: ..." file still helps.
        fs::write(
            tmp.join("chainstate-status.txt"),
            redact(
                &self_report(config_path, data_dir, &["chainstate", "status"]),
                &secrets,
            ),
        )?;
        fs::write(
            tmp.join("config-check.txt"),
            redact(
                &self_report(config_path, data_dir, &["config", "check"]),
                &secrets,
            ),
        )?;
    }

    let out = opts
        .out
        .unwrap_or_else(|| PathBuf::from(format!("stacks-support-{network}-{ts}.tar.gz")));
    let file =
        fs::File::create(&out).with_context(|| format!("cannot create {}", out.display()))?;
    let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut tar = tar::Builder::new(enc);
    tar.append_dir_all("stacks-support", &tmp)?;
    tar.into_inner()?.finish()?;
    let _ = fs::remove_dir_all(&tmp);

    println!("\n{} {}", "Wrote".green(), out.display());
    print_review_note();
    Ok(())
}

fn print_review_note() {
    println!(
        "{}",
        "Secrets (passwords, tokens, keys) were redacted — still, review the contents before sharing publicly."
            .yellow()
    );
}

fn capture_logs(data_dir: &Path, service: &str, since: &str) -> String {
    crate::utils::docker::compose_capture(
        data_dir,
        &[
            "logs",
            "--no-color",
            "--timestamps",
            "--since",
            since,
            service,
        ],
    )
    .unwrap_or_else(|e| format!("failed to collect logs: {e}\n"))
}

/// Run one of our own subcommands and capture its report (colored degrades to
/// plain text automatically because the output is not a terminal).
fn self_report(config_path: &Path, data_dir: &Path, args: &[&str]) -> String {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return format!("unavailable: {e}\n"),
    };
    let out = Command::new(exe)
        .arg("-c")
        .arg(config_path)
        .arg("--data-dir")
        .arg(data_dir)
        .args(args)
        .output();
    match out {
        Ok(o) => {
            let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
            let err = String::from_utf8_lossy(&o.stderr);
            if !err.trim().is_empty() {
                text.push_str(&err);
            }
            text
        }
        Err(e) => format!("failed to run: {e}\n"),
    }
}

/// Secret *values* known from the config, scrubbed wherever they appear
/// (configs and logs alike — the node echoes config at startup).
fn secret_values(deployment: &Deployment) -> Vec<String> {
    // Post-merge, every secret lives on the deployment itself.
    let mut v: Vec<String> = [
        deployment.postgres.password.clone(),
        deployment.bitcoind.rpc_user.clone(),
        deployment.bitcoind.rpc_password.clone(),
        deployment.stacks_node.auth_token.clone(),
    ]
    .into_iter()
    .flatten()
    .collect();
    if let (Some(user), Some(password)) = (
        &deployment.bitcoind.rpc_user,
        &deployment.bitcoind.rpc_password,
    ) {
        v.push(crate::utils::secrets::bitcoind_rpcauth(user, password));
    }
    // Longest first: the rpcauth line embeds the username, so scrubbing the
    // shorter username first would split it and leave the verifier exposed.
    v.sort_by_key(|s| std::cmp::Reverse(s.len()));
    v.retain(|s| s.len() >= 4);
    v
}

/// Two-pass scrub: exact secret values anywhere, then any line whose key
/// looks credential-ish (covers defaults we didn't collect and container
/// startup echoes).
fn redact(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_string();
    for secret in secrets {
        out = out.replace(secret, "<redacted>");
    }
    out.lines()
        .map(|line| {
            let sep = line.find(['=', ':']);
            if let Some(idx) = sep {
                let key = line[..idx].trim().to_ascii_lowercase();
                if [
                    "password",
                    "auth_token",
                    "private_key",
                    "secret",
                    "rpcpassword",
                ]
                .iter()
                .any(|k| key.ends_with(k) || key.contains(&format!("{k} ")))
                    || key.ends_with("auth_password")
                {
                    return format!("{}{} <redacted>", &line[..idx], &line[idx..=idx]);
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn copy_redacted_tree(src: &Path, dst: &Path, secrets: &[String]) -> Result<()> {
    if !src.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            fs::create_dir_all(&target)?;
            copy_redacted_tree(&path, &target, secrets)?;
        } else if let Ok(text) = fs::read_to_string(&path) {
            fs::write(&target, redact(&text, secrets))?;
        }
    }
    Ok(())
}

fn timestamp() -> String {
    Command::new("date")
        .arg("+%Y%m%d-%H%M%S")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| {
            format!(
                "epoch-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_secret_values_everywhere() {
        let out = redact("connecting with s3cretpw now", &["s3cretpw".to_string()]);
        assert_eq!(out, "connecting with <redacted> now\n");
    }

    #[test]
    fn copies_tree_with_redaction() {
        let src = std::env::temp_dir().join(format!("stacksup-export-src-{}", std::process::id()));
        let dst = std::env::temp_dir().join(format!("stacksup-export-dst-{}", std::process::id()));
        for d in [&src, &dst] {
            let _ = fs::remove_dir_all(d);
        }
        fs::create_dir_all(src.join("nested")).unwrap();
        fs::write(
            src.join("nested/app.env"),
            "PG_PASSWORD=hunter2\nPG_HOST=postgres\n",
        )
        .unwrap();
        copy_redacted_tree(&src, &dst, &[]).unwrap();
        let copied = fs::read_to_string(dst.join("nested/app.env")).unwrap();
        assert!(copied.contains("PG_PASSWORD= <redacted>"));
        assert!(copied.contains("PG_HOST=postgres"));
        for d in [&src, &dst] {
            let _ = fs::remove_dir_all(d);
        }
    }

    #[test]
    fn redacts_credential_lines() {
        let secrets = vec![];
        assert!(redact("PG_PASSWORD=hunter2", &secrets).contains("PG_PASSWORD= <redacted>"));
        assert!(redact("auth_token = \"abc\"", &secrets).contains("auth_token = <redacted>"));
        assert!(redact("stacks_private_key = \"AA\"", &secrets).contains("<redacted>"));
        assert!(
            redact("POSTGRES_PASSWORD: postgres", &secrets)
                .contains("POSTGRES_PASSWORD: <redacted>")
        );
        assert!(redact("auth_password = \"x\"", &secrets).contains("<redacted>"));
        // non-credential lines untouched
        assert_eq!(
            redact("rpc_bind = \"0.0.0.0:20443\"", &secrets),
            "rpc_bind = \"0.0.0.0:20443\"\n"
        );
    }
}
