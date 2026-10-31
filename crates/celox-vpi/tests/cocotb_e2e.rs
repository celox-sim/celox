#![allow(clippy::disallowed_methods)] // Opt-in test configuration is environment-driven.

use std::{env, fs, path::Path, process::Command};

use celox::{DomainKind, NativeProgramInstance, SimBackend};

fn trusted_attached_instance(bytes: &[u8]) -> NativeProgramInstance {
    // Safety: every caller passes a runtime compiled in-process by the test.
    unsafe { NativeProgramInstance::from_attached_bytes(bytes) }.unwrap()
}

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
fn celox_cli_builds_a_self_running_native_image() {
    for source in [
        &["counter.veryl"][..],
        &["counter.sv"],
        &["split/types.sv", "split/child.sv", "split/top.sv"],
    ] {
        check_cli_native_image(source);
    }
}

fn check_cli_native_image(source: &[&str]) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cocotb");
    let temporary = tempfile::tempdir().unwrap();
    let executable = temporary.path().join("counter-sim");

    let compile = Command::new(env!("CARGO_BIN_EXE_celox"))
        .arg("vpi")
        .arg("build")
        .args(source.iter().map(|path| fixture.join(path)))
        .args(["--top", "Top", "--output"])
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "native compilation failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    assert!(executable.is_file());
    assert!(
        celox::NativeProgramImage::discover_appended(&fs::read(&executable).unwrap())
            .unwrap()
            .is_some()
    );

    let bytes = fs::read(&executable).unwrap();
    let runtime = trusted_attached_instance(&bytes);
    assert_eq!(
        runtime.signal("Top.i_clk").unwrap().domain_kind,
        DomainKind::ClockPosedge,
        "{source:?}"
    );
    assert_eq!(
        runtime.signal("Top.i_rst").unwrap().domain_kind,
        DomainKind::ResetAsyncLow,
        "{source:?}"
    );

    let help = Command::new(&executable).arg("--help").output().unwrap();
    assert!(
        help.status.success(),
        "attached runtime help failed: {}",
        String::from_utf8_lossy(&help.stderr)
    );
    assert!(
        String::from_utf8_lossy(&help.stdout).contains("--test-module"),
        "attached runtime did not expose cocotb options"
    );
}

#[test]
#[ignore = "requires a cocotb installation; CI runs this test explicitly"]
fn cocotb_drives_an_attached_native_flip_flop() {
    for source in [
        &["counter.veryl"][..],
        &["counter.sv"],
        &["split/types.sv", "split/child.sv", "split/top.sv"],
    ] {
        check_cocotb_native_flip_flop(source);
    }
}

fn check_cocotb_native_flip_flop(source: &[&str]) {
    check_cocotb_native_design(source, "test_counter", "drives_native_flip_flop");
}

#[test]
#[ignore = "requires a cocotb installation; CI runs this test explicitly"]
fn cocotb_observes_edges_before_nonblocking_updates() {
    check_cocotb_native_design(&["edge_phase.sv"], "test_edge_phase", "edge_precedes_nba");
}

#[test]
#[ignore = "requires a cocotb installation; CI runs this test explicitly"]
fn cocotb_inertial_writes_and_native_clocks_preserve_edge_sampling() {
    for initial in ["WITH_INITIAL=0", "WITH_INITIAL=1"] {
        check_cocotb_native_design_with_options(
            &["inertial.sv"],
            "test_inertial",
            ".",
            &["-G", initial],
        );
        check_cocotb_native_design_with_environment(
            &["inertial.sv"],
            "test_inertial",
            ".",
            &["-G", initial],
            &[("COCOTB_TRUST_INERTIAL_WRITES", "0")],
        );
    }
}

#[test]
#[ignore = "requires a cocotb installation; CI runs this test explicitly"]
fn cocotb_runs_dut_processes_in_step() {
    let output = check_cocotb_native_design(&["processes.sv"], "test_processes", ".");
    // `%0t` formats in the time precision, 1 ps.
    assert!(output.contains("at 12000 count=1\n"), "{output}");
}

#[test]
#[ignore = "requires a cocotb installation; CI runs this test explicitly"]
fn cocotb_forces_and_releases_a_register_of_a_timed_design() {
    check_cocotb_native_design_with_options(
        &["force_timed.sv"],
        "test_force_timed",
        ".",
        &["--force-signal", "Top.x"],
    );
}

