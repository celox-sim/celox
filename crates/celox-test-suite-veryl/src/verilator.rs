//! Independent Verilator adapter; requires Verilator, make, C++, and GNU timeout.
use crate::process::{ProcessBackend, write_if_changed};
use crate::{Backend, BigUint, Design, Result, SignalPath};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
pub struct Verilator(ProcessBackend);

impl Verilator {
    /// Build a fresh model. Four-state cases must be classified as unsupported
    /// by the runner; Verilator's two-state execution cannot validate them.
    pub fn build(design: &Design, directory: &Path) -> Result<Self> {
        if design.four_state {
            return Err("Verilator cannot validate four-state expectations".into());
        }
        fs::create_dir_all(directory)?;
        let directory = fs::canonicalize(directory)?;
        let sources = design
            .sources
            .iter()
            .map(|s| (s.text.as_str(), s.path.as_path()))
            .collect::<Vec<_>>();
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
        let mut paths = Vec::new();
        let edges = emitted
            .event_edges(&design.top)
            .cloned()
            .ok_or_else(|| format!("top module {} not found in emitted design", design.top))?;
        for (index, (source, _)) in emitted.as_sv_sources().iter().enumerate() {
            let path = directory.join(format!("source_{index}.sv"));
            write_if_changed(&path, source.as_bytes())?;
            paths.push(path);
        }
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
            .arg(&design.top)
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
        Ok(Self(ProcessBackend::spawn(
            command,
            &directory,
            format!("TOP.{}", design.top),
            edges,
        )?))
    }
}

// A %Error prefix also covers internal failures, unsupported constructs and
// build infrastructure. Accept only reviewed source diagnostics, checking the
// whole log so a real language error cannot hide a second tool failure.
fn is_source_rejection(code: Option<i32>, log: &str, sources: &[PathBuf]) -> bool {
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
            if source_diagnostic(error, sources)
                != Some(
                    "Illegal assignment: types are not assignment compatible (IEEE 1800-2023 7.6)",
                )
            {
                return false;
            }
            rejected = true;
            source_context = true;
        } else if let Some(warning) = line.strip_prefix("%Warning-WIDTHEXPAND: ") {
            if !source_diagnostic(warning, sources)
                .is_some_and(|text| text.starts_with("Operator ASSIGN expects "))
            {
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
        "... See the manual at https://verilator.org/verilator_doc.html?",
        "... For warning description see https://verilator.org/warn/WIDTHEXPAND?",
        "... Use \"/* verilator lint_off WIDTHEXPAND */\" and lint_on around source to disable this message.",
    ].iter().any(|prefix| line.starts_with(prefix))
}

impl Backend for Verilator {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_negative_diagnostic_is_a_source_rejection() {
        let report: serde_json::Value =
            serde_json::from_str(include_str!("../verification/verilator.json")).unwrap();
        let mut checked = 0;
        for case in report["cases"].as_array().unwrap() {
            if case["status"] != "rejected" {
                continue;
            }
            let (_, log) = case["detail"].as_str().unwrap().split_once('\n').unwrap();
            let sources = [PathBuf::from("<case>/source_0.sv")];
            assert!(is_source_rejection(Some(1), log, &sources));
            assert!(!is_source_rejection(Some(1), log, &["other.sv".into()]));
            assert!(!is_source_rejection(None, log, &sources));
            checked += 1;
        }
        assert_eq!(checked, 1);
    }
}
