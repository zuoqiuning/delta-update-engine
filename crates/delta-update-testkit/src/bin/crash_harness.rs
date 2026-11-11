fn main() {
    let crash_at = std::env::var("CRASH_AT").unwrap_or_default();
    if crash_at.is_empty() {
        std::process::exit(0);
    }
    eprintln!("crash harness: aborting at {crash_at}");
    std::process::abort();
}
