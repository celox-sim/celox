//! Command-line runner shared by the optional independent simulators.

use crate::{
    Backend, BigUint, CompilationRejected, Design, Expectation, Frontend, Result, SignalPath,
    TestCase,
};
use clap::Parser;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[derive(Parser)]
#[command(about = "Check a suite's assertions against an independent simulator")]
struct Args {
    #[arg(long, default_value = "")]
    filter: String,
    #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u16).range(1..))]
    jobs: u16,
    /// Build artifacts and complete per-case logs.
    #[arg(long)]
    output: Option<PathBuf>,
    /// Also retain a portable, machine-readable report at this path.
    #[arg(long)]
    report: Option<PathBuf>,
    /// Execute known discrepancies and toolchain limitations too.
    #[arg(long)]
    include_ignored: bool,
    /// Exclude reviewed expectations that go beyond portable SV requirements.
    #[arg(long)]
    exclude_stronger_than_sv: bool,
    /// Print the selected catalogue as JSON without invoking any tools.
    #[arg(long)]
    list: bool,
    /// Reuse unchanged successful cases from --report, or output/results.json.
    /// Failed, new, and changed cases always run again.
    #[arg(long, conflicts_with = "list")]
    incremental: bool,
}

/// A suite to verify: its cases and the reviewed tool exclusions.
pub struct Suite {
    /// A short name for default output directories, such as `veryl`.
    pub name: &'static str,
    pub cases: Vec<&'static TestCase>,
    /// The reviewed exclusion of a case for a tool, as a JSON object with at
    /// least `reason` and `observed_version`.
    pub known_issue: fn(tool: &str, case: &str) -> Option<Value>,
}

/// An independent simulator with an adapter in this crate.
#[derive(Clone, Copy, Debug)]
pub enum Tool {
    Verilator,
    Icarus,
}

impl Tool {
    pub fn name(self) -> &'static str {
        match self {
            Tool::Verilator => "verilator",
            Tool::Icarus => "icarus",
        }
    }

    fn version_command(self) -> (&'static str, &'static str) {
        match self {
            Tool::Verilator => ("verilator", "--version"),
            Tool::Icarus => ("iverilog", "-V"),
        }
    }

    /// Whether the tool can validate four-state expectations.
    fn four_state(self) -> bool {
        matches!(self, Tool::Icarus)
    }
}

fn selected_cases(args: &Args, suite: &Suite) -> Vec<&'static TestCase> {
    suite
        .cases
        .iter()
        .copied()
        .filter(|case| case.name.contains(&args.filter))
        .filter(|case| !args.exclude_stronger_than_sv || !case.has_stronger_than_sv_expectations())
        .collect()
}

/// The catalogue fields of a case, as reported for every result.
pub fn case_metadata(case: &TestCase) -> Value {
    json!({
        "name": case.name,
        "category": format!("{:?}", case.category),
        "expectation": format!("{:?}", case.expectation),
        "stronger_than_sv": case.has_stronger_than_sv_expectations(),
        "tags": case.tags.iter().map(|tag| tag.as_str()).collect::<Vec<_>>(),
        "tag_reasons": case.tags.iter().map(|tag| (tag.as_str().to_owned(), json!(tag.reason()))).collect::<serde_json::Map<_, _>>(),
    })
}

pub fn panic_message(error: &(dyn std::any::Any + Send)) -> String {
    error
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| error.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_else(|| "non-string panic".into())
}

#[derive(Debug)]
pub struct EmissionError(pub String);
impl std::fmt::Display for EmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for EmissionError {}

