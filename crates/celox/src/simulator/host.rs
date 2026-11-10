mod backends;
mod declaration;
mod deferred;
#[cfg(all(test, feature = "systemverilog"))]
mod deferred_tests;
mod distribution;
mod eval;
mod event_drain;
mod file_io;
mod format;
mod fread;
mod init;
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
    /// Byte offsets of every handle slot in state memory.
    pub(super) handle_slots: std::sync::OnceLock<Arc<[(usize, celox_heap::HandleKind)]>>,
    /// Set while handles live outside state memory, where no collection
    /// may run: between the polls of concurrent testbench processes.
    pub(crate) heap_safepoints_blocked: bool,
    pub(crate) testbench_hardware_ranges: std::sync::OnceLock<Vec<std::ops::Range<usize>>>,
    pub(crate) process_heap_roots: Vec<celox_heap::Root>,
    pub(super) file_io: celox_runtime::file_io::FileIo,
    native_file_io: Option<Arc<native_io::Metadata>>,
    native_arena: native_arena::Arena,
    pub(crate) native_service_calls: u64,
    pub(crate) native_host_return_budget: u64,
    pub(crate) native_host_round_budget: u64,
    pub(crate) postponed_output: bool,
    pub(crate) native_nba_queue: std::collections::VecDeque<celox_runtime::NbaUpdate>,
    pub(crate) native_delayed_nbas: std::collections::VecDeque<(u64, celox_runtime::NbaUpdate)>,
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
    runtime_event_read_seq: Arc<AtomicU64>,
    runtime_event_drain_active: Arc<AtomicBool>,
    /// Set while the active drain paces the simulation
    /// ([`RuntimeEventDrain::set_backpressure`]).
    runtime_event_backpressure: Arc<AtomicBool>,
    /// Whether a runtime-event site records heap handles
    /// (`sites_record_handles`).
    runtime_event_handles: bool,
    runtime_event_pacing: Arc<DrainPacing>,
    pub(super) comb_observer_snapshots: Vec<crate::comb_activation::ObserverSnapshot>,
    /// Storage reused for the next snapshot.
    comb_observer_scratch: Vec<crate::comb_activation::ObserverSnapshot>,
    comb_observer_reads: crate::comb_activation::ObserverReads,
    pub(super) comb_observer_initial_eval: bool,
    /// The design has deferred assertions, whose actions mature at the
    /// end of each time slot.
    pub(crate) deferred_assertions: bool,
    /// The activation groups of combinational processes with deferred
    /// assertions, with a site of each: running the process again
    /// flushes its queue (IEEE 1800-2023 16.4.2).
    comb_deferred_groups: Vec<(u32, usize)>,
    /// A timed simulation owns this simulator: a time slot ends when its
    /// scheduler says so, not when combinational logic settles.
    pub(crate) timed: bool,
    /// Sampled variables and their copies
    /// (`RuntimeSchema::sample_shadows`).
    sample_shadows: Vec<(SignalRef, SignalRef)>,
    /// Deferred assertions or sampled copies have work when a host
    /// call's time slot is over.
    pub(crate) host_slot_work: bool,
    /// Some report site reads the time it issues its report at.
    pub(crate) timestamped_reports: bool,
    /// Identity of the state layout, computed on first checkpoint use.
    pub(super) checkpoint_fingerprint: std::sync::OnceLock<u64>,
    /// State objects as state files name them, built on first use.
    pub(super) state_schema: std::sync::OnceLock<Arc<state_file::StateSchema>>,
    pub(crate) diagnostics: crate::RuntimeDiagnostics,
    pub(crate) plusargs: celox_runtime::plusargs::Plusargs,
    tick_timing_ticks: u64,
    tick_timing_eval_apply_ns: u64,
    tick_timing_eval_comb_ns: u64,
    /// Per event ID, the derived clock domains a tick can trigger.
    /// Empty for a design without derived clocks.
    derived_ticks: celox_runtime::DerivedTickPlans<B::Event>,
    /// Boxed, so that taking it out for a tick moves one pointer.
    derived_scratch: Option<Box<celox_runtime::DerivedTickScratch>>,
    coincident_edges: celox_runtime::CoincidentEdgeCache<B::Event>,
    /// Per event ID, the generated units that run an edge of the event
    /// with its derived domains. Empty without them.
    derived_units: Arc<[Option<DerivedEdgeUnit>]>,
    /// The flag a derived-edge unit reads to settle combinational logic
    /// before the domains evaluate.
    derived_settle: Option<SignalRef>,
    /// The design finished a timed simulation (`$finish`). Evaluation,
    /// host writes and waveform dumps no longer change anything.
    pub(crate) frozen: bool,
    /// Host writes a timed simulation observes one at a time.
    pub(crate) host_watch: celox_runtime::HostWatch,
    /// What a timed simulation's host observes inside its time slots.
    pub(crate) slot_observer: Option<Box<dyn crate::SlotObserver<B>>>,
    /// The slot observer watches every update of a slot
    /// ([`crate::SlotObserver::observes_updates`]).
    pub(crate) observes_updates: bool,
    /// Per process kernel, whether it has value change points; empty
    /// when none has.
    pub(crate) value_change_kernels: Vec<bool>,
    /// The first process unit that runs with the host context: the first
    /// generated one, or the first kernel when kernels have points.
    pub(crate) scoped_from: usize,
    /// What the observer did at value change points since the last
    /// update point, encoded as `native_io::VALUE_CHANGE` results are.
    pub(crate) value_changed: u64,
    /// The slot observer wrote at the end of the last time slot.
    pub(crate) slot_writes: bool,
    /// Runtime-event sequence up to which a timed simulation looked for
    /// `$finish` records.
    pub(crate) finish_scan_seq: u64,
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
        let (derived_ticks, derived_scratch) =
            celox_runtime::build_derived_tick_plans(&backend, &schedule);
        let event_edges =
            celox_runtime::build_event_edges(&backend, &schedule.events, &schedule.kinds);
        let (derived_units, derived_settle) =
            derived_edge_units(&backend, &program.runtime_schema, &derived_ticks);
        let comb_observer_reads = crate::comb_activation::ObserverReads::new(
            &backend,
            &program.runtime_schema.comb_observers,
        );
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
        let sample_shadows: Vec<(SignalRef, SignalRef)> = program
            .runtime_schema
            .sample_shadows
            .iter()
            .map(|(variable, copy)| {
                (
                    backend.resolve_signal(variable),
                    backend.resolve_signal(copy),
                )
            })
            .collect();
        let mut comb_deferred_groups: Vec<(u32, usize)> = Vec::new();
        for observer in &program.runtime_schema.comb_observers {
            if matches!(
                sites[observer.site_id as usize].deferred,
                Some(celox_design::DeferredSite::Action { .. })
            ) && !comb_deferred_groups
                .iter()
                .any(|&(group, _)| group == observer.activation_group)
            {
                comb_deferred_groups.push((observer.activation_group, observer.site_id as usize));
            }
        }
        let runtime_event_handles =
            sites_record_handles(&program.runtime_schema.runtime_event_sites);
        let native_file_io = native_io::Metadata::new(&backend, &program);
        let mut scoped_from = program.runtime_schema.processes.len();
        // Empty without points.
        let mut value_change_kernels = Vec::new();
        if let Some(scheduler) = &program.runtime_schema.scheduler
            && !scheduler.value_change_kernels.is_empty()
        {
            value_change_kernels = vec![false; program.runtime_schema.processes.len()];
            for &kernel in &scheduler.value_change_kernels {
                value_change_kernels[kernel] = true;
            }
            scoped_from = 0;
        }
        let postponed_output = program
            .runtime_schema
            .host_request_sites
            .iter()
            .any(|site| {
                site.kind == celox_design::HostRequestKind::FileIo
                    && (16..=20).contains(&site.operation)
            });
        let mut sim = Self {
            backend,
            program,
            schedule,
            vcd_writer: None,
            sv_vcd: SvVcdControl::default(),
            dirty: false,
            declaration_phase: false,
            warnings,
            handle_slots: Default::default(),
            heap_safepoints_blocked: false,
            testbench_hardware_ranges: Default::default(),
            process_heap_roots: Vec::new(),
            file_io: Default::default(),
            native_file_io,
            native_arena: Default::default(),
            postponed_output,
            native_service_calls: 0,
            native_host_return_budget: u64::MAX,
            // Standalone native testbenches also need a finite bound on
            // Active feedback; timed runs replace this with their budget.
            native_host_round_budget: 1 << 20,
            native_nba_queue: Default::default(),
            native_delayed_nbas: Default::default(),
            pending_host_events: Arc::new(std::sync::Mutex::new(PendingRuntimeEvents {
                deferred: DeferredQueues {
                    sites: deferred_sites.clone(),
                    ..Default::default()
                },
                ..Default::default()
            })),
            deferred_assertions: !deferred_sites.is_empty(),
            comb_deferred_groups,
            timed: false,
            host_slot_work: postponed_output
                || !deferred_sites.is_empty()
                || !sample_shadows.is_empty(),
            timestamped_reports,
            sample_shadows,
            components: Default::default(),
            component_simulation: None,
            #[cfg(test)]
            tb_counters: (0, 0, 0),
            runtime_event_read_seq: Arc::new(AtomicU64::new(0)),
            runtime_event_drain_active: Arc::new(AtomicBool::new(false)),
            runtime_event_backpressure: Arc::new(AtomicBool::new(false)),
            runtime_event_handles,
            runtime_event_pacing: Arc::default(),
            comb_observer_snapshots: Vec::new(),
            comb_observer_scratch: Vec::new(),
            comb_observer_reads,
            comb_observer_initial_eval: true,
            checkpoint_fingerprint: std::sync::OnceLock::new(),
            state_schema: std::sync::OnceLock::new(),
            diagnostics: crate::RuntimeDiagnostics::default(),
            plusargs: celox_runtime::plusargs::Plusargs::default(),
            tick_timing_ticks: 0,
            tick_timing_eval_apply_ns: 0,
            tick_timing_eval_comb_ns: 0,
            derived_ticks,
            derived_scratch: Some(Box::new(derived_scratch)),
            coincident_edges: Default::default(),
            derived_units,
            derived_settle,
            frozen: false,
            host_watch: Default::default(),
            slot_observer: None,
            observes_updates: false,
            scoped_from,
            value_change_kernels,
            value_changed: 0,
            slot_writes: false,
            finish_scan_seq: 0,
            finish_sites: false,
            event_edges,
        };
        sim.initialize_copies();
        sim.comb_observer_snapshots = sim.snapshot_all_comb_observers();
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
        self.comb_observer_snapshots
            .clone_from(&other.comb_observer_snapshots);
        self.comb_observer_initial_eval = other.comb_observer_initial_eval;
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
