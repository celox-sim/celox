//! Independent Verilator adapter; requires Verilator, make, C++, and GNU timeout.
use crate::process::{ProcessBackend, write_if_changed};
use crate::{Backend, BigUint, Design, Result, SignalPath};
use std::fs::{self, File};
use std::path::Path;
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
            // Verilator also invokes a C++ compiler; its failure is not HDL rejection.
            let hdl_error = log.lines().any(|line| {
                line.starts_with("%Error")
                    && !line.contains("make")
                    && !line.contains("Command Failed")
            });
            return Err(if status.code() == Some(1) && hdl_error {
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
