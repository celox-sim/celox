#![allow(clippy::disallowed_methods)] // This binary is the process environment boundary.

use std::{
    env,
    ffi::OsStr,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use celox::{
    NativeProgramImage, NativeProgramInstance, StateDifference, StateFile, StateObject, StateRole,
    format_state_value,
};
use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};

#[path = "celox/project.rs"]
mod project;
#[path = "celox/runner.rs"]
mod runner;
use project::{Design, DesignArgs};

#[derive(Parser)]
#[command(
    name = "celox",
    version = env!("CELOX_VERSION"),
    about = "Compile and run Veryl and SystemVerilog designs with Celox"
)]
struct Cli {
    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Subcommand)]
enum CliCommand {
    /// Run a SystemVerilog or Veryl native testbench.
    Run(RunArgs),
    /// Build native executables for VPI testbenches.
    Vpi(VpiArgs),
    /// Inspect saved simulation state files.
    State(StateArgs),
}

#[derive(Args)]
struct RunArgs {
    #[command(flatten)]
    design: DesignArgs,

    #[command(flatten)]
    execution: runner::ExecutionArgs,

    /// Maximum SV scheduler steps or Veryl ticks (default: project setting or 1000000).
    /// Does not bound compilation or work within a single step/tick.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    max_steps: Option<u64>,
}

struct RunConfig {
    execution: runner::ExecutionArgs,
    design: Design,
    opt_level: celox::OptLevel,
    max_steps: u64,
}

fn resolve_design(arguments: DesignArgs) -> Design {
    arguments.resolve().unwrap_or_else(|message| {
        Cli::command()
            .error(clap::error::ErrorKind::InvalidValue, message)
            .exit()
    })
}

fn cli_from_matches(matches: &clap::ArgMatches) -> Result<Cli, clap::Error> {
    let mut cli = Cli::from_arg_matches(matches)?;
    match &mut cli.command {
        CliCommand::Run(arguments) => arguments.design.capture_input_order(
            matches
                .subcommand_matches("run")
                .expect("parsed run command"),
        ),
        CliCommand::Vpi(VpiArgs {
            command: VpiCommand::Build(arguments),
        }) => {
            arguments.design.capture_input_order(
                matches
                    .subcommand_matches("vpi")
                    .and_then(|matches| matches.subcommand_matches("build"))
                    .expect("parsed vpi build command"),
            );
        }
        CliCommand::State(_) => {}
    }
    Ok(cli)
}

fn parse_opt_level(value: &str) -> Result<celox::OptLevel, String> {
    celox::OptLevel::parse(value).ok_or_else(|| "expected O0, O1, or O2".to_string())
}

fn resolve_run_config(arguments: RunArgs) -> RunConfig {
    let design = arguments
        .design
        .resolve_for_run(arguments.execution.native_image_input.is_some())
        .unwrap_or_else(|message| {
            Cli::command()
                .error(clap::error::ErrorKind::InvalidValue, message)
                .exit()
        });
    RunConfig {
        execution: arguments.execution,
        opt_level: design.opt_level.unwrap_or(celox::OptLevel::O2),
        max_steps: arguments
            .max_steps
            .or(design.max_steps)
            .unwrap_or(1_000_000),
        design,
    }
}

fn run_testbench(arguments: RunArgs) -> Result<(), String> {
    let mut arguments = resolve_run_config(arguments);
    arguments.design.opt_level = Some(arguments.opt_level);
    if arguments.design.is_sv() {
        #[cfg(feature = "systemverilog")]
        return run_sv(arguments);
        #[cfg(not(feature = "systemverilog"))]
        return Err(
            "SystemVerilog support requires building celox-vpi with --features systemverilog"
                .to_string(),
        );
    }
    if arguments.execution.native_image_input.is_none()
        && !arguments.design.sources.iter().all(|path| {
            path.extension()
                .is_some_and(|extension| extension == "veryl")
        })
    {
        return Err("pass .sv/.v sources, or Veryl project sources".into());
    }
    runner::run(arguments.execution, arguments.design, arguments.max_steps).map_err(|error| {
        if let runner::RunError::InvalidConfiguration { .. } = &error {
            clap::Error::raw(clap::error::ErrorKind::ArgumentConflict, error.to_string()).exit();
        }
        error.to_string()
    })
}