/// Entry point for simulator-specific binaries. A report never labels an
/// unexecuted assertion as a mismatch or as a pass.
pub fn run(tool: Tool, suite: &Suite, frontend: &'static dyn Frontend) -> Result<()> {
    let tool_kind = tool;
    let (version_command, version_flag) = tool.version_command();
    let four_state = tool.four_state();
    let build_design = move |design: &Design, directory: &Path| -> Result<Box<dyn Backend>> {
        Ok(match tool {
            Tool::Verilator => Box::new(crate::verilator::Verilator::build(
                frontend, design, directory,
            )?),
            Tool::Icarus => Box::new(crate::icarus::Icarus::build(frontend, design, directory)?),
        })
    };
    let build_script = move |case: &TestCase, directory: &Path| -> Result<Box<dyn Backend>> {
        Ok(match tool {
            Tool::Verilator => Box::new(crate::verilator::Verilator::build_script(
                frontend, case, directory,
            )?),
            Tool::Icarus => Box::new(crate::icarus::Icarus::build_script(
                frontend, case, directory,
            )?),
        })
    };
    let builders = Builders {
        design: &build_design,
        script: &build_script,
    };
    let tool = tool.name();
    let args = Args::parse();
    let tests = selected_cases(&args, suite);
    if tests.is_empty() {
        return Err("no cases matched the selection".into());
    }
    if args.list {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "schema_version": 3,
                "suite_version": env!("CARGO_PKG_VERSION"),
                "exclude_stronger_than_sv": args.exclude_stronger_than_sv,
                "cases": tests.iter().map(|case| case_metadata(case)).collect::<Vec<_>>(),
            }))?
        );
        return Ok(());
    }
    let output = args
        .output
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!("target/{}-{tool}", suite.name)));
    std::fs::create_dir_all(&output)?;
    let version = std::process::Command::new(version_command)
        .arg(version_flag)
        .output()?;
    if !version.status.success() {
        return Err(format!("{version_command} {version_flag} failed").into());
    }
    let version = String::from_utf8_lossy(&version.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .to_owned();
    let context = context_fingerprint(tool_kind, suite.name, &version, args.include_ignored);
    if args.incremental && context.is_none() {
        println!(
            "incremental reuse unavailable: incomplete verifier/tool fingerprint; verifying selected cases afresh"
        );
    }
    let baseline_path = args
        .report
        .as_deref()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| output.join("results.json"));
    let previous = if args.incremental {
        read_previous(&baseline_path, context.as_deref())?
    } else {
        BTreeMap::new()
    };
    let next = AtomicUsize::new(0);
    let reused = AtomicUsize::new(0);
    let results = Mutex::new(Vec::new());
    // Compiler and assertion panics are captured, with full diagnostics on disk.
    std::panic::set_hook(Box::new(|_| {}));
    std::thread::scope(|scope| {
        for _ in 0..args.jobs {
            scope.spawn(|| {
                while let Some(case) = tests.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let issue = (suite.known_issue)(tool, case.name);
                    let fingerprint =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            case_fingerprint(case, &issue)
                        }))
                        .ok();
                    if let Some(cached) = previous
                        .get(case.name)
                        .filter(|row| fingerprint.as_deref().is_some_and(|key| reusable(row, key)))
                    {
                        let mut row = cached.clone();
                        row["reused"] = json!(true);
                        reused.fetch_add(1, Ordering::Relaxed);
                        println!("reused {}: {}", row["status"].as_str().unwrap(), case.name);
                        results.lock().unwrap().push(row);
                        continue;
                    }
                    let skip = if args.include_ignored {
                        None
                    } else {
                        issue.clone()
                    };
                    let mut row = run_case(case, &output, four_state, &builders, skip, issue);
                    row["case_fingerprint"] = json!(fingerprint);
                    row["reused"] = json!(false);
                    row["verified_at_unix"] = json!(
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs()
                    );
                    results.lock().unwrap().push(row);
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap();
    results.sort_by_key(|row| row["name"].as_str().unwrap().to_owned());
    let counts: serde_json::Map<String, Value> = [
        "passed",
        "rejected",
        "unexpected_accept",
        "mismatch",
        "emission_error",
        "compile_error",
        "runtime_error",
        "unsupported",
        "ignored",
    ]
    .into_iter()
    .map(|status| {
        (
            status.to_owned(),
            json!(results.iter().filter(|row| row["status"] == status).count()),
        )
    })
    .collect();
    let failures = results
        .iter()
        .filter(|row| is_failure(row["status"].as_str().unwrap()))
        .count();
    let reused = reused.into_inner();
    let report = json!({"schema_version": 3, "suite": suite.name, "suite_version": env!("CARGO_PKG_VERSION"), "tool": tool, "version": version, "include_ignored": args.include_ignored, "exclude_stronger_than_sv": args.exclude_stronger_than_sv, "context_fingerprint": context, "incremental": args.incremental, "run_counts": {"fresh": results.len() - reused, "reused": reused}, "counts": counts, "cases": results});
    let contents = serde_json::to_string_pretty(&report)? + "\n";
    std::fs::write(output.join("results.json"), &contents)?;
    if let Some(path) = args.report {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, &contents)?;
    }
    println!(
        "{}; {} fresh, {} reused; report: {}",
        report["counts"],
        report["run_counts"]["fresh"],
        report["run_counts"]["reused"],
        output.join("results.json").display()
    );
    if failures != 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn fingerprint(parts: &[&[u8]]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    let mut digest = String::with_capacity(64);
    for byte in hash.finalize() {
        write!(&mut digest, "{byte:02x}").unwrap();
    }
    digest
}

fn case_fingerprint(case: &TestCase, issue: &Option<Value>) -> String {
    // The AST includes stimulus/assertions and options; design() also resolves
    // stdlib source parts, which may change independently of the .vtest file.
    let mut script = case.script().clone();
    script.pos.line = 0;
    script.pos.column = 0;
    for stmt in &mut script.body {
        clear_positions(stmt);
    }
    fingerprint(&[
        format!("{script:?}").as_bytes(),
        format!("{:?}", case.design()).as_bytes(),
        serde_json::to_string(issue).unwrap().as_bytes(),
    ])
}

fn clear_positions(stmt: &mut crate::script::ast::Stmt) {
    use crate::script::ast::StmtKind;
    stmt.pos.line = 0;
    stmt.pos.column = 0;
    match &mut stmt.kind {
        StmtKind::Modify(body) | StmtKind::Block(body) | StmtKind::For(_, _, body) => {
            for stmt in body {
                clear_positions(stmt);
            }
        }
        StmtKind::If(_, yes, no) => {
            clear_positions(yes);
            if let Some(no) = no {
                clear_positions(no);
            }
        }
        _ => {}
    }
}

#[allow(
    clippy::disallowed_methods,
    reason = "External-tool identity is read at the CLI boundary, not as simulator diagnostics"
)]
fn context_fingerprint(
    tool: Tool,
    suite: &str,
    version: &str,
    include_ignored: bool,
) -> Option<String> {
    let build = env!("CELOX_VERIFICATION_BUILD_HASH");
    if build == "unavailable" {
        return None;
    }
    let tools: &[&str] = match tool {
        Tool::Verilator => &["verilator", "verilator_bin", "make", "c++", "timeout"],
        Tool::Icarus => &["iverilog", "vvp", "iverilog-vpi", "c++", "timeout"],
    };
    let path = std::env::var_os("PATH")?;
    let mut identities = Vec::new();
    for name in tools {
        let executable = std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())?;
        let bytes = std::fs::read(executable).ok()?;
        identities.push(fingerprint(&[name.as_bytes(), &bytes]));
    }
    let compiler = std::process::Command::new("c++")
        .arg("--version")
        .output()
        .ok()?;
    if !compiler.status.success() {
        return None;
    }
    let environment: Vec<_> = [
        "CC",
        "CXX",
        "CFLAGS",
        "CPPFLAGS",
        "CXXFLAGS",
        "LDFLAGS",
        "MAKEFLAGS",
        "VERILATOR_ROOT",
        "IVERILOG_ICONFIG",
        "NIX_CFLAGS_COMPILE",
        "NIX_CXXSTDLIB_COMPILE",
        "NIX_LDFLAGS",
        "CPATH",
        "CPLUS_INCLUDE_PATH",
        "LIBRARY_PATH",
    ]
    .iter()
    .map(|name| (name, std::env::var_os(name)))
    .collect();
    Some(fingerprint(&[
        build.as_bytes(),
        suite.as_bytes(),
        tool.name().as_bytes(),
        version.as_bytes(),
        &compiler.stdout,
        &compiler.stderr,
        identities.join("\n").as_bytes(),
        format!("{environment:?}").as_bytes(),
        &[u8::from(include_ignored)],
    ]))
}

