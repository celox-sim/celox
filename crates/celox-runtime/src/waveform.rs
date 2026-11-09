//! Format-dispatching waveform output: VCD text or binary FST behind the
//! recording contract the simulator already drives for VCD.
use crate::fst::FstWriter;
use crate::vcd::{VcdExternalSignalDesc, VcdSignalDesc, VcdStatistics, VcdWriter};
use num_bigint::BigUint;
use std::fs::File;
use std::io::{self, Seek, Write};
use std::path::Path;

/// Statistics shared by both waveform encoders.
pub type WaveformStatistics = VcdStatistics;

/// A waveform file format Celox can record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum WaveformFormat {
    #[default]
    Vcd,
    Fst,
}

impl WaveformFormat {
    /// Pick the format for an output path: `.fst` (any case) selects FST,
    /// matching the `$dumpfile` convention of other simulators; every other
    /// path keeps the VCD default.
    pub fn from_path<P: AsRef<Path>>(path: P) -> Self {
        match path.as_ref().extension().and_then(|ext| ext.to_str()) {
            Some(ext) if ext.eq_ignore_ascii_case("fst") => Self::Fst,
            _ => Self::Vcd,
        }
    }

    /// The conventional file extension for the format.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Vcd => "vcd",
            Self::Fst => "fst",
        }
    }
}

/// A waveform writer in whichever format was selected for its destination.
/// Method behavior follows [`VcdWriter`]; see [`FstWriter`] for the FST
/// finalization and flushing differences.
#[allow(
    clippy::large_enum_variant,
    reason = "a simulator owns at most one waveform writer, whose buffers are heap-allocated"
)]
pub enum WaveformWriter<W: Write + Seek = File> {
    Vcd(VcdWriter<W>),
    Fst(FstWriter<W>),
}

impl WaveformWriter<File> {
    pub fn new<P: AsRef<Path>>(
        path: P,
        format: WaveformFormat,
        descs: &[VcdSignalDesc],
    ) -> io::Result<Self> {
        Self::new_with_time_precision(path, format, descs, celox_design::TimePrecision::default())
    }

    pub fn new_with_time_precision<P: AsRef<Path>>(
        path: P,
        format: WaveformFormat,
        descs: &[VcdSignalDesc],
        time_precision: celox_design::TimePrecision,
    ) -> io::Result<Self> {
        Ok(match format {
            WaveformFormat::Vcd => Self::Vcd(VcdWriter::new_with_time_precision(
                path,
                descs,
                time_precision,
            )?),
            WaveformFormat::Fst => Self::Fst(FstWriter::new_with_time_precision(
                path,
                descs,
                time_precision,
            )?),
        })
    }

    /// Finish the current file and continue the waveform, in the same
    /// format, in a new file at `path`. Its timestamps start over.
    pub fn restart<P: AsRef<Path>>(&mut self, path: P) -> io::Result<()> {
        match self {
            Self::Vcd(writer) => writer.restart(path),
            Self::Fst(writer) => writer.restart(path),
        }
    }
}

impl<W: Write + Seek> WaveformWriter<W> {
    pub fn from_writer_with_time_precision(
        writer: W,
        format: WaveformFormat,
        descs: &[VcdSignalDesc],
        time_precision: celox_design::TimePrecision,
    ) -> Self {
        match format {
            WaveformFormat::Vcd => Self::Vcd(VcdWriter::from_writer_with_time_precision(
                writer,
                descs,
                time_precision,
            )),
            WaveformFormat::Fst => Self::Fst(FstWriter::from_writer_with_time_precision(
                writer,
                descs,
                time_precision,
            )),
        }
    }

    pub fn format(&self) -> WaveformFormat {
        match self {
            Self::Vcd(_) => WaveformFormat::Vcd,
            Self::Fst(_) => WaveformFormat::Fst,
        }
    }

    pub fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Vcd(writer) => writer.flush(),
            Self::Fst(writer) => writer.flush(),
        }
    }

    pub fn into_inner(self) -> io::Result<W> {
        match self {
            Self::Vcd(writer) => writer.into_inner(),
            Self::Fst(writer) => writer.into_inner(),
        }
    }

    pub fn statistics(&self) -> WaveformStatistics {
        match self {
            Self::Vcd(writer) => writer.statistics(),
            Self::Fst(writer) => writer.statistics(),
        }
    }

    pub fn last_timestamp(&self) -> Option<u64> {
        match self {
            Self::Vcd(writer) => writer.last_timestamp(),
            Self::Fst(writer) => writer.last_timestamp(),
        }
    }

    pub fn rescan(&mut self) {
        match self {
            Self::Vcd(writer) => writer.rescan(),
            Self::Fst(writer) => writer.rescan(),
        }
    }

    pub fn set_byte_limit(&mut self, limit: u64) {
        match self {
            Self::Vcd(writer) => writer.set_byte_limit(limit),
            Self::Fst(writer) => writer.set_byte_limit(limit),
        }
    }

    pub fn checkpoint(
        &mut self,
        command: &str,
        timestamp: u64,
        memory: &[u8],
        external: &[(BigUint, BigUint)],
    ) -> io::Result<()> {
        match self {
            Self::Vcd(writer) => writer.checkpoint(command, timestamp, memory, external),
            Self::Fst(writer) => writer.checkpoint(command, timestamp, memory, external),
        }
    }

    pub fn dump(&mut self, timestamp: u64, memory: &[u8]) -> io::Result<()> {
        match self {
            Self::Vcd(writer) => writer.dump(timestamp, memory),
            Self::Fst(writer) => writer.dump(timestamp, memory),
        }
    }

    pub fn dump_with_external(
        &mut self,
        timestamp: u64,
        memory: &[u8],
        external: &[(BigUint, BigUint)],
    ) -> io::Result<()> {
        match self {
            Self::Vcd(writer) => writer.dump_with_external(timestamp, memory, external),
            Self::Fst(writer) => writer.dump_with_external(timestamp, memory, external),
        }
    }

    pub fn dump_with_activity(
        &mut self,
        timestamp: u64,
        memory: &[u8],
        external: &[(BigUint, BigUint)],
        activity: Option<&[usize]>,
    ) -> io::Result<()> {
        match self {
            Self::Vcd(writer) => writer.dump_with_activity(timestamp, memory, external, activity),
            Self::Fst(writer) => writer.dump_with_activity(timestamp, memory, external, activity),
        }
    }

    pub fn dump_backend<B: crate::backend::SimBackend>(
        &mut self,
        timestamp: u64,
        backend: &mut B,
        external: &[(BigUint, BigUint)],
    ) -> io::Result<()> {
        match self {
            Self::Vcd(writer) => writer.dump_backend(timestamp, backend, external),
            Self::Fst(writer) => writer.dump_backend(timestamp, backend, external),
        }
    }

    pub fn add_external_signals(&mut self, descs: &[VcdExternalSignalDesc]) -> io::Result<()> {
        match self {
            Self::Vcd(writer) => writer.add_external_signals(descs),
            Self::Fst(writer) => writer.add_external_signals(descs),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_follows_the_fst_extension() {
        assert_eq!(WaveformFormat::from_path("waves.fst"), WaveformFormat::Fst);
        assert_eq!(WaveformFormat::from_path("waves.FST"), WaveformFormat::Fst);
        assert_eq!(WaveformFormat::from_path("waves.vcd"), WaveformFormat::Vcd);
        assert_eq!(WaveformFormat::from_path("dump"), WaveformFormat::Vcd);
    }
}
