//! Runs the elaboration tests of the sv-tests suite
//! (<https://github.com/chipsalliance/sv-tests>) against the SystemVerilog
//! frontend and compares the results with the retained expectations.
//!
//! ```text
//! cargo run --release -p celox --features systemverilog --example sv_tests -- \
//!     <sv-tests>/tests --expected conformance/sv-tests/expected.tsv [--update] [--report report.tsv]
//! ```
//!
//! A test passes when the build of its top module succeeds, or fails if the
//! test is marked `:should_fail_because:`. Each test is built in a child
//! process so that a crash or a hang only fails that test. The run fails when
//! any result differs from `--expected`; `--update` rewrites that file instead.

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

    fn types(&self) -> Vec<&str> {
        self.meta
            .get("type")
            .map_or("parsing elaboration", String::as_str)
            .split_whitespace()
            .collect()
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

/// Builds one test in this process; the exit status is 0 on success.
fn build_one(files: &[PathBuf], top: &str) -> ExitCode {
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
    match celox::Simulator::from_sv_sources(sources, top).build() {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            println!("{error}");
            ExitCode::from(1)
        }
    }
}

enum Outcome {
    Built,
    Rejected(String),
    Crashed(String),
    Timeout,
}

fn run_child(files: &[PathBuf], top: &str) -> Outcome {
    let mut child = Command::new(env::current_exe().expect("locate this executable"))
        .arg("--one")
        .arg(top)
        .args(files)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the test child");
    let start = Instant::now();
    while child.try_wait().expect("wait for the test child").is_none() {
        if start.elapsed() > TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Outcome::Timeout;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let output = child.wait_with_output().expect("collect the test output");
    match output.status.code() {
        Some(0) => Outcome::Built,
        Some(1) => Outcome::Rejected(String::from_utf8_lossy(&output.stdout).into_owned()),
        _ => Outcome::Crashed(format!(
            "{}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )),
    }
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
    if args.first().map(String::as_str) == Some("--one") && args.len() >= 3 {
        let files: Vec<PathBuf> = args[2..].iter().map(PathBuf::from).collect();
        return build_one(&files, &args[1]);
    }
    let Some(options) = parse_options(&args) else {
        eprintln!("usage: sv_tests <sv-tests>/tests [--expected FILE [--update]] [--report FILE]");
        return ExitCode::from(2);
    };
    let mut results = BTreeMap::new();
    let mut report = String::from("test\ttags\ttop\tshould_fail\tresult\tdetail\n");
    for path in test_paths(&options.tests) {
        let Some(test) = Test::read(path) else {
            continue;
        };
        if !test.types().contains(&"elaboration") {
            continue;
        }
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
            Some(top) => match run_child(&files, top) {
                Outcome::Built if should_fail => ("fail", "accepted".to_string()),
                Outcome::Built => ("pass", String::new()),
                Outcome::Rejected(error) if should_fail => ("pass", summary(&error)),
                Outcome::Rejected(error) => ("fail", summary(&error)),
                Outcome::Crashed(error) => ("crash", summary(&error)),
                Outcome::Timeout => ("timeout", String::new()),
            },
        };
        writeln!(
            report,
            "{name}\t{}\t{}\t{should_fail}\t{result}\t{detail}",
            test.meta.get("tags").map_or("", String::as_str),
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
