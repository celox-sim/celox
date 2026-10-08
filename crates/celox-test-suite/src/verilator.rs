//! Independent Verilator adapter; requires Verilator, make, C++, and GNU timeout.
use crate::frontend::{Staged, stage};
use crate::process::{ProcessBackend, write_if_changed};
use crate::{Backend, BigUint, Design, Result, SignalPath};
use crate::{Frontend, TestCase};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
pub struct Verilator(ProcessBackend);

impl Verilator {
    /// Build a fresh model. Four-state cases must be classified as unsupported
    /// by the runner; Verilator's two-state execution cannot validate them.
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
        if design.four_state {
            return Err("Verilator cannot validate four-state expectations".into());
        }
        let Staged {
            directory,
            paths,
            top,
            testbench,
            edges,
        } = stage(frontend, design, directory, script)?;
        write_if_changed(
            &directory.join("vpi_bits.hpp"),
            include_bytes!("vpi_bits.hpp"),
        )?;
        let harness = directory.join("harness.cpp");
        write_if_changed(&harness, include_bytes!("verilator.cpp"))?;
        let build_log = File::create(directory.join("build.log"))?;
        let status = Command::new("timeout")
            .args([
                "120s",
                "verilator",
                "--cc",
                "--exe",
                "--build",
                "-j",
                "1",
                "--vpi",
                "--public-flat-rw",
                "--timing",
                "--assert",
                "-Wno-fatal",
                "--x-initial",
                "0",
                "--x-assign",
                "0",
                "--prefix",
                "Vdut",
                "--top-module",
            ])
            .arg(&top)
            .arg("--Mdir")
            .arg(directory.join("obj"))
            .arg("-CFLAGS")
            .arg("-O0")
            .arg("-MAKEFLAGS")
            .arg("OPT_FAST=-O0 OPT_SLOW=-O0")
            .args(&paths)
            .arg(&harness)
            .stdout(Stdio::from(build_log.try_clone()?))
            .stderr(Stdio::from(build_log))
            .status()?;
        if !status.success() {
            let message = format!(
                "Verilator build {status}; see {}",
                directory.join("build.log").display()
            );
            let log = fs::read_to_string(directory.join("build.log")).unwrap_or_default();
            return Err(if is_source_rejection(status.code(), &log, &paths) {
                Box::new(crate::CompilationRejected(message))
            } else {
                message.into()
            });
        }

        let mut command = Command::new("timeout");
        command.arg("30s").arg(directory.join("obj/Vdut"));
        if testbench {
            command.arg("+suite_testbench");
        }
        let spawn = if testbench {
            ProcessBackend::spawn_testbench
        } else {
            ProcessBackend::spawn
        };
        Ok(Self(spawn(
            command,
            &directory,
            format!("TOP.{top}"),
            edges,
        )?))
    }
}

// A %Error prefix also covers internal failures, unsupported constructs and
// build infrastructure. Accept only reviewed source diagnostics, checking the
// whole log so a real language error cannot hide a second tool failure.
/// Whether a failed build's log shows only reviewed source diagnostics,
/// that is, a language rejection rather than a tool failure.
pub fn is_source_rejection(code: Option<i32>, log: &str, sources: &[PathBuf]) -> bool {
    if code != Some(1) {
        return false;
    }
    let mut rejected = false;
    let mut source_context = false;
    for line in log.lines().filter(|line| !line.trim().is_empty()) {
        if line
            .strip_prefix("%Error: Exiting due to ")
            .and_then(|s| s.strip_suffix(" error(s)"))
            .is_some_and(|count| count.parse::<usize>().is_ok_and(|n| n > 0))
        {
            source_context = false;
        } else if let Some(error) = line.strip_prefix("%Error: ") {
            let reviewed = source_diagnostic(error, sources).is_some_and(|diagnostic| {
                diagnostic
                    == "Illegal assignment: types are not assignment compatible (IEEE 1800-2023 7.6)"
                    // IEEE 1800-2023 6.22.2 and 7.6: unpacked arrays of
                    // nonequivalent element types or different sizes.
                    || diagnostic
                        == "Illegal assignment: Array element types are not equivalent (IEEE 1800-2023 6.22.2)"
                    || diagnostic
                        == "Assignment between 2-state and 4-state types requires equivalent element types (IEEE 1800-2023 6.22.2, 7.6)"
                    || diagnostic.starts_with("Illegal assignment: Unmatched array sizes in dimension ")
                    // IEEE 1800-2023 23.3.3.5: an instance array connection width.
                    || (diagnostic.starts_with("Input port connection '")
                        && diagnostic.contains("' as part of a module instance array requires "))
                    // IEEE 1800-2023 6.20.1: a body parameter of a module with a
                    // parameter port list is a localparam.
                    || (diagnostic.starts_with("Instance attempts to override '")
                        && diagnostic.ends_with("' as a parameter, but it is a local parameter"))
            });
            if !reviewed {
                return false;
            }
            rejected = true;
            source_context = true;
        } else if let Some(warning) = line.strip_prefix("%Warning-WIDTHEXPAND: ") {
            if !source_diagnostic(warning, sources).is_some_and(|text| {
                text.starts_with("Operator ASSIGN expects ")
                    || text.starts_with("Input port connection '")
            }) {
                return false;
            }
            source_context = true;
        } else if !(source_context && is_diagnostic_context(line)) {
            return false;
        }
    }
    rejected
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

fn is_diagnostic_context(line: &str) -> bool {
    let line = line.trim_start();
    if let Some((number, excerpt)) = line.split_once('|') {
        return number.trim().parse::<usize>().is_ok_and(|n| n > 0)
            || (number.trim().is_empty() && excerpt.chars().all(|c| matches!(c, ' ' | '^' | '~')));
    }
    [
        ": ... note: In instance '",
        ": ... Left-hand data type: '",
        ": ... Right-hand data type: '",
        ": ... Left-hand type: '",
        ": ... Right-hand type: '",
        ": ... Pin data type: '",
        ": ... Expression data type: '",
        "... See the manual at https://verilator.org/verilator_doc.html?",
        "... For warning description see https://verilator.org/warn/WIDTHEXPAND?",
        "... Use \"/* verilator lint_off WIDTHEXPAND */\" and lint_on around source to disable this message.",
    ].iter().any(|prefix| line.starts_with(prefix))
}

impl Backend for Verilator {
    fn run_testbench(&mut self) -> Result<()> {
        self.0.run_testbench()
    }
    fn write(&mut self, signal: &SignalPath, payload: BigUint, mask: BigUint) -> Result<()> {
        if mask != BigUint::default() {
            return Err("Verilator cannot drive X/Z".into());
        }
        self.0.write(signal, payload, mask)
    }
    fn read(&mut self, signal: &SignalPath) -> Result<(BigUint, BigUint)> {
        self.0.read(signal)
    }
    fn eval_comb(&mut self) -> Result<()> {
        self.0.eval_comb()
    }
    fn tick(&mut self, event: &str) -> Result<()> {
        self.0.tick(event)
    }
}
