//! Command-line runner shared by the optional independent simulators.
mod known_issues;

use crate::{Backend, BigUint, Design, Expectation, Result, SignalPath, cases};
use clap::Parser;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[derive(Parser)]
#[command(about = "Check the original Veryl suite assertions against an independent simulator")]
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
}

pub(crate) fn panic_message(error: &(dyn std::any::Any + Send)) -> String {
    error
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| error.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_else(|| "non-string panic".into())
}

#[derive(Debug)]
pub(crate) struct EmissionError(pub String);
impl std::fmt::Display for EmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for EmissionError {}

/// A language compiler rejected the HDL. Infrastructure errors, timeouts,
/// emitter panics, and C++ harness failures must not use this marker.
#[derive(Debug)]
pub(crate) struct CompilationRejected(pub String);
impl std::fmt::Display for CompilationRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for CompilationRejected {}

/// Entry point for simulator-specific binaries. A report never labels an
/// unexecuted assertion as a mismatch or as a pass.
pub fn run(
    tool: &str,
    version_command: &str,
    version_flag: &str,
    four_state: bool,
    build: fn(&Design, &Path) -> Result<Box<dyn Backend>>,
) -> Result<()> {
    let args = Args::parse();
    let output = args
        .output
        .unwrap_or_else(|| PathBuf::from(format!("target/veryl-{tool}")));
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
    let tests: Vec<_> = cases()
        .filter(|case| case.name.contains(&args.filter))
        .collect();
    if tests.is_empty() {
        return Err("no cases matched the filter".into());
    }
    let next = AtomicUsize::new(0);
    let results = Mutex::new(Vec::new());
    // Compiler and assertion panics are captured, with full diagnostics on disk.
    std::panic::set_hook(Box::new(|_| {}));
    std::thread::scope(|scope| {
        for _ in 0..args.jobs {
            scope.spawn(|| {
                while let Some(case) = tests.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let row =
                        run_case(case, &output, four_state, build, tool, args.include_ignored);
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
    let report = json!({"schema_version": 2, "suite_version": env!("CARGO_PKG_VERSION"), "tool": tool, "version": version, "include_ignored": args.include_ignored, "counts": counts, "cases": results});
    let contents = serde_json::to_string_pretty(&report)? + "\n";
    std::fs::write(output.join("results.json"), &contents)?;
    if let Some(path) = args.report {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, &contents)?;
    }
    println!(
        "{}; report: {}",
        report["counts"],
        output.join("results.json").display()
    );
    if failures != 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn is_failure(status: &str) -> bool {
    !matches!(status, "passed" | "rejected" | "unsupported" | "ignored")
}

// Test assertions may use arbitrary panic text. Distinguish those from backend
// I/O failures without guessing from Rust's panic-message formatting.
struct ObservedBackend {
    backend: Box<dyn Backend>,
    failed: Arc<AtomicBool>,
}
impl Backend for ObservedBackend {
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
}

fn run_case(
    case: &crate::TestCase,
    output: &Path,
    four_state: bool,
    build: fn(&Design, &Path) -> Result<Box<dyn Backend>>,
    tool: &str,
    include_ignored: bool,
) -> Value {
    let directory = output.join(case.name.replace("::", "/"));
    std::fs::create_dir_all(&directory).unwrap();
    let known_issue = known_issues::find(tool, case.name);
    if let Some(issue) = &known_issue
        && !include_ignored
    {
        let row = json!({
            "name": case.name,
            "category": format!("{:?}", case.category),
            "expectation": format!("{:?}", case.expectation),
            "status": "ignored",
            "phase": "skip",
            "detail": issue["reason"],
            "known_issue": issue,
        });
        std::fs::write(
            directory.join("error.log"),
            issue["reason"].as_str().unwrap(),
        )
        .unwrap();
        return write_result(&directory, row);
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
            match build(design, &build_directory) {
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
    if let Ok(absolute) = std::fs::canonicalize(&directory) {
        portable_detail = portable_detail.replace(absolute.to_string_lossy().as_ref(), "<case>");
    }
    portable_detail = portable_detail.chars().take(8000).collect();
    let mut row = json!({"name": case.name, "category": format!("{:?}", case.category), "expectation": format!("{:?}", case.expectation), "status": status, "phase": phase, "detail": portable_detail});
    if let Some(issue) = known_issue {
        row["known_issue"] = issue;
    }
    write_result(&directory, row)
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
mod tests {
    use super::*;

    #[test]
    fn known_issues_skip_only_the_affected_tool_and_can_be_rechecked() {
        let directory = std::env::temp_dir().join(format!(
            "veryl-known-issue-classification-{}",
            std::process::id()
        ));
        let mut entries = std::collections::BTreeSet::new();
        for (tool, name) in known_issues::entries() {
            assert!(
                entries.insert((tool, name)),
                "duplicate exclusion: {tool} {name}"
            );
            let issue = known_issues::find(tool, name).unwrap();
            let case = crate::case(name).expect("excluded case must exist");
            let ignored = run_case(
                case,
                &directory,
                true,
                |_, _| panic!("ignored case must not reach the compiler"),
                tool,
                false,
            );
            assert_eq!(ignored["status"], "ignored");
            assert_eq!(ignored["phase"], "skip");
            assert!(!is_failure(ignored["status"].as_str().unwrap()));
            assert!(!issue["reason"].as_str().unwrap().is_empty());
            assert!(!issue["observed_version"].as_str().unwrap().is_empty());
            let upstream = issue["upstream"].as_array().unwrap();
            let evidence = issue["evidence"].as_array().unwrap();
            assert!(!upstream.is_empty() || !evidence.is_empty());
            if issue.get("category").is_some() {
                assert!(!issue["category"].as_str().unwrap().is_empty());
                assert!(matches!(
                    issue["phase"].as_str(),
                    Some("emission" | "compile" | "execute")
                ));
            } else {
                assert!(
                    issue["standard"]
                        .as_str()
                        .unwrap()
                        .contains("IEEE 1800-2023")
                );
            }
            for evidence in evidence {
                let path = evidence.as_str().unwrap().split('#').next().unwrap();
                assert!(Path::new(env!("CARGO_MANIFEST_DIR")).join(path).is_file());
            }

            // Forced runs and unaffected tools must execute normally, including
            // surfacing new failures instead of treating them as expected.
            for (tool, include_ignored) in [(tool, true), ("unaffected", false)] {
                let executed = run_case(
                    case,
                    &directory,
                    true,
                    |_, _| Err("compiler executable unavailable".into()),
                    tool,
                    include_ignored,
                );
                assert_eq!(executed["status"], "compile_error");
                assert!(is_failure(executed["status"].as_str().unwrap()));
                assert_eq!(executed.get("known_issue").is_some(), include_ignored);
            }
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn unlisted_cases_still_report_failures() {
        let case = crate::case("operators::test_bitwise_operations").unwrap();
        let directory =
            std::env::temp_dir().join(format!("veryl-unlisted-case-{}", std::process::id()));
        for tool in ["verilator", "icarus"] {
            assert!(known_issues::find(tool, case.name).is_none());
            let result = run_case(
                case,
                &directory,
                true,
                |_, _| Err("new compiler failure".into()),
                tool,
                false,
            );
            assert_eq!(result["status"], "compile_error");
            assert!(is_failure(result["status"].as_str().unwrap()));
            assert!(result.get("known_issue").is_none());
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn negative_cases_do_not_pass_on_infrastructure_or_emission_failure() {
        let case = crate::case("hierarchy::test_instance_output_concat_advances_each_destination")
            .unwrap();
        let directory = std::env::temp_dir().join(format!(
            "veryl-rejection-classification-{}",
            std::process::id()
        ));
        let rejected = run_case(
            case,
            &directory,
            true,
            |_, _| {
                Err(Box::new(CompilationRejected(
                    "nonconstant destination".into(),
                )))
            },
            "test",
            false,
        );
        assert_eq!(rejected["status"], "rejected");
        let unavailable = run_case(
            case,
            &directory,
            true,
            |_, _| Err("compiler executable unavailable".into()),
            "test",
            false,
        );
        assert_eq!(unavailable["status"], "compile_error");
        let emission = run_case(
            case,
            &directory,
            true,
            |_, _| Err(Box::new(EmissionError("emitter panicked".into()))),
            "test",
            false,
        );
        assert_eq!(emission["status"], "emission_error");
        std::fs::remove_dir_all(directory).unwrap();
    }
}
