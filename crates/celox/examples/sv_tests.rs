//! Single-case compiler driver for the official sv-tests runner.
//!
//! Metadata, timeouts, expected failures and output assertions belong to
//! sv-tests. This process reports only compiler/simulator success or failure.
//!
//! ```text
//! sv_tests --mode preprocessing|parsing|elaboration|simulation --state-mode 2state|4state \
//!     [--top NAME] [--define NAME[=VALUE]] [--include DIR] [--simvariant ARG] -- FILE...
//! ```

#![allow(clippy::disallowed_methods)] // This binary is the process environment boundary.

use std::{collections::BTreeSet, env, path::PathBuf, process::ExitCode};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Preprocessing,
    Parsing,
    Elaboration,
    Simulation,
}

#[derive(Debug)]
struct Options {
    mode: Mode,
    four_state: bool,
    top: Option<String>,
    defines: Vec<(String, Option<String>)>,
    includes: Vec<PathBuf>,
    simvariants: Vec<String>,
    files: Vec<PathBuf>,
}

fn parse_options(args: &[String]) -> Result<Options, String> {
    let mut mode = None;
    let mut four_state = None;
    let mut top = None;
    let mut defines = Vec::new();
    let mut includes = Vec::new();
    let mut simvariants = Vec::new();
    let mut files = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            files.extend(args.map(PathBuf::from));
            break;
        }
        if !arg.starts_with('-') {
            files.push(arg.into());
            continue;
        }
        let value = args
            .next()
            .ok_or_else(|| format!("missing value for {arg}"))?;
        match arg.as_str() {
            "--mode" if mode.is_none() => {
                mode = Some(match value.as_str() {
                    "preprocessing" => Mode::Preprocessing,
                    "parsing" => Mode::Parsing,
                    "elaboration" => Mode::Elaboration,
                    "simulation" => Mode::Simulation,
                    _ => return Err(format!("unsupported mode: {value}")),
                })
            }
            "--state-mode" if four_state.is_none() => {
                four_state = Some(match value.as_str() {
                    "2state" => false,
                    "4state" => true,
                    _ => return Err(format!("unsupported state mode: {value}")),
                })
            }
            "--top" if top.is_none() && !value.is_empty() => top = Some(value.clone()),
            "--define" => {
                let (name, replacement) = value
                    .split_once('=')
                    .map_or((value.as_str(), None), |(name, value)| {
                        (name, Some(value.to_string()))
                    });
                if name.is_empty()
                    || !name.bytes().enumerate().all(|(index, byte)| {
                        byte.is_ascii_alphabetic()
                            || byte == b'_'
                            || index > 0 && (byte.is_ascii_digit() || byte == b'$')
                    })
                {
                    return Err(format!("invalid macro name: {name}"));
                }
                defines.push((name.to_string(), replacement));
            }
            "--include" => includes.push(value.into()),
            "--simvariant" => simvariants.push(value.clone()),
            _ => return Err(format!("unknown, duplicate or invalid option: {arg}")),
        }
    }
    if files.is_empty() {
        return Err("no input files".to_string());
    }
    Ok(Options {
        mode: mode.ok_or("--mode is required")?,
        four_state: four_state.ok_or("--state-mode is required")?,
        top,
        defines,
        includes,
        simvariants,
        files,
    })
}

/// Select roots from parsed instantiations, including inactive generate
/// branches (IEEE 1800-2023 23.3.1). Celox's simulator accepts one root.
fn select_top(
    parsed: &[celox_sv_analyzer::ParsedSource],
    explicit: Option<&str>,
) -> Result<String, String> {
    if let Some(top) = explicit {
        return Ok(top.to_string());
    }
    let names: BTreeSet<_> = parsed
        .iter()
        .flat_map(|source| source.module_names())
        .cloned()
        .collect();
    let referenced: BTreeSet<_> = parsed
        .iter()
        .flat_map(|source| source.component_references())
        .collect();
    let roots: Vec<_> = names.difference(&referenced).cloned().collect();
    match roots.as_slice() {
        [root] => Ok(root.clone()),
        [] => {
            Err("elaboration requires a module root; none found in the parsed sources".to_string())
        }
        _ => Err(format!(
            "multiple module roots ({}) are unsupported by Celox's single-root simulator; select one with --top",
            roots.join(", ")
        )),
    }
}

