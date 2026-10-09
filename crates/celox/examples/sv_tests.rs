//! Runs the elaboration and simulation tests of the sv-tests suite
//! (<https://github.com/chipsalliance/sv-tests>) against the SystemVerilog
//! frontend and compares the results with the retained expectations.
//!
//! ```text
//! cargo run --release -p celox --features systemverilog --example sv_tests -- \
//!     <sv-tests>/tests --expected conformance/sv-tests/expected.tsv [--update] [--report report.tsv]
//! ```
//!
//! As in sv-tests, a test passes when Celox succeeds, or fails if the test is
//! marked `:should_fail_because:`. An elaboration test succeeds when its top
//! module builds. A simulation test also runs until `$finish` or until no event
//! is left; it succeeds when the run ends without a runtime error or `$fatal`,
//! and passes only if every `:assert: <python expression>` line it prints
//! evaluates true (with `python3`, as the sv-tests log parser does). Each test
//! runs in a child process so that a crash or a hang only fails that test. The
//! run fails when any result differs from `--expected`; `--update` rewrites that
//! file instead.

#![allow(clippy::disallowed_methods)] // This binary is the process environment boundary.
#![allow(clippy::disallowed_macros)] // Progress and differences intentionally use stderr.

use std::{
    collections::BTreeMap,
    env,
    fmt::Write as _,
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Elaboration,
    Simulation,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Elaboration => "elaboration",
            Mode::Simulation => "simulation",
        }
    }
}

struct Test {
    path: PathBuf,
    meta: BTreeMap<String, String>,
}

impl Test {
    fn read(path: PathBuf) -> Option<Self> {
        let text = std::fs::read_to_string(&path).ok()?;
        let mut meta = BTreeMap::new();
        for line in text.lines() {
            if let Some(rest) = line.trim().strip_prefix(':')
                && let Some((key, value)) = rest.split_once(':')
                && !key.contains(char::is_whitespace)
            {
                meta.insert(key.to_string(), value.trim().to_string());
            }
        }
        Some(Self { path, meta })
    }

    /// The mode sv-tests would choose for a simulator: simulation when the
    /// test supports it, else elaboration. `None` for preprocessing and parsing
    /// tests.
    fn mode(&self) -> Option<Mode> {
        let types: Vec<&str> = self
            .meta
            .get("type")
            .map_or("parsing elaboration", String::as_str)
            .split_whitespace()
            .collect();
        if types.contains(&"simulation") {
            Some(Mode::Simulation)
        } else if types.contains(&"elaboration") {
            Some(Mode::Elaboration)
        } else {
            None
        }
    }

    fn should_fail(&self) -> bool {
        self.meta.contains_key("should_fail_because")
            || self.meta.get("should_fail").is_some_and(|v| v == "1")
    }

    /// The source files; `:files:` paths are relative to the sv-tests root.
    fn files(&self, tests: &Path) -> Vec<PathBuf> {
        match self.meta.get("files") {
            Some(files) => files
                .split_whitespace()
                .map(|file| tests.join("..").join(file))
                .collect(),
            None => vec![self.path.clone()],
        }
    }
}

fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
}

/// The top module: the declared one, else `top`, else the one module whose
/// name appears nowhere but in its declaration, else the last module. `None`
/// when the sources declare no module.
fn top_module(test: &Test, files: &[PathBuf], texts: &[String]) -> Option<String> {
    if let Some(top) = test.meta.get("top_module").filter(|top| !top.is_empty()) {
        return Some(top.clone());
    }
    let mut names = Vec::new();
    for (text, path) in texts.iter().zip(files) {
        match celox_sv_analyzer::source_module_names(text, path) {
            Ok(found) => names.extend(found),
            // The build reports the parse error.
            Err(_) => return Some("top".to_string()),
        }
    }
    if names.iter().any(|name| name == "top") {
        return Some("top".to_string());
    }
    let mentions = |name: &str| {
        texts
            .iter()
            .flat_map(|text| words(text))
            .filter(|word| *word == name)
            .count()
    };
    let roots: Vec<_> = names.iter().filter(|name| mentions(name) == 1).collect();
    match roots.as_slice() {
        [root] => Some((*root).clone()),
        _ => names.last().cloned(),
    }
}

