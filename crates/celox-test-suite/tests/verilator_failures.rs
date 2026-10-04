//! Check the real CLI adapter with isolated compiler/timeout process fixtures.
#![cfg(all(unix, feature = "verilator"))]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

fn script(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn tool_failures_cannot_pass_a_negative_case() {
    let directory =
        std::env::temp_dir().join(format!("veryl-verilator-failures-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    script(
        &directory.join("timeout"),
        "#!/bin/sh\nshift\nexec \"$@\"\n",
    );
    script(
        &directory.join("verilator"),
        r#"#!/bin/sh
if [ "$1" = '--version' ]; then
    printf '%s\n' 'Verilator process fixture'
    exit 0
fi
for argument do
    case "$argument" in */source_0.sv) source_path="$argument";; esac
done
case "$SUITE_VERILATOR_DIAGNOSTIC" in
    source|mixed_*)
        printf '%%Error: %s:1:1: Illegal assignment: types are not assignment compatible (IEEE 1800-2023 7.6)\n' "$source_path"
        printf '%s\n' '%Error: Exiting due to 1 error(s)'
        ;;
esac
case "$SUITE_VERILATOR_DIAGNOSTIC" in
    io|mixed_io) printf '%%Error: Cannot write %s\n' "$source_path";;
    internal|mixed_internal) printf '%s\n' '%Error: Internal Error: V3Ast.cpp:1: broken invariant';;
    located_internal) printf '%%Error: %s:1:1: Internal Error: broken invariant\n' "$source_path";;
    unsupported|mixed_unsupported) printf '%%Error-UNSUPPORTED: %s:1:1: Unsupported: example construct\n' "$source_path";;
    cpp|mixed_cpp) printf '%s\n' 'harness.cpp:1: error: compilation failed';;
    make|mixed_make) printf '%s\n' '%Error: make failed with status 2';;
    command|mixed_command) printf '%s\n' '%Error: Command Failed ulimit -s unlimited; exec verilator_bin';;
    summary) printf '%s\n' '%Error: Exiting due to 1 error(s)';;
esac
exit "$SUITE_VERILATOR_STATUS"
"#,
    );
    let mut scenarios = vec![("source", 1, "rejected")];
    for diagnostic in [
        "empty",
        "io",
        "internal",
        "located_internal",
        "unsupported",
        "cpp",
        "make",
        "command",
        "summary",
        "mixed_io",
        "mixed_internal",
        "mixed_unsupported",
        "mixed_cpp",
        "mixed_make",
        "mixed_command",
    ] {
        scenarios.push((diagnostic, 1, "compile_error"));
    }
    for code in [2, 124, 125, 126, 127, 137] {
        scenarios.push(("source", code, "compile_error"));
    }
    for (diagnostic, code, expected) in scenarios {
        let output = directory.join(format!("{diagnostic}-{code}"));
        let command = Command::new(env!("CARGO_BIN_EXE_verify-verilator"))
            .args([
                "--filter",
                "hierarchy::test_dynamic_prefix_colon_output_port_allows_zero_lsb",
                "--jobs",
                "1",
                "--output",
            ])
            .arg(&output)
            .env("PATH", &directory)
            .env("SUITE_VERILATOR_DIAGNOSTIC", diagnostic)
            .env("SUITE_VERILATOR_STATUS", code.to_string())
            .output()
            .unwrap();
        assert_eq!(
            command.status.code(),
            Some(if expected == "rejected" { 0 } else { 1 }),
            "{diagnostic} {code}: {}",
            String::from_utf8_lossy(&command.stderr)
        );
        let report: serde_json::Value =
            serde_json::from_slice(&fs::read(output.join("results.json")).unwrap()).unwrap();
        assert_eq!(
            report["cases"][0]["status"], expected,
            "{diagnostic} {code}: {report}"
        );
        assert_eq!(report["counts"][expected], 1);
        assert_eq!(report["counts"]["passed"], 0);
    }
    fs::remove_dir_all(directory).unwrap();
}
