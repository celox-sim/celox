mod backends;
mod declaration;
mod deferred;
#[cfg(test)]
mod deferred_tests;
mod distribution;
mod eval;
mod event_drain;
mod file_io;
mod format;
mod fread;
mod init;
mod memory_files;
mod monitor;
mod native_arena;
pub(crate) mod native_io;
mod native_nba;
mod runtime_events;
mod scan;
mod signals;
mod strobe;
mod system;
mod tick;
mod waveform;
use super::*;
pub(crate) use monitor::MonitorState;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
pub(crate) use strobe::StrobeCall;

use crate::backend::RuntimeEventBuffer;
#[cfg(any(
    target_arch = "x86_64",
    feature = "arm64-codegen",
    target_arch = "aarch64"
))]
use crate::backend::native::{NativeBackend, SharedNativeCode};
use crate::{
    IOContext, RuntimeErrorCode,
    backend::{MemoryLayout, SimBackend},
    ir::{
        DisplaySizing, InitialMemoryData, InitialMemoryWriteRun, InstancePath, RuntimeEventKind,
        RuntimeEventSite, RuntimeProgram, SignalRef, VariableInfo,
    },
};
use celox_design::{AssertionSite, ReportLocation, Severity};
use celox_runtime::backend::EventHandle as _;
use celox_testbench::{
    DisplayFormatArg, FieldSpec, format_sv_display_arg, format_veryl_display_arg, pad_veryl_field,
};
use num_bigint::BigUint;

pub(in crate::simulator) use deferred::*;
use eval::*;
use event_drain::*;
pub use runtime_events::*;
pub use signals::*;
use tick::*;
pub use waveform::*;

/// The core logic evaluation engine.
///
/// Encapsulates the backend, the original SIR program,
/// and an optional VCD writer. Provides low-level, event-driven control.
///
/// The default type parameter `B = DefaultBackend` means that bare `Simulator`
/// uses the custom native backend on a matching host and Cranelift for
/// cross-codegen builds or unsupported hosts.
pub struct Simulator<B: SimBackend = crate::DefaultBackend> {
    pub(crate) backend: B,
    /// The heap handles of the design state and of running processes.
    pub(crate) heap: super::heap::HeapRoots,
    pub(super) file_io: celox_runtime::file_io::FileIo,
    /// The memory files the initial state has loaded.
    memory_files: memory_files::MemoryFiles,
    /// What native process code asks of its host.
    pub(crate) native: NativeHost,
    pending_host_events: Arc<std::sync::Mutex<PendingRuntimeEvents>>,
    pub(crate) program: RuntimeProgram,
    /// The program's scheduling facts.
    pub(crate) schedule: celox_design::schedule::ScheduleModel<crate::AbsoluteAddr>,
    pub(crate) vcd_writer: Option<crate::WaveformWriter>,
    pub(crate) sv_vcd: SvVcdControl,
    pub(crate) dirty: bool,
    /// Static declaration kernels may stop before wildcard baselines are
    /// armed. Host reads and event drains must not settle comb meanwhile.
    pub(crate) declaration_phase: bool,
    pub(crate) warnings: Vec<CompilationWarning>,
    pub(crate) components: crate::component::ComponentRuntime,
    // Temporarily detached while advancing the simulator. Keep the queue
    // in place instead of copying its entire state at every clock edge.
    pub(crate) component_simulation: Option<Box<celox_runtime::SimulationState<B>>>,
    /// Generated TB scheduler unit runs, host edge batches and host
    /// statement services, which tests take.
    #[cfg(test)]
    pub(crate) tb_counters: (usize, usize, usize),
    /// The reading side of the runtime-event buffer.
    pub(crate) runtime_events: RuntimeEventStream,
    /// The combinational observers and what they last saw.
    pub(super) comb_observers: CombObservers,
    /// What the end of a time slot has to do.
    pub(crate) slot_end: SlotEnd,
    /// A timed simulation owns this simulator: a time slot ends when its
    /// scheduler says so, not when combinational logic settles.
    pub(crate) timed: bool,
    /// Some report site reads the time it issues its report at.
    pub(crate) timestamped_reports: bool,
    /// Identity of the state layout, computed on first checkpoint use.
    pub(super) checkpoint_fingerprint: std::sync::OnceLock<u64>,
    /// State objects as state files name them, built on first use.
    pub(super) state_schema: std::sync::OnceLock<Arc<state_file::StateSchema>>,
    pub(crate) diagnostics: crate::RuntimeDiagnostics,
    pub(crate) plusargs: celox_runtime::plusargs::Plusargs,
    tick_timing: TickTiming,
    /// The derived clock domains and what running them needs.
    derived: DerivedClocks<B>,
    /// The design finished a timed simulation (`$finish`). Evaluation,
    /// host writes and waveform dumps no longer change anything.
    pub(crate) frozen: bool,
    /// Host writes a timed simulation observes one at a time.
    pub(crate) host_watch: celox_runtime::HostWatch,
    /// What a timed simulation's host observes inside its time slots.
    pub(crate) observation: SlotObservation<B>,
    /// Whether a timed simulation runs the design and it has a `$finish`
    /// runtime event site.
    pub(crate) finish_sites: bool,
    /// Per event ID, its signal and polarity for explicit edges.
    event_edges: celox_runtime::EventEdges,
}