fn read_previous(path: &Path, context: Option<&str>) -> Result<BTreeMap<String, Value>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(error.into()),
    };
    let report: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid incremental report {}: {error}", path.display()))?;
    if report["schema_version"] != 3
        || context.is_none()
        || report["context_fingerprint"].as_str() != context
    {
        println!("incremental baseline is incompatible; verifying selected cases afresh");
        return Ok(BTreeMap::new());
    }
    let rows = report["cases"]
        .as_array()
        .ok_or("incremental report has no cases array")?;
    let mut previous = BTreeMap::new();
    for row in rows {
        let name = row["name"].as_str().ok_or("incremental case has no name")?;
        if previous.insert(name.to_owned(), row.clone()).is_some() {
            return Err(format!("duplicate incremental case: {name}").into());
        }
    }
    Ok(previous)
}

fn reusable(row: &Value, fingerprint: &str) -> bool {
    row["case_fingerprint"].as_str() == Some(fingerprint)
        && matches!(row["status"].as_str(), Some("passed" | "rejected"))
}

/// Whether a result status counts as a failure of the run.
pub fn is_failure(status: &str) -> bool {
    !matches!(status, "passed" | "rejected" | "unsupported" | "ignored")
}

// Test assertions may use arbitrary panic text. Distinguish those from backend
// I/O failures without guessing from Rust's panic-message formatting.
struct ObservedBackend {
    backend: Box<dyn Backend>,
    failed: Arc<AtomicBool>,
}
impl Backend for ObservedBackend {
    fn run_testbench(&mut self) -> Result<()> {
        self.backend
            .run_testbench()
            .inspect_err(|_| self.failed.store(true, Ordering::Relaxed))
    }
    fn write(&mut self, signal: &SignalPath, payload: BigUint, mask: BigUint) -> Result<()> {
        self.backend
            .write(signal, payload, mask)
            .inspect_err(|_| self.failed.store(true, Ordering::Relaxed))
    }
    fn read(&mut self, signal: &SignalPath) -> Result<(BigUint, BigUint)> {
        self.backend
            .read(signal)
            .inspect_err(|_| self.failed.store(true, Ordering::Relaxed))
    }
    fn eval_comb(&mut self) -> Result<()> {
        self.backend
            .eval_comb()
            .inspect_err(|_| self.failed.store(true, Ordering::Relaxed))
    }
    fn tick(&mut self, event: &str) -> Result<()> {
        self.backend
            .tick(event)
            .inspect_err(|_| self.failed.store(true, Ordering::Relaxed))
    }
    fn take_output(&mut self) -> Result<String> {
        self.backend
            .take_output()
            .inspect_err(|_| self.failed.store(true, Ordering::Relaxed))
    }
}

