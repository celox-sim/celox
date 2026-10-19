//! Project configuration belongs to the CLI, rather than the language suite.

use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn invoke(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_celox"))
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap()
}

fn success(output: &Output) {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn rejected(output: &Output, message: &str) {
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(message),
        "{output:?}"
    );
}

#[test]
fn veryl_projects_select_targets_and_cli_overrides_run_limits() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("top.veryl"),
        "#[test(Top)] module Top { inst clk: $tb::clock_gen; initial { clk.next(3); $finish(); } }",
    )
    .unwrap();
    fs::write(
        root.path().join("celox.toml"),
        r#"
        sources = ["top.veryl"]
        default_target = "smoke"
        opt_level = "O0"
        [targets.smoke]
        top = "Top"
        opt_level = "O2"
        max_steps = 1
        [targets.other]
        top = "Missing"
    "#,
    )
    .unwrap();
    let nested = root.path().join("nested/work");
    fs::create_dir_all(&nested).unwrap();
    let output = invoke(&nested, &["run"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("testbench ticks before completion"));
    success(&invoke(&nested, &["run", "--max-steps", "10", "-O", "O1"]));
    success(&invoke(
        &nested,
        &[
            "run",
            "--target",
            "other",
            "--top",
            "Top",
            "--max-steps",
            "10",
        ],
    ));
    rejected(
        &invoke(&nested, &["run", "--target", "missing"]),
        "unknown target `missing`",
    );
}

#[test]
fn explicit_project_paths_do_not_depend_on_working_directory() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("top.veryl"),
        "#[test(Top)] module Top { initial { $finish(); } }",
    )
    .unwrap();
    fs::write(
        root.path().join("celox.toml"),
        "sources = ['top.veryl']\n[targets.only]\ntop = 'Top'\n",
    )
    .unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let manifest = root.path().join("celox.toml");
    success(&invoke(
        elsewhere.path(),
        &["run", "--project", manifest.to_str().unwrap()],
    ));
    fs::write(
        &manifest,
        "[targets.only]\nsources = ['top.veryl']\ntop = 'Missing'\n",
    )
    .unwrap();
    success(&invoke(
        elsewhere.path(),
        &[
            "run",
            "--project",
            manifest.to_str().unwrap(),
            "--top",
            "Top",
        ],
    ));
}

#[test]
fn project_errors_are_actionable() {
    let root = tempfile::tempdir().unwrap();
    let manifest = root.path().join("celox.toml");
    for (text, expected) in [
        ("sources = [", "invalid project"),
        ("soruces = []", "unknown field"),
        (
            "sources = ['top.sv']\ntop = 'Top'\nopt_level = 'O3'",
            "invalid opt_level",
        ),
        (
            "sources = ['top.sv']\ntop = 'Top'\nmax_steps = 0",
            "max_steps must be positive",
        ),
        (
            "sources = ['top.sv']\n[targets.a]\ntop = 'A'\n[targets.b]\ntop = 'B'",
            "select --target NAME",
        ),
        (
            "sources = ['top.sv']\ndefault_target = 'missing'",
            "unknown target `missing`",
        ),
        (
            "sources = ['top.sv']\ntop = 'Top'\n[targets.a]\nsource = []",
            "unknown field",
        ),
        ("sources = ['top.sv']", "provide --top MODULE"),
        ("top = 'Top'", "provide sources"),
        (
            "sources = ['top.veryl']\ntop = 'Top'\ninclude_dirs = ['include']",
            "require SystemVerilog sources",
        ),
        (
            "sources = ['top.sv']\ntop = 'Top'\n[defines]\n'not-valid' = '1'",
            "invalid macro definition",
        ),
    ] {
        fs::write(&manifest, text).unwrap();
        rejected(&invoke(root.path(), &["run"]), expected);
    }
}

#[test]
fn filelist_errors_do_not_silently_skip_inputs_or_options() {
    let root = tempfile::tempdir().unwrap();
    for (text, expected) in [
        ("-f files.f", "recursive file list"),
        ("-f missing.f", "failed to open file list"),
        ("-f", "-f requires a value"),
        ("+incdir+", "empty +incdir+ path"),
        ("+define+", "invalid macro definition"),
        ("+define+123bad", "invalid macro definition"),
        ("-unsupported option", "unsupported file-list option"),
        ("\"missing quote", "unterminated quoted path"),
        ("/* missing comment end", "unterminated file-list comment"),
    ] {
        fs::write(root.path().join("files.f"), text).unwrap();
        rejected(
            &invoke(root.path(), &["run", "-f", "files.f", "--top", "Top"]),
            expected,
        );
    }
}

