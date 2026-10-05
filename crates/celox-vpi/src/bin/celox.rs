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
use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "celox",
    version = env!("CELOX_VERSION"),
    about = "Compile and run Veryl designs with Celox"
)]
struct Cli {
    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Subcommand)]
enum CliCommand {
    /// Build native executables for VPI testbenches.
    Vpi(VpiArgs),
    /// Inspect saved simulation state files.
    State(StateArgs),
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
    /// Compile a Veryl design into a self-contained native simulation executable.
    Build(BuildArgs),
}

#[derive(Args)]
struct BuildArgs {
    /// Veryl source file belonging to the design project.
    source: PathBuf,

    /// Top-level Veryl module name.
    #[arg(long)]
    top: String,

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

    celox_vpi::driver::compile_native_image(&arguments.source, &arguments.top)?
        .write_attached_runtime(runtime, &arguments.output)
        .map_err(|error| format!("failed to write {}: {error}", arguments.output.display()))?;
    println!("Created {}", arguments.output.display());
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
        .args([OsStr::new("-m"), OsStr::new("cocotb_tools.check_results")])
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
        None => match Cli::parse().command {
            CliCommand::Vpi(arguments) => match arguments.command {
                VpiCommand::Build(arguments) => build(arguments).map(|()| ExitCode::SUCCESS),
            },
            CliCommand::State(arguments) => run_state(arguments),
        },
    }
}

fn main() -> ExitCode {
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
