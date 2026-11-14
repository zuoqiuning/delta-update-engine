use crate::{run_command, run_command_with_env};

pub fn run() -> Result<(), String> {
    run_command("cargo", &["fmt", "--all", "--check"])?;
    // Clippy 1.99 ICEs on incremental metadata for this workspace; clippy does
    // not need incremental artifacts, so disable them for this step only.
    run_command_with_env(
        "cargo",
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
        &[("CARGO_INCREMENTAL", "0")],
    )?;
    run_command("cargo", &["test", "--workspace"])?;
    // `real-test` is excluded from the workspace; check the quickstart demo
    // explicitly so it cannot drift behind Engine API changes. Sharing the
    // target dir keeps this cheap by reusing the workspace build.
    run_command_with_env(
        "cargo",
        &["check", "--manifest-path", "real-test/demo-app/Cargo.toml"],
        &[("CARGO_TARGET_DIR", "target")],
    )?;
    super::contract::run()?;
    Ok(())
}
