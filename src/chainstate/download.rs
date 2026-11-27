//! `stacksup chainstate download`. Seed chainstate from the Hiro Archive.
//!
//! Archives are huge (10s–100s of GB), so the pipeline is two-phase by design: download to
//! `<data-dir>/downloads/<name>.partial` (resumable via HTTP Range; dropped connections resume
//! automatically, and re-running the command continues where it left off), verify sha256, and only
//! then restore.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};

use crate::config::{Deployment, ServiceMode};
use crate::utils::versions::*;

const ARCHIVE_BASE: &str = "https://archive.hiro.so";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceSel {
    Node,
    Api,
    All,
}

pub struct Opts {
    pub service: ServiceSel,
    pub archive: Option<String>,
    pub check_only: bool,
    pub yes: bool,
    pub no_verify: bool,
    pub skip_version_check: bool,
    pub keep_archives: bool,
    /// The caller will start the deployment after a successful restore (only changes the final
    /// hint; starting is the caller's job).
    pub start: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Node,
    Api,
}

struct Job {
    kind: Kind,
    /// None for a local-path archive
    url: Option<String>,
    /// Local archive path (either pre-existing via --archive, or the download target)
    file: PathBuf,
    size: Option<u64>,
    archive_version: Option<Vec<u64>>,
    /// Full image ref (repo:tag) the service is configured to run
    image: String,
}

/// Returns `true` when archives were actually restored, `false` on `--check-only` or a declined
/// confirmation, so callers gating follow-up work (like `--start`) don't act on a no-op run.
pub fn run(deployment: &Deployment, data_dir: &Path, opts: Opts) -> Result<bool> {
    let network = deployment.net.hiro_archive_path.clone().with_context(|| {
        format!(
            "network `{}` has no published archives (no hiro_archive_path in its definition). \
             Use --archive to point at a file explicitly.",
            deployment.network
        )
    })?;
    let network = network.as_str();

    if let Some(running) = crate::utils::docker::running_services(data_dir)
        && !running.is_empty()
    {
        bail!(
            "the stack is running ({}). Run `stacksup stop` first",
            running.join(", ")
        );
    }

    let downloads_dir = data_dir.join("downloads");
    fs::create_dir_all(&downloads_dir)?;

    // ---- Build the job list ---------------------------------------------
    let mut jobs: Vec<Job> = Vec::new();

    let want_node = matches!(opts.service, ServiceSel::Node | ServiceSel::All)
        && deployment.stacks_node.mode == ServiceMode::Enabled;
    let want_api = matches!(opts.service, ServiceSel::Api | ServiceSel::All)
        && deployment.stacks_api.mode == ServiceMode::Enabled;

    if matches!(opts.service, ServiceSel::Node)
        && deployment.stacks_node.mode != ServiceMode::Enabled
    {
        bail!("[stacks-node] is not enabled in stacks.toml");
    }
    if matches!(opts.service, ServiceSel::Api) && deployment.stacks_api.mode != ServiceMode::Enabled
    {
        bail!("[stacks-api] is not enabled in stacks.toml");
    }
    if want_api && deployment.postgres.mode != ServiceMode::Enabled {
        bail!(
            "restoring the API archive needs the managed postgres ([postgres] mode = \"enabled\")"
        );
    }

    if let Some(archive) = &opts.archive {
        // --archive pairs with exactly one service (enforced in main.rs too).
        let kind = if matches!(opts.service, ServiceSel::Node) {
            Kind::Node
        } else {
            Kind::Api
        };
        jobs.push(pinned_job(
            kind,
            archive,
            network,
            deployment,
            &downloads_dir,
        )?);
    } else {
        if want_node {
            jobs.push(latest_node_job(network, deployment, &downloads_dir)?);
        }
        if want_api {
            jobs.push(latest_api_job(network, deployment, &downloads_dir)?);
        }
    }

    if jobs.is_empty() {
        bail!("no enabled services match --service; nothing to download");
    }

    // ---- Plan: versions, sizes, disk ------------------------------------
    println!("network: {network}\n");
    let mut version_problem = false;
    for job in &jobs {
        let name = job.file.file_name().unwrap_or_default().to_string_lossy();
        let size = job.size.map_or("size unknown".into(), format_bytes);
        println!("  {:<12} {name}  ({size})", kind_name(job.kind));

        if job.kind == Kind::Api {
            pg_verdict(job, deployment, &mut version_problem);
        }
        match version_verdict(job) {
            VersionVerdict::Ok(a, c) => {
                println!("    {}", format!("archive {a} ≤ configured {c} ✓").green())
            }
            VersionVerdict::TooNew(a, c) => {
                println!(
                    "    {}",
                    format!(
                        "✗ archive {a} is NEWER than configured {c}. Chainstate from newer \
                         software cannot be used by older software.\n    Fix: raise [{}] version \
                         in stacks.toml, or pick an older archive with --archive.",
                        section_name(job.kind)
                    )
                    .red()
                );
                version_problem = true;
            }
            VersionVerdict::Unknown(reason) => {
                println!(
                    "    {}",
                    format!("⚠ cannot verify version compatibility: {reason}").yellow()
                )
            }
        }
    }
    if version_problem && !opts.skip_version_check {
        bail!("archive version check failed (use --skip-version-check to override)");
    }
    if opts.no_verify {
        println!(
            "\n{}",
            "⚠ sha256 verification disabled. A corrupted download may fail at restore or, \
             worse, restore silently."
                .yellow()
        );
    }

    // Disk preflight: compressed download + ~2.5x for extraction/restore.
    let total: u64 = jobs.iter().filter_map(|j| j.size).sum();
    let needed = total + (total as f64 * 2.5) as u64;
    match free_disk_bytes(data_dir) {
        Some(free) if free < needed => {
            bail!(
                "not enough disk space: need ~{} (download + restore), {} free in {}",
                format_bytes(needed),
                format_bytes(free),
                data_dir.display()
            );
        }
        Some(free) => println!(
            "\nDisk: need ~{}, {} free {}",
            format_bytes(needed),
            format_bytes(free),
            "✓".green()
        ),
        None => println!("\n{}", "⚠ could not determine free disk space".yellow()),
    }

    if opts.check_only {
        println!("\n--check-only: stopping here.");
        return Ok(false);
    }

    if !opts.yes {
        print!("\nProceed? [y/N] ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        if !matches!(input.trim(), "y" | "Y" | "yes") {
            println!("Aborted.");
            return Ok(false);
        }
    }
    println!(
        "{}",
        "\nTip: safe to Ctrl-C and re-run later. Downloads resume where they left off.\n\
         To run unattended:  nohup stacksup chainstate download --yes > download.log 2>&1 &\n"
            .dimmed()
    );

    // ---- Download, verify, restore --------------------------------------
    for job in &jobs {
        let name = job
            .file
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        if let Some(url) = &job.url {
            if job.file.exists() {
                println!("{name}: already downloaded");
            } else {
                println!("Downloading {name}...");
                download_with_retries(url, &job.file, job.size, RetryPolicy::from_env())?;
            }
        }

        if !opts.no_verify {
            match fetch_expected_sha256(job) {
                Some(expected) => {
                    println!("Verifying sha256...");
                    let actual = sha256_file(&job.file)?;
                    if actual != expected {
                        // A bad file must not survive to poison the next resume.
                        let quarantine = job.file.with_extension("corrupt");
                        fs::rename(&job.file, &quarantine)?;
                        bail!(
                            "sha256 mismatch for {name}\n  expected {expected}\n  actual   {actual}\n\
                             moved to {}. Re-run to download again",
                            quarantine.display()
                        );
                    }
                    println!("  {} checksum ok", "✓".green());
                }
                None => println!(
                    "  {}",
                    "⚠ no .sha256 available for this archive. Skipping verification".yellow()
                ),
            }
        }

        match job.kind {
            Kind::Node => restore_node(&job.file, deployment, data_dir)?,
            Kind::Api => restore_api(&job.file, deployment, data_dir, &downloads_dir)?,
        }

        if !opts.keep_archives && job.url.is_some() {
            let _ = fs::remove_file(&job.file);
        }
    }

    println!("\n{}", "Done.".green());
    if opts.start {
        println!(
            "Run `stacksup chainstate status` once services are up to confirm the tips line up."
        );
    } else {
        println!(
            "Next: `stacksup start`, then `stacksup chainstate status` to confirm the tips line up."
        );
    }
    Ok(true)
}