/// How a runner builds a case for a tool: a design for the process
/// adapters, and a script case as a generated testbench.
pub struct Builders<'a> {
    pub design: &'a (dyn Fn(&Design, &Path) -> Result<Box<dyn Backend>> + Sync),
    pub script: &'a (dyn Fn(&TestCase, &Path) -> Result<Box<dyn Backend>> + Sync),
}

/// Run a script case through its generated testbench. Returns the status,
/// phase and detail.
fn run_script(
    case: &TestCase,
    directory: &Path,
    four_state: bool,
    build: &(dyn Fn(&TestCase, &Path) -> Result<Box<dyn Backend>> + Sync),
) -> (&'static str, &'static str, String) {
    if case.script().four_state && !four_state {
        return ("unsupported", "compile", "four-state expectations".into());
    }
    let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| build(case, directory)));
    let mut backend = match built {
        Err(panic) => return ("compile_error", "compile", panic_message(panic.as_ref())),
        Ok(Err(error)) => {
            let detail = error.to_string();
            return if error.is::<CompilationRejected>() {
                if case.expectation == Expectation::CompilationError {
                    ("rejected", "compile", detail)
                } else {
                    ("compile_error", "compile", detail)
                }
            } else if error.is::<EmissionError>() {
                ("emission_error", "emission", detail)
            } else if error.is::<crate::script::sv::Unsupported>() {
                ("unsupported", "compile", detail)
            } else {
                ("compile_error", "compile", detail)
            };
        }
        Ok(Ok(backend)) => backend,
    };
    if case.expectation == Expectation::CompilationError {
        return (
            "unexpected_accept",
            "execute",
            format!("invalid design was accepted: {}", case.name),
        );
    }
    match backend.run_testbench() {
        Ok(()) => {
            let log = std::fs::read(directory.join("protocol.log")).unwrap_or_default();
            match crate::script::sv::check_output(&String::from_utf8_lossy(&log)) {
                Ok(()) => ("passed", "execute", String::new()),
                Err(detail) => ("mismatch", "execute", detail),
            }
        }
        Err(error) => {
            let log = std::fs::read_to_string(directory.join("protocol.log")).unwrap_or_default();
            let assertions: Vec<&str> = log
                .lines()
                .filter_map(|line| line.trim_start().strip_prefix("@suite assert "))
                .collect();
            if assertions.is_empty() {
                ("runtime_error", "execute", error.to_string())
            } else {
                ("mismatch", "execute", assertions.join("\n"))
            }
        }
    }
}

