//! Shared integration test utilities for this crate.

/// Environment variable that enables tests which LAUNCH CHILD PROCESSES (executable copies, windows, DLL-named files
/// beside an `.exe`): `TESELA_PROCESS_TESTS=1`. Desktop antivirus may block this pattern on a development machine; CI
/// (an ephemeral runner) always runs them. Locally, they are skipped with a message.
pub const PROCESS_TESTS_VAR: &str = "TESELA_PROCESS_TESTS";

/// Is `TESELA_PROCESS_TESTS=1` set? If not, print a message to stderr and the test must return without doing anything
/// (the assertions are unchanged: with the variable set, the tests run fully).
///
/// In CI (when the `CI` variable is set, as in GitHub Actions), skipping is NOT allowed: `cargo test` hides stderr from
/// passing tests, so a missing workflow variable could make these tests pass without running. Missing variable there = `panic!`.
pub fn process_tests_enabled() -> bool {
    if std::env::var(PROCESS_TESTS_VAR).is_ok_and(|v| v == "1") {
        return true;
    }
    assert!(
        std::env::var_os("CI").is_none(),
        "CI sin {PROCESS_TESTS_VAR}=1: las pruebas con procesos hijo no pueden saltarse en CI (revisa el workflow)"
    );
    eprintln!(
        "pruebas con procesos hijo desactivadas: defina {PROCESS_TESTS_VAR}=1 para ejecutar esta prueba"
    );
    false
}