// ---------------------------------------------------------------------------
// Job construction

/// Resolve the newest *versioned* archive from the bucket listing. We never use the `-latest`
/// objects: they are byte-identical pointers to the newest dated archive but carry no version in
/// their name, which would defeat the archive-version ≤ configured-version check.
fn latest_node_job(network: &str, deployment: &Deployment, downloads: &Path) -> Result<Job> {
    let base = format!("{ARCHIVE_BASE}/{network}/stacks-blockchain");
    // Usually the path segment, but e.g. staking-testnet's archives are named signet-*.
    let prefix = deployment.net.hiro_archive_prefix().unwrap_or(network);
    let name = newest_in_listing(
        &base,
        &format!("{prefix}-stacks-blockchain-"),
        &[".tar.zst", ".tar.gz"],
    )?;
    versioned_job(Kind::Node, &base, &name, deployment, downloads)
}

fn latest_api_job(network: &str, deployment: &Deployment, downloads: &Path) -> Result<Job> {
    let base = format!("{ARCHIVE_BASE}/{network}/stacks-blockchain-api-pg");
    let name = newest_in_listing(&base, "stacks-blockchain-api-pg-", &[".dump"])?;
    versioned_job(Kind::Api, &base, &name, deployment, downloads)
}

fn versioned_job(
    kind: Kind,
    base: &str,
    name: &str,
    deployment: &Deployment,
    downloads: &Path,
) -> Result<Job> {
    let url = format!("{base}/{name}");
    let size = head_content_length(&url).with_context(|| format!("archive not found at {url}"))?;
    let image = match kind {
        Kind::Node => crate::utils::services::stacks_node_image(deployment),
        Kind::Api => crate::utils::services::stacks_api_image(deployment),
    };
    Ok(Job {
        kind,
        url: Some(url),
        file: downloads.join(name),
        size,
        archive_version: parse_version_from_name(name, kind),
        image,
    })
}

/// Fetch the archive directory's HTML bucket listing and return the newest dated archive matching
/// `prefix` + one of `extensions` (excluding -latest).
fn newest_in_listing(base: &str, prefix: &str, extensions: &[&str]) -> Result<String> {
    let listing_url = format!("{base}/");
    let html = ureq::get(&listing_url)
        .call()
        .with_context(|| format!("could not fetch archive listing at {listing_url}"))?
        .into_string()?;

    let mut best: Option<(u64, Vec<u64>, String)> = None;
    for name in extract_hrefs(&html) {
        if !name.starts_with(prefix)
            || name.contains("-latest")
            || !extensions.iter().any(|e| name.ends_with(e))
        {
            continue;
        }
        let Some(date) = parse_date_from_name(&name) else {
            continue;
        };
        let version = parse_version_from_name(&name, Kind::Node).unwrap_or_default();
        let candidate = (date, version, name);
        if best
            .as_ref()
            .is_none_or(|b| (candidate.0, &candidate.1) > (b.0, &b.1))
        {
            best = Some(candidate);
        }
    }
    best.map(|(_, _, name)| name).with_context(|| {
        format!(
            "no versioned archives matching `{prefix}*` found in {listing_url}. \
             The listing format may have changed. Use --archive to pin a file explicitly."
        )
    })
}

/// Filenames from `href="..."` attributes, basename only.
fn extract_hrefs(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find("href=\"") {
        rest = &rest[start + 6..];
        if let Some(end) = rest.find('"') {
            let target = &rest[..end];
            if let Some(name) = target.rsplit('/').next()
                && !name.is_empty()
            {
                out.push(name.to_string());
            }
            rest = &rest[end..];
        } else {
            break;
        }
    }
    out
}

/// The YYYYMMDD segment of a dated archive name.
fn parse_date_from_name(name: &str) -> Option<u64> {
    name.split(['-', '.'])
        .filter(|seg| seg.len() == 8 && seg.chars().all(|c| c.is_ascii_digit()))
        .filter_map(|seg| seg.parse().ok())
        .next_back()
}

