//! Independent Icarus Verilog adapter with X/Z support.
//! Requires `iverilog`, `iverilog-vpi`, `vvp`, C++, and GNU `timeout` on PATH.
use crate::process::{ProcessBackend, write_if_changed};
use crate::{Backend, BigUint, Design, Result, SignalPath};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct Icarus {
    process: ProcessBackend,
    four_state: bool,
}

impl Icarus {
    pub fn build(design: &Design, directory: &Path) -> Result<Self> {
        fs::create_dir_all(directory)?;
        let directory = fs::canonicalize(directory)?;
        let sources: Vec<_> = design
            .sources
            .iter()
            .map(|s| (s.text.as_str(), s.path.as_path()))
            .collect();
        for (index, (source, _)) in sources.iter().enumerate() {
            write_if_changed(
                &directory.join(format!("input_{index}.veryl")),
                source.as_bytes(),
            )?;
        }
        let emitted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::emit::emit_veryl_sources(&sources)
        }))
        .map_err(|error| {
            crate::verification::EmissionError(crate::verification::panic_message(error.as_ref()))
        })?;
        let edges = emitted
            .event_edges(&design.top)
            .cloned()
            .ok_or("top module not found")?;
        let mut paths = Vec::new();
        for (index, (source, _)) in emitted.as_sv_sources().iter().enumerate() {
            let path = directory.join(format!("source_{index}.sv"));
            write_if_changed(&path, source.as_bytes())?;
            paths.push(path);
        }
        write_if_changed(
            &directory.join("vpi_bits.hpp"),
            include_bytes!("vpi_bits.hpp"),
        )?;
        write_if_changed(&directory.join("harness.cpp"), include_bytes!("icarus.cpp"))?;
        let build_log = File::create(directory.join("build.log"))?;
        let status = Command::new("timeout")
            .args(["120s", "iverilog", "-g2012", "-gstrict-expr-width", "-s"])
            .arg(&design.top)
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
        Ok(Self {
            process: ProcessBackend::spawn(command, &directory, design.top.clone(), edges)?,
            four_state: design.four_state,
        })
    }
}

// Icarus returns ordinary nonzero statuses for tool failures too. Recognize
// the source diagnostics observed in the negative fixtures, not just an error
// count or a generic "error:" prefix. Unknown/mixed diagnostics fail closed:
// they remain build failures until their source-rejection meaning is reviewed.
fn is_source_rejection(code: Option<i32>, log: &str, sources: &[PathBuf]) -> bool {
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
                || (error.starts_with("Array ") && error.ends_with(" needs an array index here."));
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
            || diagnostic == "warning: always_comb process has no sensitivities.")
        {
            return false;
        }
    }
    rejected
}

impl Backend for Icarus {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_negative_diagnostics_are_source_rejections() {
        let report: serde_json::Value =
            serde_json::from_str(include_str!("../verification/icarus.json")).unwrap();
        let mut checked = 0;
        for case in report["cases"].as_array().unwrap() {
            if case["status"] != "rejected" {
                continue;
            }
            let (status, log) = case["detail"].as_str().unwrap().split_once('\n').unwrap();
            let code = status
                .strip_prefix("Icarus build exit status: ")
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .parse()
                .unwrap();
            assert!(
                is_source_rejection(Some(code), log, &["<case>/source_0.sv".into()]),
                "{}",
                case["name"]
            );
            checked += 1;
        }
        assert_eq!(checked, 8);
    }

    #[test]
    fn incomplete_or_unattributed_diagnostics_do_not_prove_rejection() {
        let sources = [PathBuf::from("design.sv")];
        for log in [
            "",
            "Elaboration failed",
            "1 error(s) during elaboration.",
            "design.sv:1: error: Function f port q is not an input port.\ndesign.sv:1:      : Function arguments must be input ports.\n1 error(s) during elaboration.",
            "unknown.sv:1: error: Bit select expressions must be a constant integral value.",
            "design.sv:1: error: internal compiler error",
            "design.sv:1: error: failed to read input",
        ] {
            assert!(!is_source_rejection(Some(1), log, &sources), "{log}");
        }
        let source_error =
            "design.sv:1: error: Bit select expressions must be a constant integral value.\n";
        for code in [
            None,
            Some(0),
            Some(124),
            Some(125),
            Some(126),
            Some(127),
            Some(137),
        ] {
            assert!(!is_source_rejection(code, source_error, &sources));
        }
        for failure in [
            "internal compiler error",
            "design.sv: No such file or directory",
            "ivl: Assertion failed",
        ] {
            assert!(!is_source_rejection(
                Some(1),
                &format!("{source_error}{failure}"),
                &sources
            ));
        }
    }
}
