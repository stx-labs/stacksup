//! `stacksup upgrade` — suggestions only, never applies anything.
//!
//! Compares each enabled service's configured version against what the image
//! registries offer and prints per-service guidance. Domain rules baked in:
//! same-major bumps are routine (edit stacks.toml, pull, restart); a MAJOR
//! stacks-api bump is DB-breaking and requires wiping the API's postgres data
//! and re-seeding via `chainstate download`; a major postgres bump requires a
//! data migration; node and signer versions should move together.

use anyhow::{Context, Result};
use colored::Colorize;

use crate::config::{Deployment, ServiceMode};
use crate::utils::versions::*;

struct Row {
    name: &'static str,
    current: Option<Vec<u64>>,
    current_note: &'static str,
    /// Tag pins only a major (e.g. postgres `17`): minors float with pulls,
    /// so only a new major is a real suggestion.
    major_pin: bool,
    same_major: Option<Vec<u64>>,
    next_major: Option<Vec<u64>>,
    error: Option<String>,
}

pub fn run(deployment: &Deployment, service: Option<&str>) -> Result<()> {
    let targets: Vec<(&'static str, ServiceMode, String)> = [
        (
            "bitcoind",
            deployment.bitcoind.mode,
            crate::utils::services::bitcoind_image(deployment),
        ),
        (
            "stacks-node",
            deployment.stacks_node.mode,
            crate::utils::services::stacks_node_image(deployment),
        ),
        (
            "stacks-signer",
            deployment.stacks_signer.mode,
            crate::utils::services::stacks_signer_image(deployment),
        ),
        (
            "stacks-api",
            deployment.stacks_api.mode,
            crate::utils::services::stacks_api_image(deployment),
        ),
        (
            "stacks-mesh-api",
            deployment.stacks_mesh_api.mode,
            crate::utils::services::stacks_mesh_api_image(deployment),
        ),
        (
            "postgres",
            deployment.postgres.mode,
            crate::utils::services::postgres_image(deployment),
        ),
    ]
    .into_iter()
    .filter(|(n, m, _)| *m == ServiceMode::Enabled && service.is_none_or(|s| s == *n))
    .collect();

    if targets.is_empty() {
        anyhow::bail!(match service {
            Some(s) => format!("`{s}` is not an enabled service in stacks.toml"),
            None => "no enabled services in stacks.toml".to_string(),
        });
    }

    println!("Checking registries for newer image versions...\n");

    let mut rows = Vec::new();
    for (name, _, image) in &targets {
        rows.push(check_service(name, image));
    }

    // Table
    println!(
        "{:<15} {:<12} {:<14} verdict",
        "service", "current", "available"
    );
    let mut upgrades = 0u32;
    for row in &rows {
        let current = row
            .current
            .as_ref()
            .map(|v| format!("{}{}", version_string(v), row.current_note))
            .unwrap_or_else(|| "unknown".into());
        let (available, verdict) = verdict(row, &mut upgrades);
        println!(
            "{:<15} {:<12} {:<14} {verdict}",
            row.name, current, available
        );
    }

    // Per-service guidance for anything actionable.
    let mut printed_header = false;
    for row in &rows {
        if let Some(text) = guidance(row) {
            if !printed_header {
                println!("\nguidance");
                printed_header = true;
            }
            println!("{text}");
        }
    }

    // Node/signer must move in lockstep.
    if let (Some(node), Some(signer)) = (
        rows.iter()
            .find(|r| r.name == "stacks-node")
            .and_then(|r| r.current.clone()),
        rows.iter()
            .find(|r| r.name == "stacks-signer")
            .and_then(|r| r.current.clone()),
    ) && node != signer
    {
        println!(
                "\n{}",
                format!(
                    "⚠ stacks-node ({}) and stacks-signer ({}) versions differ — they should be upgraded together",
                    version_string(&node),
                    version_string(&signer)
                )
                .yellow()
            );
    }

    if upgrades == 0 {
        println!("\n{}", "Everything is up to date.".green());
    } else {
        println!(
            "\n{}",
            format!("{upgrades} upgrade(s) available. This command only suggests — apply by editing stacks.toml.")
                .yellow()
        );
    }
    Ok(())
}

