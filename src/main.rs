mod chainstate;
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
    /// Manage the stack's configuration (stacks.toml and rendered files)
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Validate config, render service configs, and start enabled services
    Start,
    /// Stop enabled services (external services are never touched)
    Stop,
    /// Show the state of every service in the stack
    Status,
    /// Tail logs from managed services
    Logs {
        /// Service name (e.g. stacks-node); omit for all
        service: Option<String>,
    },
    /// Check config coherence and connectivity to every service
    Doctor,
    /// Operations on the stack's on-disk state
    Chainstate {
        #[command(subcommand)]
        command: ChainstateCommand,
    },
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Create a stacks.toml in the current directory
    Init {
        /// Overwrite an existing stacks.toml
        #[arg(long)]
        force: bool,
    },
    /// Render all service configs (compose file, node TOML, API env) without starting anything
    Render,
}

#[derive(Subcommand)]
enum ChainstateCommand {
    /// Permanently delete the chainstate directory (bitcoind, stacks-node,
    /// signer, and Postgres data) — asks for confirmation first
    Wipe {
        /// Skip the confirmation prompt (for scripts)
        #[arg(long)]
        yes: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Config { command: ConfigCommand::Init { force } } => config::init(force),
        Command::Config { command: ConfigCommand::Render } => {
            let stack = config::load(&cli.config)?;
            let dir = render::render(&stack, &cli.data_dir)?;
            println!("Rendered service configs to {}/", dir.display());
            Ok(())
        }
        Command::Start => {
            let stack = config::load(&cli.config)?;
            render::render(&stack, &cli.data_dir)?;
            docker::start(&stack, &cli.data_dir)
        }
        Command::Stop => {
            let stack = config::load(&cli.config)?;
            docker::stop(&stack, &cli.data_dir)
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
        // Deliberately does not load stacks.toml: wiping state must work even
        // when the config is broken or gone.
        Command::Chainstate { command: ChainstateCommand::Wipe { yes } } => {
            chainstate::wipe(&cli.data_dir, yes)
        }
    }
}