fn pinned_job(
    kind: Kind,
    archive: &str,
    network: &str,
    deployment: &Deployment,
    downloads: &Path,
) -> Result<Job> {
    let image = match kind {
        Kind::Node => crate::utils::services::stacks_node_image(deployment),
        Kind::Api => crate::utils::services::stacks_api_image(deployment),
    };

    // Local file?
    let as_path = Path::new(archive);
    if as_path.exists() {
        return Ok(Job {
            kind,
            url: None,
            file: as_path.to_path_buf(),
            size: as_path.metadata().ok().map(|m| m.len()),
            archive_version: parse_version_from_name(
                &as_path.file_name().unwrap_or_default().to_string_lossy(),
                kind,
            ),
            image,
        });
    }

    // Full URL or bare filename resolved against the network path.
    let url = if archive.starts_with("http://") || archive.starts_with("https://") {
        archive.to_string()
    } else {
        let dir = match kind {
            Kind::Node => "stacks-blockchain",
            Kind::Api => "stacks-blockchain-api-pg",
        };
        format!("{ARCHIVE_BASE}/{network}/{dir}/{archive}")
    };
    let name = url.rsplit('/').next().unwrap_or(archive).to_string();

    // `-latest` objects are byte-identical to the newest dated archive but carry no version,
    // defeating the compatibility check. Point at the versioned equivalent instead (or just omit
    // --archive).
    if name.contains("-latest") {
        bail!(
            "`{name}` is a -latest pointer; use the equivalent versioned archive \
             (or omit --archive to auto-select the newest versioned one)"
        );
    }

    // A filename that names another network is a subtle disaster (a different genesis); block it.
    if let Some(other) = other_network_archive(&name, deployment) {
        bail!(
            "archive `{name}` is for {other}, but stacks.toml says network = \"{}\"",
            deployment.net.name
        );
    }

    let size = head_content_length(&url).with_context(|| format!("archive not found at {url}"))?;
    Ok(Job {
        kind,
        url: Some(url),
        file: downloads.join(&name),
        size,
        archive_version: parse_version_from_name(&name, kind),
        image,
    })
}

/// The built-in network a node archive filename belongs to, when that isn't this deployment's.
fn other_network_archive(name: &str, deployment: &Deployment) -> Option<&'static str> {
    let own = deployment.net.hiro_archive_prefix();
    crate::config::network::builtin_archive_prefixes()
        .into_iter()
        .find(|(_, prefix)| {
            Some(prefix.as_str()) != own
                && name.starts_with(&format!("{prefix}-stacks-blockchain-"))
        })
        .map(|(other, _)| other)
}

// ---------------------------------------------------------------------------
// Version validation

/// API dumps are produced by a specific postgres major (it's in the archive name); pg_restore into
/// an OLDER server is not supported. Same ladder as the service check: explicit tag settles it,
/// floating tags consult the pulled image, unknown warns.
fn pg_verdict(job: &Job, deployment: &Deployment, version_problem: &mut bool) {
    let archive_pg = job
        .file
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .and_then(|n| parse_pg_major_from_name(&n));
    let pg_image = crate::utils::services::postgres_image(deployment);
    // Distro-suffixed tags ("16.5-alpine") must still gate: strip the suffix
    // so the numeric prefix classifies, instead of falling through to the
    // pulled-image label (which official postgres images don't carry).
    let pg_tag = image_tag(&pg_image);
    match classify_version(archive_pg.as_deref(), strip_tag_suffix(&pg_tag), || {
        pulled_image_version(&pg_image)
    }) {
        VersionVerdict::Ok(a, c) => {
            println!(
                "    {}",
                format!("archive postgres {a} ≤ deployed postgres {c} ✓").green()
            )
        }
        VersionVerdict::TooNew(a, c) => {
            println!(
                "    {}",
                format!(
                    "✗ archive was dumped by postgres {a}, newer than deployed postgres {c}. \
                     pg_restore into an older server is not supported.\n    Fix: raise [postgres] \
                     version in stacks.toml, or pick an older archive with --archive."
                )
                .red()
            );
            *version_problem = true;
        }
        VersionVerdict::Unknown(reason) => {
            println!(
                "    {}",
                format!("⚠ cannot verify postgres compatibility: {reason}").yellow()
            )
        }
    }
}

enum VersionVerdict {
    Ok(String, String),
    TooNew(String, String),
    Unknown(String),
}

fn version_verdict(job: &Job) -> VersionVerdict {
    let tag = image_tag(&job.image);
    // Only inspected when the tag alone can't settle it (floating tags like `9` or `latest`): the
    // pulled image's OCI version label carries the concrete version (e.g. tag `9` -> label
    // `9.0.2`).
    classify_version(job.archive_version.as_deref(), &tag, || {
        pulled_image_version(&job.image)
    })
}

fn classify_version(
    archive: Option<&[u64]>,
    tag: &str,
    pulled: impl FnOnce() -> Option<Vec<u64>>,
) -> VersionVerdict {
    let Some(archive) = archive else {
        return VersionVerdict::Unknown("no version found in the archive filename".into());
    };
    let a = version_string(archive);

    match parse_version(tag) {
        Some(conf) => {
            if compare_versions(archive, &conf) != std::cmp::Ordering::Greater {
                return VersionVerdict::Ok(a, version_string(&conf));
            }
            // Archive "newer" than the tag, but a partial tag like `9` is a floating tag that may
            // currently BE 9.0.2. If the tag is a prefix of the archive version, ask the pulled
            // image.
            if archive.starts_with(&conf) {
                match pulled() {
                    Some(p) if compare_versions(archive, &p) != std::cmp::Ordering::Greater => {
                        VersionVerdict::Ok(
                            a,
                            format!("{} (pulled image for tag `{tag}`)", version_string(&p)),
                        )
                    }
                    Some(p) => VersionVerdict::TooNew(
                        a,
                        format!(
                            "{} (pulled image for tag `{tag}`. `docker pull` a newer one)",
                            version_string(&p)
                        ),
                    ),
                    None => VersionVerdict::Unknown(format!(
                        "tag `{tag}` is a floating tag and the archive is {a}; could not read the \
                         pulled image's version label (image not pulled?). `docker pull` it and re-run"
                    )),
                }
            } else {
                VersionVerdict::TooNew(a, version_string(&conf))
            }
        }
        None => match pulled() {
            Some(p) if compare_versions(archive, &p) != std::cmp::Ordering::Greater => {
                VersionVerdict::Ok(
                    a,
                    format!("{} (pulled image for tag `{tag}`)", version_string(&p)),
                )
            }
            Some(p) => VersionVerdict::TooNew(
                a,
                format!(
                    "{} (pulled image for tag `{tag}`. `docker pull` a newer one)",
                    version_string(&p)
                ),
            ),
            None => VersionVerdict::Unknown(format!(
                "configured tag is `{tag}` and no pulled image to inspect. Pin a version in \
                 stacks.toml or `docker pull` the image to make this check meaningful"
            )),
        },
    }
}

