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
fn explicit_veryl_sources_accept_existing_project_settings() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("top.veryl"),
        "#[test(Top)] module Top { initial { $finish(); } }",
    )
    .unwrap();
    for settings in [
        "exclude = ['src/legacy/**']",
        "[test]\nsources = ['test_veryl']",
        "[simulation]\nmax_steps = 1000",
        "exclude = ['src/legacy/**']\n[test]\nsources = ['test_veryl']\n[simulation]\nmax_steps = 1000",
    ] {
        fs::write(root.path().join("celox.toml"), settings).unwrap();
        for command in [vec!["run"], vec!["vpi", "build"]] {
            let args = command
                .into_iter()
                .chain(["top.veryl", "--top", "Top", "--project", "celox.toml"])
                .collect::<Vec<_>>();
            success(&invoke(root.path(), &args));
        }
        assert!(root.path().join("celox.out").is_file());
    }
}

#[test]
fn project_errors_are_actionable() {
    let root = tempfile::tempdir().unwrap();
    let manifest = root.path().join("celox.toml");
    for (text, expected) in [
        ("sources = [", "invalid project"),
        ("soruces = []", "unknown field"),
        ("exclude = 1", "invalid project"),
        ("[test]\nsources = 1", "invalid project"),
        ("[simulation]\nmax_steps = -1", "invalid project"),
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

#[cfg(feature = "systemverilog")]
#[test]
fn native_build_applies_target_and_cli_signal_access_contracts() {
    use celox::{BigUint, NativeProgramInstance, SimBackend};
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("top.sv"), "module Top(input logic [7:0] a, output logic [7:0] y); logic [7:0] hidden; assign hidden=a+7; assign y=a+1; endmodule").unwrap();
    fs::write(
        root.path().join("celox.toml"),
        r#"
      sources = ["top.sv"]
      top = "Top"
      opt_level = "O0"
      live_signals = ["Top.bad_common"]
      force_signals = ["Top.bad_common"]
      [targets.selected]
      opt_level = "O2"
      live_signals = ["Top.hidden"]
      force_signals = ["Top.hidden"]
      [targets.empty]
      live_signals = []
      force_signals = []
    "#,
    )
    .unwrap();
    for arguments in [
        vec!["--target", "selected"],
        vec![
            "-O",
            "O2",
            "--live-signal",
            "Top.hidden",
            "--force-signal",
            "Top.hidden",
        ],
    ] {
        let output = invoke(
            root.path(),
            &[vec!["vpi", "build", "-o", "sim"], arguments].concat(),
        );
        success(&output);
        let bytes = fs::read(root.path().join("sim")).unwrap();
        let image = celox::NativeProgramImage::discover_appended(&bytes)
            .unwrap()
            .unwrap()
            .image;
        // Safety: this CLI invocation compiled the trusted test design.
        let mut sim = unsafe { NativeProgramInstance::from_image(image) }.unwrap();
        let lookup = |sim: &NativeProgramInstance, path: &str| {
            celox::ReflectionSignalId(
                sim.reflection()
                    .signals()
                    .iter()
                    .position(|signal| signal.full_name == path)
                    .unwrap() as u32,
            )
        };
        let a = lookup(&sim, "Top.a");
        let hidden = lookup(&sim, "Top.hidden");
        let signal = sim.reflection().signal(a).unwrap().signal;
        sim.backend_mut().set(signal, 11u8);
        sim.eval_comb().unwrap();
        let signal = sim.reflection().signal(hidden).unwrap().signal;
        assert_eq!(
            sim.backend().get_four_state(signal),
            (BigUint::from(18u8), BigUint::from(0u8))
        );
        assert!(!sim.force_signal(a, 1u8.into(), 0u8.into()));
        assert!(sim.force_signal(hidden, 40u8.into(), 0u8.into()));
    }
    success(&invoke(
        root.path(),
        &["vpi", "build", "--target", "empty", "-o", "empty"],
    ));
    success(&invoke(
        root.path(),
        &[
            "vpi",
            "build",
            "--no-live-signals",
            "--no-force-signals",
            "-o",
            "cleared",
        ],
    ));
    let output = invoke(
        root.path(),
        &[
            "vpi",
            "build",
            "--target",
            "selected",
            "--force-signal",
            "Top.typo",
        ],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Top.typo"));
}

#[cfg(feature = "systemverilog")]
#[test]
fn top_parameters_merge_common_target_and_cli_without_editing_sv() {
    let root = tempfile::tempdir().unwrap();
    let source = "module Top #(parameter W=8, parameter V=1, parameter NEXT=V+1) (output logic [W-1:0] y); assign y=NEXT; initial begin #1; $display(\"v=%0d w=%0d\", y, $bits(y)); $finish; end endmodule";
    fs::write(root.path().join("top.sv"), source).unwrap();
    fs::write(
        root.path().join("celox.toml"),
        r#"
      sources = ["top.sv"]
      top = "Top"
      [parameters]
      W = 8
      V = 2
      [targets.wide.parameters]
      W = 16
      V = "0x1233"
    "#,
    )
    .unwrap();
    let output = invoke(root.path(), &["run", "--target", "wide"]);
    success(&output);
    assert_eq!(output.stdout, b"v=4660 w=16\n");
    let output = invoke(
        root.path(),
        &[
            "run",
            "--target",
            "wide",
            "-GV=0b11",
            "--parameter",
            "V=0o10",
        ],
    );
    success(&output);
    assert_eq!(output.stdout, b"v=9 w=16\n");
    success(&invoke(
        root.path(),
        &[
            "vpi",
            "build",
            "--target",
            "wide",
            "--param",
            "V=18446744073709551615",
            "-o",
            "sim",
        ],
    ));
    let bytes = fs::read(root.path().join("sim")).unwrap();
    let image = celox::NativeProgramImage::discover_appended(&bytes)
        .unwrap()
        .unwrap()
        .image;
    // Safety: the CLI just compiled this test's source.
    let sim = unsafe { celox::NativeProgramInstance::from_image(image) }.unwrap();
    let signal = sim.signal_ref("Top.y").unwrap();
    assert_eq!(signal.width, 16);
    assert_eq!(celox::SimBackend::get_as::<u16>(sim.backend(), signal), 0);
    assert_eq!(
        fs::read_to_string(root.path().join("top.sv")).unwrap(),
        source
    );
    let output = invoke(root.path(), &["run", "--target", "wide", "-GMISSING=1"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unknown top-level parameter override")
    );
}

#[test]
fn top_parameters_reach_veryl_run_and_native_build() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("tb.veryl"),
        "#[test(Top)] module Top #(param V: u32 = 1) { initial { $assert(V == 7); $finish(); } }",
    )
    .unwrap();
    success(&invoke(
        root.path(),
        &["run", "tb.veryl", "--top", "Top", "-G", "V=7"],
    ));
    fs::write(
        root.path().join("dut.veryl"),
        "module Top #(param W: u32 = 8, param V: u32 = 1) (y: output logic<W>) { assign y=V; }",
    )
    .unwrap();
    success(&invoke(
        root.path(),
        &[
            "vpi",
            "build",
            "dut.veryl",
            "--top",
            "Top",
            "-GW=16",
            "-GV=0x1234",
            "-o",
            "sim",
        ],
    ));
    let bytes = fs::read(root.path().join("sim")).unwrap();
    let image = celox::NativeProgramImage::discover_appended(&bytes)
        .unwrap()
        .unwrap()
        .image;
    // Safety: the CLI just compiled this test's source.
    let sim = unsafe { celox::NativeProgramInstance::from_image(image) }.unwrap();
    let signal = sim.signal_ref("Top.y").unwrap();
    assert_eq!(signal.width, 16);
    assert_eq!(
        celox::SimBackend::get_as::<u16>(sim.backend(), signal),
        0x1234
    );
    let output = invoke(
        root.path(),
        &["vpi", "build", "dut.veryl", "--top", "Top", "-GWIDHT=16"],
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unknown top-level parameter override")
    );
}

#[test]
fn invalid_parameter_assignments_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    for value in [
        "V", "=1", "x.y=1", "V=", "V=--1", "V=1+2", "V=8'hG", "V=0x", "V=0b2", "V=_1",
    ] {
        let output = invoke(root.path(), &["run", "dut.sv", "--top", "Top", "-G", value]);
        rejected(&output, "parameter");
    }
}