#[cfg(feature = "systemverilog")]
fn run_sv(arguments: RunConfig) -> Result<(), String> {
    arguments.execution.validate_sv().unwrap_or_else(|message| {
        clap::Error::raw(clap::error::ErrorKind::ArgumentConflict, message).exit()
    });
    let sources = arguments.design.sv_sources()?;
    let source_refs = sources
        .iter()
        .map(|(text, path)| (text.as_str(), path.as_path()))
        .collect();
    let builder = celox::Simulation::from_sv_sources(source_refs, &arguments.design.top)
        .four_state(arguments.design.four_state.unwrap_or(false))
        .threads(arguments.execution.threads as usize)
        .opt_level(arguments.opt_level)
        .diagnostic_policy(arguments.design.diagnostics.clone())
        .sv_timescale(arguments.design.timescale)
        .signal_access(arguments.design.signal_access.clone());
    let builder = arguments.design.apply_parameters(builder);
    let max_steps = if arguments.execution.unlimited {
        u64::MAX
    } else {
        arguments.max_steps
    };
    macro_rules! execute {
        ($build:ident) => {{
            let simulation = builder
                .$build()
                .map_err(|error| format!("compilation failed:\n{error}"))?;
            report_warnings(simulation.warnings())?;
            simulate_sv(simulation, max_steps, &mut std::io::stdout().lock())
        }};
    }
    match arguments.execution.backend {
        runner::Backend::Native => execute!(build),
        #[cfg(feature = "cranelift")]
        runner::Backend::Cranelift => execute!(build_cranelift),
        runner::Backend::Interpreter => execute!(build_interpreter),
        #[cfg(feature = "wasm-runtime")]
        runner::Backend::Wasm => execute!(build_wasm),
        runner::Backend::Tiered => execute!(build_tiered),
    }
}

#[cfg(feature = "systemverilog")]
fn simulate_sv<B: celox::SimBackend>(
    mut simulation: celox::Simulation<B>,
    max_steps: u64,
    output: &mut impl Write,
) -> Result<(), String> {
    let mut failed = false;
    let initial = simulation.eval_comb();
    drain_sv_output(&mut simulation, output, &mut failed)?;
    initial.map_err(|error| error.to_string())?;
    let mut steps = 0;
    while !simulation.is_finished() && simulation.next_event_time().is_some() {
        if steps == max_steps {
            return Err(format!("simulation exceeded --max-steps {}", max_steps));
        }
        let step = simulation.step();
        drain_sv_output(&mut simulation, output, &mut failed)?;
        if step.map_err(|error| error.to_string())?.is_none() {
            break;
        }
        steps += 1;
    }
    if failed {
        return Err("SystemVerilog testbench reported an assertion failure or $error".to_string());
    }
    Ok(())
}

#[cfg(feature = "systemverilog")]
fn drain_sv_output<B: celox::SimBackend>(
    simulation: &mut celox::Simulation<B>,
    output: &mut impl Write,
    failed: &mut bool,
) -> Result<(), String> {
    let mut fatal = false;
    let mut stopped = false;
    let mut missed = 0;
    for event in simulation.drain_runtime_events() {
        let (message, newline) = match event {
            celox::RuntimeEvent::Display { message } => (message, true),
            celox::RuntimeEvent::Write { message } => (message, false),
            // Only errors fail the run (IEEE 1800-2023 20.10).
            celox::RuntimeEvent::Report(report) => {
                match report.severity {
                    celox::Severity::Fatal => fatal = true,
                    celox::Severity::Error => *failed = true,
                    celox::Severity::Warning | celox::Severity::Info => {}
                }
                (report.render(), true)
            }
            celox::RuntimeEvent::Finish => continue,
            celox::RuntimeEvent::Stop => {
                stopped = true;
                continue;
            }
            celox::RuntimeEvent::Missed { count } => {
                missed += count;
                continue;
            }
        };
        output
            .write_all(&message)
            .and_then(|()| {
                if newline {
                    output.write_all(b"\n")
                } else {
                    Ok(())
                }
            })
            .map_err(|error| format!("failed to write simulation output: {error}"))?;
    }
    output
        .flush()
        .map_err(|error| format!("failed to flush simulation output: {error}"))?;
    if missed != 0 {
        return Err(format!("{missed} runtime events were lost"));
    }
    if fatal {
        return Err("SystemVerilog testbench reported $fatal".to_string());
    }
    if stopped || simulation.is_stopped() {
        return Err(
            "SystemVerilog testbench requested $stop; interactive simulation is unavailable"
                .to_string(),
        );
    }
    Ok(())
}