/// Extract the service version from a versioned archive filename.
/// node: `mainnet-stacks-blockchain-3.1.0.0.8-20260803.tar.gz`
/// api:  `stacks-blockchain-api-pg-17-8.1.0-20260803.dump`
/// The version is the dotted numeric segment (dates have no dots, the pg major has no dots).
fn parse_version_from_name(name: &str, _kind: Kind) -> Option<Vec<u64>> {
    name.split(['-', '_'])
        .filter(|seg| seg.contains('.'))
        // strip file extensions from the last segment (e.g. "8.1.0.tar.gz")
        .filter_map(|seg| {
            let cleaned: Vec<&str> = seg
                .split('.')
                .take_while(|part| part.chars().all(|c| c.is_ascii_digit()) && !part.is_empty())
                .collect();
            if cleaned.len() >= 2 {
                parse_version(&cleaned.join("."))
            } else {
                None
            }
        })
        .next()
}

/// The postgres major that produced an API dump:
/// `stacks-blockchain-api-pg-17-9.0.2-20260803.dump` -> [17].
fn parse_pg_major_from_name(name: &str) -> Option<Vec<u64>> {
    let mut parts = name.split('-');
    while let Some(seg) = parts.next() {
        if seg == "pg" {
            return parts.next().and_then(parse_version);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Download engine

/// Follow redirects manually and return the final URL. archive.hiro.so 302s to short-lived
/// presigned R2 URLs, and resume only works if the Range header is sent to the final URL, so every
/// request path resolves first.
fn resolve_redirects(url: &str) -> Result<String> {
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(CONNECT_TIMEOUT)
        .timeout_read(READ_TIMEOUT)
        .build();
    let mut current = url.to_string();
    for _ in 0..5 {
        // Range 0-0 keeps the status probe from opening a full-body stream
        // on the final hop; redirect hops ignore it.
        let resp = match agent.get(&current).set("Range", "bytes=0-0").call() {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(e) => return Err(e.into()),
        };
        match resp.status() {
            301 | 302 | 303 | 307 | 308 => {
                let loc = resp
                    .header("Location")
                    .context("redirect without Location header")?
                    .to_string();
                current = loc;
            }
            _ => return Ok(current),
        }
    }
    bail!("too many redirects for {url}");
}

/// Size probe via a 1-byte Range GET (HEAD doesn't survive the presigned redirect). 206 -> total
/// from Content-Range; 200 -> Content-Length.
fn head_content_length(url: &str) -> Option<Option<u64>> {
    let resolved = resolve_redirects(url).ok()?;
    let resp = ureq::get(&resolved).set("Range", "bytes=0-0").call().ok()?;
    match resp.status() {
        206 => {
            // "bytes 0-0/123456789"
            let total = resp
                .header("Content-Range")
                .and_then(|v| v.rsplit('/').next())
                .and_then(|t| t.parse().ok());
            Some(total)
        }
        200 => Some(resp.header("Content-Length").and_then(|v| v.parse().ok())),
        _ => None,
    }
}

/// Retry policy for archive downloads. Multi-hundred-GB streams from archive.hiro.so routinely get
/// cut mid-body ("response body closed before all bytes were read"), so transient failures resume
/// automatically instead of asking the operator to re-run.
#[derive(Debug, Clone, Copy)]
struct RetryPolicy {
    /// Consecutive attempts allowed to fail without writing a single new byte. Any attempt that
    /// makes progress resets the counter, so a long download survives any number of drops.
    max_retries: u32,
    /// First backoff delay; doubles per consecutive failure, capped at `max_delay`.
    base_delay: Duration,
    max_delay: Duration,
}

/// Env override for [`RetryPolicy::max_retries`].
const RETRIES_ENV: &str = "STACKSUP_DOWNLOAD_RETRIES";
const DEFAULT_RETRIES: u32 = 10;
/// A socket read idle this long counts as a dropped connection (and is retried), instead of
/// hanging forever on a stalled stream.
const READ_TIMEOUT: Duration = Duration::from_secs(120);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

impl RetryPolicy {
    fn from_env() -> Self {
        let max_retries = std::env::var(RETRIES_ENV)
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(DEFAULT_RETRIES);
        RetryPolicy {
            max_retries,
            base_delay: Duration::from_secs(2),
            max_delay: Duration::from_secs(60),
        }
    }

    /// Delay before retry number `attempt` (1-based).
    fn delay(&self, attempt: u32) -> Duration {
        let factor = 1u32
            .checked_shl(attempt.saturating_sub(1))
            .unwrap_or(u32::MAX);
        self.base_delay.saturating_mul(factor).min(self.max_delay)
    }
}

/// Marks a failure as worth retrying (the stream broke or ended short; the bytes on disk are fine).
#[derive(Debug)]
struct Transient(String);

impl std::fmt::Display for Transient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Transient {}

/// Network-level failures, 408/429 and 5xx are retried; anything else (404, 403, disk errors) is
/// returned immediately because retrying cannot fix it.
fn is_transient(err: &anyhow::Error) -> bool {
    err.chain().any(|e| {
        if e.downcast_ref::<Transient>().is_some() {
            return true;
        }
        match e.downcast_ref::<ureq::Error>() {
            Some(ureq::Error::Transport(_)) => true,
            Some(ureq::Error::Status(code, _)) => matches!(code, 408 | 429 | 500..=599),
            None => false,
        }
    })
}

fn download_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(CONNECT_TIMEOUT)
        .timeout_read(READ_TIMEOUT)
        .build()
}

fn partial_path(dest: &Path) -> PathBuf {
    dest.with_extension(format!(
        "{}.partial",
        dest.extension().unwrap_or_default().to_string_lossy()
    ))
}

/// [`download_resumable`] in a loop: transient failures back off and resume from the `.partial`
/// file. Gives up only after `max_retries` consecutive attempts that made no progress.
fn download_with_retries(
    url: &str,
    dest: &Path,
    expected_size: Option<u64>,
    policy: RetryPolicy,
) -> Result<()> {
    let partial = partial_path(dest);
    let partial_len = || partial.metadata().map(|m| m.len()).unwrap_or(0);
    let mut failures = 0u32;
    loop {
        let before = partial_len();
        let err = match download_resumable(url, dest, expected_size) {
            Ok(()) => return Ok(()),
            Err(e) if is_transient(&e) => e,
            Err(e) => return Err(e),
        };
        if partial_len() != before {
            failures = 0;
        }
        failures += 1;
        if failures > policy.max_retries {
            return Err(err.context(format!(
                "download failed after {} retries without progress — re-run to resume \
                 (set {RETRIES_ENV} to retry longer)",
                policy.max_retries
            )));
        }
        let delay = policy.delay(failures);
        println!(
            "  {} {err:#}. Retrying in {}s ({failures}/{})...",
            "⚠".yellow(),
            delay.as_secs(),
            policy.max_retries
        );
        std::thread::sleep(delay);
    }
}

/// Resumable download: writes `<dest>.partial` + `<dest>.etag`, continues via HTTP Range, restarts
/// cleanly if the server object changed or Range is unsupported, renames into place when complete.
fn download_resumable(url: &str, dest: &Path, expected_size: Option<u64>) -> Result<()> {
    let partial = partial_path(dest);
    let etag_file = partial.with_extension("etag");

    let offset = partial.metadata().map(|m| m.len()).unwrap_or(0);

    // Re-resolve every attempt: presigned redirect targets expire.
    let final_url = resolve_redirects(url)?;
    let mut req = download_agent().get(&final_url);
    if offset > 0 {
        req = req.set("Range", &format!("bytes={offset}-"));
    }
    let resp = req
        .call()
        .with_context(|| format!("request failed: {url}"))?;

    // Stale-partial guard: if the server's object changed since we started, the old bytes belong to
    // a different archive.
    let server_etag = resp
        .header("ETag")
        .or_else(|| resp.header("Last-Modified"))
        .unwrap_or("")
        .to_string();
    let stored_etag = fs::read_to_string(&etag_file).unwrap_or_default();
    let resumed = resp.status() == 206;
    if offset > 0 && (!resumed || (!stored_etag.is_empty() && stored_etag != server_etag)) {
        println!(
            "  {}",
            "partial download is stale or server does not support resume — starting over".yellow()
        );
        let _ = fs::remove_file(&partial);
        // Re-request without Range so we stream the whole object.
        return download_resumable_fresh(&final_url, dest, expected_size);
    }
    fs::write(&etag_file, &server_etag)?;

    let total = expected_size.or_else(|| {
        resp.header("Content-Length")
            .and_then(|v| v.parse::<u64>().ok())
            .map(|l| l + offset)
    });
    stream_to_file(resp.into_reader(), &partial, offset, total)?;
    finish_download(&partial, &etag_file, dest, total)
}

fn download_resumable_fresh(url: &str, dest: &Path, expected_size: Option<u64>) -> Result<()> {
    let partial = partial_path(dest);
    let etag_file = partial.with_extension("etag");
    let resp = download_agent()
        .get(url)
        .call()
        .with_context(|| format!("request failed: {url}"))?;
    let etag = resp
        .header("ETag")
        .or_else(|| resp.header("Last-Modified"))
        .unwrap_or("")
        .to_string();
    fs::write(&etag_file, etag)?;
    let total =
        expected_size.or_else(|| resp.header("Content-Length").and_then(|v| v.parse().ok()));
    stream_to_file(resp.into_reader(), &partial, 0, total)?;
    finish_download(&partial, &etag_file, dest, total)
}

fn stream_to_file(
    mut reader: impl Read,
    partial: &Path,
    offset: u64,
    total: Option<u64>,
) -> Result<()> {
    let bar = byte_bar(total, offset);
    let file = if offset > 0 {
        OpenOptions::new().append(true).open(partial)?
    } else {
        File::create(partial)?
    };
    let mut writer = BufWriter::new(file);
    let mut buf = [0u8; 1 << 16];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                // Keep what we already received so the retry resumes after it.
                writer.flush()?;
                bar.abandon();
                return Err(Transient(format!("download interrupted: {e}")).into());
            }
        };
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n])?;
        bar.inc(n as u64);
    }
    writer.flush()?;
    bar.finish();
    Ok(())
}