impl<B: SimBackend> std::fmt::Debug for Simulator<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Simulator").finish()
    }
}

// ── Generic methods available for any backend ────────────────────────
impl<B: SimBackend> Simulator<B> {
    pub(crate) fn decorate_runtime_error(&self, err: RuntimeErrorCode) -> RuntimeErrorCode {
        match err {
            RuntimeErrorCode::DetectedTrueLoopCode(code) => {
                if self.comb_fatal_site(code).is_some() {
                    return RuntimeErrorCode::Fatal {
                        message: "Fatal assertion (runtime event record overwritten)".to_string(),
                        signals: Vec::new(),
                    };
                }
                let Some(info) = self.program.runtime_schema.runtime_errors.get(&code) else {
                    return RuntimeErrorCode::DetectedTrueLoop;
                };
                let signals = info
                    .signals
                    .iter()
                    .map(|addr| self.program.get_path(addr))
                    .collect::<Vec<_>>();
                if info.message == "Detected True Loop" {
                    RuntimeErrorCode::DetectedTrueLoopAt { signals }
                } else {
                    RuntimeErrorCode::Runtime {
                        message: info.message.clone(),
                        signals,
                    }
                }
            }
            other => other,
        }
    }

    pub fn with_backend_and_program(
        backend: B,
        program: RuntimeProgram,
        warnings: Vec<CompilationWarning>,
    ) -> Self {
        let mut sim = Self::with_backend_and_program_for_simulation(backend, program, warnings);
        // Simulator does not execute initial kernels. Release combinational
        // time-zero gates that would otherwise wait forever for them.
        for process in &sim.program.runtime_schema.processes {
            let status = sim.backend.resolve_signal(&process.status);
            if sim.backend.get_as::<u8>(status) == 0 {
                sim.backend
                    .set(status, celox_design::ProcessStatus::Done.code());
            }
        }
        sim
    }

