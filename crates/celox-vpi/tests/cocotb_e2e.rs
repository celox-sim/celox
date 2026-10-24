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
fn cocotb_runs_dut_processes_in_step() {
    check_cocotb_native_design(&["processes.sv"], "test_processes", ".");
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

fn check_cocotb_native_design(source: &[&str], module: &str, filter: &str) {
    check_cocotb_native_design_with_options(source, module, filter, &[]);
}

fn check_cocotb_native_design_with_options(
    source: &[&str],
    module: &str,
    filter: &str,
    options: &[&str],
) {
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
        .env("COCOTB_TESTCASE", "inherited_selector_must_be_cleared")
        .env("PYTHONPATH", &python_path)
        .output()
        .unwrap();
    assert!(
        runtime.status.success(),
        "cocotb runtime failed for {source:?}:\n{}\n{}",
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
