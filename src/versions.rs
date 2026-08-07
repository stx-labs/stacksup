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

pub fn image_tag(image: &str) -> String {
    image.rsplit(':').next().unwrap_or("latest").to_string()
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
    let label = String::from_utf8_lossy(&out.stdout)
        .trim()
        .trim_start_matches('v')
        .to_string();
    parse_version(&label)
}