/// `total` is the archive size when known (listing or Content-Length); a body that ended short of it
/// is a dropped stream, not a finished download.
fn finish_download(
    partial: &Path,
    etag_file: &Path,
    dest: &Path,
    total: Option<u64>,
) -> Result<()> {
    if let Some(expected) = total {
        let actual = partial.metadata()?.len();
        if actual < expected {
            return Err(Transient(format!(
                "download incomplete ({} of {})",
                format_bytes(actual),
                format_bytes(expected)
            ))
            .into());
        }
    }
    fs::rename(partial, dest)?;
    let _ = fs::remove_file(etag_file);
    Ok(())
}

// ---------------------------------------------------------------------------
// Verification

fn fetch_expected_sha256(job: &Job) -> Option<String> {
    let name = job.file.file_name()?.to_string_lossy().to_string();
    let candidates: Vec<String> = match &job.url {
        Some(url) => {
            // node: `...-latest.tar.zst` -> `...-latest.sha256`; also try `<full>.sha256`
            let base = url.rsplit_once('/').map(|(b, _)| b.to_string())?;
            let stem = name
                .split(".tar")
                .next()
                .unwrap_or(&name)
                .trim_end_matches(".dump");
            vec![format!("{base}/{stem}.sha256"), format!("{}.sha256", url)]
        }
        None => {
            // local archive: look for a sidecar next to it
            let sidecar = job.file.with_extension("sha256");
            return fs::read_to_string(sidecar)
                .ok()
                .and_then(|s| parse_sha_line(&s));
        }
    };
    for url in candidates {
        if let Ok(resp) = ureq::get(&url).call()
            && let Ok(body) = resp.into_string()
            && let Some(hash) = parse_sha_line(&body)
        {
            return Some(hash);
        }
    }
    None
}

fn parse_sha_line(s: &str) -> Option<String> {
    let first = s.split_whitespace().next()?;
    (first.len() == 64 && first.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| first.to_lowercase())
}

