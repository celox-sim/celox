//! Single-case compiler driver for the official sv-tests runner.
//!
//! Metadata, timeouts, expected failures and output assertions belong to
//! sv-tests. This process reports only compiler/simulator success or failure.
//!
//! ```text
//! sv_tests --mode preprocessing|parsing|elaboration|plan|simulation --state-mode 2state|4state \
//!     [--source-encoding utf8|non-utf8-comments] [--top NAME] [--define NAME[=VALUE]] [--include DIR] [--simvariant ARG] -- FILE...
//! ```

#![allow(clippy::disallowed_methods)] // This binary is the process environment boundary.

// The allocator of the `celox` CLI, which this driver stands in for.
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::{collections::BTreeSet, env, path::PathBuf, process::ExitCode};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Preprocessing,
    Parsing,
    Elaboration,
    /// Elaboration up to the compiled scheduler's plan, which is printed;
    /// no code is generated (not an sv-tests mode).
    Plan,
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
    source_encoding: celox_sv_analyzer::syntax::SourceEncoding,
    /// Stop a simulation once its time reaches this (not an sv-tests
    /// option): a window of a long run, for measuring it.
    max_time: Option<u64>,
}

fn parse_options(args: &[String]) -> Result<Options, String> {
    let mut mode = None;
    let mut four_state = None;
    let mut top = None;
    let mut defines = Vec::new();
    let mut includes = Vec::new();
    let mut simvariants = Vec::new();
    let mut files = Vec::new();
    let mut source_encoding = Default::default();
    let mut max_time = None;
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
                    "plan" => Mode::Plan,
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
            "--source-encoding" => {
                source_encoding = match value.as_str() {
                    "utf8" => celox_sv_analyzer::syntax::SourceEncoding::Utf8,
                    "non-utf8-comments" => {
                        celox_sv_analyzer::syntax::SourceEncoding::NonUtf8Comments
                    }
                    _ => return Err(format!("unsupported source encoding: {value}")),
                };
            }
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
            "--max-time" if max_time.is_none() => {
                max_time = Some(
                    value
                        .parse()
                        .map_err(|_| format!("invalid simulation time: {value}"))?,
                );
            }
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
        source_encoding,
        max_time,
    })
}

/// Select roots from parsed instantiations, including inactive generate
/// branches (IEEE 1800-2023 23.3.1). Celox's simulator accepts one root, so
/// simulation requires exactly one; elaboration accepts every root in turn,
/// as tools that elaborate all top-level modules do.
fn select_tops(
    parsed: &[celox_sv_analyzer::ParsedSource],
    explicit: Option<&str>,
    mode: Mode,
) -> Result<Vec<String>, String> {
    if let Some(top) = explicit {
        return Ok(vec![top.to_string()]);
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
        [] => {
            Err("elaboration requires a module root; none found in the parsed sources".to_string())
        }
        _ if mode == Mode::Elaboration => Ok(roots),
        [root] => Ok(vec![root.clone()]),
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
            let text = options
                .source_encoding
                .read(path)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            celox_sv_analyzer::syntax::preprocess_source_with_encoding(
                &text,
                path,
                &options.defines,
                &includes,
                options.source_encoding,
            )
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
    // Sources of packages and compilation-unit items alone, such as
    // classes, elaborate those scopes (IEEE 1800-2023 3.12.1).
    if options.mode == Mode::Elaboration
        && options.top.is_none()
        && parsed.iter().all(|source| source.module_names().is_empty())
    {
        let parsed = parsed.iter().collect::<Vec<_>>();
        let packages = celox_sv_analyzer::ParsedSource::analyze_packages(&parsed)
            .map_err(|error| error.to_string())?;
        if let Some(error) = packages.compilation_unit_error() {
            return Err(error.to_string());
        }
        return Ok(());
    }
    let tops = select_tops(&parsed, options.top.as_deref(), options.mode)?;
    match options.mode {
        Mode::Preprocessing | Mode::Parsing => {
            unreachable!("preprocessing and parsing returned before root selection")
        }
        // A Simulator rejects designs with processes, which it does not run.
        // The driver is an application boundary for diagnostics such as
        // `CELOX_SV_RECURSION_REPORT`.
        Mode::Plan => {
            let start = std::time::Instant::now();
            let report = celox::Simulation::from_sv_sources(sources, &tops[0])
                .four_state(options.four_state)
                .diagnostics_from_env()
                .build_plan();
            tracing::debug!("[elaboration] plan: {:?}", start.elapsed());
            print!("{}", report.map_err(|error| error.to_string())?);
            Ok(())
        }
        Mode::Elaboration => {
            let start = std::time::Instant::now();
            for top in &tops {
                celox::Simulation::from_sv_sources(sources.clone(), top)
                    .four_state(options.four_state)
                    .diagnostics_from_env()
                    .build()
                    .map_err(|error| error.to_string())?;
            }
            tracing::debug!("[elaboration] total: {:?}", start.elapsed());
            Ok(())
        }
        Mode::Simulation => {
            let top = &tops[0];
            // Upstream simvariants are separate runs, each with one plusarg.
            let variants: Vec<Vec<String>> = if options.simvariants.is_empty() {
                vec![Vec::new()]
            } else {
                options
                    .simvariants
                    .iter()
                    .map(|arg| vec![arg.clone()])
                    .collect()
            };
            if let [plusargs] = variants.as_slice() {
                let sim = celox::Simulation::from_sv_sources(sources, top)
                    .four_state(options.four_state)
                    .diagnostics_from_env()
                    .plusargs(plusargs.clone())
                    .build()
                    .map_err(|error| error.to_string())?;
                return simulate(sim, options.max_time);
            }
            simulate_variants(
                sources,
                top,
                options.four_state,
                &variants,
                options.max_time,
            )
        }
    }
}

/// Run each of `variants` in a fresh simulation of `top`. Plusargs are read
/// at run time (IEEE 1800-2023 21.6), so the design compiles once and each
/// variant starts its own instance of the one compiled program: no variable,
/// heap, random or scheduler state is shared, while the analysis and code
/// generation of a large design such as UVM run once rather than per
/// variant.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn simulate_variants(
    sources: Vec<(&str, &std::path::Path)>,
    top: &str,
    four_state: bool,
    variants: &[Vec<String>],
    max_time: Option<u64>,
) -> Result<(), String> {
    let image = celox::Simulation::from_sv_sources(sources, top)
        .four_state(four_state)
        .diagnostics_from_env()
        .compile_native()
        .map_err(|error| error.to_string())?
        .into_program_image();
    for plusargs in variants {
        let sim = celox::Simulation::from_sv_sources(Vec::new(), "")
            .diagnostics_from_env()
            .plusargs(plusargs.clone())
            .build_native_from_image(image.clone())
            .map_err(|error| error.to_string())?;
        simulate(sim, max_time)?;
    }
    Ok(())
}

