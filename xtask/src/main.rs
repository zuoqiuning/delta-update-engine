mod tasks;

use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let task = std::env::args().nth(1).unwrap_or_default();
    let result = match task.as_str() {
        "ci" => tasks::ci::run(),
        "docs-check" => tasks::docs_links::run(),
        "version-sync" => tasks::version_sync::run(),
        "fixtures" => tasks::fixtures::run(),
        other => {
            eprintln!("unknown task: {other}");
            eprintln!("available: ci, docs-check, version-sync, fixtures");
            return ExitCode::FAILURE;
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask failed: {error}");
            ExitCode::FAILURE
        }
    }
}

pub(crate) fn run_command(program: &str, args: &[&str]) -> Result<(), String> {
    run_command_with_env(program, args, &[])
}

pub(crate) fn run_command_with_env(
    program: &str,
    args: &[&str],
    envs: &[(&str, &str)],
) -> Result<(), String> {
    println!("$ {program} {}", args.join(" "));
    let mut command = Command::new(program);
    command.args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    let status = command
        .status()
        .map_err(|err| format!("cannot run {program}: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} exited with {status}"))
    }
}