fn sha256_file(path: &Path) -> Result<String> {
    let file = File::open(path)?;
    let total = file.metadata()?.len();
    let bar = byte_bar(Some(total), 0);
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 1 << 16];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        bar.inc(n as u64);
    }
    bar.finish();
    Ok(format!("{:x}", hasher.finalize()))
}

// ---------------------------------------------------------------------------
// Restore: node

fn restore_node(archive: &Path, deployment: &Deployment, data_dir: &Path) -> Result<()> {
    let mode = deployment.net.node.burnchain_mode.as_str();
    let target_root = data_dir.join("chainstate/stacks-node");
    let tmp = data_dir.join("chainstate/.restore-tmp");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp)?;

    println!("Extracting node chainstate...");
    let file = File::open(archive)?;
    let total = file.metadata()?.len();
    let bar = byte_bar(Some(total), 0);
    let counted = CountingReader {
        inner: BufReader::new(file),
        bar: bar.clone(),
    };

    let name = archive.file_name().unwrap_or_default().to_string_lossy();
    if name.ends_with(".zst") {
        tar::Archive::new(zstd::Decoder::new(counted)?).unpack(&tmp)?;
    } else {
        tar::Archive::new(flate2::read::GzDecoder::new(counted)).unpack(&tmp)?;
    }
    bar.finish();

    // The archive is the node's working_dir contents. Two known shapes:
    //   <mode>/{chainstate,burnchain,...}  (working_dir tarred whole)
    //   {chainstate,burnchain,...}         (the mode dir's contents tarred)
    let extracted_mode_dir = tmp.join(mode);
    let (src, dst) = if extracted_mode_dir.is_dir() {
        (extracted_mode_dir, target_root.join(mode))
    } else if tmp.join("chainstate").is_dir() || tmp.join("burnchain").is_dir() {
        (tmp.clone(), target_root.join(mode))
    } else {
        bail!(
            "unrecognized archive layout in {} — expected a `{mode}/` dir or chainstate/burnchain dirs",
            tmp.display()
        );
    };

    fs::create_dir_all(&target_root)?;
    if dst.exists() {
        println!("  replacing existing {}", dst.display());
        fs::remove_dir_all(&dst)?;
    }
    fs::rename(&src, &dst)?;
    let _ = fs::remove_dir_all(&tmp);
    println!(
        "  {} node chainstate restored to {}",
        "✓".green(),
        dst.display()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Restore: API (pg_restore into the managed postgres)

fn restore_api(
    dump: &Path,
    deployment: &Deployment,
    data_dir: &Path,
    downloads_dir: &Path,
) -> Result<()> {
    // pg_restore --jobs needs a seekable file inside the container; the postgres service mounts
    // <data-dir>/downloads at /downloads (read-only).
    let dump = if dump.starts_with(downloads_dir) {
        dump.to_path_buf()
    } else {
        let staged = downloads_dir.join(dump.file_name().unwrap_or_default());
        println!(
            "Staging dump into {} (visible to the postgres container)...",
            downloads_dir.display()
        );
        fs::copy(dump, &staged)?;
        staged
    };
    let in_container = format!(
        "/downloads/{}",
        dump.file_name().unwrap_or_default().to_string_lossy()
    );
    let user = deployment.postgres.user.as_deref().unwrap_or("postgres");

    println!("Starting postgres...");
    crate::utils::docker::compose_up_service(data_dir, "postgres")?;
    for _ in 0..60 {
        let ready = Command::new("docker")
            .args(["exec", "stacks-postgres", "pg_isready", "-U", user])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ready {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }

    println!("Restoring API database (pg_restore --jobs 4)...");
    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::with_template("{spinner} {elapsed} restored {pos} objects").unwrap(),
    );
    let mut child = Command::new("docker")
        .args([
            "exec",
            "stacks-postgres",
            "pg_restore",
            "--username",
            user,
            "--verbose",
            "--jobs",
            "4",
            "--clean",
            "--if-exists",
            "--no-owner",
            "--no-acl",
            "--dbname",
            "stacks_blockchain_api",
            &in_container,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to run pg_restore in the postgres container")?;

    let mut last_lines: Vec<String> = Vec::new();
    if let Some(stderr) = child.stderr.take() {
        for line in io::BufRead::lines(BufReader::new(stderr)).map_while(Result::ok) {
            spinner.inc(1);
            spinner.tick();
            last_lines.push(line);
            if last_lines.len() > 20 {
                last_lines.remove(0);
            }
        }
    }
    let status = child.wait()?;
    spinner.finish();
    if !status.success() {
        bail!("pg_restore failed:\n{}", last_lines.join("\n"));
    }
    println!("  {} API database restored", "✓".green());

    // While postgres is still up, check tip consistency against the node.
    println!("\nChecking chain tip consistency:");
    let verdict = crate::chainstate::status(deployment, data_dir);

    println!("Stopping postgres...");
    crate::utils::docker::compose_stop_service(data_dir, "postgres")?;
    verdict
}

// ---------------------------------------------------------------------------
// Helpers

struct CountingReader<R> {
    inner: R,
    bar: ProgressBar,
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.bar.inc(n as u64);
        Ok(n)
    }
}

fn byte_bar(total: Option<u64>, start: u64) -> ProgressBar {
    let bar = match total {
        Some(t) => {
            let b = ProgressBar::new(t);
            b.set_style(
                ProgressStyle::with_template(
                    "  [{bar:30}] {bytes}/{total_bytes} {bytes_per_sec} eta {eta}",
                )
                .unwrap()
                .progress_chars("=> "),
            );
            b
        }
        None => {
            let b = ProgressBar::new_spinner();
            b.set_style(
                ProgressStyle::with_template("  {spinner} {bytes} {bytes_per_sec}").unwrap(),
            );
            b
        }
    };
    bar.set_position(start);
    bar
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Node => "stacks-node",
        Kind::Api => "stacks-api",
    }
}

fn section_name(k: Kind) -> &'static str {
    match k {
        Kind::Node => "stacks-node",
        Kind::Api => "stacks-api",
    }
}

fn format_bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    format!("{v:.1} {}", UNITS[unit])
}

