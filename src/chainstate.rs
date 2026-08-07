//! `stacks chainstate` — operations on the stack's on-disk state.
//! More subcommands (snapshot, restore, ...) will land here.

use std::io::{self, Write};
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use colored::Colorize;

use crate::config::{Network, ServiceMode, Stack};

/// One service's view of the chain: (stacks height, bitcoin height).
struct Tip {
    service: &'static str,
    source: String,
    stacks: Option<u64>,
    bitcoin: Option<u64>,
    note: Option<String>,
}

/// `stacks chainstate status` — compare every enabled service's chain tip.
///
/// Works whether the stack is running or stopped, with different coverage:
/// the node's tips are read straight from its sqlite files (safe read-only
/// even while the node writes), while the API's tip lives in Postgres and
/// bitcoind's in LevelDB — those two are only checkable while running.
pub fn status(stack: &Stack, data_dir: &Path) -> Result<()> {
    let running = crate::docker::running_services(data_dir).unwrap_or_default();
    let mut tips: Vec<Tip> = Vec::new();

    if stack.stacks_node.mode == ServiceMode::Enabled {
        tips.push(node_tip(stack, data_dir));
    }
    if stack.bitcoind.mode == ServiceMode::Enabled {
        tips.push(bitcoind_tip(stack, running.iter().any(|s| s == "bitcoind")));
    }
    if stack.stacks_api.mode == ServiceMode::Enabled {
        tips.push(api_tip(stack, running.iter().any(|s| s == "postgres")));
    }

    if tips.is_empty() {
        bail!("no enabled services hold chainstate — nothing to check");
    }

    println!("{:<14} {:>14} {:>15}  source", "service", "stacks height", "bitcoin height");
    for t in &tips {
        let fmt = |v: Option<u64>| v.map_or("-".to_string(), |h| h.to_string());
        println!("{:<14} {:>14} {:>15}  {}", t.service, fmt(t.stacks), fmt(t.bitcoin), t.source);
        if let Some(note) = &t.note {
            println!("{:<14} {}", "", note.yellow());
        }
    }

    // Verdict: what matters is the stacks height of the node vs the API,
    // and crucially WHICH ONE is ahead — the failure modes are asymmetric.
    let node_stacks = tips.iter().find(|t| t.service == "stacks-node").and_then(|t| t.stacks);
    let api_stacks = tips.iter().find(|t| t.service == "stacks-api").and_then(|t| t.stacks);

    println!();
    match (node_stacks, api_stacks) {
        (Some(node), Some(api)) if node == api => {
            println!("{}", format!("✓ stacks-node and stacks-api agree on the stacks chain tip ({node})").green());
        }
        (Some(node), Some(api)) if node < api => {
            let warning = format!(
                "⚠ stacks-node ({node}) is BEHIND stacks-api ({api}).\n  \
                 This is recoverable: as the node syncs it will reach height {api} and\n  \
                 the API will follow along normally from there. No action needed —\n  \
                 let the node catch up."
            );
            println!("{}", warning.yellow());
        }
        (Some(node), Some(api)) => {
            let error = format!(
                "✗ stacks-node ({node}) is AHEAD of stacks-api ({api}).\n  \
                 This is NOT recoverable: the node has already processed blocks the API\n  \
                 never received events for, and it will not re-send them. The API's\n  \
                 database is permanently missing those blocks.\n  \
                 Fix: `stacks stop && stacks chainstate wipe && stacks start` to re-sync\n  \
                 from genesis, or restore a consistent snapshot."
            );
            println!("{}", error.red());
            bail!("chainstate is inconsistent");
        }
        _ => {
            println!("Not enough readable tips to compare — need both the stacks-node and");
            println!("stacks-api heights (start the stack for full coverage).");
        }
    }
    Ok(())
}

/// The node's canonical view, read from its on-disk sqlite DBs (read-only
/// opens are safe while the node runs).
///
/// Bitcoin height: latest pox-valid snapshot in the sortition DB. Stacks
/// height: MAX over the chainstate headers DB (nakamoto_block_headers +
/// epoch2 block_headers) — NOT the sortition snapshot's
/// canonical_stacks_tip_height, which only advances per *burn* block and so
/// lags by up to a tenure's worth of stacks blocks post-Nakamoto.
fn node_tip(stack: &Stack, data_dir: &Path) -> Tip {
    let mode = match stack.network {
        Network::Mainnet => "mainnet",
        Network::Testnet => "krypton",
        Network::Mocknet => "mocknet",
    };
    let node_dir = data_dir.join("chainstate/stacks-node").join(mode);
    let sort_db = node_dir.join("burnchain/sortition/marf.sqlite");
    let headers_db = node_dir.join("chainstate/vm/index.sqlite");
    let mut tip = Tip {
        service: "stacks-node",
        source: "node chainstate dbs (on disk)".into(),
        stacks: None,
        bitcoin: None,
        note: None,
    };
    if !sort_db.exists() {
        tip.note = Some(format!("no sortition db at {} — has the node run yet?", sort_db.display()));
        return tip;
    }

    let open = |db: &Path| {
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    };

    match open(&sort_db).and_then(|conn| {
        conn.query_row(
            "SELECT block_height FROM snapshots WHERE pox_valid = 1 \
             ORDER BY block_height DESC LIMIT 1",
            [],
            |row| row.get::<_, u64>(0),
        )
    }) {
        Ok(btc) => tip.bitcoin = Some(btc),
        Err(e) => tip.note = Some(format!("could not read sortition db ({e})")),
    }

    if headers_db.exists() {
        match open(&headers_db).and_then(|conn| {
            conn.query_row(
                "SELECT MAX(h) FROM (\
                   SELECT MAX(block_height) AS h FROM nakamoto_block_headers \
                   UNION ALL \
                   SELECT MAX(block_height) AS h FROM block_headers)",
                [],
                |row| row.get::<_, Option<u64>>(0),
            )
        }) {
            Ok(Some(stx)) => tip.stacks = Some(stx),
            Ok(None) => tip.note = Some("headers db has no blocks yet".into()),
            Err(e) => tip.note = Some(format!("could not read headers db ({e})")),
        }
    } else {
        tip.note = Some(format!("no headers db at {}", headers_db.display()));
    }
    tip
}