#[cfg(feature = "systemverilog")]
fn sv_project(root: &Path) {
    for dir in ["rtl/space dir", "lists/nested", "include", "work"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    fs::write(
        root.join("rtl/prelude.sv"),
        "package prelude; parameter int BASE = 1; endpackage",
    )
    .unwrap();
    fs::write(
        root.join("rtl/space dir/types.sv"),
        "package values; parameter int BASE = prelude::BASE; endpackage",
    )
    .unwrap();
    fs::write(root.join("rtl/dut.sv"), "module Dut(output logic [7:0] q);\n`include \"value.svh\"\nassign q = values::BASE + VALUE; endmodule").unwrap();
    fs::write(
        root.join("include/value.svh"),
        "localparam int VALUE = `RESULT;\n",
    )
    .unwrap();
    fs::write(root.join("tb.sv"), "module Tb; wire [7:0] q; Dut dut(q); initial begin #1; $display(\"q=%0d\", q); $finish; end endmodule").unwrap();
    fs::write(
        root.join("lists/common.f"),
        "# comment\n\"../rtl/space dir/types.sv\"\n/* block comment */ -f nested/dut.f\n",
    )
    .unwrap();
    fs::write(
        root.join("lists/nested/dut.f"),
        "+incdir+../../include\n+define+RESULT=20\n../../rtl/dut.sv // line comment\n",
    )
    .unwrap();
    fs::write(
        root.join("celox.toml"),
        r#"
        sources = ["rtl/prelude.sv"]
        filelists = ["lists/common.f"]
        default_target = "smoke"
        [defines]
        RESULT = "10"
        [targets.smoke]
        sources = ["tb.sv"]
        top = "Tb"
        opt_level = "O2"
        max_steps = 20
        [targets.smoke.defines]
        RESULT = "41"
        [targets.dut]
        top = "Dut"
    "#,
    )
    .unwrap();
}

#[cfg(feature = "systemverilog")]
#[test]
fn sv_projects_resolve_ordered_lists_includes_defines_and_targets() {
    let root = tempfile::tempdir().unwrap();
    sv_project(root.path());
    let output = invoke(&root.path().join("work"), &["run"]);
    success(&output);
    assert_eq!(output.stdout, b"q=42\n");
    let output = invoke(
        root.path(),
        &["run", "--target", "smoke", "-D", "RESULT=99"],
    );
    success(&output);
    assert_eq!(output.stdout, b"q=100\n");

    let output = invoke(
        root.path(),
        &["vpi", "build", "--target", "dut", "-o", "dut-sim"],
    );
    success(&output);
    let bytes = fs::read(root.path().join("dut-sim")).unwrap();
    let attached = celox::NativeProgramImage::discover_appended(&bytes)
        .unwrap()
        .unwrap();
    assert!(
        attached
            .image
            .reflection()
            .scopes()
            .iter()
            .any(|scope| scope.full_name == "Dut")
    );
}

#[cfg(feature = "systemverilog")]
#[test]
fn standalone_filelists_and_cli_preprocessing_options_work_for_both_commands() {
    let root = tempfile::tempdir().unwrap();
    sv_project(root.path());
    fs::write(
        root.path().join("all.f"),
        "rtl/prelude.sv\n-f lists/common.f\n-Iinclude\n-DRESULT=41\n+define+UNUSED+OTHER=2\ntb.sv\n",
    )
    .unwrap();
    let output = invoke(
        root.path(),
        &["run", "-f", "all.f", "--top", "Tb", "-DRESULT=41"],
    );
    success(&output);
    assert_eq!(output.stdout, b"q=42\n");
    let output = invoke(
        root.path(),
        &[
            "vpi", "build", "-f", "all.f", "--top", "Dut", "-o", "raw-sim",
        ],
    );
    success(&output);

    let output = invoke(
        root.path(),
        &[
            "run",
            "rtl/prelude.sv",
            "rtl/space dir/types.sv",
            "rtl/dut.sv",
            "tb.sv",
            "--top",
            "Tb",
            "-I",
            "include",
            "-D",
            "RESULT=41",
        ],
    );
    success(&output);
    assert_eq!(output.stdout, b"q=42\n");
}

#[cfg(not(feature = "systemverilog"))]
#[test]
fn sv_project_without_feature_reports_the_required_build_option() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("celox.toml"),
        "sources = ['top.sv']\ntop = 'Top'\n",
    )
    .unwrap();
    for args in [&["run"][..], &["vpi", "build"]] {
        let output = invoke(root.path(), args);
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("--features systemverilog"));
    }
}
