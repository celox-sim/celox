//! Source waveform effects are owned and persisted by the embedding system.
use super::Simulation;
use crate::{SimBackend, simulator::SvVcdControl};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
};

/// Source-driven waveform configuration, separate from core execution state.
/// A host can serialize this alongside a core checkpoint and reconnect output
/// to a new segment when restoring. Already delivered waveform bytes stay external.
#[derive(Clone, Serialize, Deserialize)]
pub struct SourceWaveformState {
    control: SvVcdControl,
}

impl SourceWaveformState {
    /// Whether `$dumpvars` has started recording (including `$dumpoff`).
    pub fn is_active(&self) -> bool {
        self.control.started.is_some()
    }

    /// Requested source output path, or the SystemVerilog default.
    pub fn path(&self) -> &Path {
        self.control
            .path
            .as_deref()
            .unwrap_or(Path::new("dump.vcd"))
    }
}

impl<B: SimBackend> Simulation<B> {
    /// Capture source-driven waveform controls, without capturing output bytes
    /// or modifying the core checkpoint. Explicit host-installed writers are
    /// managed by their host instead.
    pub fn source_waveform_state(&self) -> SourceWaveformState {
        SourceWaveformState {
            control: self.simulator.sv_vcd.clone(),
        }
    }

    /// Restore waveform controls after restoring the core. Supply a destination path and empty file
    /// exactly when `state.is_active()` is true. The host chooses its destination;
    /// the writer starts a standalone segment at the restored simulation time,
    /// with saved selections, suspension and a fresh per-file byte-limit budget.
    ///
    /// # Safety
    /// State must be an unmodified snapshot from this same compiled design and
    /// compatible Celox build. Serialized selections contain memory offsets and
    /// external signal indices; never restore untrusted or foreign state.
    pub unsafe fn restore_source_waveform(
        &mut self,
        state: SourceWaveformState,
        output: Option<(PathBuf, File)>,
    ) -> io::Result<()> {
        if state.is_active() != output.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "waveform output does not match saved recording state",
            ));
        }
        self.simulator.flush_vcd()?;
        self.simulator.vcd_writer = None;
        self.simulator.sv_vcd = state.control;
        let Some((path, output)) = output else {
            return Ok(());
        };
        let sim = &mut self.simulator;
        sim.sv_vcd.path = Some(path);
        let mut writer = crate::VcdWriter::from_writer_with_time_precision(
            output,
            &sim.sv_vcd.selections,
            sim.program.design.time_precision,
        );
        writer.set_byte_limit(sim.sv_vcd.limit);
        writer.add_external_signals(
            &sim.sv_vcd
                .external_selections
                .iter()
                .map(|(_, desc)| desc.clone())
                .collect::<Vec<_>>(),
        )?;
        sim.sv_vcd.active_external_indices = sim
            .sv_vcd
            .external_selections
            .iter()
            .map(|(index, _)| *index)
            .collect();
        sim.sv_vcd.source_writer = true;
        sim.sv_vcd.pending = false;
        let (ptr, size) = sim.backend.memory_as_ptr();
        // SAFETY: execution is suspended and the caller guarantees valid selections.
        let memory = unsafe { std::slice::from_raw_parts(ptr, size) };
        writer.checkpoint(
            if sim.sv_vcd.suspended {
                "dumpoff"
            } else {
                "dumpall"
            },
            self.state.time(),
            memory,
            &sim.source_vcd_external_values(),
        )?;
        writer.flush()?;
        sim.vcd_writer = Some(writer);
        Ok(())
    }
}