#[test]
#[ignore = "requires a cocotb installation; CI runs this test explicitly"]
fn cocotb_ends_at_the_designs_finish() {
    let output = check_cocotb_native_design(&["finish.sv"], "test_finish", ".");
    assert!(output.contains("finishing at 30\n"), "{output}");
    assert!(!output.contains("late"), "{output}");
}

/// The `TRACE` lines a cocotb run of `test_processes` logged.
fn traces(output: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(output)
        .lines()
        .filter_map(|line| {
            line.split_once("TRACE ")
                .map(|(_, trace)| trace.to_string())
        })
        .collect()
}

/// The DUT processes of `processes.sv` see what they see on Icarus.
#[test]
#[ignore = "requires cocotb and Icarus Verilog; run locally"]
fn icarus_and_celox_agree_on_dut_processes() {
    let python = env::var_os("CELOX_COCOTB_PYTHON")
        .expect("CELOX_COCOTB_PYTHON must name the cocotb Python interpreter");
    let python = python.to_string_lossy().into_owned();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cocotb");
    let temporary = tempfile::tempdir().unwrap();
    let executable = temporary.path().join("processes-sim");
    let compile = Command::new(env!("CARGO_BIN_EXE_celox"))
        .args(["vpi", "build"])
        .arg(fixture.join("processes.sv"))
        .args(["--top", "Top", "--output"])
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let site_packages = python_output(
        &python,
        &["-c", "import site; print(':'.join(site.getsitepackages()))"],
    );
    let celox = Command::new(&executable)
        .args(["--test-module", "test_processes", "--python", &python])
        .arg("--results-file")
        .arg(temporary.path().join("results.xml"))
        .current_dir(&fixture)
        .env(
            "PYTHONPATH",
            format!("{}:{site_packages}", fixture.display()),
        )
        .output()
        .unwrap();
    assert!(
        celox.status.success(),
        "{}",
        String::from_utf8_lossy(&celox.stderr)
    );
    let icarus = Command::new(&python)
        .arg(fixture.join("run_icarus.py"))
        .arg(fixture.join("processes.sv"))
        .arg("test_processes")
        .arg(temporary.path().join("icarus"))
        .env("PYTHONPATH", &fixture)
        .output()
        .unwrap();
    assert!(
        icarus.status.success(),
        "{}",
        String::from_utf8_lossy(&icarus.stderr)
    );
    let expected = traces(&icarus.stdout);
    assert!(
        !expected.is_empty(),
        "{}",
        String::from_utf8_lossy(&icarus.stdout)
    );
    assert_eq!(traces(&celox.stdout), expected);
}

#[test]
#[ignore = "requires a cocotb installation; CI runs this test explicitly"]
fn cocotb_accepts_femtosecond_clock_periods() {
    check_cocotb_native_design_with_options(
        &["edge_phase.sv"],
        "test_fs_clock",
        "femtosecond_clock",
        &["--timescale", "1ns/1fs"],
    );
}

fn check_cocotb_native_design(source: &[&str], module: &str, filter: &str) -> String {
    check_cocotb_native_design_with_options(source, module, filter, &[])
}

/// Build `source`, run `module`'s tests matching `filter` on it, and check
/// that bad selections fail. Returns the standard output of the run.
fn check_cocotb_native_design_with_options(
    source: &[&str],
    module: &str,
    filter: &str,
    options: &[&str],
) -> String {
    check_cocotb_native_design_with_environment(source, module, filter, options, &[])
}

