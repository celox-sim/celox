//! Independent xezim adapter, driven through its command line only.
//! Requires `xezim` and GNU `timeout` on PATH.
//!
//! xezim runs testbenches only: a generated testbench reports through its
//! stdout markers and a native testbench through its completion. The design
//! is first checked with `--compile`, so that a source rejection is told
//! apart from a run that ends in `$fatal`, and then simulated by a second
//! xezim process, which parses and elaborates it again.
use crate::frontend::{Staged, stage};
use crate::{Backend, BigUint, Design, Result, SignalPath};
use crate::{Frontend, TestCase};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The predefined macro that selects the xezim branches of a generated
/// testbench.
const DEFINE: &str = "CELOX_SUITE_XEZIM";

/// xezim's own time limit in nanoseconds, past every time a script can reach
/// (`RUN_UNTIL_LIMIT`), so that the testbench's deadline after `run_to_finish`
/// is the one that fires. A design still running here ends the run without
/// `$finish`, which fails.
const MAX_TIME_NS: u64 = crate::script::ast::RUN_UNTIL_LIMIT + 1;

pub struct Xezim {
    directory: PathBuf,
    /// The pending run of a testbench; `None` for a design without one.
    run: Option<Command>,
}

impl Xezim {
    /// Build `design`. Only a native testbench can then run; xezim has no
    /// signal access here.
    pub fn build(frontend: &dyn Frontend, design: &Design, directory: &Path) -> Result<Self> {
        Self::build_inner(frontend, design, directory, None)
    }

    /// Build a script case as a generated testbench; run it with
    /// `run_testbench`. A failed assertion prints an `@suite assert` line to
    /// `protocol.log`.
    pub fn build_script(
        frontend: &dyn Frontend,
        case: &TestCase,
        directory: &Path,
    ) -> Result<Self> {
        Self::build_inner(frontend, &case.design(), directory, Some(case.script()))
    }

    fn build_inner(
        frontend: &dyn Frontend,
        design: &Design,
        directory: &Path,
        script: Option<&crate::script::ScriptCase>,
    ) -> Result<Self> {
        let Staged {
            directory,
            paths,
            top,
            testbench,
            ..
        } = stage(frontend, design, directory, script, &[DEFINE])?;
        let build_log = File::create(directory.join("build.log"))?;
        let status = xezim("120s")
            .args(["--compile", "-D", DEFINE, "-s"])
            .arg(&top)
            .args(&paths)
            .stdout(Stdio::from(build_log.try_clone()?))
            .stderr(Stdio::from(build_log))
            .status()?;
        if !status.success() {
            let message = format!(
                "xezim build {status}; see {}",
                directory.join("build.log").display()
            );
            let log = fs::read_to_string(directory.join("build.log")).unwrap_or_default();
            return Err(if is_source_rejection(status.code(), &log, &paths) {
                Box::new(crate::CompilationRejected(message))
            } else {
                message.into()
            });
        }
        let run = testbench.then(|| {
            let mut command = xezim("60s");
            command
                .args(["--error-exit", "-D", DEFINE])
                .arg("--max-time")
                .arg(MAX_TIME_NS.to_string())
                .arg("-s")
                .arg(&top)
                .args(&paths)
                .current_dir(&directory);
            if !design.four_state {
                // The suite's two-state storage starts at zero. xezim zeroes
                // flip-flops and memories, but other variables keep X.
                command
                    .env("XEZIM_INIT_REG", "0")
                    .env("XEZIM_INIT_MEM", "0");
            }
            command
        });
        Ok(Self { directory, run })
    }
}

/// xezim under a wall-clock `limit`. xezim handles SIGTERM, so a run that
/// does not stop within 10 s of it is killed. xezim reads many `XEZIM_*`
/// settings from the environment; none of the caller's apply.
#[allow(
    clippy::disallowed_methods,
    reason = "Clears the external tool's own settings at the adapter boundary"
)]
fn xezim(limit: &str) -> Command {
    let mut command = Command::new("timeout");
    command.args(["-k", "10s", limit, "xezim"]);
    for (name, _) in std::env::vars_os() {
        if name.to_str().is_some_and(|name| name.starts_with("XEZIM_")) {
            command.env_remove(name);
        }
    }
    command
}

