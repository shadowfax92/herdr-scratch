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
    }
}