fn check_cocotb_native_design_with_environment(
    source: &[&str],
    module: &str,
    filter: &str,
    options: &[&str],
    environment: &[(&str, &str)],
) -> String {
    let python = env::var_os("CELOX_COCOTB_PYTHON")
        .expect("CELOX_COCOTB_PYTHON must name the cocotb Python interpreter");
    let python = python.to_string_lossy().into_owned();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cocotb");
    let temporary = tempfile::tempdir().unwrap();
    let executable = temporary.path().join("counter-sim");
    let results = temporary.path().join("results.xml");

    let compile = Command::new(env!("CARGO_BIN_EXE_celox"))
        .arg("vpi")
        .arg("build")
        .args(source.iter().map(|path| fixture.join(path)))
        .args(options)
        .args(["--top", "Top", "--output"])
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "native compilation failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let site_packages = python_output(
        &python,
        &["-c", "import site; print(':'.join(site.getsitepackages()))"],
    );
    let python_path = format!("{}:{site_packages}", fixture.display());
    let runtime = Command::new(&executable)
        .args([
            "--test-module",
            module,
            "--test-filter",
            filter,
            "--python",
            &python,
        ])
        .arg("--results-file")
        .arg(&results)
        .current_dir(&fixture)
        .env_remove("COCOTB_TRUST_INERTIAL_WRITES")
        .envs(environment.iter().copied())
        .env("COCOTB_TESTCASE", "inherited_selector_must_be_cleared")
        .env("PYTHONPATH", &python_path)
        .output()
        .unwrap();
    assert!(
        runtime.status.success(),
        "cocotb runtime failed for {source:?} {options:?} {environment:?}:\n{}\n{}",
        String::from_utf8_lossy(&runtime.stdout),
        String::from_utf8_lossy(&runtime.stderr)
    );

    assert!(results.is_file());

    let unmatched = Command::new(&executable)
        .args([
            "--test-module",
            module,
            "--test-filter",
            "^no_such_cocotb_test$",
            "--python",
            &python,
        ])
        .arg("--results-file")
        .arg(&results)
        .current_dir(&fixture)
        .env("PYTHONPATH", &python_path)
        .output()
        .unwrap();
    assert!(
        !unmatched.status.success(),
        "an empty test selection was reported as successful"
    );
    assert!(
        String::from_utf8_lossy(&unmatched.stderr).contains("no cocotb tests were executed"),
        "{}",
        String::from_utf8_lossy(&unmatched.stderr)
    );

    let failed_runtime = Command::new(&executable)
        .args(["--test-module", "missing_test_module", "--python", &python])
        .arg("--results-file")
        .arg(&results)
        .current_dir(&fixture)
        .output()
        .unwrap();
    assert!(
        !failed_runtime.status.success(),
        "cocotb startup failure reused stale passing results:\n{}\n{}",
        String::from_utf8_lossy(&failed_runtime.stdout),
        String::from_utf8_lossy(&failed_runtime.stderr)
    );
    assert!(
        !results.exists(),
        "failed cocotb startup left stale results in place"
    );
    String::from_utf8_lossy(&runtime.stdout).into_owned()
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
    let runtime = trusted_attached_instance(&bytes);
    assert_eq!(
        runtime.signal("Top.clk").unwrap().domain_kind,
        DomainKind::ClockNegedge
    );
    assert_eq!(
        runtime.signal("Top.rst").unwrap().domain_kind,
        DomainKind::ResetAsyncHigh
    );
}

#[test]
fn compile_resolves_standalone_readmem_relative_to_the_source() {
    let temporary = tempfile::tempdir().unwrap();
    let source_dir = temporary.path().join("standalone");
    fs::create_dir(&source_dir).unwrap();
    fs::write(source_dir.join("mem.hex"), "a5\n5a\n").unwrap();
    let source = source_dir.join("top.veryl");
    fs::write(
        &source,
        r#"
module Top (
    y: output logic<8>,
) {
    #[allow(initial_assign)]
    var mem: logic<8> [2];
    initial {
        $readmemh("mem.hex", mem);
    }
    assign y = mem[0];
}
"#,
    )
    .unwrap();

    let executable = temporary.path().join("standalone-sim");
    let compile = Command::new(env!("CARGO_BIN_EXE_celox-vpi-compile"))
        .arg(&source)
        .args(["--top", "Top", "--runtime"])
        .arg(env!("CARGO_BIN_EXE_celox-vpi-runtime"))
        .arg("--output")
        .arg(&executable)
        .current_dir(temporary.path())
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "native compilation failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let bytes = fs::read(executable).unwrap();
    let mut runtime = trusted_attached_instance(&bytes);
    runtime.eval_comb().unwrap();
    let output = runtime.signal_ref("Top.y").unwrap();
    assert_eq!(runtime.backend().get_as::<u8>(output), 0xa5);
}