/// Whether `stdout` ends with xezim's notice of a run that ended with
/// `$finish`. xezim prints its notice last, after a partial line of the
/// design's output if there is one.
fn finish_notice_offset(stdout: &[u8]) -> Option<usize> {
    const NOTICE: &[u8] = b"Simulation finished at time ";
    let text = stdout.strip_suffix(b"\n")?;
    let line = text
        .rsplit(|byte| *byte == b'\n')
        .next()
        .unwrap_or_default();
    line.windows(NOTICE.len())
        .rposition(|window| window == NOTICE)
        .filter(|&at| {
            line[at + NOTICE.len()..]
                .strip_suffix(b" ($finish called)")
                .is_some_and(|time| !time.is_empty() && time.iter().all(u8::is_ascii_digit))
        })
        .map(|at| text.len() - line.len() + at)
}

fn finished(stdout: &[u8]) -> bool {
    finish_notice_offset(stdout).is_some()
}

/// `stdout` without xezim's start-up banner, which is not design output.
fn without_banner(stdout: &[u8]) -> &[u8] {
    let mut rest = stdout;
    for prefix in [&b"=== xezim "[..], b"git "] {
        match rest.iter().position(|byte| *byte == b'\n') {
            Some(end) if rest.starts_with(prefix) => rest = &rest[end + 1..],
            _ => break,
        }
    }
    rest
}

// xezim exits with status 1 for any compilation failure. It prints a parse
// diagnostic as `FILE:LINE:COLUMN: error: MESSAGE` with a source excerpt and
// an elaboration diagnostic as `error: FILE:LINE:COLUMN: MESSAGE` after its
// parse summary. Accept only the reviewed messages of the negative fixtures;
// an unknown or additional diagnostic fails closed as a build failure.
/// Whether a failed build's log shows only reviewed source diagnostics,
/// that is, a language rejection rather than a tool failure.
pub fn is_source_rejection(code: Option<i32>, log: &str, sources: &[PathBuf]) -> bool {
    if code != Some(1) {
        return false;
    }
    let mut rejected = false;
    for line in log.lines().filter(|line| !line.trim().is_empty()) {
        let error = match line.strip_prefix("error: ") {
            Some(rest) => source_diagnostic(rest, sources),
            None => source_diagnostic(line, sources)
                .map(|diagnostic| diagnostic.strip_prefix("error: ").unwrap_or("")),
        };
        if let Some(error) = error {
            if !is_reviewed_rejection(error) {
                return false;
            }
            rejected = true;
        } else if !(is_excerpt(line) || is_file_header(line, sources) || is_parse_summary(line)) {
            return false;
        }
    }
    rejected
}

/// The source diagnostics that establish a language rejection.
fn is_reviewed_rejection(error: &str) -> bool {
    // IEEE 1800-2023 6.22.2 and 7.6: unpacked arrays of nonequivalent element
    // types or different sizes.
    error.starts_with("cannot assign ")
        && (error.ends_with(" are not equivalent (IEEE 1800-2017 \u{a7}6.22.2, \u{a7}7.6)")
            && error.contains(": element types ")
            || error.contains(": the arrays have ")
                && error.ends_with(" elements (IEEE 1800-2017 \u{a7}7.6)"))
}

/// The summary xezim prints once it has parsed the sources without errors.
fn is_parse_summary(line: &str) -> bool {
    line.strip_prefix("Parsed ")
        .and_then(|rest| rest.split_once(" file(s): "))
        .is_some_and(|(files, rest)| {
            files.parse::<usize>().is_ok() && rest.ends_with(" descriptions, 0 errors, 0 warnings")
        })
}

fn source_diagnostic<'a>(line: &'a str, sources: &[PathBuf]) -> Option<&'a str> {
    sources.iter().find_map(|source| {
        let tail = line.strip_prefix(source.to_str()?)?.strip_prefix(':')?;
        let (number, tail) = tail.split_once(':')?;
        let (column, diagnostic) = tail.split_once(':')?;
        for position in [number, column] {
            position.parse::<usize>().ok().filter(|n| *n > 0)?;
        }
        Some(diagnostic.trim_start())
    })
}

/// A source excerpt line under a diagnostic: `  12 | text` or `     | ^~~`.
fn is_excerpt(line: &str) -> bool {
    let Some((number, excerpt)) = line.trim_start().split_once('|') else {
        return false;
    };
    let number = number.trim();
    number.parse::<usize>().is_ok_and(|n| n > 0)
        || (number.is_empty() && excerpt.chars().all(|c| matches!(c, ' ' | '^' | '~')))
}