fn run(options: &Options) -> Result<(), String> {
    for include in &options.includes {
        if !include.is_dir() {
            return Err(format!(
                "include directory is unavailable: {}",
                include.display()
            ));
        }
    }
    let includes: Vec<_> = options.includes.iter().map(PathBuf::as_path).collect();
    let texts: Vec<_> = options
        .files
        .iter()
        .map(|path| {
            let text = std::fs::read_to_string(path)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            celox_sv_analyzer::syntax::preprocess_source(&text, path, &options.defines, &includes)
                .map_err(|error| error.to_string())
        })
        .collect::<Result<_, _>>()?;
    if options.mode == Mode::Preprocessing {
        for text in &texts {
            print!("{text}");
        }
        return Ok(());
    }
    let sources: Vec<_> = texts
        .iter()
        .zip(&options.files)
        .map(|(text, path)| (text.as_str(), path.as_path()))
        .collect();
    // ParsedSource also builds semantic module metadata. Parsing-only cases
    // must stop at the syntax tree, before that analysis or root selection.
    if options.mode == Mode::Parsing {
        for (text, path) in &sources {
            celox_sv_analyzer::syntax::parse_source(text, path)
                .map_err(|error| error.to_string())?;
        }
        return Ok(());
    }
    let parsed = sources
        .iter()
        .map(|(text, path)| celox_sv_analyzer::ParsedSource::parse(text, path))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let top = select_top(&parsed, options.top.as_deref())?;
    match options.mode {
        Mode::Preprocessing | Mode::Parsing => {
            unreachable!("preprocessing and parsing returned before root selection")
        }
        // A Simulator rejects designs with processes, which it does not run.
        Mode::Elaboration => celox::Simulation::from_sv_sources(sources, &top)
            .four_state(options.four_state)
            .build()
            .map(|_| ())
            .map_err(|error| error.to_string()),
        Mode::Simulation => {
            // Upstream simvariants are separate runs, each with one plusarg.
            // Recreate the simulation so no variable or scheduler state leaks.
            let variants: Vec<Vec<String>> = if options.simvariants.is_empty() {
                vec![Vec::new()]
            } else {
                options
                    .simvariants
                    .iter()
                    .map(|arg| vec![arg.clone()])
                    .collect()
            };
            for plusargs in variants {
                let sim = celox::Simulation::from_sv_sources(sources.clone(), &top)
                    .four_state(options.four_state)
                    .plusargs(plusargs)
                    .build()
                    .map_err(|error| error.to_string())?;
                simulate(sim)?;
            }
            Ok(())
        }
    }
}

/// Runs the simulation until `$finish` or until no event is left.
fn simulate(mut sim: celox::Simulation) -> Result<(), String> {
    use std::io::Write as _;
    let mut stdout = std::io::stdout().lock();
    let mut output = |sim: &mut celox::Simulation| -> Result<(), String> {
        for event in sim.drain_runtime_events() {
            match event {
                celox::RuntimeEvent::Report(celox::Report {
                    severity: celox::Severity::Fatal,
                    message,
                    ..
                }) => {
                    stdout.write_all(&message).unwrap();
                    stdout.write_all(b"\n").unwrap();
                    return Err(format!(
                        "$fatal: {}",
                        std::str::from_utf8(&message)
                            .map(str::to_owned)
                            .unwrap_or_else(|_| message.escape_ascii().to_string())
                    ));
                }
                celox::RuntimeEvent::Display { message }
                | celox::RuntimeEvent::Report(celox::Report { message, .. }) => {
                    stdout.write_all(&message).unwrap();
                    stdout.write_all(b"\n").unwrap();
                }
                celox::RuntimeEvent::Write { message } => stdout.write_all(&message).unwrap(),
                celox::RuntimeEvent::Finish | celox::RuntimeEvent::Stop => {}
                celox::RuntimeEvent::Missed { count } => {
                    return Err(format!("{count} runtime events were lost"));
                }
            }
        }
        Ok(())
    };
    let initial = sim.eval_comb().map_err(|error| error.to_string());
    output(&mut sim)?;
    initial?;
    while !sim.is_finished() {
        let step = sim.step().map_err(|error| error.to_string());
        output(&mut sim)?;
        // A batch run has no interactive mode to suspend into, so it ends at
        // `$stop`, as `vvp -n` does.
        if step?.is_none() || sim.is_stopped() {
            break;
        }
    }
    Ok(())
}