/// bitcoind's own height via bitcoin-cli inside the running container.
/// Offline its state is LevelDB, which we don't parse.
fn bitcoind_tip(stack: &Stack, running: bool) -> Tip {
    let mut tip = Tip {
        service: "bitcoind",
        source: "bitcoin-cli (running container)".into(),
        stacks: None,
        bitcoin: None,
        note: None,
    };
    if !running {
        tip.note = Some("bitcoind is not running — height in LevelDB is not readable offline".into());
        return tip;
    }
    let chain = if stack.network == Network::Mainnet { "main" } else { "test" };
    let out = Command::new("docker")
        .args([
            "exec",
            "stacks-bitcoind",
            "bitcoin-cli",
            &format!("-chain={chain}"),
            &format!("-rpcuser={}", stack.bitcoind.rpc_user.as_deref().unwrap_or("stacks")),
            &format!("-rpcpassword={}", stack.bitcoind.rpc_password.as_deref().unwrap_or("stacks")),
            "getblockcount",
        ])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            tip.bitcoin = String::from_utf8_lossy(&o.stdout).trim().parse().ok();
        }
        Ok(o) => tip.note = Some(format!("bitcoin-cli failed: {}", String::from_utf8_lossy(&o.stderr).trim())),
        Err(e) => tip.note = Some(format!("could not run bitcoin-cli: {e}")),
    }
    tip
}

/// The API's indexed tip from its Postgres chain_tip table (single-row table
/// with block_height and burn_block_height). Needs the postgres server up.
fn api_tip(stack: &Stack, postgres_running: bool) -> Tip {
    let mut tip = Tip {
        service: "stacks-api",
        source: "postgres chain_tip table".into(),
        stacks: None,
        bitcoin: None,
        note: None,
    };
    if stack.postgres.mode != ServiceMode::Enabled {
        tip.note = Some("API uses an external/disabled postgres — not checked by this tool".into());
        return tip;
    }
    if !postgres_running {
        tip.note = Some("postgres is not running — `stacks start` to check the API's tip".into());
        return tip;
    }
    let user = stack.postgres.user.as_deref().unwrap_or("postgres");
    let out = Command::new("docker")
        .args([
            "exec",
            "stacks-postgres",
            "psql",
            "-U",
            user,
            "-d",
            "stacks_blockchain_api",
            "-tAc",
            "SELECT block_height, burn_block_height FROM stacks_blockchain_api.chain_tip",
        ])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            let mut parts = text.trim().split('|');
            tip.stacks = parts.next().and_then(|s| s.trim().parse().ok());
            tip.bitcoin = parts.next().and_then(|s| s.trim().parse().ok());
            if tip.stacks.is_none() {
                tip.note = Some("chain_tip is empty — the API hasn't indexed a block yet".into());
            }
        }
        Ok(o) => tip.note = Some(format!("psql failed: {}", String::from_utf8_lossy(&o.stderr).trim())),
        Err(e) => tip.note = Some(format!("could not run psql: {e}")),
    }
    tip
}

pub fn wipe(data_dir: &Path, yes: bool) -> Result<()> {
    let dir = data_dir.join("chainstate");
    if !dir.exists() {
        println!("Nothing to wipe: {} does not exist.", dir.display());
        return Ok(());
    }

    // Wiping state under running containers corrupts them; refuse first.
    if let Some(running) = crate::docker::running_services(data_dir) {
        if !running.is_empty() {
            bail!(
                "the stack is still running ({}) — run `stacks stop` first",
                running.join(", ")
            );
        }
    }

    let contents: Vec<String> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();

    println!("{}", format!("This will permanently delete {}", dir.display()).yellow());
    if !contents.is_empty() {
        println!("{}", format!("  contents: {}", contents.join(", ")).yellow());
    }
    println!("{}", "Synced chainstate can take days to rebuild. THIS CANNOT BE UNDONE.".red().bold());

    if !yes {
        print!("Type 'yes' to delete, anything else to abort: ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        if input.trim() != "yes" {
            println!("Aborted — nothing was deleted.");
            return Ok(());
        }
    }

    std::fs::remove_dir_all(&dir)
        .with_context(|| format!("failed to delete {}", dir.display()))?;
    println!("Deleted {}", dir.display());
    Ok(())
}
