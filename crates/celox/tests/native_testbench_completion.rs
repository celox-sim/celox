#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

// This is a Celox adapter contract test, rather than a portable HDL case:
// the ordinary native execution API intentionally permits fall-through.
fn check_completion(backend: &str) {
    use celox_test_suite_veryl::Design;

    for (name, body, expected_error) in [
        ("finish", "$assert(1'd1); $finish();", None),
        (
            "fallthrough",
            "$assert(1'd1);",
            Some("without reaching $finish"),
        ),
        (
            "skipped_finish",
            "if enabled { $finish(); }",
            Some("without reaching $finish"),
        ),
        (
            "nested_finish",
            "if !enabled { $finish(); } $assert(1'd0);",
            None,
        ),
        (
            "failed_assertion",
            "$assert(1'd0); $finish();",
            Some("assertion failed"),
        ),
    ] {
        let code = format!(
            "#[test(Top)] module Top {{ var enabled: logic; initial {{ enabled = 1'd0; {body} }} }}"
        );
        let mut sim = test_utils::suite::build(&Design::new(&code, "Top"), backend).unwrap();
        let result = sim.run_testbench();
        if let Some(message) = expected_error {
            let error = result.expect_err(&format!("{backend}: {name}"));
            assert!(
                error.to_string().contains(message),
                "{backend}: {name}: {error}"
            );
        } else {
            result.unwrap_or_else(|error| panic!("{backend}: {name}: {error}"));
        }
    }
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn native() {
    check_completion("native");
}
#[test]
fn cranelift() {
    check_completion("cranelift");
}
#[test]
fn wasm() {
    check_completion("wasm");
}
#[test]
fn interpreter() {
    check_completion("interp");
}

#[test]
fn ordinary_execution_still_allows_fallthrough() {
    use celox::testbench::{compile_initial_testbench, run_compiled_testbench};
    let mut sim = celox::Simulator::builder(
        "#[test(Top)] module Top { initial { $assert(1'd1); } }",
        "Top",
    )
    .build_interpreter()
    .unwrap();
    let program = compile_initial_testbench(&sim).unwrap();
    assert_eq!(
        run_compiled_testbench(&mut sim, &program),
        celox::TestResult::Pass
    );
}
