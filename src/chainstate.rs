//! `stacks chainstate` — operations on the stack's on-disk state.
//! More subcommands (snapshot, restore, ...) will land here.

use std::io::{self, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};

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

    println!("This will permanently delete {}", dir.display());
    if !contents.is_empty() {
        println!("  contents: {}", contents.join(", "));
    }
    println!("Synced chainstate can take days to rebuild. THIS CANNOT BE UNDONE.");

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
