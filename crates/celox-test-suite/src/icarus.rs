//! Independent Icarus Verilog adapter with X/Z support.
//! Requires `iverilog`, `iverilog-vpi`, `vvp`, C++, and GNU `timeout` on PATH.
use crate::frontend::{Staged, stage};
use crate::process::{ProcessBackend, write_if_changed};
use crate::{Backend, BigUint, Design, Result, SignalPath};
use crate::{Frontend, TestCase};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct Icarus {
    process: ProcessBackend,
    four_state: bool,
}

impl Icarus {
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
            edges,
        } = stage(frontend, design, directory, script)?;
        write_if_changed(
            &directory.join("vpi_bits.hpp"),
            include_bytes!("vpi_bits.hpp"),
        )?;
        write_if_changed(&directory.join("harness.cpp"), include_bytes!("icarus.cpp"))?;
        let build_log = File::create(directory.join("build.log"))?;
        let status = Command::new("timeout")
            .args([
                "120s",
                "iverilog",
                "-g2012",
                "-gstrict-expr-width",
                "-DCELOX_SUITE_ICARUS",
                "-s",
            ])
            .arg(&top)
            .arg("-o")
            .arg(directory.join("model.vvp"))
            .args(&paths)
            .stdout(Stdio::from(build_log.try_clone()?))
            .stderr(Stdio::from(build_log.try_clone()?))
            .status()?;
        if !status.success() {
            let message = format!(
                "Icarus build {status}; see {}",
                directory.join("build.log").display()
            );
            let log = fs::read_to_string(directory.join("build.log")).unwrap_or_default();
            return Err(if is_source_rejection(status.code(), &log, &paths) {
                Box::new(crate::CompilationRejected(message))
            } else {
                message.into()
            });
        }

        let status = Command::new("timeout")
            .args(["120s", "iverilog-vpi", "--name=harness", "harness.cpp"])
            .current_dir(&directory)
            .stdout(Stdio::from(build_log.try_clone()?))
            .stderr(Stdio::from(build_log))
            .status()?;
        if !status.success() {
            return Err(format!("Icarus VPI build {status}").into());
        }
        let mut command = Command::new("timeout");
        command
            .args(["30s", "vvp", "-M"])
            .arg(&directory)
            .args(["-m", "harness"])
            .arg(directory.join("model.vvp"));
        if !design.four_state {
            command.arg("+suite_two_state");
        }
        if testbench {
            command.arg("+suite_testbench");
        }
        let spawn = if testbench {
            ProcessBackend::spawn_testbench
        } else {
            ProcessBackend::spawn
        };
        Ok(Self {
            process: spawn(command, &directory, top, edges)?,
            four_state: design.four_state,
        })
    }
}

// Icarus returns ordinary nonzero statuses for tool failures too. Recognize
// the source diagnostics observed in the negative fixtures, not just an error
// count or a generic "error:" prefix. Unknown/mixed diagnostics fail closed:
// they remain build failures until their source-rejection meaning is reviewed.
/// Whether a failed build's log shows only reviewed source diagnostics,
/// that is, a language rejection rather than a tool failure.
pub fn is_source_rejection(code: Option<i32>, log: &str, sources: &[PathBuf]) -> bool {
    if !matches!(code, Some(1..=123)) {
        return false;
    }
    let mut rejected = false;
    for line in log.lines().filter(|line| !line.trim().is_empty()) {
        if line == "Elaboration failed"
            || line
                .strip_suffix(" error(s) during elaboration.")
                .is_some_and(|count| count.parse::<usize>().is_ok_and(|n| n > 0))
        {
            continue;
        }
        let Some(diagnostic) = sources.iter().find_map(|source| {
            let tail = line.strip_prefix(source.to_str()?)?.strip_prefix(':')?;
            let (number, diagnostic) = tail.split_once(':')?;
            number.parse::<usize>().ok().filter(|n| *n > 0)?;
            Some(diagnostic.trim_start())
        }) else {
            return false;
        };
        if let Some(error) = diagnostic.strip_prefix("error: ") {
            let invalid_source = matches!(
                error,
                "Bit select expressions must be a constant integral value."
                    | "Indexed part select base expression must be a constant integral value in this context."
                    | "Output port expression must support a continuous assignment."
            ) || (error.starts_with("A reference to a net or variable (`")
                && error.ends_with("') is not allowed in a constant expression."))
                || (error.starts_with("Array ") && error.ends_with(" needs an array index here."))
                // IEEE 1800-2023 5.6.1, 23.9: a duplicate name in one scope.
                || (error.starts_with('\'')
                    && error.ends_with("' has already been declared in this scope."))
                // IEEE 1800-2023 23.3.3.5: an instance array connection width.
                || (error.starts_with("Port expression width ")
                    && error.contains(" does not match expected width "))
                // IEEE 1800-2023 6.20.1: an override of a localparam.
                || (error.starts_with("Cannot override parameter `")
                    && error.ends_with(
                        "Parameter cannot be overridden in the scope it has been declared in.",
                    ));
            if invalid_source {
                rejected = true;
            } else if !(error.starts_with("Function ") && error.ends_with(" is not an input port."))
            {
                return false;
            }
            // The function-port diagnostic is an Icarus limitation, and alone
            // does not establish rejection of an invalid output destination.
        } else if !(diagnostic.starts_with(": This expression violates that rule: ")
            || diagnostic.starts_with(": Port ")
            || diagnostic == ": Function arguments must be input ports."
            || diagnostic.starts_with(": It was declared here as ")
            || diagnostic == "warning: always_comb process has no sensitivities.")
        {
            return false;
        }
    }
    rejected
}

impl Backend for Icarus {
    fn run_testbench(&mut self) -> Result<()> {
        self.process.run_testbench()
    }
    fn write(&mut self, signal: &SignalPath, payload: BigUint, mask: BigUint) -> Result<()> {
        self.process.write(signal, payload, mask)
    }
    fn read(&mut self, signal: &SignalPath) -> Result<(BigUint, BigUint)> {
        let (payload, mask) = self.process.read(signal)?;
        if !self.four_state && mask != BigUint::default() {
            return Err(format!("Icarus produced X/Z for two-state signal {signal:?}: payload={payload:x}, mask={mask:x}").into());
        }
        Ok((payload, mask))
    }
    fn eval_comb(&mut self) -> Result<()> {
        self.process.eval_comb()
    }
    fn tick(&mut self, event: &str) -> Result<()> {
        self.process.tick(event)
    }
}