#[cfg(feature = "systemverilog")]
#[test]
fn sv_parameter_literals_preserve_types_across_project_target_and_cli() {
    let root = tempfile::tempdir().unwrap();
    let source = r#"
        module Top #(parameter VENDOR="GENERIC", parameter NEG=3,
            parameter logic [64:0] WIDE=0, parameter logic [57:0] FILL=0,
            parameter logic [3:0] UNKNOWN=0)(output logic matched);
            assign matched=(VENDOR == "XILINX") && (NEG == -8) && (NEG < 0)
                && ($bits(NEG) == 32) && ($bits(VENDOR) == 48)
                && (WIDE == 65'h10000000000000001) && (FILL == 58'h3ffffffffffffff)
                && (UNKNOWN === 4'b10xz);
            initial begin #1; assert(matched) else $fatal(1,"literal parameter types");
                $display("ok"); $finish; end
        endmodule
    "#;
    fs::write(root.path().join("top.sv"), source).unwrap();
    fs::write(
        root.path().join("celox.toml"),
        r#"
        sources=["top.sv"]
        top="Top"
        [parameters]
        VENDOR="GENERIC"
        NEG=3
        FILL=0
        [targets.literals.parameters]
        VENDOR="XILINX"
        NEG=-8
        WIDE="65'h10000000000000001"
        FILL="'1"
        UNKNOWN="4'b10xz"
    "#,
    )
    .unwrap();
    let output = invoke(root.path(), &["run", "--target", "literals"]);
    success(&output);
    assert_eq!(output.stdout, b"ok\n");
    let args = [
        "--target",
        "literals",
        "-GVENDOR=0",
        "-GVENDOR=\"XILINX\"",
        "-GNEG=8",
        "-GNEG=-8",
        "-GFILL=0",
        "-GFILL='1",
    ];
    let output = invoke(root.path(), &[&["run"][..], &args].concat());
    success(&output);
    assert_eq!(output.stdout, b"ok\n");
    success(&invoke(
        root.path(),
        &[&["vpi", "build"][..], &args, &["-o", "sim"]].concat(),
    ));
    let bytes = fs::read(root.path().join("sim")).unwrap();
    let image = celox::NativeProgramImage::discover_appended(&bytes)
        .unwrap()
        .unwrap()
        .image;
    // Safety: the CLI compiled this test's source into the image above.
    let instance = unsafe { celox::NativeProgramInstance::from_image(image) }.unwrap();
    let matched = instance.signal_ref("Top.matched").unwrap();
    assert_eq!(
        celox::SimBackend::get_as::<u8>(instance.backend(), matched),
        1
    );
    assert_eq!(
        fs::read_to_string(root.path().join("top.sv")).unwrap(),
        source
    );
}