/// Run each of `variants` in a simulation of `top` built for it, on a host
/// without native images.
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn simulate_variants(
    sources: Vec<(&str, &std::path::Path)>,
    top: &str,
    four_state: bool,
    variants: &[Vec<String>],
    max_time: Option<u64>,
) -> Result<(), String> {
    for plusargs in variants {
        let sim = celox::Simulation::from_sv_sources(sources.clone(), top)
            .four_state(four_state)
            .diagnostics_from_env()
            .plusargs(plusargs.clone())
            .build()
            .map_err(|error| error.to_string())?;
        simulate(sim, max_time)?;
    }
    Ok(())
}

/// Runs the simulation until `$finish` or until no event is left. A run
/// that issued a run-time error (`$error`, or a failed assertion's default
/// action) still runs to its end, then fails, as a Verilator model does: the
/// exit status of a simulation is tool-specific (IEEE 1800-2023 20.10).
/// With `max_time`, the run ends at that time without finishing: no event
/// after it runs, and the errors reported before it still fail the run.
fn simulate(mut sim: celox::Simulation, max_time: Option<u64>) -> Result<(), String> {
    use std::io::Write as _;
    let mut stdout = std::io::stdout().lock();
    let mut errors = 0usize;
    let mut output = |sim: &mut celox::Simulation| -> Result<(), String> {
        let mut fatal = None;
        for event in sim.drain_runtime_events() {
            if let celox::RuntimeEvent::Report(celox::Report {
                severity: celox::Severity::Error,
                ..
            }) = event
            {
                errors += 1;
            }
            match event {
                celox::RuntimeEvent::Report(celox::Report {
                    severity: celox::Severity::Fatal,
                    message,
                    ..
                }) => {
                    stdout.write_all(&message).unwrap();
                    stdout.write_all(b"\n").unwrap();
                    fatal = Some(format!(
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
        fatal.map_or(Ok(()), Err)
    };
    let start = std::time::Instant::now();
    let mut steps = 0u64;
    let mut last_report = start;
    let initial = sim.eval_comb().map_err(|error| error.to_string());
    output(&mut sim)?;
    initial?;
    // Whether the run stopped at the end of the window.
    let mut window = false;
    while !sim.is_finished() {
        if last_report.elapsed() >= std::time::Duration::from_secs(30) {
            report_simulation(&sim, start, steps, "progress");
            last_report = std::time::Instant::now();
        }
        steps += 1;
        // A window runs no event past its end: each step stops there.
        let ended = match max_time {
            Some(max_time) => sim
                .run_until_bounded(max_time, 1)
                .map(|reached| reached || sim.time() >= max_time),
            None => sim.step().map(|step| step.is_none()),
        }
        .map_err(|error| error.to_string());
        output(&mut sim)?;
        // A batch run has no interactive mode to suspend into, so it ends at
        // `$stop`, as `vvp -n` does.
        if sim.is_stopped() {
            break;
        }
        if ended? {
            window = max_time.is_some();
            break;
        }
    }
    if !window && !sim.is_stopped() {
        // Quiescence ends this batch run, but is not itself terminal for the
        // reusable Simulation API (a host can still supply new inputs).
        let finished = sim.finish().map_err(|error| error.to_string());
        output(&mut sim)?;
        finished?;
    }
    report_simulation(&sim, start, steps, if window { "window" } else { "end" });
    match errors {
        0 => Ok(()),
        count => Err(format!("run-time errors reported: {count}")),
    }
}

/// Reports, as a `[simulation]` timing event (see [`PhaseTimingReport`]),
/// how long the simulation ran, how far and what its scheduler did.
fn report_simulation(sim: &celox::Simulation, start: std::time::Instant, steps: u64, at: &str) {
    let stats = sim.scheduler_stats();
    tracing::debug!(
        "[simulation] {at} wall={:?} time={} steps={steps} compiled_slots={} compiled_runs={} reference_steps={} native_service_calls={}",
        start.elapsed(),
        sim.time(),
        stats.compiled_slots,
        stats.compiled_runs,
        stats.reference_steps,
        stats.native_service_calls,
    );
}

/// Prints the compiler's timing events (`[phase-timing]`, `[sv-frontend]`,
/// `[native-timing]` and the like) to stderr, each with the time since the
/// driver started, when `CELOX_PHASE_TIMING` asks for them. The library
/// emits them as `tracing` events and installs no subscriber of its own.
struct PhaseTimingReport {
    start: std::time::Instant,
}

impl tracing::Subscriber for PhaseTimingReport {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        *metadata.level() <= tracing::Level::DEBUG
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    #[allow(clippy::disallowed_macros)] // The report goes to the driver's stderr.
    fn event(&self, event: &tracing::Event<'_>) {
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                use std::fmt::Write;
                if field.name() == "message" {
                    let _ = write!(self.0, "{value:?}");
                } else {
                    let _ = write!(self.0, " {}={value:?}", field.name());
                }
            }
        }
        let mut message = Message(String::new());
        event.record(&mut message);
        // Phase-level lines only; the compiler's per-unit statistics would
        // dominate the run they measure.
        if ![
            "[phase-timing]",
            "[sv-frontend]",
            "[analysis]",
            "[flatten]",
            "[elaboration]",
            "[simulation]",
        ]
        .iter()
        .any(|prefix| message.0.starts_with(prefix))
        {
            return;
        }
        eprintln!(
            "[{:>10.1} ms] {}",
            self.start.elapsed().as_secs_f64() * 1000.0,
            message.0
        );
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

#[allow(clippy::disallowed_macros)] // CLI diagnostics intentionally use stderr.
fn main() -> ExitCode {
    if env::var_os("CELOX_PHASE_TIMING").is_some_and(|value| value != "0") {
        let _ = tracing::subscriber::set_global_default(PhaseTimingReport {
            start: std::time::Instant::now(),
        });
    }
    let args: Vec<String> = env::args().skip(1).collect();
    if args.as_slice() == ["--version"] {
        println!("Celox {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let options = match parse_options(&args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!(
                "{error}\nusage: sv_tests --mode preprocessing|parsing|elaboration|plan|simulation --state-mode 2state|4state [--source-encoding utf8|non-utf8-comments] [--top NAME] [--define NAME[=VALUE]] [--include DIR] [--simvariant ARG] -- FILE..."
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
        assert_eq!(
            select_tops(&parsed, None, Mode::Simulation).unwrap(),
            ["Parent"]
        );
        let parsed = [parse("module A; endmodule module top; endmodule")];
        assert!(
            select_tops(&parsed, None, Mode::Simulation)
                .unwrap_err()
                .contains("multiple module roots")
        );
        assert_eq!(
            select_tops(&parsed, None, Mode::Elaboration).unwrap(),
            ["A", "top"]
        );
        assert_eq!(
            select_tops(&parsed, Some("A"), Mode::Simulation).unwrap(),
            ["A"]
        );
        let parsed = [parse(
            "module Leaf; endmodule module Parent; if (0) begin Leaf u(); end endmodule",
        )];
        assert_eq!(
            select_tops(&parsed, None, Mode::Simulation).unwrap(),
            ["Parent"]
        );
        let parsed = [parse("typedef logic T;")];
        assert!(
            select_tops(&parsed, None, Mode::Simulation)
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
            source_encoding: Default::default(),
            max_time: None,
        }
    }

    #[test]
    fn non_utf8_comments_are_allowed_by_default_in_sources_and_nested_headers() {
        use celox_sv_analyzer::syntax::SourceEncoding;
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().join("top.sv");
        std::fs::write(&top, b"// copyright \xa9\n`include \"outer.svh\"\nmodule Top; initial begin if (`VALUE != 42) $fatal; $finish; end endmodule").unwrap();
        fixture(dir.path(), "outer.svh", "`include \"inner.svh\"\n");
        std::fs::write(
            dir.path().join("inner.svh"),
            b"/* \xff\xfe */\n`define VALUE 42\n",
        )
        .unwrap();
        let mut options = fixture_options(vec![top.clone()], Mode::Simulation);
        run(&options).unwrap();
        options.source_encoding = SourceEncoding::Utf8;
        assert!(run(&options).unwrap_err().contains("utf-8"));
        // A UTF-8 top must still enforce strict encoding in nested includes.
        std::fs::write(&top, "`include \"outer.svh\"\nmodule Top; endmodule").unwrap();
        assert!(run(&options).unwrap_err().contains("cannot read include"));
        options.source_encoding = SourceEncoding::default();
        run(&options).unwrap();
        std::fs::write(&top, b"module Top; initial $display(\"\xff\"); endmodule").unwrap();
        assert!(run(&options).unwrap_err().contains("outside a comment"));
        assert_eq!(
            parse_options(
                &[
                    "--mode",
                    "parsing",
                    "--state-mode",
                    "2state",
                    "--source-encoding",
                    "utf8",
                    "--",
                    "top.sv"
                ]
                .map(str::to_owned)
            )
            .unwrap()
            .source_encoding,
            SourceEncoding::Utf8
        );
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
    fn simulation_runs_final_on_quiescence_but_elaboration_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture(
            dir.path(),
            "final.sv",
            "module Top; initial #2; final $fatal(1, \"from final\"); endmodule",
        );
        run(&fixture_options(vec![path.clone()], Mode::Elaboration)).unwrap();
        let error = run(&fixture_options(vec![path], Mode::Simulation)).unwrap_err();
        assert!(error.contains("from final"), "{error}");
    }

    #[test]
    fn simulation_that_reports_an_error_runs_to_its_end_then_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture(
            dir.path(),
            "error.sv",
            "module Top; initial begin $warning(\"w\"); $error(\"e\"); #1 $error(\"late\"); end endmodule",
        );
        let error = run(&fixture_options(vec![path], Mode::Simulation)).unwrap_err();
        assert_eq!(error, "run-time errors reported: 2");
        let path = fixture(
            dir.path(),
            "warning.sv",
            "module Top; initial begin $warning(\"w\"); $info(\"i\"); end endmodule",
        );
        run(&fixture_options(vec![path], Mode::Simulation)).unwrap();
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
            // Elaboration of packages and compilation-unit items alone
            // elaborates those scopes (IEEE 1800-2023 3.12.1); a simulation
            // needs a module root.
            run(&options).unwrap_or_else(|error| panic!("{name}: {error}"));
            options.mode = Mode::Simulation;
            assert!(run(&options).unwrap_err().contains("module root"));
            options.mode = Mode::Elaboration;
            options.top = Some("Missing".into());
            assert!(run(&options).is_err());
        }
        // A class or an interface no module instantiates elaborates as well,
        // and has no module root to simulate.
        for (name, code) in [
            ("class.sv", "class C; int x; endclass"),
            ("interface.sv", "interface Bus; logic x; endinterface"),
        ] {
            let path = fixture(dir.path(), name, code);
            let mut options = fixture_options(vec![path], Mode::Elaboration);
            run(&options).unwrap_or_else(|error| panic!("{name}: {error}"));
            options.mode = Mode::Simulation;
            assert!(run(&options).unwrap_err().contains("module root"), "{name}");
        }
        // Errors of such sources are errors of the elaboration.
        for (name, code, diagnostic) in [
            ("parse.sv", "typedef logic [7:0]", "parse error"),
            (
                "bad_unit.sv",
                "function automatic int f(); #1; return 0; endfunction",
                "delay control in a function",
            ),
            (
                "undeclared_unit.sv",
                "function automatic int f(); return missing; endfunction",
                "`missing` is not declared",
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
