//! Waveform dumps of a [`Simulator`].

use super::*;

#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct SvVcdControl {
    pub path: Option<std::path::PathBuf>,
    pub started: Option<u64>,
    pub pending: bool,
    pub selections: Vec<crate::VcdSignalDesc>,
    pub external_selections: Vec<(usize, celox_runtime::VcdExternalSignalDesc)>,
    pub source_writer: bool,
    pub active_external_indices: Vec<usize>,
    pub limit: u64,
    pub suspended: bool,
}

/// Why [`Simulator::try_dump`] failed.
#[derive(Debug, thiserror::Error)]
pub enum DumpError {
    #[error("evaluating combinational logic failed: {0}")]
    Runtime(RuntimeErrorCode),
    #[error(transparent)]
    Io(std::io::Error),
}

impl<B: SimBackend> Simulator<B> {
    /// Captures the current state of all signals and writes them to the waveform file.
    ///
    /// # Panics
    ///
    /// Panics where [`Self::try_dump`] returns an error.
    pub fn dump(&mut self, timestamp: u64) {
        if let Err(error) = self.try_dump(timestamp) {
            panic!("waveform dump at {timestamp} failed: {error}");
        }
    }

    /// Captures the current state of all signals and writes them to the
    /// waveform file. Fails if combinational evaluation or writing fails, or
    /// if `timestamp` is earlier than the last dumped timestamp.
    pub fn try_dump(&mut self, timestamp: u64) -> Result<(), DumpError> {
        if self.frozen {
            return Ok(());
        }
        if self.dirty {
            self.eval_comb_checked().map_err(DumpError::Runtime)?;
            self.dirty = false;
        }
        let component_traces = self.vcd_external_values();
        if let Some(ref mut writer) = self.vcd_writer {
            writer
                .dump_backend(timestamp, &mut self.backend, &component_traces)
                .map_err(DumpError::Io)?;
        }
        Ok(())
    }

    /// Dump unless `timestamp` precedes what the waveform file already
    /// records, as after `run_until` to an earlier time.
    pub(crate) fn dump_unless_rewound(&mut self, timestamp: u64) {
        let rewound = self
            .vcd_writer
            .as_ref()
            .and_then(|writer| writer.last_timestamp())
            .is_some_and(|last| timestamp < last);
        if !rewound {
            self.dump(timestamp);
        }
    }

    pub(crate) fn try_dump_unless_rewound(&mut self, timestamp: u64) -> Result<(), DumpError> {
        let rewound = self
            .vcd_writer
            .as_ref()
            .and_then(|writer| writer.last_timestamp())
            .is_some_and(|last| timestamp < last);
        if rewound {
            Ok(())
        } else {
            self.try_dump(timestamp)
        }
    }

    /// Reject a return to `time` if the waveform file has already passed it.
    pub(crate) fn check_vcd_rewind(&self, time: u64) -> Result<(), CheckpointError> {
        match self
            .vcd_writer
            .as_ref()
            .and_then(|writer| writer.last_timestamp())
        {
            Some(last_dumped) if time < last_dumped => {
                Err(CheckpointError::VcdRewind { time, last_dumped })
            }
            _ => Ok(()),
        }
    }

    /// Finish the current waveform file and continue the waveform in a new
    /// file at `path`, whose timestamps start over. Use it to record a
    /// simulation rewound by a restore. Fails if the simulator was built
    /// without waveform output.
    pub fn switch_vcd(&mut self, path: impl AsRef<std::path::Path>) -> std::io::Result<()> {
        let writer = self.vcd_writer.as_mut().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "waveform output was not enabled when the simulator was built",
            )
        })?;
        writer.restart(path)
    }

    /// Make buffered waveform output visible and report write errors.
    pub fn flush_vcd(&mut self) -> std::io::Result<()> {
        if let Some(writer) = self.vcd_writer.as_mut() {
            writer.flush()?;
        }
        Ok(())
    }

    pub fn vcd_statistics(&self) -> Option<celox_runtime::WaveformStatistics> {
        self.vcd_writer
            .as_ref()
            .map(crate::WaveformWriter::statistics)
    }

    /// Take ownership of the waveform writer, if one was configured at
    /// build time.
    ///
    /// Builders create the writer themselves so that layout selection can
    /// account for waveform recording; callers building handles around a
    /// backend use this to keep tracing alive without rebuilding
    /// descriptors.
    pub fn take_waveform_writer(&mut self) -> Option<crate::WaveformWriter> {
        self.vcd_writer.take()
    }

    /// Take ownership of the VCD writer, if the configured waveform is
    /// VCD. An FST writer stays installed; use
    /// [`Self::take_waveform_writer`] for either format.
    pub fn take_vcd_writer(&mut self) -> Option<crate::VcdWriter> {
        if matches!(self.vcd_writer, Some(crate::WaveformWriter::Vcd(_))) {
            match self.vcd_writer.take() {
                Some(crate::WaveformWriter::Vcd(writer)) => Some(writer),
                _ => unreachable!("variant checked above"),
            }
        } else {
            None
        }
    }

    /// Build VCD signal descriptors for all instances.
    ///
    /// The returned descriptors are self-contained (no IR references) and can
    /// be cached alongside `SharedJitCode` so that VCD works on cache-hit
    /// paths without the original [`RuntimeProgram`].
    pub fn build_vcd_descs(&self, four_state_mode: bool) -> Vec<crate::VcdSignalDesc> {
        let mut descs = Vec::new();
        let mut sorted_instances = self.program.design.instances().collect::<Vec<_>>();
        sorted_instances.sort_by_key(|instance| instance.id);

        for instance in sorted_instances {
            let scope = format!("{}", instance.id);

            let mut sorted_vars: Vec<_> = instance
                .state_addresses()
                .iter()
                .filter_map(|address| self.program.design.variable(address))
                .filter(|variable| instance.resolves_path_to(&variable.path, variable.address))
                // A handle slot holds a heap reference, not a value to
                // show; strings have no VCD representation yet.
                .filter(|variable| {
                    !self
                        .program
                        .design
                        .state_objects
                        .get(&variable.address)
                        .is_some_and(|metadata| metadata.value_kind.is_handle())
                })
                .collect();
            sorted_vars.sort_by(|a, b| a.path.cmp(&b.path));

            for variable in sorted_vars {
                let name = variable.path.join(".");
                let signal = self.backend.resolve_signal(&variable.address);

                descs.push(crate::VcdSignalDesc {
                    scope: scope.clone(),
                    name,
                    offset: signal.offset,
                    width: signal.width,
                    is_4state: four_state_mode && signal.is_4state,
                });
            }
        }
        descs
    }
}