/// Run one case with `tool` and write its result row under `output`. A
/// case with a `skip` issue is reported as ignored without building it; a
/// `known_issue` is attached to the row of a case that does run.
pub fn run_case(
    case: &TestCase,
    output: &Path,
    four_state: bool,
    builders: &Builders<'_>,
    skip: Option<Value>,
    known_issue: Option<Value>,
) -> Value {
    let directory = output.join(case.name.replace("::", "/"));
    std::fs::create_dir_all(&directory).unwrap();
    if let Some(issue) = &skip {
        let mut row = case_metadata(case);
        row["status"] = json!("ignored");
        row["phase"] = json!("skip");
        row["detail"] = issue["reason"].clone();
        row["known_issue"] = issue.clone();
        std::fs::write(
            directory.join("error.log"),
            issue["reason"].as_str().unwrap(),
        )
        .unwrap();
        return write_result(&directory, row);
    }
    let script = case.script();
    if !crate::script::sv::is_native_testbench(script) {
        let (status, phase, detail) = run_script(case, &directory, four_state, builders.script);
        return finish(case, &directory, status, phase, detail, known_issue);
    }
    let adapter_failed = Arc::new(AtomicBool::new(false));
    let mut phase = "setup";
    let mut unsupported = false;
    let mut design_index = 0;
    let mut build_error = None;
    let mut rejected = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        case.run(&mut |design| {
            phase = "compile";
            if design.four_state && !four_state {
                unsupported = true;
                return Err("four-state expectations cannot be validated by this simulator".into());
            }
            let build_directory = if design_index == 0 {
                directory.clone()
            } else {
                directory.join(format!("design_{design_index}"))
            };
            design_index += 1;
            match (builders.design)(design, &build_directory) {
                Ok(backend) => {
                    phase = "execute";
                    Ok(Box::new(ObservedBackend {
                        backend,
                        failed: adapter_failed.clone(),
                    }) as Box<dyn Backend>)
                }
                Err(error) => {
                    rejected = error.is::<CompilationRejected>();
                    build_error = Some(error.to_string());
                    if error.is::<EmissionError>() {
                        phase = "emission";
                    }
                    Err(error)
                }
            }
        });
    }));
    let (status, detail) = match result {
        _ if unsupported => ("unsupported", "four-state expectations".to_owned()),
        Ok(()) if case.expectation == Expectation::CompilationError => {
            let status = if rejected {
                "rejected"
            } else if phase == "emission" {
                "emission_error"
            } else {
                "compile_error"
            };
            (
                status,
                build_error.unwrap_or_else(|| "missing compilation diagnostic".into()),
            )
        }
        Ok(()) => ("passed", String::new()),
        Err(error) => {
            let detail = panic_message(error.as_ref());
            let status = match phase {
                "emission" => "emission_error",
                "compile" => "compile_error",
                "execute" if case.expectation == Expectation::CompilationError => {
                    "unexpected_accept"
                }
                "execute" if !adapter_failed.load(Ordering::Relaxed) => "mismatch",
                _ => "runtime_error",
            };
            (status, detail)
        }
    };
    finish(case, &directory, status, phase, detail, known_issue)
}

