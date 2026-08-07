#![allow(clippy::disallowed_methods)] // Opt-in test configuration is environment-driven.

use std::{env, fs, path::Path, process::Command};

use celox::{DomainKind, NativeProgramInstance};

fn python_output(python: &str, arguments: &[&str]) -> String {
    let output = Command::new(python).args(arguments).output().unwrap();
    assert!(
        output.status.success(),
        "python helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

#[test]
fn cocotb_drives_an_attached_native_flip_flop() {
    let Some(python) = env::var_os("CELOX_COCOTB_PYTHON") else {
        return;
    };
    let python = python.to_string_lossy().into_owned();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cocotb");
    let temporary = tempfile::tempdir().unwrap();
    let executable = temporary.path().join("counter-sim");
    let results = temporary.path().join("results.xml");

    let compile = Command::new(env!("CARGO_BIN_EXE_celox-vpi-compile"))
        .arg(fixture.join("counter.veryl"))
        .args(["--top", "Top", "--runtime"])
        .arg(env!("CARGO_BIN_EXE_celox-vpi-runtime"))
        .arg("--output")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "native compilation failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let libpython = python_output(&python, &["-m", "cocotb_tools.config", "--libpython"]);
    let vpi = python_output(
        &python,
        &[
            "-c",
            "from pathlib import Path; import cocotb; print(Path(cocotb.__file__).parent / 'libs' / 'libcocotbvpi_icarus.vpl')",
        ],
    );
    let site_packages = python_output(
        &python,
        &["-c", "import site; print(':'.join(site.getsitepackages()))"],
    );
    let python_path = format!("{}:{site_packages}", fixture.display());
    let runtime = Command::new(&executable)
        .args(["--vpi", &vpi])
        .current_dir(&fixture)
        .env("LIBPYTHON_LOC", libpython)
        .env("PYGPI_PYTHON_BIN", &python)
        .env("PYTHONPATH", python_path)
        .env("COCOTB_TOPLEVEL", "Top")
        .env("COCOTB_TEST_MODULES", "test_counter")
        .env("TOPLEVEL_LANG", "verilog")
        .env("COCOTB_RESULTS_FILE", &results)
        .output()
        .unwrap();
    assert!(
        runtime.status.success(),
        "cocotb runtime failed:\n{}\n{}",
        String::from_utf8_lossy(&runtime.stdout),
        String::from_utf8_lossy(&runtime.stderr)
    );

    let checked = Command::new(&python)
        .args(["-m", "cocotb_tools.check_results"])
        .arg(&results)
        .output()
        .unwrap();
    assert!(
        checked.status.success(),
        "cocotb reported a failure:\n{}\n{}",
        String::from_utf8_lossy(&checked.stdout),
        String::from_utf8_lossy(&checked.stderr)
    );
}

#[test]
fn compile_uses_the_source_projects_clock_and_reset_metadata() {
    let temporary = tempfile::tempdir().unwrap();
    let source_dir = temporary.path().join("src");
    fs::create_dir(&source_dir).unwrap();
    fs::write(
        temporary.path().join("Veryl.toml"),
        r#"
[project]
name = "vpi_metadata"
version = "0.1.0"

[build]
clock_type = "negedge"
reset_type = "async_high"
sources = ["src"]
"#,
    )
    .unwrap();
    let source = source_dir.join("top.veryl");
    fs::write(
        &source,
        r#"
module Top (
    clk: input clock,
    rst: input reset,
    q: output logic,
) {
    always_ff (clk, rst) {
        if_reset {
            q = 0;
        } else {
            q = 1;
        }
    }
}
"#,
    )
    .unwrap();
    let executable = temporary.path().join("metadata-sim");
    let compile = Command::new(env!("CARGO_BIN_EXE_celox-vpi-compile"))
        .arg(&source)
        .args(["--top", "Top", "--runtime"])
        .arg(env!("CARGO_BIN_EXE_celox-vpi-runtime"))
        .arg("--output")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "native compilation failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let bytes = fs::read(executable).unwrap();
    let runtime = NativeProgramInstance::from_attached_bytes(&bytes).unwrap();
    assert_eq!(
        runtime.signal("Top.clk").unwrap().domain_kind,
        DomainKind::ClockNegedge
    );
    assert_eq!(
        runtime.signal("Top.rst").unwrap().domain_kind,
        DomainKind::ResetAsyncHigh
    );
}