#[derive(Args)]
struct StateArgs {
    #[command(subcommand)]
    command: StateCommand,
}

#[derive(Subcommand)]
enum StateCommand {
    /// Print the saved values of a state file.
    Dump(StateDumpArgs),
    /// Print the objects whose saved values differ between two state files.
    /// Exits with status 1 when they differ.
    Diff(StateDiffArgs),
}

#[derive(Args)]
struct StateDumpArgs {
    file: PathBuf,

    /// Also print combinational objects, which are recomputed on load.
    #[arg(long)]
    comb: bool,
}

#[derive(Args)]
struct StateDiffArgs {
    left: PathBuf,
    right: PathBuf,

    /// Also compare combinational objects. Optimizations may leave their
    /// saved values stale.
    #[arg(long)]
    comb: bool,
}

#[derive(Args)]
struct VpiArgs {
    #[command(subcommand)]
    command: VpiCommand,
}

#[derive(Subcommand)]
enum VpiCommand {
    /// Compile a Veryl or SystemVerilog design into a native simulation executable.
    Build(BuildArgs),
}

#[derive(Args)]
struct BuildArgs {
    #[command(flatten)]
    design: DesignArgs,

    /// Native simulation executable to create.
    #[arg(short, long, default_value = "celox.out")]
    output: PathBuf,
}

#[derive(Parser)]
#[command(
    name = "celox simulation",
    about = "Run an attached Celox design with cocotb"
)]
struct SimulationArgs {
    /// Python module containing cocotb tests (comma-separated for multiple modules).
    #[arg(long, value_name = "MODULE")]
    test_module: Option<String>,

    /// Run only the named cocotb test case.
    #[arg(long, conflicts_with = "test_filter")]
    testcase: Option<String>,

    /// Run cocotb tests whose fully qualified names match this regular expression.
    #[arg(long, conflicts_with = "testcase")]
    test_filter: Option<String>,

    /// Python interpreter whose cocotb installation should be used.
    #[arg(long, value_name = "PATH")]
    python: Option<PathBuf>,

    /// Explicit path to cocotb's Icarus VPI adapter.
    #[arg(long, value_name = "PATH")]
    vpi: Option<PathBuf>,

    /// cocotb xUnit result file.
    #[arg(long, value_name = "PATH")]
    results_file: Option<PathBuf>,
}

fn build(arguments: BuildArgs) -> Result<(), String> {
    let design = resolve_design(arguments.design);
    let runtime = env::current_exe()
        .map_err(|error| format!("failed to locate the celox executable: {error}"))?;
    if arguments.output.exists()
        && arguments
            .output
            .canonicalize()
            .is_ok_and(|output| output == runtime)
    {
        return Err("the output path cannot overwrite the running celox executable".to_string());
    }
    if let Some(parent) = arguments
        .output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create output directory {}: {error}",
                parent.display()
            )
        })?;
    }

    compile_native_design(&design)?
        .write_attached_runtime(runtime, &arguments.output)
        .map_err(|error| format!("failed to write {}: {error}", arguments.output.display()))?;
    println!("Created {}", arguments.output.display());
    Ok(())
}

fn compile_native_design(design: &Design) -> Result<NativeProgramImage, String> {
    if design.is_sv() {
        #[cfg(feature = "systemverilog")]
        {
            let sources = design.sv_sources()?;
            let refs = sources
                .iter()
                .map(|(text, path)| (text.as_str(), path.as_path()))
                .collect();
            let builder = celox::Simulator::from_sv_sources(refs, &design.top)
                .four_state(design.four_state.unwrap_or(false))
                .diagnostic_policy(design.diagnostics.clone())
                .sv_timescale(design.timescale)
                .signal_access(design.signal_access.clone())
                .opt_level(design.opt_level.unwrap_or(celox::OptLevel::O1));
            let simulator = design
                .apply_parameters(builder)
                .compile_native()
                .map_err(|error| format!("compilation failed:\n{error}"))?;
            report_warnings(simulator.warnings())?;
            return Ok(simulator.into_program_image());
        }
        #[cfg(not(feature = "systemverilog"))]
        return Err(
            "SystemVerilog support requires building celox-vpi with --features systemverilog"
                .to_string(),
        );
    }
    if design.sources.len() != 1
        || !design.sources[0]
            .extension()
            .is_some_and(|extension| extension == "veryl")
    {
        return Err("pass .sv/.v sources, or one .veryl project entry file".to_string());
    }
    let (image, warnings) = celox_vpi::driver::compile_native_image_with_state(
        &design.sources[0],
        &design.top,
        design.diagnostics.clone(),
        design.opt_level.unwrap_or(celox::OptLevel::O1),
        design.signal_access.clone(),
        &design.parameters,
        design.four_state.unwrap_or(false),
    )?;
    report_warnings(&warnings)?;
    Ok(image)
}