/// The line that introduces the diagnostics of one source file.
fn is_file_header(line: &str, sources: &[PathBuf]) -> bool {
    ["Parse errors in '", "Strict check failed in '"]
        .iter()
        .filter_map(|prefix| line.strip_prefix(prefix))
        .any(|rest| {
            sources.iter().any(|source| {
                source.to_str().is_some_and(|source| {
                    rest.strip_prefix(source)
                        .and_then(|rest| rest.strip_prefix("' (file "))
                        .and_then(|rest| rest.strip_suffix("):"))
                        .and_then(|rest| rest.split_once(" of "))
                        .is_some_and(|(index, count)| {
                            index.parse::<usize>().is_ok() && count.parse::<usize>().is_ok()
                        })
                })
            })
        })
}

impl Backend for Xezim {
    fn run_testbench(&mut self) -> Result<()> {
        let mut command = self
            .run
            .take()
            .ok_or("no pending native testbench execution")?;
        let output = command
            .stderr(Stdio::from(File::create(
                self.directory.join("runtime.log"),
            )?))
            .output()?;
        let stdout = without_banner(&output.stdout);
        let protocol = &stdout[..finish_notice_offset(stdout).unwrap_or(stdout.len())];
        fs::write(self.directory.join("protocol.log"), protocol)?;
        // A run that ends any other way than `$finish` (the time limit, no
        // further events, an internal error) fails, whatever it printed.
        let finished = finished(stdout);
        if !output.status.success() || !finished {
            return Err(format!(
                "xezim testbench failed: {}{}; see runtime.log and protocol.log",
                output.status,
                if finished { "" } else { ", without $finish" }
            )
            .into());
        }
        Ok(())
    }
    fn write(&mut self, _: &SignalPath, _: BigUint, _: BigUint) -> Result<()> {
        Err("the xezim adapter runs testbenches only".into())
    }
    fn read(&mut self, _: &SignalPath) -> Result<(BigUint, BigUint)> {
        Err("the xezim adapter runs testbenches only".into())
    }
    fn eval_comb(&mut self) -> Result<()> {
        Err("the xezim adapter runs testbenches only".into())
    }
    fn tick(&mut self, _: &str) -> Result<()> {
        Err("the xezim adapter runs testbenches only".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_finish_ends_a_run_and_the_banner_is_not_output() {
        assert!(finished(
            b"Simulation finished at time 0 ($finish called)\n"
        ));
        assert!(finished(
            b"a\npartialSimulation finished at time 1250 ($finish called)\n"
        ));
        let partial = b"a\npartialSimulation finished at time 1250 ($finish called)\n";
        assert_eq!(
            &partial[..finish_notice_offset(partial).unwrap()],
            b"a\npartial"
        );
        assert!(!finished(b"Simulation finished at time 10\n"));
        assert!(!finished(
            b"Simulation finished at time  ($finish called)\n"
        ));
        // A design that prints the notice and then runs out of events.
        assert!(!finished(
            b"Simulation finished at time 0 ($finish called)Simulation finished at time 0\n"
        ));
        assert!(!finished(
            b"Simulation finished at time 0 ($finish called)\nlater\n"
        ));
        let stdout = b"=== xezim 0.11.3 ===\ngit 058f2bba (date)\nhi\n";
        assert_eq!(without_banner(stdout), b"hi\n");
        assert_eq!(without_banner(b"hi\n"), b"hi\n");
    }

    #[test]
    fn unreviewed_diagnostics_are_not_rejections() {
        let sources = [PathBuf::from("/stage/source_0.sv")];
        let log =
            "/stage/source_0.sv:1:38: error: Undeclared identifier 'b'\n    1 | x\n      |   ^\n";
        assert!(!is_source_rejection(Some(1), log, &sources));
        assert!(!is_source_rejection(Some(124), "", &sources));
        let summary = "Parsed 1 file(s): 1 descriptions, 0 errors, 0 warnings\n";
        let reviewed = format!(
            "{summary}error: /stage/source_0.sv:8:34: cannot assign logic [7:0] $[3] to 'two' of type \
             logic [7:0] $[2]: the arrays have 2 and 3 elements (IEEE 1800-2017 \u{a7}7.6)\n"
        );
        assert!(is_source_rejection(Some(1), &reviewed, &sources));
        assert!(!is_source_rejection(Some(2), &reviewed, &sources));
        // A reviewed diagnostic does not cover another failure.
        assert!(!is_source_rejection(
            Some(1),
            &format!("{reviewed}{log}"),
            &sources
        ));
        assert!(!is_source_rejection(
            Some(1),
            &format!("{reviewed}thread panicked\n"),
            &sources
        ));
        assert!(!is_source_rejection(Some(1), summary, &sources));
    }
}