/// Runs one test in this process. The exit status is 0 on success, 1 when the
/// build fails and 3 when the simulation fails; a simulation prints its
/// `$display` output to stdout.
fn run_one(mode: &str, top: &str, files: &[PathBuf]) -> ExitCode {
    let texts: Vec<String> = match files.iter().map(std::fs::read_to_string).collect() {
        Ok(texts) => texts,
        Err(error) => {
            println!("io: {error}");
            return ExitCode::from(2);
        }
    };
    let sources = texts
        .iter()
        .zip(files)
        .map(|(text, path)| (text.as_str(), path.as_path()))
        .collect();
    if mode == Mode::Elaboration.name() {
        return match celox::Simulator::from_sv_sources(sources, top).build() {
            Ok(_) => ExitCode::SUCCESS,
            Err(error) => {
                println!("{error}");
                ExitCode::from(1)
            }
        };
    }
    let simulation = match celox::Simulation::from_sv_sources(sources, top).build() {
        Ok(simulation) => simulation,
        Err(error) => {
            println!("{error}");
            return ExitCode::from(1);
        }
    };
    match simulate(simulation) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(3)
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
                celox::RuntimeEvent::Display { message }
                | celox::RuntimeEvent::AssertContinue { message } => {
                    writeln!(stdout, "{message}").unwrap();
                }
                celox::RuntimeEvent::Write { message } => write!(stdout, "{message}").unwrap(),
                celox::RuntimeEvent::AssertFatal { message } => {
                    writeln!(stdout, "{message}").unwrap();
                    return Err(format!("$fatal: {message}"));
                }
                celox::RuntimeEvent::Finish => {}
                celox::RuntimeEvent::Missed { count } => {
                    return Err(format!("{count} runtime events were lost"));
                }
            }
        }
        Ok(())
    };
    sim.eval_comb().map_err(|error| error.to_string())?;
    output(&mut sim)?;
    while !sim.is_finished() {
        let step = sim.step().map_err(|error| error.to_string());
        output(&mut sim)?;
        if step?.is_none() {
            break;
        }
    }
    Ok(())
}

enum Outcome {
    /// The build, and a simulation, succeeded; the simulation output.
    Succeeded(String),
    Rejected(String),
    /// The simulation failed: its output and the error.
    SimulationFailed(String, String),
    Crashed(String),
    Timeout,
}