fn report_warnings(warnings: &[celox::CompilationWarning]) -> Result<(), String> {
    let mut output = std::io::stderr().lock();
    for warning in warnings {
        writeln!(output, "{}", celox::render_diagnostic(warning))
            .map_err(|error| format!("failed to write diagnostics: {error}"))?;
    }
    Ok(())
}

fn environment_path(name: &str) -> Option<PathBuf> {
    env::var_os(name).map(PathBuf::from)
}

fn python_output(python: &Path, arguments: &[&OsStr], purpose: &str) -> Result<String, String> {
    let output = Command::new(python)
        .args(arguments)
        .output()
        .map_err(|error| {
            format!(
                "failed to run Python interpreter `{}` while discovering {purpose}: {error}",
                python.display()
            )
        })?;
    if !output.status.success() {
        let diagnostics = if output.stderr.is_empty() {
            &output.stdout
        } else {
            &output.stderr
        };
        return Err(format!(
            "Python could not discover {purpose}: {}",
            String::from_utf8_lossy(diagnostics).trim()
        ));
    }
    let value = String::from_utf8(output.stdout)
        .map_err(|_| format!("Python returned a non-UTF-8 path for {purpose}"))?;
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("Python returned an empty path for {purpose}"));
    }
    Ok(value.to_string())
}

fn discover_libpython(python: &Path) -> Result<PathBuf, String> {
    python_output(
        python,
        &[
            OsStr::new("-m"),
            OsStr::new("cocotb_tools.config"),
            OsStr::new("--libpython"),
        ],
        "libpython",
    )
    .map(PathBuf::from)
}

fn discover_pygpi_entry_point(python: &Path) -> Result<Option<String>, String> {
    const SUPPORTED_PREFIX: &str = "celox-pygpi-entry-point:";
    const UNSUPPORTED: &str = "celox-pygpi-entry-point-unavailable";

    // cocotb 2.1 exposes PyGPI as a separate GPI user. Older cocotb releases
    // embed Python directly and do not provide this configuration function.
    let output = python_output(
        python,
        &[
            OsStr::new("-c"),
            OsStr::new(
                "from cocotb_tools import config; entry = getattr(config, 'pygpi_entry_point', None); print('celox-pygpi-entry-point:' + entry() if entry is not None else 'celox-pygpi-entry-point-unavailable')",
            ),
        ],
        "cocotb's PyGPI entry point",
    )?;
    if output == UNSUPPORTED {
        return Ok(None);
    }
    output
        .strip_prefix(SUPPORTED_PREFIX)
        .filter(|entry| !entry.is_empty())
        .map(|entry| Some(entry.to_string()))
        .ok_or_else(|| format!("Python returned an invalid PyGPI entry point: {output}"))
}

fn gpi_users_value(libpython: &Path, pygpi_entry_point: &str) -> String {
    format!("{};{pygpi_entry_point}", libpython.display())
}

fn discover_python(python: &Path) -> Result<PathBuf, String> {
    python_output(
        python,
        &[
            OsStr::new("-c"),
            OsStr::new("import sys; print(sys.executable)"),
        ],
        "the Python executable",
    )
    .map(PathBuf::from)
}