#[cfg(feature = "systemverilog")]
#[test]
fn state_mode_merges_project_target_and_cli_for_run_and_native_build() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("top.sv"),
        r#"
        module Top(output logic[3:0] y);
            logic[3:0] uninitialized;
            assign y=uninitialized;
            initial begin #1; $display("value=%b",y); $finish; end
        endmodule
    "#,
    )
    .unwrap();
    fs::write(
        root.path().join("celox.toml"),
        "sources=['top.sv']\ntop='Top'\nfour_state=true\n[targets.two]\nfour_state=false\n",
    )
    .unwrap();
    for (args, expected, is_four) in [
        (vec![], "value=xxxx\n", true),
        (vec!["--two-state"], "value=0000\n", false),
        (vec!["--target", "two"], "value=0000\n", false),
        (
            vec!["--target", "two", "--four-state"],
            "value=xxxx\n",
            true,
        ),
    ] {
        let run = invoke(root.path(), &[&["run"][..], &args].concat());
        success(&run);
        assert_eq!(String::from_utf8(run.stdout).unwrap(), expected);
        success(&invoke(
            root.path(),
            &[&["vpi", "build"][..], &args, &["-o", "sim"]].concat(),
        ));
        let bytes = fs::read(root.path().join("sim")).unwrap();
        let image = celox::NativeProgramImage::discover_appended(&bytes)
            .unwrap()
            .unwrap()
            .image;
        // Safety: the CLI compiled this test's source into this image.
        let instance = unsafe { celox::NativeProgramInstance::from_image(image) }.unwrap();
        assert_eq!(instance.four_state(), is_four);
        let (value, mask) = celox::SimBackend::get_four_state(
            instance.backend(),
            instance.signal_ref("Top.y").unwrap(),
        );
        assert_eq!(
            mask.to_u64_digits(),
            if is_four { vec![15] } else { vec![] }
        );
        if !is_four {
            assert_eq!(value.to_u64_digits(), Vec::<u64>::new());
        }
    }
}