#[allow(clippy::disallowed_macros)] // CLI diagnostics intentionally use stderr.
fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.as_slice() == ["--version"] {
        println!("Celox {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let options = match parse_options(&args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!(
                "{error}\nusage: sv_tests --mode preprocessing|parsing|elaboration|simulation --state-mode 2state|4state [--top NAME] [--define NAME[=VALUE]] [--include DIR] [--simvariant ARG] -- FILE..."
            );
            return ExitCode::from(2);
        }
    };
    match run(&options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn options(args: &[&str]) -> Result<Options, String> {
        parse_options(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn accepts_compiler_inputs_and_rejects_unimplemented_modes() {
        let o = options(&[
            "--mode",
            "simulation",
            "--state-mode",
            "4state",
            "--top",
            "Top",
            "--define",
            "W=8",
            "--define",
            "FLAG",
            "--include",
            "headers",
            "--",
            "a.sv",
            "b.sv",
        ])
        .unwrap();
        assert_eq!(o.mode, Mode::Simulation);
        assert!(o.four_state);
        assert_eq!(o.top.as_deref(), Some("Top"));
        assert_eq!(
            o.defines,
            [("W".into(), Some("8".into())), ("FLAG".into(), None)]
        );
        assert_eq!(o.includes, [PathBuf::from("headers")]);
        assert_eq!(o.files, [PathBuf::from("a.sv"), PathBuf::from("b.sv")]);
        for args in [
            vec!["--mode", "unknown", "--state-mode", "2state", "a.sv"],
            vec!["--mode", "elaboration", "--state-mode", "both", "a.sv"],
            vec![
                "--mode",
                "elaboration",
                "--state-mode",
                "2state",
                "--define",
                "2bad",
                "a.sv",
            ],
            vec!["--mode", "elaboration", "--state-mode", "2state"],
            vec!["a.sv"],
        ] {
            assert!(options(&args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn detects_roots_from_syntax_instead_of_identifier_mentions() {
        let parse =
            |code| celox_sv_analyzer::ParsedSource::parse(code, Path::new("root.sv")).unwrap();
        let parsed = [parse(
            "module Leaf; endmodule module Parent; Leaf u(); string name = \"Parent\"; endmodule",
        )];
        assert_eq!(select_top(&parsed, None).unwrap(), "Parent");
        let parsed = [parse("module A; endmodule module top; endmodule")];
        assert!(
            select_top(&parsed, None)
                .unwrap_err()
                .contains("multiple module roots")
        );
        assert_eq!(select_top(&parsed, Some("A")).unwrap(), "A");
        let parsed = [parse(
            "module Leaf; endmodule module Parent; if (0) begin Leaf u(); end endmodule",
        )];
        assert_eq!(select_top(&parsed, None).unwrap(), "Parent");
        let parsed = [parse("typedef logic T;")];
        assert!(
            select_top(&parsed, None)
                .unwrap_err()
                .contains("none found")
        );
    }
    fn fixture(dir: &Path, name: &str, code: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, code).unwrap();
        path
    }

    fn fixture_options(files: Vec<PathBuf>, mode: Mode) -> Options {
        Options {
            mode,
            four_state: false,
            top: None,
            defines: Vec::new(),
            includes: Vec::new(),
            simvariants: Vec::new(),
            files,
        }
    }

    #[test]
    fn missing_include_directory_is_a_driver_error_even_without_an_include_directive() {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture(dir.path(), "valid.sv", "module Top; endmodule");
        let mut opts = fixture_options(vec![path], Mode::Preprocessing);
        opts.includes = vec![dir.path().join("missing")];
        assert!(
            run(&opts)
                .unwrap_err()
                .contains("include directory is unavailable")
        );
    }

    #[test]
    fn simulation_variants_use_separate_arguments_and_fresh_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture(
            dir.path(),
            "variants.sv",
            r#"
module Top;
    int value = 0;
    initial begin
        if (!$value$plusargs("value=%d", value)) $fatal(1, "missing variant");
        if (value != 1 && value != 2) $fatal(1, "bad variant");
        if ($test$plusargs("value=1") && $test$plusargs("value=2"))
            $fatal(1, "arguments combined");
        #1;
        value = 99;
        $finish;
    end
endmodule
"#,
        );
        let mut opts = fixture_options(vec![path], Mode::Simulation);
        opts.simvariants = vec!["value=1".into(), "value=2".into()];
        run(&opts).unwrap();
        opts.simvariants = vec!["other=1".into(), "value=1".into()];
        assert!(run(&opts).is_err());
        opts.simvariants = vec!["value=1".into(), "value=3".into()];
        assert!(run(&opts).is_err());
        let opts = options(&[
            "--mode",
            "simulation",
            "--state-mode",
            "2state",
            "--simvariant",
            "value=1",
            "--simvariant",
            "value=2",
            "a.sv",
        ])
        .unwrap();
        assert_eq!(opts.simvariants, ["value=1", "value=2"]);
    }

    #[test]
    fn preprocessing_stops_before_syntax_and_reports_directive_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture(
            dir.path(),
            "tokens.sv",
            "`define WORD expanded\n`WORD\nmodule unfinished;",
        );
        let mut opts = fixture_options(vec![path], Mode::Preprocessing);
        run(&opts).unwrap();
        opts.mode = Mode::Parsing;
        assert!(run(&opts).is_err());
        let path = fixture(dir.path(), "invalid.sv", "`include \"missing.svh\"\n");
        assert!(run(&fixture_options(vec![path], Mode::Preprocessing)).is_err());
        assert_eq!(
            options(&["--mode", "preprocessing", "--state-mode", "2state", "a.sv"])
                .unwrap()
                .mode,
            Mode::Preprocessing
        );
    }

    #[test]
    fn parsing_stops_before_metadata_roots_elaboration_and_simulation() {
        let dir = tempfile::tempdir().unwrap();
        for (name, code) in [
            ("unit.sv", "typedef logic [7:0] Byte;"),
            ("package.sv", "package P; typedef logic Byte; endpackage"),
            ("roots.sv", "module A; endmodule module B; endmodule"),
            ("unresolved.sv", "module Top; Missing child(); endmodule"),
            ("semantic.sv", "module Top(input MissingType x); endmodule"),
            ("runtime.sv", "module Top; initial $fatal(1); endmodule"),
        ] {
            let path = fixture(dir.path(), name, code);
            let mut options = fixture_options(vec![path], Mode::Parsing);
            // Even an explicit nonexistent top is irrelevant to syntax parsing.
            options.top = Some("Missing".into());
            run(&options).unwrap_or_else(|error| panic!("{name}: {error}"));
        }
        for code in [
            "module Top;",
            "typedef logic [7:0]",
            "`include \"missing.svh\"\n",
        ] {
            let path = fixture(dir.path(), "invalid.sv", code);
            let options = fixture_options(vec![path], Mode::Parsing);
            assert!(
                run(&options)
                    .unwrap_err()
                    .to_lowercase()
                    .contains("parse error")
            );
        }
        assert_eq!(
            options(&["--mode", "parsing", "--state-mode", "2state", "a.sv"])
                .unwrap()
                .mode,
            Mode::Parsing
        );
    }

    #[test]
    fn rejects_sources_without_a_module_root() {
        let dir = tempfile::tempdir().unwrap();
        for (name, code) in [
            (
                "unit.sv",
                "typedef logic [7:0] Byte; function automatic int twice(int x); return x * 2; endfunction",
            ),
            (
                "package.sv",
                "package P; parameter int W=8; typedef logic [W-1:0] Byte; endpackage",
            ),
        ] {
            let path = fixture(dir.path(), name, code);
            let mut options = fixture_options(vec![path], Mode::Elaboration);
            assert!(run(&options).unwrap_err().contains("module root"));
            options.mode = Mode::Simulation;
            assert!(run(&options).unwrap_err().contains("module root"));
            options.mode = Mode::Elaboration;
            options.top = Some("Missing".into());
            assert!(run(&options).is_err());
        }
        for (name, code, diagnostic) in [
            ("class.sv", "class C; int x; endclass", "module root"),
            (
                "interface.sv",
                "interface Bus; logic x; endinterface",
                "module root",
            ),
            (
                "unresolved_unit.sv",
                "function automatic int f(); return missing; endfunction",
                "module root",
            ),
            ("parse.sv", "typedef logic [7:0]", "parse error"),
            (
                "bad_unit.sv",
                "function automatic int f(); #1; return 0; endfunction",
                "module root",
            ),
            (
                "bad_interface.sv",
                "interface Bus; logic x; assign x = missing; endinterface",
                "module root",
            ),
        ] {
            let path = fixture(dir.path(), name, code);
            let options = fixture_options(vec![path], Mode::Elaboration);
            let error = run(&options)
                .err()
                .unwrap_or_else(|| panic!("{name} was accepted"));
            assert!(error.to_lowercase().contains(diagnostic), "{name}: {error}");
        }
    }

    #[test]
    fn preprocesses_real_includes_defines_and_preserves_compiler_directives() {
        let dir = tempfile::tempdir().unwrap();
        let headers = dir.path().join("headers");
        std::fs::create_dir(&headers).unwrap();
        fixture(&headers, "width.svh", "`define WIDTH `COMMAND_WIDTH\n");
        let child = fixture(
            dir.path(),
            "child.sv",
            "`timescale 1ns/1ps\nmodule Child(output logic [7:0] y); assign y=8'd42; endmodule",
        );
        let top = fixture(
            dir.path(),
            "top.sv",
            r#"`include "width.svh"
`timescale 1ns/1ps
`default_nettype none
`line 30 "authored.sv" 0
module Top;
    logic [`WIDTH-1:0] y;
    Child c(.y);
    initial begin
        #1;
        if ($bits(y) != 8 || y != 42 || $time != 1 || `FLAG != 1)
            $fatal(1, "preprocessor inputs or time scope changed");
        $finish;
    end
endmodule
"#,
        );
        let mut options = fixture_options(vec![child, top], Mode::Simulation);
        options.defines = vec![
            ("COMMAND_WIDTH".into(), Some("8".into())),
            ("FLAG".into(), None),
        ];
        options.includes = vec![headers];
        run(&options).unwrap();
        let code = std::fs::read_to_string(&options.files[1]).unwrap();
        let includes: Vec<_> = options.includes.iter().map(PathBuf::as_path).collect();
        let expanded = celox_sv_analyzer::syntax::preprocess_source(
            &code,
            &options.files[1],
            &options.defines,
            &includes,
        )
        .unwrap();
        for directive in [
            "`timescale 1ns/1ps",
            "`default_nettype none",
            "`line 30 \"authored.sv\" 0",
        ] {
            assert!(expanded.contains(directive), "{expanded}");
        }
        options.includes.clear();
        assert!(run(&options).unwrap_err().contains("parse error"));
        let missing_net = fixture(
            dir.path(),
            "nettype.sv",
            "`default_nettype none\nmodule Top; assign undeclared = 1'b1; endmodule",
        );
        options.files = vec![missing_net];
        options.mode = Mode::Elaboration;
        assert!(run(&options).is_err());
    }
}
