//! Memory files `$readmemh` and `$readmemb` read while the design runs
//! (IEEE 1800-2023 21.4).

use super::*;
use celox_design::{InitialStateWriteRun, MemoryFileLoad};
use celox_frontend_core::memory_file::{MemoryLoad, load_memory_file};

/// The memory files an instance's initial state loads, each read once.
#[derive(Default)]
pub(crate) struct MemoryFiles {
    loaded: crate::HashMap<(crate::AbsoluteAddr, MemoryFileLoad), Arc<[InitialStateWriteRun]>>,
}

impl<B: SimBackend> Simulator<B> {
    /// The words `load` writes into `address` when the instance starts. The
    /// file is read, and the load reports its warning or error, the first
    /// time; a later application of the initial state writes the same words.
    pub(super) fn initial_memory_file(
        &mut self,
        address: crate::AbsoluteAddr,
        load: &MemoryFileLoad,
    ) -> Arc<[InitialStateWriteRun]> {
        let key = (address, load.clone());
        if let Some(runs) = self.memory_files.loaded.get(&key) {
            return Arc::clone(runs);
        }
        let outcome = self.run_memory_file(load);
        let scope = self.instance_scope(address.instance_id);
        self.report_memory_file(&outcome, scope, load.location.clone());
        let runs: Arc<[InitialStateWriteRun]> = outcome.runs.into();
        self.memory_files.loaded.insert(key, Arc::clone(&runs));
        runs
    }

    /// Run `load`, reading its file through the simulation's file system.
    pub(super) fn run_memory_file(&self, load: &MemoryFileLoad) -> MemoryLoad {
        load_memory_file(load, |path| self.file_io.read_to_string(path))
    }

    /// Run the load of a testbench statement.
    pub(crate) fn run_testbench_memory_file(&self, load: &MemoryFileLoad) -> MemoryLoad {
        self.run_memory_file(load)
    }

    /// Report the warning or the error of the load of a testbench statement.
    pub(crate) fn report_testbench_memory_file(
        &mut self,
        outcome: &MemoryLoad,
        load: &MemoryFileLoad,
    ) {
        let scope = self
            .program
            .design
            .root_instance()
            .and_then(|root| self.instance_scope(root.id));
        self.report_memory_file(outcome, scope, load.location.clone());
    }

    /// Report the warning or the error of a load that ran (IEEE 1800-2023
    /// 21.4). An error terminates the load, not the simulation.
    pub(super) fn report_memory_file(
        &mut self,
        outcome: &MemoryLoad,
        scope: Option<String>,
        location: Option<ReportLocation>,
    ) {
        let reports = [
            (Severity::Warning, outcome.warning.as_ref()),
            (Severity::Error, outcome.error.as_ref()),
        ];
        for (severity, message) in reports {
            let Some(message) = message else { continue };
            self.queue_host_runtime_event(
                RuntimeEventSite {
                    kind: RuntimeEventKind::Report(severity),
                    template: Some(message.replace('%', "%%")),
                    sizing: DisplaySizing::Ieee,
                    scope: scope.clone(),
                    location: location.clone(),
                    assertion: None,
                    deferred: None,
                    timestamp_arg: false,
                    arg_widths: Vec::new(),
                    arg_signed: Vec::new(),
                    arg_is_string: Vec::new(),
                    arg_float_precision: Vec::new(),
                    arg_is_handle: Vec::new(),
                },
                Vec::new(),
            );
        }
    }
}

/// Loads the memory files compiled code reads when its `$readmemh` or
/// `$readmemb` executes (IEEE 1800-2023 21.4), through a simulation's file
/// system, or the host's when it has none.
pub(crate) struct HeapMemoryFileReader {
    files: Option<celox_runtime::file_io::FileReader>,
}

impl HeapMemoryFileReader {
    pub(crate) fn new(files: Option<celox_runtime::file_io::FileReader>) -> Self {
        Self { files }
    }
}

impl celox_heap::memory_files::MemoryFileReader for HeapMemoryFileReader {
    fn load(
        &self,
        descriptor: &[u8],
        file: &[u8],
        range: Option<(i64, i64)>,
    ) -> celox_heap::memory_files::MemoryFileWords {
        let Some(load) =
            celox_frontend_core::memory_file::decode_descriptor(descriptor, file, range)
        else {
            return celox_heap::memory_files::MemoryFileWords {
                error: Some(b"invalid memory file load".to_vec()),
                ..Default::default()
            };
        };
        let outcome = load_memory_file(&load, |path| match &self.files {
            Some(files) => files.read_to_string(path),
            None => std::fs::read_to_string(path),
        });
        memory_file_words(load.element_width, outcome)
    }
}

/// The words of `outcome`, whose words are `width` bits wide, one by one.
fn memory_file_words(
    width: usize,
    outcome: MemoryLoad,
) -> celox_heap::memory_files::MemoryFileWords {
    let chunks = width.div_ceil(64);
    let mut words = celox_heap::memory_files::MemoryFileWords {
        chunks,
        warning: outcome.warning.map(String::into_bytes),
        error: outcome.error.map(String::into_bytes),
        ..Default::default()
    };
    if width == 0 {
        return words;
    }
    let word_mask = (BigUint::from(1u8) << width) - 1u8;
    for run in outcome.runs {
        let value = BigUint::from_bytes_le(&run.value_bytes);
        let mask = BigUint::from_bytes_le(&run.mask_bytes);
        for k in 0..run.bit_width / width {
            words.positions.push((run.bit_offset / width + k) as u64);
            for (plane, out) in [(&value, &mut words.values), (&mask, &mut words.masks)] {
                let mut digits = ((plane >> (k * width)) & &word_mask).to_u64_digits();
                digits.resize(chunks, 0);
                out.extend(digits);
            }
        }
    }
    words
}

impl<B: SimBackend> Simulator<B> {
    /// Simulate with file system `file_io`, through which the design's file
    /// I/O and memory files go.
    pub(crate) fn set_file_io(&mut self, file_io: celox_runtime::file_io::FileIo) {
        let reader = HeapMemoryFileReader::new(Some(file_io.reader()));
        self.backend
            .heap_mut()
            .set_memory_file_reader(Arc::new(reader));
        self.file_io = file_io;
    }
}