fn discover_vpi(python: &Path) -> Result<PathBuf, String> {
    let configured = python_output(
        python,
        &[
            OsStr::new("-m"),
            OsStr::new("cocotb_tools.config"),
            OsStr::new("--lib-name-path"),
            OsStr::new("vpi"),
            OsStr::new("icarus"),
        ],
        "cocotb's VPI adapter",
    )
    .map(PathBuf::from)?;
    if configured.is_file() {
        return Ok(configured);
    }

    // cocotb 2.0 reports Icarus' loadable module without its `.vpl`
    // extension, while later releases report the complete shared-library path.
    for extension in ["vpl", "so", "dylib", "dll"] {
        let candidate = configured.with_extension(extension);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(format!(
        "cocotb reported VPI adapter `{}`, but no loadable library exists there",
        configured.display()
    ))
}

fn check_results(python: &Path, results_file: &Path) -> Result<(), String> {
    let output = Command::new(python)
        .args(["-c", r#"
import sys
from xml.etree import ElementTree

cases = list(ElementTree.parse(sys.argv[1]).getroot().iter("testcase"))
executed = [case for case in cases if case.find("skipped") is None]
failed = [case for case in executed if case.find("failure") is not None or case.find("error") is not None]
if not executed:
    sys.exit("no cocotb tests were executed (check the test filter and skip settings)")
if failed:
    names = ", ".join(case.get("name", "<unnamed>") for case in failed[:8])
    sys.exit(f"{len(failed)} cocotb test(s) failed: {names}")
"#])
        .arg(results_file)
        .output()
        .map_err(|error| format!("failed to check cocotb results: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let diagnostics = if output.stderr.is_empty() {
        &output.stdout
    } else {
        &output.stderr
    };
    Err(format!(
        "cocotb reported a test failure: {}",
        String::from_utf8_lossy(diagnostics).trim()
    ))
}

fn set_environment(name: &str, value: impl AsRef<OsStr>) {
    // Safety: an attached image takes this single-threaded path before loading
    // cocotb or invoking any foreign runtime code.
    unsafe { env::set_var(name, value) };
}

fn remove_environment(name: &str) {
    // Safety: an attached image takes this single-threaded path before loading
    // cocotb or invoking any foreign runtime code.
    unsafe { env::remove_var(name) };
}

fn remove_stale_results(results_file: &Path) -> Result<(), String> {
    match std::fs::remove_file(results_file) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "failed to remove stale cocotb results {}: {error}",
            results_file.display()
        )),
    }
}

fn run_simulation(image: NativeProgramImage, arguments: SimulationArgs) -> Result<(), String> {
    let test_module = arguments
        .test_module
        .or_else(|| env::var_os("COCOTB_TEST_MODULES").map(|value| value.to_string_lossy().into()))
        .ok_or_else(|| {
            "pass --test-module MODULE or set COCOTB_TEST_MODULES to select a cocotb test"
                .to_string()
        })?;
    let python_command = arguments
        .python
        .or_else(|| environment_path("PYGPI_PYTHON_BIN"))
        .unwrap_or_else(|| PathBuf::from("python3"));
    let python = discover_python(&python_command)?;
    let vpi = arguments
        .vpi
        .or_else(|| environment_path("CELOX_COCOTB_VPI"))
        .map_or_else(|| discover_vpi(&python), Ok)?;
    let libpython = match environment_path("LIBPYTHON_LOC") {
        Some(path) => path,
        None => discover_libpython(&python)?,
    };
    // Preserve an explicitly configured user chain. Otherwise, match cocotb
    // 2.1's runner and load libpython before the PyGPI entry point.
    let gpi_users = if env::var_os("GPI_USERS").is_none() {
        discover_pygpi_entry_point(&python)?
            .map(|pygpi_entry_point| gpi_users_value(&libpython, &pygpi_entry_point))
    } else {
        None
    };
    let results_file = arguments
        .results_file
        .or_else(|| environment_path("COCOTB_RESULTS_FILE"))
        .unwrap_or_else(|| PathBuf::from("results.xml"));
    let top = image
        .reflection()
        .scopes()
        .iter()
        .find(|scope| scope.parent.is_none())
        .map(|scope| scope.full_name.clone())
        .ok_or_else(|| "attached design has no top-level scope".to_string())?;

    // A VPI startup failure can return control without producing a new xUnit
    // file. Invalidate an earlier run so it cannot make this run look successful.
    remove_stale_results(&results_file)?;

    set_environment("PYGPI_PYTHON_BIN", &python);
    set_environment("LIBPYTHON_LOC", libpython);
    if let Some(gpi_users) = gpi_users {
        set_environment("GPI_USERS", gpi_users);
    }
    set_environment("COCOTB_TOPLEVEL", top);
    set_environment("COCOTB_TEST_MODULES", test_module);
    set_environment("TOPLEVEL_LANG", "verilog");
    if let Some(testcase) = arguments.testcase {
        remove_environment("COCOTB_TEST_FILTER");
        set_environment("COCOTB_TESTCASE", testcase);
    }
    if let Some(test_filter) = arguments.test_filter {
        remove_environment("COCOTB_TESTCASE");
        set_environment("COCOTB_TEST_FILTER", test_filter);
    }
    set_environment("COCOTB_RESULTS_FILE", &results_file);

    // Safety: the image is compiler-produced data attached to this executable.
    let instance = unsafe { NativeProgramInstance::from_image(image) }
        .map_err(|error| format!("failed to load attached design: {error}"))?;
    celox_vpi::driver::run_cocotb(instance, &vpi)?;
    check_results(&python, &results_file)
}

fn read_state_file(path: &Path) -> Result<StateFile, String> {
    let file = std::fs::File::open(path)
        .map_err(|error| format!("failed to open {}: {error}", path.display()))?;
    StateFile::read_from(std::io::BufReader::new(file))
        .map_err(|error| format!("failed to read {}: {error}", path.display()))
}

fn format_object(object: &StateObject) -> String {
    format_state_value(&object.value, object.mask.as_deref(), object.width)
}

fn dump_state(file: &StateFile, comb: bool, out: &mut impl Write) -> std::io::Result<()> {
    writeln!(
        out,
        "four-state: {}",
        if file.four_state { "yes" } else { "no" }
    )?;
    if let Some(schedule) = &file.schedule {
        writeln!(out, "time: {}", schedule.time)?;
        for (event, period) in &schedule.clocks {
            writeln!(out, "clock {event} period {period}")?;
        }
        let mut events: Vec<_> = schedule.events.iter().collect();
        events.sort_by(|a, b| (a.time, &a.event).cmp(&(b.time, &b.event)));
        for event in events {
            writeln!(
                out,
                "event at {}: {} <= {} ({})",
                event.time, event.signal, event.value, event.event
            )?;
        }
    }
    for object in &file.objects {
        if object.role == StateRole::Comb && !comb {
            continue;
        }
        let role = match object.role {
            StateRole::State => "state",
            StateRole::Comb => "comb ",
        };
        writeln!(
            out,
            "{role} {}[{}] = {}",
            object.path,
            object.width,
            format_object(object)
        )?;
    }
    Ok(())
}

/// Print the differences and report whether there were any.
fn diff_state(
    left: &StateFile,
    right: &StateFile,
    comb: bool,
    out: &mut impl Write,
) -> std::io::Result<bool> {
    let differences = left.diff(right, comb);
    for difference in &differences {
        match difference {
            StateDifference::Changed { path, left, right } => writeln!(
                out,
                "~ {path}: {} -> {}",
                format_object(left),
                format_object(right)
            )?,
            StateDifference::OnlyLeft(object) => {
                writeln!(out, "- {} = {}", object.path, format_object(object))?
            }
            StateDifference::OnlyRight(object) => {
                writeln!(out, "+ {} = {}", object.path, format_object(object))?
            }
        }
    }
    Ok(!differences.is_empty())
}

fn run_state(arguments: StateArgs) -> Result<ExitCode, String> {
    let mut out = std::io::stdout().lock();
    let io_error = |error: std::io::Error| format!("failed to write output: {error}");
    match arguments.command {
        StateCommand::Dump(arguments) => {
            let file = read_state_file(&arguments.file)?;
            dump_state(&file, arguments.comb, &mut out).map_err(io_error)?;
            Ok(ExitCode::SUCCESS)
        }
        StateCommand::Diff(arguments) => {
            let left = read_state_file(&arguments.left)?;
            let right = read_state_file(&arguments.right)?;
            let differ = diff_state(&left, &right, arguments.comb, &mut out).map_err(io_error)?;
            Ok(if differ {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
    }
}

fn run() -> Result<ExitCode, String> {
    match NativeProgramImage::discover_in_current_executable()
        .map_err(|error| format!("failed to inspect the celox executable: {error}"))?
    {
        Some(attached) => {
            run_simulation(attached.image, SimulationArgs::parse()).map(|()| ExitCode::SUCCESS)
        }
        None => match cli_from_matches(&Cli::command().get_matches())
            .unwrap_or_else(|error| error.exit())
            .command
        {
            CliCommand::Run(arguments) => run_testbench(arguments).map(|()| ExitCode::SUCCESS),
            CliCommand::Vpi(arguments) => match arguments.command {
                VpiCommand::Build(arguments) => build(arguments).map(|()| ExitCode::SUCCESS),
            },
            CliCommand::State(arguments) => run_state(arguments),
        },
    }
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("off")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    match run() {
        Ok(code) => code,
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "celox: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_defaults_to_o2_for_both_languages_and_allows_overrides() {
        let temporary = tempfile::tempdir().unwrap();
        for project_level in [None, Some("O0"), Some("O1"), Some("O2")] {
            let project = temporary.path().join("celox.toml");
            if let Some(level) = project_level {
                std::fs::write(&project, format!("opt_level = '{level}'\n")).unwrap();
            }
            for source in ["top.sv", "top.v", "top.veryl"] {
                for extra in [
                    vec![],
                    vec!["-O", "O0"],
                    vec!["-O", "O1"],
                    vec!["--opt-level", "O2"],
                ] {
                    let mut argv = vec!["celox", "run", source, "--top", "Top"];
                    if project_level.is_some() {
                        argv.extend(["--project", project.to_str().unwrap()]);
                    }
                    argv.extend(extra.iter().copied());
                    let cli = Cli::try_parse_from(argv).unwrap();
                    let CliCommand::Run(arguments) = cli.command else {
                        panic!("expected the run command");
                    };
                    let expected =
                        parse_opt_level(extra.last().copied().or(project_level).unwrap_or("O2"))
                            .unwrap();
                    assert_eq!(
                        resolve_run_config(arguments).opt_level,
                        expected,
                        "{source}: {project_level:?} {extra:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn source_and_filelist_arguments_keep_their_command_line_order() {
        let temporary = tempfile::tempdir().unwrap();
        let list = temporary.path().join("sources.f");
        std::fs::write(&list, "middle.sv\n").unwrap();
        for command in [vec!["celox", "run"], vec!["celox", "vpi", "build"]] {
            let matches = Cli::command()
                .try_get_matches_from(command.into_iter().chain([
                    "first.sv",
                    "-f",
                    list.to_str().unwrap(),
                    "last.sv",
                    "--top",
                    "Top",
                ]))
                .unwrap();
            let cli = cli_from_matches(&matches).unwrap();
            let design = match cli.command {
                CliCommand::Run(args) => args.design.resolve().unwrap(),
                CliCommand::Vpi(VpiArgs {
                    command: VpiCommand::Build(args),
                }) => args.design.resolve().unwrap(),
                _ => panic!("expected a compilation command"),
            };
            assert_eq!(
                design.sources,
                [
                    PathBuf::from("first.sv"),
                    temporary.path().join("middle.sv"),
                    PathBuf::from("last.sv")
                ]
            );
        }
    }

    #[cfg(feature = "systemverilog")]
    #[test]
    fn sv_scheduler_limit_fails_and_preserves_scheduled_output() {
        let source =
            "module Top(input bit clk); always @(posedge clk) $display(\"tick\"); endmodule";
        let mut simulation =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top")
                .build()
                .unwrap();
        simulation.add_clock("clk", 2, 1);
        let mut output = Vec::new();
        let error = simulate_sv(simulation, 3, &mut output).unwrap_err();
        assert!(error.contains("exceeded --max-steps 3"));
        assert!(String::from_utf8(output).unwrap().contains("tick"));
    }

    /// Only `$error` and `$fatal` fail a run; every report is printed with
    /// its severity and location (IEEE 1800-2023 20.10).
    #[cfg(feature = "systemverilog")]
    #[test]
    fn sv_reports_fail_the_run_only_from_errors() {
        let run = |task: &str| {
            let source = format!("module Top; initial {task}(\"note\"); endmodule");
            let simulation = celox::Simulation::from_sv_sources(
                vec![(source.as_str(), Path::new("top.sv"))],
                "Top",
            )
            .build()
            .unwrap();
            let mut output = Vec::new();
            let result = simulate_sv(simulation, 10, &mut output);
            (result, String::from_utf8(output).unwrap())
        };
        let (result, output) = run("$info");
        assert!(result.is_ok());
        assert!(output.starts_with("Info: top.sv:1: note"), "{output}");
        let (result, output) = run("$warning");
        assert!(result.is_ok());
        assert!(output.starts_with("Warning: top.sv:1: note"), "{output}");
        let (result, output) = run("$error");
        assert!(result.is_err());
        assert!(output.starts_with("Error: top.sv:1: note"), "{output}");
    }

    fn state_file(count: u8, stale: u8) -> StateFile {
        let object = |path: &str, role, value: u8, mask: u8| StateObject {
            path: path.into(),
            width: 8,
            role,
            is_4state: true,
            value: vec![value],
            mask: Some(vec![mask]),
        };
        StateFile {
            four_state: true,
            objects: vec![
                object("count", StateRole::State, count, 0),
                object("flags", StateRole::State, 0x30, 0x0f),
                object("next", StateRole::Comb, stale, 0),
            ],
            schedule: None,
            strings: Vec::new(),
        }
    }

    #[test]
    fn state_dump_hides_combinational_objects_by_default() {
        let mut out = Vec::new();
        dump_state(&state_file(5, 6), false, &mut out).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "four-state: yes\nstate count[8] = 05\nstate flags[8] = 3x\n"
        );
        let mut out = Vec::new();
        dump_state(&state_file(5, 6), true, &mut out).unwrap();
        assert!(
            String::from_utf8(out)
                .unwrap()
                .ends_with("comb  next[8] = 06\n")
        );
    }

    #[test]
    fn state_diff_reports_changed_state() {
        let mut out = Vec::new();
        assert!(!diff_state(&state_file(5, 6), &state_file(5, 9), false, &mut out).unwrap());
        assert!(out.is_empty());
        assert!(diff_state(&state_file(5, 6), &state_file(7, 6), false, &mut out).unwrap());
        assert_eq!(String::from_utf8(out).unwrap(), "~ count: 05 -> 07\n");
    }

    #[test]
    fn state_subcommands_parse() {
        let cli = Cli::try_parse_from(["celox", "state", "diff", "a.state", "b.state", "--comb"])
            .expect("valid state diff command");
        let CliCommand::State(StateArgs {
            command: StateCommand::Diff(arguments),
        }) = cli.command
        else {
            panic!("expected the state diff command");
        };
        assert!(arguments.comb);
        assert_eq!(arguments.right, Path::new("b.state"));
    }

    #[test]
    fn vpi_build_arguments_have_a_default_output() {
        let cli = Cli::try_parse_from(["celox", "vpi", "build", "top.veryl", "--top", "Top"])
            .expect("valid VPI build command");
        let CliCommand::Vpi(arguments) = cli.command else {
            panic!("expected the vpi command");
        };
        let VpiCommand::Build(arguments) = arguments.command;
        assert_eq!(arguments.output, Path::new("celox.out"));
    }

    #[test]
    fn simulation_arguments_accept_the_cocotb_entry_points() {
        let arguments = SimulationArgs::try_parse_from([
            "sim",
            "--test-module",
            "test_counter",
            "--python",
            "/usr/bin/python3",
            "--results-file",
            "build/results.xml",
        ])
        .expect("valid simulation command");
        assert_eq!(arguments.test_module.as_deref(), Some("test_counter"));
        assert_eq!(
            arguments.python.as_deref(),
            Some(Path::new("/usr/bin/python3"))
        );
        assert_eq!(
            arguments.results_file.as_deref(),
            Some(Path::new("build/results.xml"))
        );
    }

    #[test]
    fn stale_results_are_removed_before_a_run() {
        let temporary = tempfile::tempdir().unwrap();
        let results = temporary.path().join("results.xml");
        std::fs::write(&results, "stale passing results").unwrap();

        remove_stale_results(&results).unwrap();
        assert!(!results.exists());
        remove_stale_results(&results).unwrap();
    }

    #[test]
    fn gpi_users_loads_libpython_before_pygpi() {
        let value = gpi_users_value(
            Path::new("/opt/python/lib/libpython3.14.so"),
            "/opt/python/lib/python3.14/site-packages/cocotb/libs/libpygpi.so:pygpi_entry_point",
        );

        assert_eq!(
            value,
            "/opt/python/lib/libpython3.14.so;/opt/python/lib/python3.14/site-packages/cocotb/libs/libpygpi.so:pygpi_entry_point"
        );
    }
}
