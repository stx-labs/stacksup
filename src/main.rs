mod chainstate;
mod config;
mod docker;
mod doctor;
mod download;
mod render;
mod services;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::Colorize;

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
    /// Pull the latest images for every enabled service
    Pull,
    /// Show the state of every service in the stack
    Status,
    /// Tail logs from managed services
    Logs {
        /// Service name (e.g. stacks-node); omit for all
        service: Option<String>,
    },
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
    /// Check config coherence and connectivity to every service
    Check,
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
    /// Show each service's chain tip (stacks + bitcoin heights) and whether they agree
    Status,
    /// Download and restore chainstate from the Hiro Archive (resumable)
    Download {
        /// Which service's archive to fetch
        #[arg(long, value_enum, default_value_t = ServiceArg::All)]
        service: ServiceArg,
        /// Specific archive: a filename (resolved against the network's
        /// archive path), a full URL, or a local file path. Requires
        /// --service node or --service api.
        #[arg(long)]
        archive: Option<String>,
        /// Print the plan (sizes, versions, disk) and exit
        #[arg(long)]
        check_only: bool,
        /// Skip the confirmation prompt (for scripts / nohup)
        #[arg(long)]
        yes: bool,
        /// Skip sha256 verification of downloaded archives
        #[arg(long)]
        no_verify: bool,
        /// Proceed even if the archive version is newer than the configured version
        #[arg(long)]
        skip_version_check: bool,
        /// Keep downloaded archives after a successful restore
        #[arg(long)]
        keep_archives: bool,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum ServiceArg {
    Node,
    Api,
    All,
}

fn main() {
    if let Err(e) = run() {
        // `{e:#}` renders the whole context chain on one line; colored
        // degrades to plain text when stderr isn't a terminal.
        eprintln!("{}", format!("Error: {e:#}").red());
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
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
        Command::Pull => {
            let stack = config::load(&cli.config)?;
            render::render(&stack, &cli.data_dir)?;
            docker::pull(&stack, &cli.data_dir)
        }
        Command::Status => {
            let stack = config::load(&cli.config)?;
            docker::status(&stack, &cli.data_dir)
        }
        Command::Logs { service } => {
            let stack = config::load(&cli.config)?;
            docker::logs(&stack, &cli.data_dir, service.as_deref())
        }
        Command::Config { command: ConfigCommand::Check } => {
            let stack = config::load(&cli.config)?;
            doctor::run(&stack)
        }
        // Deliberately does not load stacks.toml: wiping state must work even
        // when the config is broken or gone.
        Command::Chainstate { command: ChainstateCommand::Wipe { yes } } => {
            chainstate::wipe(&cli.data_dir, yes)
        }
        Command::Chainstate { command: ChainstateCommand::Status } => {
            let stack = config::load(&cli.config)?;
            chainstate::status(&stack, &cli.data_dir)
        }
        Command::Chainstate {
            command:
                ChainstateCommand::Download {
                    service,
                    archive,
                    check_only,
                    yes,
                    no_verify,
                    skip_version_check,
                    keep_archives,
                },
        } => {
            let service = match service {
                ServiceArg::Node => download::ServiceSel::Node,
                ServiceArg::Api => download::ServiceSel::Api,
                ServiceArg::All => download::ServiceSel::All,
            };
            if archive.is_some() && matches!(service, download::ServiceSel::All) {
                anyhow::bail!("--archive requires exactly one service: --service node or --service api");
            }
            let stack = config::load(&cli.config)?;
            download::run(
                &stack,
                &cli.data_dir,
                download::Opts {
                    service,
                    archive,
                    check_only,
                    yes,
                    no_verify,
                    skip_version_check,
                    keep_archives,
                },
            )
        }
    }
}