fn finish(
    case: &crate::TestCase,
    directory: &Path,
    status: &str,
    phase: &str,
    detail: String,
    known_issue: Option<Value>,
) -> Value {
    std::fs::write(directory.join("error.log"), &detail).unwrap();
    let mut portable_detail = detail;
    if status == "compile_error" || status == "rejected" || status == "runtime_error" {
        // Keep actual compiler diagnostics in the retained report.
        if let Ok(log) = std::fs::read_to_string(directory.join(
            if status == "compile_error" || status == "rejected" {
                "build.log"
            } else {
                "runtime.log"
            },
        )) {
            portable_detail.push('\n');
            portable_detail.extend(log.chars().take(6000));
        }
    }
    // Reports do not depend on a developer's absolute checkout/cache path.
    if let Ok(absolute) = std::fs::canonicalize(directory) {
        portable_detail = portable_detail.replace(absolute.to_string_lossy().as_ref(), "<case>");
    }
    portable_detail = portable_detail.chars().take(8000).collect();
    let mut row = case_metadata(case);
    row["status"] = json!(status);
    row["phase"] = json!(phase);
    row["detail"] = json!(portable_detail);
    if let Some(issue) = known_issue {
        row["known_issue"] = issue;
    }
    write_result(directory, row)
}

fn write_result(directory: &Path, row: Value) -> Value {
    std::fs::write(
        directory.join("result.json"),
        serde_json::to_string_pretty(&row).unwrap(),
    )
    .unwrap();
    let status = row["status"].as_str().unwrap();
    let name = row["name"].as_str().unwrap();
    if status == "ignored" {
        println!("{status}: {name}: {}", row["detail"].as_str().unwrap());
    } else {
        println!("{status}: {name}");
    }
    row
}

#[cfg(test)]
mod incremental_tests {
    use super::*;

    fn case(text: &str, resolver: crate::StdResolver) -> &'static TestCase {
        crate::load_group("incremental.vtest", text, resolver)[0]
    }

    const SCRIPT: &str = r#"(group incremental (category combinational))
        (case basic (source "t.veryl" (std "fixture")) (top Top)
          (modify (set a 1)) (for i (range 0 2) (do (assert_eq o (+ i 1)))))"#;

    #[test]
    fn fingerprints_ignore_case_positions_but_cover_source_stimulus_assertions_and_exclusions() {
        fn old(_: &str) -> String {
            "module Top {}".into()
        }
        fn new(_: &str) -> String {
            "module Top { var a: logic; }".into()
        }
        let original = case_fingerprint(case(SCRIPT, old), &None);
        assert_eq!(
            original,
            case_fingerprint(case(&format!("\n\n; comment\n{SCRIPT}"), old), &None)
        );
        assert_ne!(original, case_fingerprint(case(SCRIPT, new), &None));
        for changed in [
            SCRIPT.replace("set a 1", "set a 2"),
            SCRIPT.replace("(+ i 1)", "(+ i 2)"),
            SCRIPT.replace("(top Top)", "(top Other)"),
        ] {
            assert_ne!(original, case_fingerprint(case(&changed, old), &None));
        }
        assert_ne!(
            original,
            case_fingerprint(case(SCRIPT, old), &Some(json!({"reason": "unsupported"})))
        );
    }

    #[test]
    fn only_unchanged_successes_are_reusable() {
        for status in [
            "passed",
            "rejected",
            "mismatch",
            "emission_error",
            "compile_error",
            "runtime_error",
            "unexpected_accept",
            "ignored",
            "unsupported",
        ] {
            let row = json!({"status": status, "case_fingerprint": "current"});
            assert_eq!(
                reusable(&row, "current"),
                matches!(status, "passed" | "rejected")
            );
            assert!(!reusable(&row, "changed"));
        }
        assert!(!reusable(&json!({"status": "passed"}), "current"));
    }

    #[test]
    fn baseline_checks_context_duplicates_and_corruption() {
        let directory =
            std::env::temp_dir().join(format!("celox-incremental-baseline-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("report.json");
        assert!(read_previous(&path, Some("current")).unwrap().is_empty());
        let row = json!({"name": "case", "status": "passed", "case_fingerprint": "case-key"});
        let mut report =
            json!({"schema_version": 3, "context_fingerprint": "current", "cases": [row]});
        std::fs::write(&path, report.to_string()).unwrap();
        assert_eq!(read_previous(&path, Some("current")).unwrap().len(), 1);
        assert!(
            read_previous(&path, Some("changed-tool-or-verifier"))
                .unwrap()
                .is_empty()
        );
        assert!(read_previous(&path, None).unwrap().is_empty());
        report["cases"] = json!([row, row]);
        std::fs::write(&path, report.to_string()).unwrap();
        assert!(read_previous(&path, Some("current")).is_err());
        std::fs::write(&path, "broken JSON").unwrap();
        assert!(read_previous(&path, Some("current")).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
