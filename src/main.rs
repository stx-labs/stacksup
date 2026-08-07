mod chainstate;
mod config;
mod docker;
mod doctor;
mod download;
mod export;
mod render;
mod services;
mod upgrade;
mod versions;

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
    Start {
        /// Single service to start (e.g. stacks-node); omit to start all
        service: Option<String>,
        /// Start from the existing rendered/ configs without re-rendering
        #[arg(long)]
        no_render: bool,
    },
    /// Stop enabled services (external services are never touched)
    Stop {
        /// Single service to stop (e.g. stacks-node); omit to stop all
        service: Option<String>,
        /// Also remove containers and the network (deletes container logs)
        #[arg(long)]
        destroy: bool,
    },
    /// Pull the latest images for every enabled service
    Pull,
    /// Check registries for newer image versions and print upgrade guidance
    /// (suggestions only — never applies anything)
    Upgrade {
        /// Single service to check; omit for all enabled services
        service: Option<String>,
    },
    /// Show the state of every service in the stack
    Status,
    /// Tail logs from managed services
    Logs {
        /// Service name (e.g. stacks-node); omit for all
        service: Option<String>,
        #[command(subcommand)]
        command: Option<LogsCommand>,
    },
    /// Operations on the stack's on-disk state
    Chainstate {
        #[command(subcommand)]
        command: ChainstateCommand,
    },
}

#[derive(Subcommand)]
enum LogsCommand {
    /// Export logs (and diagnostics) into a shareable, redacted bundle
    Export {
        /// Single service to export (e.g. stacks-node); omit for all
        service: Option<String>,
        /// How far back to collect logs (docker duration, e.g. 2h, 30m)
        #[arg(long, default_value = "24h")]
        since: String,
        /// Only logs — skip versions, configs, and diagnostic reports
        #[arg(long)]
        logs_only: bool,
        /// Output path (default: stacks-support-<network>-<timestamp>.tar.gz,
        /// or <service>-<timestamp>.log for single-service --logs-only)
        #[arg(short, long)]
        out: Option<PathBuf>,
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
    /// Permanently delete on-disk chainstate — asks for confirmation first
    Wipe {
        /// Single service's state to wipe (bitcoind, stacks-node,
        /// stacks-signer, postgres); omit to wipe everything
        service: Option<String>,
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
        Command::Start { service, no_render } => {
            let stack = config::load(&cli.config)?;
            if !no_render {
                render::render(&stack, &cli.data_dir)?;
            }
            docker::start(&stack, &cli.data_dir, service.as_deref())
        }
        Command::Stop { service, destroy } => {
            let stack = config::load(&cli.config)?;
            docker::stop(&stack, &cli.data_dir, service.as_deref(), destroy)
        }
        Command::Pull => {
            let stack = config::load(&cli.config)?;
            render::render(&stack, &cli.data_dir)?;
            docker::pull(&stack, &cli.data_dir)
        }
        Command::Upgrade { service } => {
            let stack = config::load(&cli.config)?;
            upgrade::run(&stack, service.as_deref())
        }
        Command::Status => {
            let stack = config::load(&cli.config)?;
            docker::status(&stack, &cli.data_dir)
        }
        Command::Logs { service, command: None } => {
            let stack = config::load(&cli.config)?;
            docker::logs(&stack, &cli.data_dir, service.as_deref())
        }
        Command::Logs {
            command: Some(LogsCommand::Export { service, since, logs_only, out }),
            ..
        } => {
            let stack = config::load(&cli.config)?;
            export::run(
                &stack,
                &cli.config,
                &cli.data_dir,
                export::Opts { service, since, logs_only, out },
            )
        }
        Command::Config { command: ConfigCommand::Check } => {
            let stack = config::load(&cli.config)?;
            doctor::run(&stack)
        }
        // Deliberately does not load stacks.toml: wiping state must work even
        // when the config is broken or gone.
        Command::Chainstate { command: ChainstateCommand::Wipe { service, yes } } => {
            chainstate::wipe(&cli.data_dir, service.as_deref(), yes)
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
