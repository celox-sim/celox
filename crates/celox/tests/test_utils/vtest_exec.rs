//! Optional process boundary between libtest registrations and the executor.

#[allow(
    clippy::disallowed_methods,
    reason = "test runner configuration boundary"
)]
pub fn dispatch(language: &str, name: &str, backend: &str) -> bool {
    let Some(executable) = std::env::var_os("CELOX_VTEST_RUNNER") else {
        return false;
    };
    let status = std::process::Command::new(&executable)
        .args([language, name, backend])
        // The executor shares these adapters. Do not dispatch recursively.
        .env_remove("CELOX_VTEST_RUNNER")
        .status()
        .unwrap_or_else(|error| panic!("start vtest executor {executable:?}: {error}"));
    assert!(
        status.success(),
        "vtest {language}/{name}/{backend}: {status}"
    );
    true
}
