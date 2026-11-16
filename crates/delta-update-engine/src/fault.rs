//! Crash points for recovery tests.
//!
//! Compiled only with the `fault-injection` feature, which the testkit enables.
//! The hook reads an environment variable so a spawned worker can abort at a
//! named stage; no production binary can reach it.

#[cfg(feature = "fault-injection")]
pub(crate) fn crash_point(name: &str) {
    if std::env::var("DELTA_UPDATE_CRASH_AT").as_deref() == Ok(name) {
        eprintln!("fault injection: aborting at {name}");
        std::process::abort();
    }
}

#[cfg(not(feature = "fault-injection"))]
#[inline(always)]
pub(crate) fn crash_point(_name: &str) {}
