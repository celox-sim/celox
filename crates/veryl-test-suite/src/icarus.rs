//! Independent Icarus Verilog adapter with X/Z support.
//! Requires `iverilog`, `iverilog-vpi`, `vvp`, C++, and GNU `timeout` on PATH.
use crate::process::{ProcessBackend, write_if_changed};
use crate::{Backend, BigUint, Design, Result, SignalPath};
use std::fs::{self, File};
use std::path::Path;
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
            return Err(if matches!(status.code(), Some(1..=123)) {
                Box::new(crate::verification::CompilationRejected(message))
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
