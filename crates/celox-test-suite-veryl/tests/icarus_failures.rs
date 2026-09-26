//! Exercise the CLI's actual process adapter without depending on installed HDL tools.
#![cfg(all(unix, feature = "icarus"))]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

fn script(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn process_failures_cannot_pass_a_negative_case() {
    let directory =
        std::env::temp_dir().join(format!("veryl-icarus-failures-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    // Only forward the command: the fake compiler also supplies reserved timeout
    // statuses. No global PATH mutation and no dependency on GNU timeout here.
    script(
        &directory.join("timeout"),
        "#!/bin/sh\nshift\nexec \"$@\"\n",
    );
    script(
        &directory.join("iverilog"),
        r#"#!/bin/sh
if [ "$1" = '-V' ]; then
    printf '%s\n' 'Icarus process fixture'
    exit 0
fi
for argument do
    case "$argument" in */source_0.sv) source_path="$argument";; esac
done
case "$SUITE_ICARUS_DIAGNOSTIC" in
    source|mixed)
        printf '%s:1: error: Bit select expressions must be a constant integral value.\n' "$source_path"
        printf '%s\n' '1 error(s) during elaboration.'
        if [ "$SUITE_ICARUS_DIAGNOSTIC" = mixed ]; then
            printf '%s\n' 'ivl: internal compiler error'
        fi
        ;;
    io) printf '%s: No such file or directory\n' "$source_path";;
    internal) printf '%s:1: error: internal compiler error\n' "$source_path";;
    summary) printf '%s\n' '1 error(s) during elaboration.';;
esac
exit "$SUITE_ICARUS_STATUS"
"#,
    );
    let mut scenarios = vec![("source", 1, "rejected")];
    for code in [1, 2, 123] {
        for diagnostic in ["empty", "io", "internal", "summary", "mixed"] {
            scenarios.push((diagnostic, code, "compile_error"));
        }
    }
    for code in [124, 125, 126, 127, 137] {
        scenarios.push(("source", code, "compile_error"));
    }
    for (diagnostic, code, expected) in scenarios {
        let output = directory.join(format!("{diagnostic}-{code}"));
        let command = Command::new(env!("CARGO_BIN_EXE_verify-icarus"))
            .args([
                "--filter",
                "hierarchy::test_dynamic_output_port_rmw_preserves_unselected_bits",
                "--jobs",
                "1",
                "--output",
            ])
            .arg(&output)
            .env("PATH", &directory)
            .env("SUITE_ICARUS_DIAGNOSTIC", diagnostic)
            .env("SUITE_ICARUS_STATUS", code.to_string())
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
