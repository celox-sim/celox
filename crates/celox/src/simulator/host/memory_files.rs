//! Memory files `$readmemh` and `$readmemb` read while the design runs
//! (IEEE 1800-2023 21.4).

use super::*;
use celox_design::{InitialStateWriteRun, MemoryFileLoad};
use celox_frontend_core::memory_file::{MemoryLoad, load_memory_file};

/// The memory files an instance's initial state loads, each read once.
#[derive(Default)]
pub(crate) struct MemoryFiles {
    /// By the load, its destination and how many equal loads before it the
    /// initial state makes (see [`LoadOccurrences`]).
    loaded:
        crate::HashMap<(crate::AbsoluteAddr, MemoryFileLoad, usize), Arc<[InitialStateWriteRun]>>,
}

/// How many times one application of the initial state has made each load so
/// far: equal loads, such as those of an unrolled loop, are separate calls,
/// each read and reported, while applying the initial state again repeats
/// them.
#[derive(Default)]
pub(super) struct LoadOccurrences(crate::HashMap<(crate::AbsoluteAddr, MemoryFileLoad), usize>);

impl<B: SimBackend> Simulator<B> {
    /// The words `load` writes into `address` when the instance starts. The
    /// file is read, and the load reports its warning or error, the first
    /// time; a later application of the initial state writes the same words.
    pub(super) fn initial_memory_file(
        &mut self,
        address: crate::AbsoluteAddr,
        load: &MemoryFileLoad,
        occurrences: &mut LoadOccurrences,
    ) -> Arc<[InitialStateWriteRun]> {
        let occurrence = occurrences.0.entry((address, load.clone())).or_default();
        let key = (address, load.clone(), *occurrence);
        *occurrence += 1;
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
    pub(super) fn run_memory_file(&mut self, load: &MemoryFileLoad) -> MemoryLoad {
        let file_io = self.files_mut();
        load_memory_file(load, |path| file_io.read_to_string(path))
    }

    /// Run the load of a testbench statement.
    pub(crate) fn run_testbench_memory_file(&mut self, load: &MemoryFileLoad) -> MemoryLoad {
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
                    time_format_arg: false,
                    arg_widths: Vec::new(),
                    arg_signed: Vec::new(),
                    arg_is_string: Vec::new(),
                    arg_float_precision: Vec::new(),
                    arg_is_handle: Vec::new(),
                    home: None,
                },
                Vec::new(),
            );
        }
    }
}

impl<B: SimBackend> Simulator<B> {
    /// Simulate with the file system `files` makes, through which the
    /// design's file I/O and the memory files its code loads both go.
    pub(crate) fn use_file_system(
        &mut self,
        files: &celox_runtime::file_io_backend::FileSystemFactory,
    ) {
        self.install_file_io(celox_runtime::file_io::FileIo::new(files.create()));
    }

    /// Simulate with file I/O `file_io`. The heap the code calls owns it with
    /// its memory-file reader, so a load reads what the design's file writes
    /// wrote, through the same descriptors and file system.
    pub(super) fn install_file_io(&mut self, file_io: celox_runtime::file_io::FileIo) {
        let reader = crate::HostMemoryFileReader::with_file_io(
            self.program.design.memory_file_bases(),
            file_io,
        );
        self.backend
            .heap_mut()
            .set_memory_file_reader(reader.into_heap_reader());
    }

    /// The simulator's own memory-file reader in its heap.
    fn host_reader(heap: &celox_heap::Heap) -> &crate::HostMemoryFileReader {
        heap.memory_file_reader()
            .and_then(|reader| reader.as_any().downcast_ref())
            .expect("a simulator's heap holds its file I/O")
    }

    /// The design's file I/O.
    pub(crate) fn files(&self) -> &celox_runtime::file_io::FileIo {
        Self::host_reader(self.backend.heap()).file_io()
    }

    /// The design's file I/O.
    pub(crate) fn files_mut(&mut self) -> &mut celox_runtime::file_io::FileIo {
        self.backend
            .heap_mut()
            .memory_file_reader_mut()
            .and_then(|reader| {
                reader
                    .as_any_mut()
                    .downcast_mut::<crate::HostMemoryFileReader>()
            })
            .expect("a simulator's heap holds its file I/O")
            .file_io_mut()
    }
}