/// Free bytes on the filesystem containing `dir`, via `df -Pk` (portable across macOS/Linux; no
/// direct std API for statvfs).
fn free_disk_bytes(dir: &Path) -> Option<u64> {
    let out = Command::new("df").arg("-Pk").arg(dir).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().nth(1)?;
    let avail_kb: u64 = line.split_whitespace().nth(3)?.parse().ok()?;
    Some(avail_kb * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_node_archive_version() {
        assert_eq!(
            parse_version_from_name(
                "mainnet-stacks-blockchain-3.1.0.0.8-20260803.tar.gz",
                Kind::Node
            ),
            Some(vec![3, 1, 0, 0, 8])
        );
    }

    #[test]
    fn parses_api_archive_version() {
        assert_eq!(
            parse_version_from_name("stacks-blockchain-api-pg-17-8.1.0-20260803.dump", Kind::Api),
            Some(vec![8, 1, 0])
        );
    }

    #[test]
    fn latest_has_no_version() {
        assert_eq!(
            parse_version_from_name("mainnet-stacks-blockchain-latest.tar.zst", Kind::Node),
            None
        );
        assert_eq!(
            parse_version_from_name("stacks-blockchain-api-pg-17-latest.dump", Kind::Api),
            None
        );
    }

    #[test]
    fn version_comparison() {
        use std::cmp::Ordering::*;
        assert_eq!(compare_versions(&[3, 1, 0], &[3, 2]), Less);
        assert_eq!(compare_versions(&[8, 2, 0], &[8, 1, 0]), Greater);
        assert_eq!(compare_versions(&[3, 1], &[3, 1, 0]), Equal);
    }

    #[test]
    fn listing_selection_prefers_newest_date() {
        let html = r#"
            <a href="/t/x/testnet-stacks-blockchain-latest.tar.zst">l</a>
            <a href="/t/x/testnet-stacks-blockchain-3.4.0.0.3-20260708.tar.zst">a</a>
            <a href="/t/x/testnet-stacks-blockchain-3.4.0.0.4-20260713.tar.zst">b</a>
            <a href="/t/x/testnet-stacks-blockchain-3.4.0.0.4-20260713.sha256">s</a>
            <a href="/t/x/testnet-stacks-blockchain-3.4.0.0.4-20260712.tar.zst">c</a>
        "#;
        let names = extract_hrefs(html);
        assert!(
            names.contains(&"testnet-stacks-blockchain-3.4.0.0.4-20260713.tar.zst".to_string())
        );
        // simulate newest_in_listing's filter+pick on the extracted names
        let best = names
            .iter()
            .filter(|n| {
                n.starts_with("testnet-stacks-blockchain-")
                    && !n.contains("-latest")
                    && n.ends_with(".tar.zst")
            })
            .filter_map(|n| parse_date_from_name(n).map(|d| (d, n)))
            .max();
        assert_eq!(
            best.unwrap().1,
            "testnet-stacks-blockchain-3.4.0.0.4-20260713.tar.zst"
        );
    }

    #[test]
    fn floating_tag_resolved_by_pulled_image() {
        // tag `9`, archive 9.0.2, pulled image says 9.0.2 -> compatible
        let v = classify_version(Some(&[9, 0, 2]), "9", || Some(vec![9, 0, 2]));
        assert!(matches!(v, VersionVerdict::Ok(..)));
        // pulled image is older than the archive -> too new
        let v = classify_version(Some(&[9, 0, 2]), "9", || Some(vec![9, 0, 1]));
        assert!(matches!(v, VersionVerdict::TooNew(..)));
        // no pulled image to consult -> warn, don't block
        let v = classify_version(Some(&[9, 0, 2]), "9", || None);
        assert!(matches!(v, VersionVerdict::Unknown(..)));
        // different major is provably incompatible regardless of image
        let v = classify_version(Some(&[10, 0, 0]), "9", || Some(vec![9, 0, 2]));
        assert!(matches!(v, VersionVerdict::TooNew(..)));
        // full tag ≥ archive never needs the image
        let v = classify_version(Some(&[9, 0, 2]), "9.0.2", || panic!("should not inspect"));
        assert!(matches!(v, VersionVerdict::Ok(..)));
        // `latest` falls back to the pulled image
        let v = classify_version(Some(&[9, 0, 2]), "latest", || Some(vec![9, 0, 2]));
        assert!(matches!(v, VersionVerdict::Ok(..)));
    }

    #[test]
    fn suffixed_postgres_tags_still_gate() {
        // "16.5-alpine" must block a pg-17 dump, not fall through to Unknown
        let v = classify_version(Some(&[17]), strip_tag_suffix("16.5-alpine"), || None);
        assert!(matches!(v, VersionVerdict::TooNew(..)));
        let v = classify_version(Some(&[17]), strip_tag_suffix("17.2-alpine"), || None);
        assert!(matches!(v, VersionVerdict::Ok(..)));
    }

    #[test]
    fn parses_pg_major_from_api_dumps() {
        assert_eq!(
            parse_pg_major_from_name("stacks-blockchain-api-pg-17-9.0.2-20260811.dump"),
            Some(vec![17])
        );
        assert_eq!(
            parse_pg_major_from_name("stacks-blockchain-api-pg-17-latest.dump"),
            Some(vec![17])
        );
        // node archives carry no pg segment
        assert_eq!(
            parse_pg_major_from_name("testnet-stacks-blockchain-4.0.1-20260811.tar.zst"),
            None
        );
    }

    #[test]
    fn parses_date_segment() {
        assert_eq!(
            parse_date_from_name("stacks-blockchain-api-pg-17-8.15.0-20260711.dump"),
            Some(20260711)
        );
        assert_eq!(
            parse_date_from_name("testnet-stacks-blockchain-latest.tar.zst"),
            None
        );
    }

    #[test]
    fn staking_testnet_archives_use_the_signet_prefix() {
        assert_eq!(
            parse_version_from_name(
                "signet-stacks-blockchain-4.0.4-20260927.tar.zst",
                Kind::Node
            ),
            Some(vec![4, 0, 4])
        );
        let d = crate::config::test_deployment("network = \"staking-testnet\"");
        assert_eq!(d.net.hiro_archive_path.as_deref(), Some("staking-testnet"));
        assert_eq!(d.net.hiro_archive_prefix(), Some("signet"));
    }

    #[test]
    fn archives_from_another_network_are_named() {
        let staking = crate::config::test_deployment("network = \"staking-testnet\"");
        let testnet = crate::config::test_deployment("network = \"testnet\"");
        let mainnet = crate::config::test_deployment("network = \"mainnet\"");
        let signet = "signet-stacks-blockchain-4.0.4-20260927.tar.zst";
        let krypton = "testnet-stacks-blockchain-4.0.4-20260927.tar.zst";
        assert_eq!(other_network_archive(signet, &staking), None);
        assert_eq!(other_network_archive(krypton, &staking), Some("testnet"));
        assert_eq!(
            other_network_archive(signet, &testnet),
            Some("staking-testnet")
        );
        assert_eq!(other_network_archive(krypton, &mainnet), Some("testnet"));
        assert_eq!(
            other_network_archive(
                "mainnet-stacks-blockchain-3.1.0.0.8-20260803.tar.gz",
                &testnet
            ),
            Some("mainnet")
        );
        // API dumps carry no network in their name
        assert_eq!(
            other_network_archive("stacks-blockchain-api-pg-17-9.3.1-20260927.dump", &staking),
            None
        );
    }

    #[test]
    fn extracts_version_with_trailing_extension() {
        // version segment glued to extension: `...-8.1.0.tar.gz` style
        assert_eq!(
            parse_version_from_name("mainnet-stacks-blockchain-api-8.1.0.tar.gz", Kind::Api),
            Some(vec![8, 1, 0])
        );
    }

    // ---- download retries --------------------------------------------------------------------

    use std::io::{BufRead, BufReader as IoBufReader};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    enum Behavior {
        /// Serve Range requests; cut the first `n` multi-byte bodies off a third of the way in.
        DropBodies(usize),
        /// Answer every request with this status.
        Status(u16),
    }

    /// Minimal HTTP/1.1 server with Range + ETag support, one request per connection. Returns the
    /// base URL and a request counter.
    fn serve(body: Vec<u8>, behavior: Behavior) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/archive.tar.zst", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();
        std::thread::spawn(move || {
            let mut drops_left = match behavior {
                Behavior::DropBodies(n) => n,
                Behavior::Status(_) => 0,
            };
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                counter.fetch_add(1, Ordering::SeqCst);
                let mut range: Option<(usize, usize)> = None;
                let mut reader = IoBufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let line = line.trim_end();
                    if line.is_empty() {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                        let (a, b) = v.split_once('-').unwrap();
                        let start = a.parse().unwrap();
                        let end = if b.is_empty() {
                            body.len() - 1
                        } else {
                            b.parse().unwrap()
                        };
                        range = Some((start, end));
                    }
                }
                if let Behavior::Status(code) = behavior {
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {code} Nope\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    continue;
                }
                let (status, start, end) = match range {
                    Some((a, b)) => ("206 Partial Content", a, b),
                    None => ("200 OK", 0, body.len() - 1),
                };
                let slice = &body[start..=end];
                let mut head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nETag: \"v1\"\r\nConnection: close\r\n",
                    slice.len()
                );
                if range.is_some() {
                    head += &format!("Content-Range: bytes {start}-{end}/{}\r\n", body.len());
                }
                head += "\r\n";
                let _ = stream.write_all(head.as_bytes());
                let cut = slice.len() > 1 && drops_left > 0;
                if cut {
                    drops_left -= 1;
                    let _ = stream.write_all(&slice[..slice.len() / 3]);
                } else {
                    let _ = stream.write_all(slice);
                }
                let _ = stream.flush();
                // Dropping the stream closes the connection, mid-body when `cut`.
            }
        });
        (url, requests)
    }

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("stacksup-dl-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fast_policy(max_retries: u32) -> RetryPolicy {
        RetryPolicy {
            max_retries,
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
        }
    }

    #[test]
    fn dropped_streams_resume_automatically() {
        let body: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let (url, _) = serve(body.clone(), Behavior::DropBodies(3));
        let dir = test_dir("resume");
        let dest = dir.join("archive.tar.zst");

        // Fewer retries than drops: progress on every attempt keeps resetting the counter.
        download_with_retries(&url, &dest, Some(body.len() as u64), fast_policy(1)).unwrap();

        assert_eq!(fs::read(&dest).unwrap(), body);
        assert!(!partial_path(&dest).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dropped_body_is_the_transient_error_seen_in_the_field() {
        let body = vec![1u8; 50_000];
        let (url, _) = serve(body.clone(), Behavior::DropBodies(1));
        let dir = test_dir("field");
        let dest = dir.join("archive.tar.zst");

        let err = download_resumable(&url, &dest, Some(body.len() as u64)).unwrap_err();

        assert!(is_transient(&err));
        assert!(
            format!("{err:#}").contains("response body closed before all bytes were read"),
            "{err:#}"
        );
        // The received third is kept for the resume.
        assert_eq!(partial_path(&dest).metadata().unwrap().len(), 50_000 / 3);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn short_body_without_known_size_is_retried() {
        let body: Vec<u8> = (0..100_000u32).map(|i| (i % 7) as u8).collect();
        let (url, _) = serve(body.clone(), Behavior::DropBodies(1));
        let dir = test_dir("nosize");
        let dest = dir.join("archive.tar.zst");

        download_with_retries(&url, &dest, None, fast_policy(2)).unwrap();

        assert_eq!(fs::read(&dest).unwrap(), body);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn server_errors_give_up_after_retries_without_progress() {
        let (url, requests) = serve(vec![0; 10], Behavior::Status(503));
        let dir = test_dir("503");
        let dest = dir.join("archive.tar.zst");

        let err = download_with_retries(&url, &dest, Some(10), fast_policy(2)).unwrap_err();

        assert!(format!("{err:#}").contains("after 2 retries without progress"));
        // 3 attempts, each a redirect probe + the download request
        assert_eq!(requests.load(Ordering::SeqCst), 6);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn not_found_is_not_retried() {
        let (url, requests) = serve(vec![0; 10], Behavior::Status(404));
        let dir = test_dir("404");
        let dest = dir.join("archive.tar.zst");

        let err = download_with_retries(&url, &dest, Some(10), fast_policy(5)).unwrap_err();

        assert!(!is_transient(&err));
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn retry_backoff_doubles_up_to_the_cap() {
        let p = RetryPolicy {
            max_retries: 10,
            base_delay: Duration::from_secs(2),
            max_delay: Duration::from_secs(60),
        };
        let secs: Vec<u64> = (1..=7).map(|n| p.delay(n).as_secs()).collect();
        assert_eq!(secs, vec![2, 4, 8, 16, 32, 60, 60]);
        assert_eq!(p.delay(200).as_secs(), 60);
    }
}