fn verdict(row: &Row, upgrades: &mut u32) -> (String, String) {
    if let Some(e) = &row.error {
        return ("-".into(), format!("⚠ {e}").yellow().to_string());
    }
    let Some(current) = &row.current else {
        return (
            row.same_major
                .as_ref()
                .or(row.next_major.as_ref())
                .map(|v| version_string(v))
                .unwrap_or_else(|| "-".into()),
            "⚠ current version unknown — pin a version or `stacksup pull`"
                .yellow()
                .to_string(),
        );
    };
    let newer_minor = row
        .same_major
        .as_ref()
        .filter(|v| !row.major_pin && compare_versions(v, current) == std::cmp::Ordering::Greater);
    let newer_major = row.next_major.as_ref();

    match (newer_minor, newer_major) {
        (None, None) if row.major_pin => {
            let latest = row
                .same_major
                .as_ref()
                .map(|v| version_string(v))
                .unwrap_or_default();
            (
                latest,
                "✓ tracking latest in this major".green().to_string(),
            )
        }
        (None, None) => ("-".into(), "✓ up to date".green().to_string()),
        (Some(m), None) => {
            *upgrades += 1;
            (
                version_string(m),
                "⬆ upgrade available".yellow().to_string(),
            )
        }
        (None, Some(mj)) => {
            *upgrades += 1;
            (
                version_string(mj),
                "⬆ new MAJOR available".yellow().to_string(),
            )
        }
        (Some(m), Some(mj)) => {
            *upgrades += 1;
            (
                format!("{} / {}", version_string(m), version_string(mj)),
                "⬆ upgrade + new MAJOR".yellow().to_string(),
            )
        }
    }
}

/// The domain knowledge: what upgrading actually entails per service.
fn guidance(row: &Row) -> Option<String> {
    let current = row.current.as_ref()?;
    let minor = row
        .same_major
        .as_ref()
        .filter(|v| !row.major_pin && compare_versions(v, current) == std::cmp::Ordering::Greater);
    let major = row.next_major.as_ref();
    if minor.is_none() && major.is_none() {
        return None;
    }
    let mut out = String::new();
    match row.name {
        "stacks-api" => {
            if let Some(m) = minor {
                out.push_str(&format!(
                    "  stacks-api {}: same-major — DB-compatible. Set [stacks-api] version = \"{}\", \
                     then `stacksup pull && stacksup stop && stacksup start`.\n",
                    version_string(m),
                    version_string(m)
                ));
            }
            if let Some(mj) = major {
                out.push_str(
                    &format!(
                        "  stacks-api {mj}: MAJOR — DB-BREAKING. The new API cannot migrate the old database:\n\
                             1. `stacksup stop`\n\
                             2. `stacksup chainstate wipe postgres` (API data only)\n\
                             3. set [stacks-api] version = \"{mj}\" and `stacksup pull`\n\
                             4. `stacksup chainstate download --service api` (archive matching the new major)\n\
                             5. `stacksup start`",
                        mj = version_string(mj)
                    )
                    .red()
                    .to_string(),
                );
            }
        }
        "postgres" => {
            if let Some(m) = minor {
                out.push_str(&format!(
                    "  postgres {}: patch/minor — safe. Set [postgres] version = \"{}\", then pull + restart.\n",
                    version_string(m),
                    version_string(m)
                ));
            }
            if let Some(mj) = major {
                out.push_str(&format!(
                    "  postgres {}: MAJOR — the data directory does not migrate itself. Either stay on \
                     the current major, or wipe + re-seed the API database after switching.",
                    version_string(mj)
                ));
            }
        }
        "stacks-node" | "stacks-signer" => {
            let v = minor.or(major)?;
            out.push_str(&format!(
                "  {}: set version = \"{}\" (upgrade stacks-node and stacks-signer together), \
                 then `stacksup pull && stacksup stop && stacksup start`. Chainstate migrates forward automatically.",
                row.name,
                version_string(v)
            ));
        }
        _ => {
            if let Some(m) = minor {
                out.push_str(&format!(
                    "  {}: set version = \"{}\", then `stacksup pull && stacksup stop && stacksup start`.\n",
                    row.name,
                    version_string(m)
                ));
            }
            if let Some(mj) = major {
                out.push_str(&format!(
                    "  {}: major {} also available — review its release notes before crossing majors.",
                    row.name,
                    version_string(mj)
                ));
            }
        }
    }
    Some(out.trim_end().to_string())
}

fn check_service(name: &'static str, image: &str) -> Row {
    let repo = image.rsplit_once(':').map(|(r, _)| r).unwrap_or(image);
    let tag = image_tag(image);

    // Current: full pin from the tag; a bare-major tag (`17`) is a deliberate
    // floating pin within that major; otherwise the pulled image's label.
    let (current, current_note, major_pin) = match parse_version(&tag) {
        Some(v) if v.len() >= 2 => (Some(v), "", false),
        Some(v) => (Some(v), ".x", true),
        None => match pulled_image_version(image) {
            Some(v) => (Some(v), " (pulled)", false),
            None => (None, "", false),
        },
    };

    let mut row = Row {
        name,
        current,
        current_note,
        major_pin,
        same_major: None,
        next_major: None,
        error: None,
    };

    let available = match registry_versions(repo) {
        Ok(v) if v.is_empty() => {
            row.error = Some("no version tags found in registry".into());
            return row;
        }
        Ok(v) => v,
        Err(e) => {
            row.error = Some(format!("registry check failed: {e:#}"));
            return row;
        }
    };

    if let Some(current) = &row.current {
        row.same_major = available
            .iter()
            .filter(|v| v[0] == current[0])
            .max()
            .cloned();
        row.next_major = available
            .iter()
            .filter(|v| v[0] > current[0])
            .max()
            .cloned();
    } else {
        row.same_major = available.iter().max().cloned();
    }
    row
}