fn run_child(mode: Mode, files: &[PathBuf], top: &str) -> Outcome {
    let mut child = Command::new(env::current_exe().expect("locate this executable"))
        .arg("--one")
        .arg(mode.name())
        .arg(top)
        .args(files)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the test child");
    // Drain the pipes while waiting so that a chatty simulation cannot block.
    let stdout = read_pipe(child.stdout.take());
    let stderr = read_pipe(child.stderr.take());
    let start = Instant::now();
    while child.try_wait().expect("wait for the test child").is_none() {
        if start.elapsed() > TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Outcome::Timeout;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let status = child.wait().expect("wait for the test child");
    let stdout = stdout.join().expect("read the test output");
    let stderr = stderr.join().expect("read the test output");
    match status.code() {
        Some(0) => Outcome::Succeeded(stdout),
        Some(1) => Outcome::Rejected(stdout),
        Some(3) => Outcome::SimulationFailed(stdout, stderr),
        _ => Outcome::Crashed(format!("{status}: {stderr}")),
    }
}

fn read_pipe(pipe: Option<impl std::io::Read + Send + 'static>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

/// The expression of an `:assert:` line: as in the sv-tests log parser, the
/// first `:name:` marker of the line decides.
fn assertion(line: &str) -> Option<&str> {
    let line = line.trim();
    for (at, _) in line.match_indices(':') {
        let rest = &line[at + 1..];
        let name = rest.bytes().take_while(u8::is_ascii_lowercase).count();
        if name > 0 && rest[name..].starts_with(':') {
            return (&rest[..name] == "assert").then(|| &rest[name + 1..]);
        }
    }
    None
}

/// Evaluates each expression in Python, as sv-tests does.
const EVALUATE: &str = "\
import sys
for expression in sys.stdin.read().split('\\n'):
    try:
        print(int(bool(eval(expression))))
    except Exception:
        print(0)
";

/// The first `:assert:` expression in `output` that does not hold.
fn failed_assertion(output: &str) -> Option<String> {
    use std::io::Write as _;
    let expressions: Vec<&str> = output.lines().filter_map(assertion).collect();
    if expressions.is_empty() {
        return None;
    }
    let mut python = Command::new("python3")
        .args(["-I", "-c", EVALUATE])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("run python3 to evaluate the :assert: lines");
    python
        .stdin
        .take()
        .unwrap()
        .write_all(expressions.join("\n").as_bytes())
        .expect("pass the :assert: lines to python3");
    let output = python
        .wait_with_output()
        .expect("evaluate the :assert: lines");
    let results = String::from_utf8_lossy(&output.stdout).into_owned();
    let mut results = results.lines();
    expressions
        .iter()
        .find(|_| results.next() != Some("1"))
        .map(|expression| expression.trim().to_string())
}

/// The code and message of a rendered diagnostic, on one line.
fn summary(text: &str) -> String {
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let first = lines.next().unwrap_or("");
    let mut message = lines
        .by_ref()
        .find_map(|line| line.strip_prefix('×'))
        .map_or(String::new(), |line| line.trim().to_string());
    for line in lines.map_while(|line| line.strip_prefix('│')) {
        message.push(' ');
        message.push_str(line.trim());
    }
    format!("{first}: {message}").replace(['\t', '\n'], " ")
}

struct Options {
    tests: PathBuf,
    expected: Option<PathBuf>,
    update: bool,
    report: Option<PathBuf>,
}

fn parse_options(args: &[String]) -> Option<Options> {
    let mut options = Options {
        tests: PathBuf::new(),
        expected: None,
        update: false,
        report: None,
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--expected" => options.expected = Some(args.next()?.into()),
            "--report" => options.report = Some(args.next()?.into()),
            "--update" => options.update = true,
            _ if options.tests.as_os_str().is_empty() && !arg.starts_with("--") => {
                options.tests = arg.into();
            }
            _ => return None,
        }
    }
    (!options.tests.as_os_str().is_empty() && (!options.update || options.expected.is_some()))
        .then_some(options)
}

fn test_paths(tests: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut stack = vec![tests.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read the test directory") {
            let path = entry.expect("read the test directory").path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .is_some_and(|ext| ext == "sv" || ext == "v")
            {
                paths.push(path);
            }
        }
    }
    paths.sort();
    paths
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--one") && args.len() >= 4 {
        let files: Vec<PathBuf> = args[3..].iter().map(PathBuf::from).collect();
        return run_one(&args[1], &args[2], &files);
    }
    let Some(options) = parse_options(&args) else {
        eprintln!("usage: sv_tests <sv-tests>/tests [--expected FILE [--update]] [--report FILE]");
        return ExitCode::from(2);
    };
    let mut results = BTreeMap::new();
    let mut report = String::from("test\ttags\tmode\ttop\tshould_fail\tresult\tdetail\n");
    for path in test_paths(&options.tests) {
        let Some(test) = Test::read(path) else {
            continue;
        };
        let Some(mode) = test.mode() else {
            continue;
        };
        let name = test
            .path
            .strip_prefix(&options.tests)
            .unwrap_or(&test.path)
            .to_string_lossy()
            .replace('\\', "/");
        let files = test.files(&options.tests);
        let texts: Vec<String> = files
            .iter()
            .map(|file| std::fs::read_to_string(file).unwrap_or_default())
            .collect();
        let should_fail = test.should_fail();
        let top = top_module(&test, &files, &texts);
        let (result, detail) = match &top {
            None => ("no_top", String::new()),
            Some(top) => match run_child(mode, &files, top) {
                Outcome::Succeeded(_) if should_fail => ("fail", "accepted".to_string()),
                Outcome::Succeeded(output) => match failed_assertion(&output) {
                    Some(expression) => ("fail", format!("assert: {expression}")),
                    None => ("pass", String::new()),
                },
                Outcome::Rejected(error) if should_fail => ("pass", summary(&error)),
                Outcome::Rejected(error) => ("fail", summary(&error)),
                Outcome::SimulationFailed(output, error) => {
                    let error = format!("runtime: {}", error.trim()).replace(['\t', '\n'], " ");
                    match failed_assertion(&output) {
                        Some(expression) => ("fail", format!("assert: {expression}")),
                        None if should_fail => ("pass", error),
                        None => ("fail", error),
                    }
                }
                Outcome::Crashed(error) => ("crash", summary(&error)),
                Outcome::Timeout => ("timeout", String::new()),
            },
        };
        writeln!(
            report,
            "{name}\t{}\t{}\t{}\t{should_fail}\t{result}\t{detail}",
            test.meta.get("tags").map_or("", String::as_str),
            mode.name(),
            top.as_deref().unwrap_or(""),
        )
        .unwrap();
        results.insert(name, result);
    }
    if let Some(path) = &options.report {
        std::fs::write(path, &report).expect("write the report");
    }
    let mut counts = BTreeMap::<&str, usize>::new();
    for result in results.values() {
        *counts.entry(result).or_default() += 1;
    }
    eprintln!("{counts:?}");

    let Some(expected_path) = &options.expected else {
        return ExitCode::SUCCESS;
    };
    if options.update {
        let mut expected = String::from("test\tresult\n");
        for (name, result) in &results {
            writeln!(expected, "{name}\t{result}").unwrap();
        }
        std::fs::write(expected_path, expected).expect("write the expectations");
        return ExitCode::SUCCESS;
    }
    let text = std::fs::read_to_string(expected_path).expect("read the expectations");
    let expected: BTreeMap<&str, &str> = text
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once('\t'))
        .collect();
    let mut changes = Vec::new();
    for (name, result) in &results {
        match expected.get(name.as_str()) {
            Some(before) if before == result => {}
            Some(before) => changes.push(format!("{name}: {before} -> {result}")),
            None => changes.push(format!("{name}: new test, {result}")),
        }
    }
    for name in expected.keys().filter(|name| !results.contains_key(**name)) {
        changes.push(format!("{name}: missing"));
    }
    if changes.is_empty() {
        return ExitCode::SUCCESS;
    }
    eprintln!("results differ from {}:", expected_path.display());
    for change in &changes {
        eprintln!("  {change}");
    }
    eprintln!("rerun with --update to accept them after review");
    ExitCode::FAILURE
}
