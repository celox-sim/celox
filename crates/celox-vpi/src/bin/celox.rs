#![allow(clippy::disallowed_methods)] // This binary is the process environment boundary.

use std::{
    env,
    ffi::OsStr,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use celox::{NativeProgramImage, NativeProgramInstance};
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

fn run() -> Result<(), String> {
    match NativeProgramImage::discover_in_current_executable()
        .map_err(|error| format!("failed to inspect the celox executable: {error}"))?
    {
        Some(attached) => run_simulation(attached.image, SimulationArgs::parse()),
        None => match Cli::parse().command {
            CliCommand::Vpi(arguments) => match arguments.command {
                VpiCommand::Build(arguments) => build(arguments),
            },
        },
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
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
    fn vpi_build_arguments_have_a_default_output() {
        let cli = Cli::try_parse_from(["celox", "vpi", "build", "top.veryl", "--top", "Top"])
            .expect("valid VPI build command");
        let CliCommand::Vpi(arguments) = cli.command;
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
}