    /// Construct the engine before a timed scheduler starts initial kernels.
    pub(crate) fn with_backend_and_program_for_simulation(
        backend: B,
        program: RuntimeProgram,
        warnings: Vec<CompilationWarning>,
    ) -> Self {
        let schedule =
            celox_design::schedule::ScheduleModel::derive(&program.design, &program.runtime_schema);
        let derived = DerivedClocks::new(&backend, &schedule, &program.runtime_schema);
        let event_edges =
            celox_runtime::build_event_edges(&backend, &schedule.events, &schedule.kinds);
        let comb_observers = CombObservers::new(&backend, &program.runtime_schema);
        let sites = &program.runtime_schema.runtime_event_sites;
        let timestamped_reports = sites.iter().any(|site| site.timestamp_arg);
        let deferred_sites = if sites.iter().any(|site| site.deferred.is_some()) {
            sites
                .iter()
                .map(|site| {
                    site.deferred
                        .map(|deferred| (deferred, site.kind.is_fatal()))
                })
                .collect()
        } else {
            Vec::new()
        };
        let slot_end = SlotEnd::new(
            &backend,
            &program.runtime_schema,
            !deferred_sites.is_empty(),
        );
        let runtime_events = RuntimeEventStream::new(sites);
        let native = NativeHost::new(&backend, &program);
        let observation = SlotObservation::new(&program.runtime_schema);
        let mut sim = Self {
            backend,
            program,
            schedule,
            vcd_writer: None,
            sv_vcd: SvVcdControl::default(),
            dirty: false,
            declaration_phase: false,
            warnings,
            heap: Default::default(),
            file_io: Default::default(),
            memory_files: Default::default(),
            native,
            pending_host_events: Arc::new(std::sync::Mutex::new(PendingRuntimeEvents {
                deferred: DeferredQueues {
                    sites: deferred_sites,
                    ..Default::default()
                },
                ..Default::default()
            })),
            slot_end,
            timed: false,
            timestamped_reports,
            components: Default::default(),
            component_simulation: None,
            #[cfg(test)]
            tb_counters: (0, 0, 0),
            runtime_events,
            comb_observers,
            checkpoint_fingerprint: std::sync::OnceLock::new(),
            state_schema: std::sync::OnceLock::new(),
            diagnostics: crate::RuntimeDiagnostics::default(),
            plusargs: celox_runtime::plusargs::Plusargs::default(),
            tick_timing: TickTiming::default(),
            derived,
            frozen: false,
            host_watch: Default::default(),
            observation,
            finish_sites: false,
            event_edges,
        };
        sim.initialize_copies();
        // The design's memory files go through its file system.
        let file_io = std::mem::take(&mut sim.file_io);
        sim.set_file_io(file_io);
        sim.comb_observers.snapshots = sim.snapshot_all_comb_observers();
        sim
    }

    /// Make the design state, the host state design code observes and
    /// the undrained runtime events those of `other`, a simulator of the
    /// same design. Host state outside the memory image, such as open
    /// files, is not copied.
    pub(crate) fn mirror(&mut self, other: &mut Self) {
        other.stash_runtime_events();
        self.stash_runtime_events();
        let pending = other.pending_host_events.lock().unwrap().clone();
        *self.pending_host_events.lock().unwrap() = pending;
        let fingerprint = self.state_fingerprint();
        crate::simulator::StateImage::capture(&other.backend, other.state_fingerprint())
            .restore_into(&mut self.backend, fingerprint)
            .expect("a simulator of the same design");
        self.frozen = other.frozen;
        self.dirty = other.dirty;
        self.declaration_phase = other.declaration_phase;
        self.comb_observers
            .snapshots
            .clone_from(&other.comb_observers.snapshots);
        self.comb_observers.initial_eval = other.comb_observers.initial_eval;
        self.host_watch.set_log(other.host_watch.log().to_vec());
    }

    /// Returns a reference to the compiled SIR program.
    pub fn program(&self) -> &RuntimeProgram {
        &self.program
    }

    /// Returns a reference to the backend (for signal/event resolution).
    pub fn backend_ref(&self) -> &B {
        &self.backend
    }

    /// Returns warnings emitted during compilation.
    pub fn warnings(&self) -> &[CompilationWarning] {
        &self.warnings
    }

    /// Consume the simulator and return its compilation diagnostics.
    pub fn into_warnings(self) -> Vec<CompilationWarning> {
        self.warnings
    }
}

#[cfg(test)]
mod tests;
