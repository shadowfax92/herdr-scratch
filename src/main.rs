use std::io::{self, IsTerminal, Write};

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
    let command = Cli::parse().command;
    let popup = matches!(command, Command::RunPopup);
    if let Err(error) = run(command) {
        eprintln!("herdr-scratch: {error:#}");
        // Native popups disappear on process exit. Retain failures on either
        // side of SSH until acknowledged, while piped/worker invocations stay
        // nonblocking and successful hide/detach never needs an extra key.
        if popup && io::stdin().is_terminal() {
            eprint!("Press Enter to close...");
            let _ = io::stderr().flush();
            let _ = io::stdin().read_line(&mut String::new());
        }
        std::process::exit(1);
    }
}

fn run(command: Command) -> Result<()> {
    match command {
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
