//! Test hook for the client crash tests (T054): with `LODE_TEST_CRASH_AT=<point>` set, the
//! process exits abruptly at that point, as if it were killed. No cleanup runs and no
//! destructors; whatever is on disk at that moment is what the next run finds.

/// Exit code of a simulated crash.
pub const CRASH_EXIT: i32 = 86;

/// Exits at once if the crash test asked for `point`.
pub fn point(name: &str) {
    if std::env::var_os("LODE_TEST_CRASH_AT").is_some_and(|v| v == name) {
        std::process::exit(CRASH_EXIT);
    }
}