#[test]
fn compile_includes_project_dependency_sources() {
    let temporary = tempfile::tempdir().unwrap();
    let dependency = temporary.path().join("dependency");
    let dependency_src = dependency.join("src");
    let root = temporary.path().join("root");
    let root_src = root.join("src");
    fs::create_dir_all(&dependency_src).unwrap();
    fs::create_dir_all(&root_src).unwrap();
    fs::write(
        dependency.join("Veryl.toml"),
        r#"
[project]
name = "dependency"
version = "0.1.0"

[build]
sources = ["src"]
"#,
    )
    .unwrap();
    fs::write(
        dependency_src.join("dep.veryl"),
        r#"
pub module Dep (
    a: input logic<8>,
    y: output logic<8>,
) {
    assign y = a + 1;
}
"#,
    )
    .unwrap();
    fs::write(
        root.join("Veryl.toml"),
        r#"
[project]
name = "root"
version = "0.1.0"

[build]
sources = ["src"]

[dependencies]
dep = { path = "../dependency" }
"#,
    )
    .unwrap();
    let source = root_src.join("top.veryl");
    fs::write(
        &source,
        r#"
module Top (
    a: input logic<8>,
    y: output logic<8>,
) {
    inst u_dep: dep::Dep (a, y);
}
"#,
    )
    .unwrap();

    let executable = temporary.path().join("dependency-sim");
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
    let mut runtime = trusted_attached_instance(&bytes);
    let input = runtime.signal_ref("Top.a").unwrap();
    let output = runtime.signal_ref("Top.y").unwrap();
    runtime.backend_mut().set(input, 41u8);
    runtime.eval_comb().unwrap();
    assert_eq!(runtime.backend().get_as::<u8>(output), 42);
}

#[test]
fn compile_includes_the_explicit_project_example_source() {
    let temporary = tempfile::tempdir().unwrap();
    let source_dir = temporary.path().join("src");
    let example_dir = temporary.path().join("examples");
    fs::create_dir(&source_dir).unwrap();
    fs::create_dir(&example_dir).unwrap();
    fs::write(
        temporary.path().join("Veryl.toml"),
        r#"
[project]
name = "explicit_example"
version = "0.1.0"

[build]
sources = ["src"]
"#,
    )
    .unwrap();
    fs::write(source_dir.join("unrelated.veryl"), "module Unrelated {}\n").unwrap();
    let source = example_dir.join("top.veryl");
    fs::write(
        &source,
        r#"
module Top (
    y: output logic<8>,
) {
    assign y = 42;
}
"#,
    )
    .unwrap();

    let executable = temporary.path().join("example-sim");
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
    let runtime = trusted_attached_instance(&bytes);
    let output = runtime.signal_ref("Top.y").unwrap();
    assert_eq!(runtime.backend().get_as::<u8>(output), 42);
}

#[test]
fn legacy_compiler_accepts_split_systemverilog_sources() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cocotb/split");
    let temporary = tempfile::tempdir().unwrap();
    let executable = temporary.path().join("split-sim");
    let compile = Command::new(env!("CARGO_BIN_EXE_celox-vpi-compile"))
        .args(["types.sv", "child.sv", "top.sv"].map(|file| fixture.join(file)))
        .args(["--top", "Top", "--runtime"])
        .arg(env!("CARGO_BIN_EXE_celox-vpi-runtime"))
        .arg("--output")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let bytes = fs::read(executable).unwrap();
    let runtime = trusted_attached_instance(&bytes);
    assert_eq!(runtime.signal("Top.child.q").unwrap().signal.width, 8);
    assert_eq!(
        runtime.signal("Top.i_clk").unwrap().domain_kind,
        DomainKind::ClockPosedge
    );
}

#[test]
fn compile_rejects_empty_or_mixed_source_lists() {
    let error = celox_vpi::driver::compile_native_sources(&[], "Top")
        .err()
        .unwrap();
    assert!(error.contains("missing source path"), "{error}");
    for paths in [
        [Path::new("top.sv"), Path::new("child.veryl")],
        [Path::new("top.veryl"), Path::new("child.sv")],
        [Path::new("top.veryl"), Path::new("child.veryl")],
    ] {
        let error = celox_vpi::driver::compile_native_sources(&paths, "Top")
            .err()
            .unwrap();
        assert!(
            error.contains("multiple source paths require only .sv files"),
            "{error}"
        );
    }
}

#[test]
fn compile_reports_a_missing_additional_source() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cocotb/counter.sv");
    let temporary = tempfile::tempdir().unwrap();
    let missing = temporary.path().join("missing.sv");
    let error = celox_vpi::driver::compile_native_sources(&[&fixture, &missing], "Top")
        .err()
        .unwrap();
    assert!(
        error.contains(&format!("failed to read {}:", missing.display())),
        "{error}"
    );
}