/// Concrete version tags (>= 2 numeric components) available in the image's
/// registry. Anonymous APIs: Docker Hub's tag listing, GHCR's token + v2 flow.
fn registry_versions(repo: &str) -> Result<Vec<Vec<u64>>> {
    let tags = if let Some(path) = repo.strip_prefix("ghcr.io/") {
        ghcr_tags(path)?
    } else {
        let path = if repo.contains('/') {
            repo.to_string()
        } else {
            format!("library/{repo}")
        };
        dockerhub_tags(&path)?
    };
    Ok(tags
        .iter()
        .filter_map(|t| parse_version(t))
        .filter(|v| v.len() >= 2)
        .collect())
}

fn dockerhub_tags(path: &str) -> Result<Vec<String>> {
    let url = format!(
        "https://hub.docker.com/v2/repositories/{path}/tags?page_size=100&ordering=last_updated"
    );
    let body: serde_json::Value = ureq::get(&url)
        .timeout(std::time::Duration::from_secs(15))
        .call()
        .with_context(|| format!("docker hub: {path}"))?
        .into_json()?;
    Ok(body["results"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| r["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default())
}

fn ghcr_tags(path: &str) -> Result<Vec<String>> {
    let token_body: serde_json::Value = ureq::get(&format!(
        "https://ghcr.io/token?scope=repository:{path}:pull&service=ghcr.io"
    ))
    .timeout(std::time::Duration::from_secs(15))
    .call()
    .with_context(|| format!("ghcr token: {path}"))?
    .into_json()?;
    let token = token_body["token"]
        .as_str()
        .context("no ghcr token")?
        .to_string();
    let body: serde_json::Value = ureq::get(&format!("https://ghcr.io/v2/{path}/tags/list?n=200"))
        .set("Authorization", &format!("Bearer {token}"))
        .timeout(std::time::Duration::from_secs(15))
        .call()
        .with_context(|| format!("ghcr tags: {path}"))?
        .into_json()?;
    Ok(body["tags"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|t| t.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        current: Option<Vec<u64>>,
        major_pin: bool,
        same: Option<Vec<u64>>,
        next: Option<Vec<u64>>,
    ) -> Row {
        Row {
            name: "stacks-api",
            current,
            current_note: "",
            major_pin,
            same_major: same,
            next_major: next,
            error: None,
        }
    }

    fn verdict_text(r: &Row) -> (String, String, u32) {
        let mut upgrades = 0;
        let (available, verdict) = verdict(r, &mut upgrades);
        (available, verdict, upgrades)
    }

    #[test]
    fn up_to_date_counts_no_upgrade() {
        let (_, v, n) = verdict_text(&row(Some(vec![9, 0, 2]), false, Some(vec![9, 0, 2]), None));
        assert!(v.contains("up to date"));
        assert_eq!(n, 0);
    }

    #[test]
    fn minor_and_major_both_reported() {
        let (avail, v, n) = verdict_text(&row(
            Some(vec![8, 5, 0]),
            false,
            Some(vec![8, 15, 4]),
            Some(vec![9, 0, 2]),
        ));
        assert!(avail.contains("8.15.4") && avail.contains("9.0.2"));
        assert!(v.contains("upgrade + new MAJOR"));
        assert_eq!(n, 1);
    }

    #[test]
    fn major_pin_only_alerts_on_new_major() {
        // tracking `17`: newer 17.x is NOT an upgrade suggestion
        let (_, v, n) = verdict_text(&row(Some(vec![17]), true, Some(vec![17, 9]), None));
        assert!(v.contains("tracking latest"));
        assert_eq!(n, 0);
        let (_, v, n) = verdict_text(&row(
            Some(vec![17]),
            true,
            Some(vec![17, 9]),
            Some(vec![18, 4]),
        ));
        assert!(v.contains("new MAJOR"));
        assert_eq!(n, 1);
    }

    #[test]
    fn api_major_guidance_is_db_breaking_with_reseed_steps() {
        let text = guidance(&row(Some(vec![8, 5, 0]), false, None, Some(vec![9, 0, 2]))).unwrap();
        assert!(text.contains("DB-BREAKING"));
        assert!(text.contains("stacksup chainstate wipe postgres"));
        assert!(text.contains("chainstate download --service api"));
    }

    #[test]
    fn postgres_major_guidance_warns_about_migration() {
        let mut r = row(Some(vec![17]), true, None, Some(vec![18, 4]));
        r.name = "postgres";
        let text = guidance(&r).unwrap();
        assert!(text.contains("does not migrate itself"));
    }

    #[test]
    fn no_guidance_when_current() {
        assert!(guidance(&row(Some(vec![9, 0, 2]), false, Some(vec![9, 0, 2]), None)).is_none());
    }
}