#[test]
fn sv_literal_parameters_are_rejected_for_veryl_in_both_commands() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("top.veryl"),
        "module Top #(param W:u32=8) (y:output logic<W>) { assign y=0; }",
    )
    .unwrap();
    for prefix in [&["run"][..], &["vpi", "build"][..]] {
        let output = invoke(
            root.path(),
            &[prefix, &["top.veryl", "--top", "Top", "-GW=-8"]].concat(),
        );
        rejected(&output, "SV parameter literals require");
    }
}

#[test]
fn veryl_native_images_preserve_the_selected_state_mode() {
    for with_metadata in [false, true] {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("top.veryl"),
            "module Top (y:output logic<4>) { assign y=3; }",
        )
        .unwrap();
        if with_metadata {
            fs::write(
                root.path().join("Veryl.toml"),
                "[project]\nname='state_mode'\nversion='0.1.0'\n",
            )
            .unwrap();
        }
        for (flag, four_state) in [("--two-state", false), ("--four-state", true)] {
            let output = invoke(
                root.path(),
                &[
                    "vpi",
                    "build",
                    "top.veryl",
                    "--top",
                    "Top",
                    flag,
                    "-o",
                    "sim",
                ],
            );
            success(&output);
            let bytes = fs::read(root.path().join("sim")).unwrap();
            let image = celox::NativeProgramImage::discover_appended(&bytes)
                .unwrap()
                .unwrap()
                .image;
            // Safety: the CLI compiled this test's source into this image.
            let instance = unsafe { celox::NativeProgramInstance::from_image(image) }.unwrap();
            assert_eq!(instance.four_state(), four_state);
        }
    }
}

#[cfg(feature = "systemverilog")]
#[test]
fn sv_cli_defaults_to_two_state_for_run_and_native_images() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("top.sv"),
        r#"module Top(output logic[3:0] y);
      logic[3:0] uninitialized;
      assign y=uninitialized;
      initial begin #1; $display("value=%b", y); $finish; end
    endmodule"#,
    )
    .unwrap();
    let run = invoke(root.path(), &["run", "top.sv", "--top", "Top"]);
    success(&run);
    assert_eq!(run.stdout, b"value=0000\n");
    success(&invoke(
        root.path(),
        &["vpi", "build", "top.sv", "--top", "Top", "-o", "sim"],
    ));
    let bytes = fs::read(root.path().join("sim")).unwrap();
    let image = celox::NativeProgramImage::discover_appended(&bytes)
        .unwrap()
        .unwrap()
        .image;
    // Safety: this test compiled the source into the image above.
    let instance = unsafe { celox::NativeProgramInstance::from_image(image) }.unwrap();
    assert!(!instance.four_state());
}

#[cfg(feature = "systemverilog")]
#[test]
fn timescale_reaches_run_and_native_images_with_target_and_cli_precedence() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("top.sv"),
        r#"
`timescale 1ns/1ps
module Top(input bit clk, output bit q);
    always_ff @(posedge clk) q <= !q;
    initial begin
        $display("SCALE %0d %0d", $timeprecision(), $timeprecision($root));
        #1ns $finish;
    end
endmodule
"#,
    )
    .unwrap();
    fs::write(
        root.path().join("celox.toml"),
        r#"
sources = ["top.sv"]
top = "Top"
timescale = "1ns/10fs"
[targets.fine]
timescale = "1ns/1fs"
"#,
    )
    .unwrap();
    for (options, precision) in [
        (vec![], -14),
        (vec!["--target", "fine"], -15),
        (vec!["--target", "fine", "--timescale", "10ns/100fs"], -13),
        (vec!["--timescale", "1ns/10ps"], -12),
    ] {
        let output = invoke(root.path(), &[vec!["run"], options.clone()].concat());
        success(&output);
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(&format!("SCALE -12 {precision}")),
            "{output:?}"
        );
        success(&invoke(
            root.path(),
            &[vec!["vpi", "build", "-o", "sim"], options].concat(),
        ));
        let bytes = fs::read(root.path().join("sim")).unwrap();
        // Safety: the test compiled this trusted runtime itself.
        let instance =
            unsafe { celox::NativeProgramInstance::from_attached_bytes(&bytes) }.unwrap();
        assert_eq!(instance.time_precision().exponent(), precision);
    }
    rejected(
        &invoke(root.path(), &["run", "--timescale", "1ps/1ns"]),
        "invalid timescale",
    );
    fs::write(
        root.path().join("celox.toml"),
        "sources=['top.sv']\ntop='Top'\ntimescale='1ns/2fs'",
    )
    .unwrap();
    rejected(&invoke(root.path(), &["run"]), "invalid timescale");
}
