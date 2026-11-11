use clap::Parser;

use delta_update_cli::cli::{Cli, Command};
use delta_update_cli::commands;

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Release(args) => commands::release::run(&args),
        Command::Verify(args) => commands::verify::run(&args),
        Command::VerifyKey(args) => commands::verify_key::run(&args),
        Command::Doctor(args) => commands::doctor::run(&args),
        Command::Inspect(args) => commands::inspect::run(&args),
    }
}
