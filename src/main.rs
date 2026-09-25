use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(author, version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Toggle {
        #[arg(long, visible_alias = "kind")]
        scratch: String,
    },
    RunPopup,
    Config,
    /// Preview reclaimable sessions; --apply performs one bounded sweep.
    Cleanup {
        #[arg(long)]
        apply: bool,
    },
    /// Reap stale sessions using the cleanup policy and notify the Herdr client.
    Reap,
    /// Ensure the independent background cleanup worker is running.
    CleanupStart,
    /// Ask the worker to stop; existing scratches remain intact.
    CleanupStop,
    #[command(hide = true)]
    CleanupWorker,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("herdr-scratch: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    match Cli::parse().command {
        Command::Toggle { scratch } => herdr_scratch::toggle(&scratch),
        Command::RunPopup => herdr_scratch::run_popup(),
        Command::Config => herdr_scratch::show_config(),
        Command::Cleanup { apply } => herdr_scratch::cleanup::inspect(apply),
        Command::Reap => herdr_scratch::cleanup::reap(),
        Command::CleanupStart => herdr_scratch::cleanup::start(),
        Command::CleanupStop => herdr_scratch::cleanup::stop(),
        Command::CleanupWorker => herdr_scratch::cleanup::worker(),
    }
}