#[cfg(feature = "systemverilog")]
#[test]
#[ignore = "requires a cocotb installation"]
fn cocotb_uses_declared_force_targets_in_an_optimized_native_image() {
    let python = env::var("CELOX_COCOTB_PYTHON").expect("set CELOX_COCOTB_PYTHON");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cocotb");
    let temporary = tempfile::tempdir().unwrap();
    let executable = temporary.path().join("force-sim");
    let results = temporary.path().join("results.xml");
    let compile = Command::new(env!("CARGO_BIN_EXE_celox"))
        .args(["vpi", "build"])
        .arg(fixture.join("force_profile.sv"))
        .args(["--top", "Top", "-O", "O2", "--force-signal", "Top.x", "-o"])
        .arg(&executable)
        .output()
        .unwrap();
    assert!(compile.status.success(), "{compile:?}");
    let site = python_output(
        &python,
        &["-c", "import site; print(':'.join(site.getsitepackages()))"],
    );
    let runtime = Command::new(&executable)
        .args([
            "--test-module",
            "test_force_profile",
            "--python",
            &python,
            "--results-file",
        ])
        .arg(&results)
        .current_dir(&fixture)
        .env("PYTHONPATH", format!("{}:{site}", fixture.display()))
        .output()
        .unwrap();
    assert!(runtime.status.success(), "cocotb force failed: {runtime:?}");
    let xml = fs::read_to_string(results).unwrap();
    assert_eq!(xml.matches("<testcase ").count(), 1, "{xml}");
    assert!(!xml.contains("<failure"), "{xml}");
    assert!(!xml.contains("<error"), "{xml}");
}

#[test]
#[ignore = "requires a cocotb installation; CI runs this test explicitly"]
fn cocotb_indexes_unpacked_arrays_and_observes_element_changes() {
    check_cocotb_native_design(&["arrays.sv"], "test_arrays", "indexes_unpacked_arrays");
}
/// Configuration discovery must support both cocotb layouts, use the CLI
/// fallback when loaded-library lookup is unavailable, and honor request flags.
#[test]
#[ignore = "requires a Python interpreter; CI runs cocotb tests explicitly"]
fn cocotb_configuration_discovery_preserves_fallbacks_and_overrides() {
    let python = env::var_os("CELOX_COCOTB_PYTHON")
        .expect("CELOX_COCOTB_PYTHON must name the cocotb Python interpreter");
    let temporary = tempfile::tempdir().unwrap();
    let package = temporary.path().join("cocotb_tools");
    fs::create_dir(&package).unwrap();
    fs::write(package.join("__init__.py"), "").unwrap();
    // Simulate a platform without dladdr so the public cocotb CLI fallback
    // is exercised even on hosts that support direct loaded-library lookup.
    fs::write(temporary.path().join("ctypes.py"), "").unwrap();
    let script = format!(
        "import sys; sys.path.insert(0, {}); exec({})",
        serde_json::to_string(&temporary.path().to_string_lossy()).unwrap(),
        serde_json::to_string(include_str!("../src/cocotb_config.py")).unwrap()
    );
    let run = |flags: [&str; 3]| {
        let output = Command::new(&python)
            .args(["-c", &script])
            .args(flags)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    let fallback = r#"
import sys
from pathlib import Path

def lib_name_path(interface, simulator):
    assert (interface, simulator) == ("vpi", "icarus")
    return Path("adapter.vpl")

def main():
    assert sys.argv[1:] == ["--libpython"]
    print("libpython.so")
"#;
    fs::write(package.join("config.py"), fallback).unwrap();
    let old = run(["1", "1", "1"]);
    assert_eq!(old["vpi"], "adapter.vpl");
    assert_eq!(old["libpython"], "libpython.so");
    assert!(old["pygpi_entry_point"].is_null());
    fs::write(
        package.join("config.py"),
        format!("{fallback}\ndef pygpi_entry_point():\n    return 'simulator.so,initialize'\n"),
    )
    .unwrap();
    let new = run(["1", "1", "1"]);
    assert_eq!(new["pygpi_entry_point"], "simulator.so,initialize");
    let partial = run(["0", "1", "0"]);
    assert!(partial["vpi"].is_null());
    assert_eq!(partial["libpython"], "libpython.so");
    assert!(partial["pygpi_entry_point"].is_null());
    let partial = run(["1", "0", "1"]);
    assert_eq!(partial["vpi"], "adapter.vpl");
    assert!(partial["libpython"].is_null());
    assert_eq!(partial["pygpi_entry_point"], "simulator.so,initialize");
    // An entirely explicit configuration must not import cocotb at all.
    fs::write(
        package.join("config.py"),
        "raise RuntimeError('unexpected configuration import')\n",
    )
    .unwrap();
    let explicit = run(["0", "0", "0"]);
    assert!(
        explicit["python"]
            .as_str()
            .is_some_and(|path| !path.is_empty())
    );
    assert!(explicit["vpi"].is_null());
    assert!(explicit["libpython"].is_null());
    assert!(explicit["pygpi_entry_point"].is_null());
}
