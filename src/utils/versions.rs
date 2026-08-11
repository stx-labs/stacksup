//! Version parsing/comparison shared by `chainstate download` and `upgrade`:
//! numeric-tuple versions (semver `9.1.0` and stacks-core's five-part
//! `3.1.0.0.8` compare the same way) and pulled-image version resolution.

use std::process::Command;

pub fn parse_version(s: &str) -> Option<Vec<u64>> {
    let parts: Result<Vec<u64>, _> = s.split('.').map(str::parse).collect();
    parts.ok().filter(|v: &Vec<u64>| !v.is_empty())
}

pub fn compare_versions(a: &[u64], b: &[u64]) -> std::cmp::Ordering {
    let len = a.len().max(b.len());
    for i in 0..len {
        let (x, y) = (a.get(i).unwrap_or(&0), b.get(i).unwrap_or(&0));
        match x.cmp(y) {
            std::cmp::Ordering::Equal => continue,
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

pub fn version_string(v: &[u64]) -> String {
    v.iter().map(u64::to_string).collect::<Vec<_>>().join(".")
}

/// Numeric prefix of a distro-suffixed tag ("16.5-alpine" -> "16.5",
/// "17-bookworm" -> "17"); tags without a numeric prefix ("latest",
/// "bookworm") are returned unchanged.
pub fn strip_tag_suffix(tag: &str) -> &str {
    match tag.split_once('-') {
        Some((prefix, _)) if parse_version(prefix).is_some() => prefix,
        _ => tag,
    }
}

/// Whether an image ref carries an explicit tag. A colon whose right side
/// contains `/` is a registry port (`localhost:5000/repo`), not a tag.
pub fn has_explicit_tag(image: &str) -> bool {
    matches!(image.rsplit_once(':'), Some((_, tag)) if !tag.contains('/'))
}

/// Tag of an image ref, defaulting to `latest` when untagged. A colon whose
/// right side contains `/` is a registry port (`localhost:5000/repo`), not a tag.
pub fn image_tag(image: &str) -> String {
    match image.rsplit_once(':') {
        Some((_, tag)) if !tag.contains('/') => tag.to_string(),
        _ => "latest".to_string(),
    }
}

/// Concrete version of a locally pulled image, from its OCI version label
/// (`org.opencontainers.image.version` — present on stacks-core and
/// stacks-blockchain-api images). Local inspect only; never pulls.
pub fn pulled_image_version(image: &str) -> Option<Vec<u64>> {
    let out = Command::new("docker")
        .args([
            "image",
            "inspect",
            "--format",
            r#"{{index .Config.Labels "org.opencontainers.image.version"}}"#,
            image,
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    version_from_label(&String::from_utf8_lossy(&out.stdout))
}

/// Parse a version out of an OCI label value ("9.0.2", "v9.0.2", trailing newline).
fn version_from_label(label: &str) -> Option<Vec<u64>> {
    parse_version(label.trim().trim_start_matches('v'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering::*;

    #[test]
    fn parses_versions() {
        assert_eq!(parse_version("9.0.2"), Some(vec![9, 0, 2]));
        assert_eq!(parse_version("3.1.0.0.8"), Some(vec![3, 1, 0, 0, 8]));
        // a bare major parses; callers decide whether that counts as a full pin
        assert_eq!(parse_version("9"), Some(vec![9]));
        assert_eq!(parse_version("latest"), None);
        assert_eq!(parse_version("9.0.2-alpine"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn compares_with_zero_padding() {
        assert_eq!(compare_versions(&[9, 1, 0], &[9, 0, 2]), Greater);
        assert_eq!(compare_versions(&[3, 1], &[3, 1, 0]), Equal);
        assert_eq!(compare_versions(&[3, 1, 0], &[3, 2]), Less);
        assert_eq!(compare_versions(&[10], &[9, 9, 9]), Greater);
    }

    #[test]
    fn version_string_round_trips() {
        let v = parse_version("3.1.0.0.8").unwrap();
        assert_eq!(version_string(&v), "3.1.0.0.8");
    }

    #[test]
    fn strips_distro_suffixes() {
        assert_eq!(strip_tag_suffix("16.5-alpine"), "16.5");
        assert_eq!(strip_tag_suffix("17-bookworm"), "17");
        assert_eq!(strip_tag_suffix("17"), "17");
        assert_eq!(strip_tag_suffix("latest"), "latest");
        // non-numeric prefixes are not versions; leave untouched
        assert_eq!(strip_tag_suffix("stacks3.0-0a2c0e2"), "stacks3.0-0a2c0e2");
    }

    #[test]
    fn detects_explicit_tags() {
        assert!(has_explicit_tag("postgres:17"));
        assert!(has_explicit_tag("localhost:5000/repo:1.2"));
        assert!(!has_explicit_tag("postgres"));
        assert!(!has_explicit_tag("localhost:5000/repo"));
    }

    #[test]
    fn extracts_image_tags() {
        assert_eq!(image_tag("postgres:17"), "17");
        assert_eq!(
            image_tag("ghcr.io/stacks-network/stacks-core:4.0.1"),
            "4.0.1"
        );
        // untagged refs default to latest
        assert_eq!(image_tag("postgres"), "latest");
        // a registry port is not a tag
        assert_eq!(image_tag("localhost:5000/repo"), "latest");
        assert_eq!(image_tag("localhost:5000/repo:1.2"), "1.2");
    }

    #[test]
    fn parses_oci_version_labels() {
        assert_eq!(version_from_label("9.0.2\n"), Some(vec![9, 0, 2]));
        assert_eq!(version_from_label("v4.0.1"), Some(vec![4, 0, 1]));
        assert_eq!(version_from_label(""), None);
        assert_eq!(version_from_label("<no value>"), None);
    }
}
