//! `veryl celox ...` runs `veryl-celox`, which forwards to `celox`.

use std::process::Command;

#[test]
fn info_is_the_one_line_description_veryl_lists() {
    let output = Command::new(env!("CARGO_BIN_EXE_veryl-celox"))
        .arg("--info")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let line = stdout.strip_suffix('\n').unwrap();
    assert!(!line.is_empty() && !line.contains('\n'), "{stdout:?}");
    assert!(line.chars().count() <= 160, "{line}");
}

#[test]
fn arguments_and_exit_status_reach_the_celox_command() {
    let version = Command::new(env!("CARGO_BIN_EXE_veryl-celox"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(version.status.success());
    assert_eq!(
        String::from_utf8(version.stdout).unwrap(),
        format!("celox {}\n", env!("CARGO_PKG_VERSION"))
    );

    let invalid = Command::new(env!("CARGO_BIN_EXE_veryl-celox"))
        .arg("no-such-command")
        .output()
        .unwrap();
    let direct = Command::new(env!("CARGO_BIN_EXE_celox"))
        .arg("no-such-command")
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    assert_eq!(invalid.status.code(), direct.status.code());
    assert_eq!(invalid.stderr, direct.stderr);
}
