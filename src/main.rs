use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use herdr_scratch::ScratchKind;

#[derive(Debug, Parser)]
#[command(author, version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Toggle {
        #[arg(long, value_enum)]
        kind: Kind,
    },
    RunPopup {
        #[arg(long, value_enum)]
        kind: Kind,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Kind {
    Nvim,
    Shell,
}

impl From<Kind> for ScratchKind {
    fn from(kind: Kind) -> Self {
        match kind {
            Kind::Nvim => Self::Nvim,
            Kind::Shell => Self::Shell,
        }
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("herdr-scratch: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    match Cli::parse().command {
        Command::Toggle { kind } => herdr_scratch::toggle(kind.into()),
        Command::RunPopup { kind } => herdr_scratch::run_popup(kind.into()),
    }
}
