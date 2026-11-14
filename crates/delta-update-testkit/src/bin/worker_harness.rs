//! Replacement-process harness for process-level worker tests.
//!
//! The test copies this executable into a temporary install root and starts it
//! with the real worker arguments. Public keys come from the environment so the
//! harness can play both the verified and the rejected case.

fn main() {
    let keys: Vec<String> = std::env::var("WORKER_HARNESS_KEYS")
        .ok()
        .map(|raw| {
            raw.split(',')
                .filter(|key| !key.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if let Some(code) = delta_update_engine::apply::run_applier_from_args_with_keys(&keys) {
        std::process::exit(code);
    }
    eprintln!("worker harness: not started in worker mode");
    std::process::exit(2);
}
