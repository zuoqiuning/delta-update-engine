use clap::Parser;

use delta_update_cli::cli::{Cli, Command};
use delta_update_cli::commands;
use delta_update_engine::EngineError;

fn main() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "delta_update_engine=warn,delta_update_cli=info".into());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();

    let cli = Cli::parse();
    if let Err(error) = run(cli.command) {
        eprintln!("Error: {error:?}");
        std::process::exit(exit_code(&error));
    }
}

fn run(command: Command) -> anyhow::Result<()> {
    match command {
        Command::Release(args) => commands::release::run(&args),
        Command::Verify(args) => commands::verify::run(&args),
        Command::VerifyKey(args) => commands::verify_key::run(&args),
        Command::KeyInfo(args) => commands::key_info::run(&args),
        Command::Doctor(args) => commands::doctor::run(&args),
        Command::Inspect(args) => commands::inspect::run(&args),
        Command::Init(args) => commands::init::run(&args),
        Command::Migrate(args) => commands::migrate::run(&args),
        Command::Report(args) => commands::report::run(&args),
        Command::Serve(args) => commands::serve::run(&args),
        Command::Rollback(args) => commands::rollback::run(&args),
        Command::Promote(args) => commands::promote::run(&args),
    }
}

/// Engine errors carry a stable exit code; anything else (I/O, usage) stays 1.
fn exit_code(error: &anyhow::Error) -> i32 {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<EngineError>())
        .map_or(1, EngineError::exit_code)
}
