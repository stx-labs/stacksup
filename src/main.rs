mod config;
mod docker;
mod doctor;
mod render;
mod services;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "stacks",
    version,
    about = "Run and manage a Stacks node stack (bitcoind, stacks-node, signer, APIs, Postgres)"
)]
struct Cli {
    /// Path to the stack config: a stacks.toml file or a directory containing
    /// one [default: ./stacks.toml]
    #[arg(short, long, global = true, default_value = "stacks.toml")]
    config: PathBuf,

    /// Where the stack lives on disk: rendered configs, the compose file, and
    /// all service data (chainstate, Postgres, ...) go under this directory
    #[arg(long, global = true, default_value = ".")]
    data_dir: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a stacks.toml in the current directory
    Init {
        /// Overwrite an existing stacks.toml
        #[arg(long)]
        force: bool,
    },
    /// Validate config, render service configs, and start managed services
    Up,
    /// Stop managed services (external services are never touched)
    Down,
    /// Render all service configs (compose file, node TOML, API env) without starting anything
    Render,
    /// Show the state of every service in the stack
    Status,
    /// Tail logs from managed services
    Logs {
        /// Service name (e.g. stacks-node); omit for all
        service: Option<String>,
    },
    /// Check config coherence and connectivity to every service
    Doctor,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Init { force } => config::init(force),
        Command::Render => {
            let stack = config::load(&cli.config)?;
            let dir = render::render(&stack, &cli.data_dir)?;
            println!("Rendered service configs to {}/", dir.display());
            Ok(())
        }
        Command::Up => {
            let stack = config::load(&cli.config)?;
            render::render(&stack, &cli.data_dir)?;
            docker::up(&stack, &cli.data_dir)
        }
        Command::Down => {
            let stack = config::load(&cli.config)?;
            docker::down(&stack, &cli.data_dir)
        }
        Command::Status => {
            let stack = config::load(&cli.config)?;
            docker::status(&stack, &cli.data_dir)
        }
        Command::Logs { service } => {
            let stack = config::load(&cli.config)?;
            docker::logs(&stack, &cli.data_dir, service.as_deref())
        }
        Command::Doctor => {
            let stack = config::load(&cli.config)?;
            doctor::run(&stack)
        }
    }
}
