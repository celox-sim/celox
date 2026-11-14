//! Tiered execution backend: interpret first, promote to compiled code.
//!
//! [`TieredBackend`] starts every simulation on the Tier-0 interpreter so
//! execution begins the moment the state layout is finalized, hiding the
//! code-generation latency behind the first simulated steps. A worker thread
//! compiles the program for the host's default compiled tier in the
//! background — the direct native backend where available (matching
//! [`crate::DefaultBackend`]'s selection), Cranelift otherwise — and the next
//! scheduler safe point after completion adopts the compiled code, moving the
//! live memory image across without any translation because both tiers run
//! against the same finalized layout.
//!
//! By default, promotion is whole-program. On x86-64, a baseline native image is prepared
//! first, followed by an optimizing image which replaces it at a safe point.
//! Single-edge derived kernels are compiled only after a host requests them;
//! coincident-edge hosts can adopt an optimized core without those kernels.
//! Every image uses the same live state and event buffers. The interpreter
//! remains the permanent fallback whenever baseline compilation fails, and
//! [`TierPromotion::Never`] skips background compilation entirely so the
//! simulation stays interpreted without paying for a worker thread.
//! [`TierPromotion::AfterSteps`] keeps compiling eagerly but delays adoption
//! until the interpreter has executed a minimum number of evaluation steps,
//! protecting short simulations from the promotion cost.
//!
//! `SimulatorOptions::hotspot_jit`, on by default, instead retains a mixed
//! interpreter/native executor on supported single-lane native x86-64 and
//! AArch64 builds.
//! Its independently published entries are reported by `hotspot_stats`; they
//! do not change the whole-image meaning of `is_compiled`.
//!
//! Adoption never moves the live memory image: tiered builds reserve arena
//! headroom up front and grow within that allocation, so views handed out
//! before promotion (for example zero-copy host buffers) stay valid. If a
//! compiled image would exceed the reserved arena, adoption is declined and
//! the reason is reported through [`TieredBackend::promotion_error`].
//!
//! Background compilation is cooperatively cancellable on the native tier:
//! dropping the backend (or calling
//! [`TieredBackend::cancel_background_compilation`]) flags the worker, which
//! unwinds at the next task boundary instead of finishing the image. Drop
//! joins the owned worker so compilation never leaks beyond the backend's
//! lifetime. The Cranelift tier runs to completion once started, so its drop
//! may wait for that compilation to finish.

#![cfg(feature = "host-runtime")]

use std::sync::Arc;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use num_bigint::BigUint;

use super::compile_cancel::CompileCancel;
use super::{
    EventHandle, MemoryLayout, RuntimeEventBuffer, SimBackend, SimulatorErrorCode,
    memory_image::MemoryImage,
};
use crate::backend::InterpBackend;
#[cfg(any(
    target_arch = "x86_64",
    feature = "arm64-codegen",
    target_arch = "aarch64"
))]
use crate::backend::native::SharedNativeCode;
#[cfg(feature = "cranelift")]
use crate::backend::{JitBackend, SharedJitCode};
use crate::{
    SimulatorError, SimulatorOptions,
    ir::{AbsoluteAddr, LaidOutProgram, SignalRef},
};

mod compiled;
mod sim_backend;
pub(crate) use compiled::*;

/// Whether this host's default compiled tier is the direct native backend
/// (mirroring [`crate::DefaultBackend`]'s selection) rather than Cranelift.
pub(crate) fn native_is_default_target() -> bool {
    #[cfg(any(
        all(target_arch = "x86_64", not(feature = "arm64-codegen")),
        all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
    ))]
    {
        true
    }
    #[cfg(not(any(
        all(target_arch = "x86_64", not(feature = "arm64-codegen")),
        all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
    )))]
    {
        false
    }
}

/// The memory-layout mode shared by the interpreter and every compiled tier.
pub(crate) fn default_target_layout_mode() -> crate::backend::memory_layout::MemoryLayoutMode {
    crate::backend::memory_layout::MemoryLayoutMode::ElementStrided
}

/// The layout mode for one tiered build.
///
/// VCD tracing reads whole signals as contiguous packed bytes, so a build
/// that records traces selects the packed layout; element-strided arrays are
/// not representable in the current VCD descriptor format.
pub(crate) fn tiered_layout_mode(
    vcd_recording: bool,
) -> crate::backend::memory_layout::MemoryLayoutMode {
    if vcd_recording {
        crate::backend::memory_layout::MemoryLayoutMode::Packed
    } else {
        default_target_layout_mode()
    }
}

/// Stable event handle for the tiered backend.
///
/// Both inner backends assign identical trigger id spaces for the same
/// laid-out program (deterministic `FxHashMap` iteration over identical
/// maps), so the address and id observed before promotion stay meaningful
/// after adopting the compiled tier.
#[derive(Clone, Copy, Debug)]
pub struct TieredEventRef {
    addr: AbsoluteAddr,
    id: usize,
}

impl EventHandle for TieredEventRef {
    fn id(&self) -> usize {
        self.id
    }

    fn addr(&self) -> AbsoluteAddr {
        self.addr
    }
}

enum Phase {
    Interpreting(Option<Box<InterpBackend>>),
    Compiled(CompiledTier),
}

enum Promotion {
    /// Background compilation still running; the receiver yields exactly one
    /// result when it finishes.
    Pending(mpsc::Receiver<Result<CompiledCode, SimulatorError>>),
    /// Compilation failed permanently; remain on the interpreter forever.
    Failed(SimulatorError),
    /// [`TierPromotion::Never`] — no worker was ever spawned and the
    /// simulation stays interpreted by policy (not by failure).
    Disabled,
    Promoted,
}

/// Active execution tier reported by [`TieredExecutionStats`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TieredExecutionTier {
    /// Evaluation is currently performed by the SIR interpreter.
    Interpreter,
    /// Evaluation is currently performed by generated code.
    Compiled,
}

/// State of background compilation and promotion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TieredPromotionStatus {
    /// The background compiler has not yet delivered a result.
    Pending,
    /// Compilation or adoption failed and execution will remain interpreted.
    Failed,
    /// Promotion was disabled by policy, so no compiler worker was started.
    Disabled,
    /// Generated code has been adopted.
    Promoted,
}

/// Deterministic execution measurements for one tiered simulator.
///
/// This snapshot deliberately contains counts and lifecycle state rather than
/// wall-clock samples. Applications can correlate it with structured
/// `celox.tiered.compile` and `celox.tiered.promote` tracing spans, while
/// performance tests should measure operations with a statistical benchmark
/// harness such as Criterion. Reading a snapshot has no side effects and does
/// not poll the compiler worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct TieredExecutionStats {
    /// Tier that will execute the next evaluation.
    pub tier: TieredExecutionTier,
    /// Background compilation and promotion state.
    pub promotion: TieredPromotionStatus,
    /// Evaluation iterations attempted by the interpreter.
    pub interpreted_evaluations: u64,
    /// Evaluation iterations attempted by generated code.
    pub compiled_evaluations: u64,
    /// Interpreter evaluation count at the point generated code was adopted.
    pub promoted_after_interpreted_evaluations: Option<u64>,
    /// Number of scheduler safe-point polls performed while interpreted.
    pub safe_point_polls: u64,
    /// Polls that could not promote because an evaluate/apply pair was open.
    pub split_apply_deferrals: u64,
    /// Polls that were held below the configured promotion threshold.
    pub threshold_deferrals: u64,
}

/// Opt-in wall-clock measurement for a tiered execution interval.
///
/// Ordinary simulations do not read the host clock. Benchmark callers start
/// timing immediately before executing a workload and finish it afterwards;
/// `promotion_elapsed` is the time from that start until generated code is
/// adopted at a scheduler safe point, and `upgrade_elapsed` until the
/// optimizing image replaced it. A compiled testbench also records its tick
/// at each of these points, so the share of the workload each tier ran is
/// visible.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TieredExecutionTiming {
    promotion: Option<TierTransition>,
    upgrade: Option<TierTransition>,
    finish_tick: Option<u64>,
    /// A build compiles at most a baseline, an optimizing and a derived-edge
    /// image.
    compiles: [Option<TieredCompileStage>; 3],
}

/// When a tier took over, measured from the start of execution timing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TierTransition {
    /// Wall-clock time from the measurement start.
    pub elapsed: Duration,
    /// The compiled testbench's tick, when one runs.
    pub tick: Option<u64>,
}

/// One background compilation of a tiered simulation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TieredCompileStage {
    pub kind: TieredCompileKind,
    /// Wall-clock time the compilation took on the worker.
    pub elapsed: Duration,
    pub completed: bool,
}

/// Which image a background compilation produced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TieredCompileKind {
    /// The only stage of a single-stage build.
    Single,
    /// The low-latency image a two-stage build adopts first.
    Baseline,
    /// The optimizing image that replaces the baseline.
    Optimizing,
    /// The optimizing image with derived-edge units, built on demand.
    DerivedEdges,
}

impl TieredExecutionTiming {
    /// Time from the measurement start until the compiled tier was adopted.
    pub fn promotion_elapsed(self) -> Option<Duration> {
        self.promotion.map(|transition| transition.elapsed)
    }

    /// When generated code was adopted.
    pub fn promotion(self) -> Option<TierTransition> {
        self.promotion
    }

    /// When the optimizing image replaced the baseline image.
    pub fn upgrade(self) -> Option<TierTransition> {
        self.upgrade
    }

    /// The compiled testbench's tick when timing finished.
    pub fn finish_tick(self) -> Option<u64> {
        self.finish_tick
    }

    /// The background compilations that finished, in completion order. A
    /// stage still running when timing finished is absent.
    pub fn compiles(self) -> impl Iterator<Item = TieredCompileStage> {
        self.compiles.into_iter().flatten()
    }
}

struct ActiveExecutionTiming {
    started_at: Instant,
    promotion: Option<TierTransition>,
    upgrade: Option<TierTransition>,
}

/// Background compilations the worker finished, shared with the backend.
#[expect(
    clippy::disallowed_types,
    reason = "cross-thread: the celox-jit-compile worker thread appends finished compile stages and the simulation thread reads them for tier stats"
)]
type CompileLog = Arc<std::sync::Mutex<Vec<TieredCompileStage>>>;

/// Run one background compilation, recording how long it took.
fn logged<T>(
    log: &CompileLog,
    kind: TieredCompileKind,
    compile: impl FnOnce() -> Result<T, SimulatorError>,
) -> Result<T, SimulatorError> {
    let started = Instant::now();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(compile))
        .unwrap_or_else(|_| Err(SimulatorError::from(crate::RuntimeErrorCode::InternalError)));
    let elapsed = started.elapsed();
    match &result {
        Ok(_) => tracing::info!(
            ?kind,
            elapsed_ms = elapsed.as_millis() as u64,
            "tiered compilation completed"
        ),
        Err(error) => {
            tracing::warn!(?kind, elapsed_ms = elapsed.as_millis() as u64, error = %error, "tiered compilation failed")
        }
    }
    if let Ok(mut log) = log.lock() {
        log.push(TieredCompileStage {
            kind,
            elapsed,
            completed: result.is_ok(),
        });
    }
    result
}

// An optimized core can be adopted without the optional single-edge units.
struct NativeUpgrade {
    code: CompiledCode,
    derived_edges: bool,
}

struct DerivedCompileRequest {
    sent: bool,
    sender: mpsc::Sender<()>,
}

impl DerivedCompileRequest {
    fn request(&mut self) {
        if !std::mem::replace(&mut self.sent, true) {
            let _ = self.sender.send(());
        }
    }
}

/// Static analysis profile of the laid-out design.
#[derive(Clone, Copy, Debug, Default)]
struct DesignProfile {
    total_instructions: usize,
    has_unsupported_calls: bool,
    has_wide_registers: bool,
}

impl DesignProfile {
    fn analyze(laid_out: &LaidOutProgram) -> Self {
        let mut total_instructions: usize = 0;
        let mut has_unsupported_calls = false;
        let mut has_wide_registers = false;
        for unit in laid_out.sir.units() {
            if unit.register_map.values().any(|ty| ty.width() > 4096) {
                has_wide_registers = true;
            }
            for block in unit.blocks.values() {
                total_instructions = total_instructions.saturating_add(block.instructions.len());
                for inst in &block.instructions {
                    if matches!(inst, celox_sir::SIRInstruction::Call { .. }) {
                        has_unsupported_calls = true;
                    }
                }
            }
        }
        Self {
            total_instructions,
            has_unsupported_calls,
            has_wide_registers,
        }
    }

    fn requires_whole_image_escalation(&self) -> bool {
        self.has_unsupported_calls || self.has_wide_registers || self.total_instructions > 4096
    }
}

/// The tick counter of the program's compiled testbench, if it has one.
fn testbench_ticks(laid_out: &LaidOutProgram, interp: &InterpBackend) -> Option<SignalRef> {
    let scheduler = laid_out.runtime.testbench.as_ref()?.scheduler()?;
    Some(interp.resolve_signal(&scheduler.control[celox_testbench::TB_TICKS]))
}

/// A [`SimBackend`] that interprets immediately and promotes to generated
/// code as soon as background compilation completes.
pub struct TieredBackend {
    phase: Phase,
    promotion: Promotion,
    /// Event handles in trigger-id order, resolved once from the initial
    /// interpreter and valid across promotion thanks to the shared id space.
    events: Vec<TieredEventRef>,
    /// Shared with the background worker; flagged on drop or on an explicit
    /// cancellation request so the worker unwinds at the next task boundary.
    cancel: CompileCancel,
    /// Owned worker handle. Keeping this attached to the backend lets drop
    /// cancel and join compilation instead of leaking work into later users
    /// of the process (notably subsequent benchmark samples).
    compiler_worker: Option<JoinHandle<()>>,
    /// Second-stage receiver for a native baseline-pipeline build. After the
    /// first stage adopts a low-latency baseline image, this receiver yields
    /// optimized core and, if requested, the derived-edge image for in-place
    /// shared-code replacement.
    upgrade_receiver: Option<mpsc::Receiver<Result<NativeUpgrade, SimulatorError>>>,
    // Dropping the sender wakes a worker waiting for its first single-edge use.
    derived_compile_request: Option<DerivedCompileRequest>,
    /// The adopted code has the generated derived-edge units: a single-stage
    /// image or the optimizing one. The baseline image leaves them out, so
    /// their build cost lands in the optimizing tier only on demand.
    derived_edges_live: bool,
    /// Evaluate-only events whose apply phase has not run yet. Promotion is
    /// deferred while this is non-zero so the compiled apply phase always
    /// observes the interpreted evaluate-only results through an intact
    /// sparse-metadata image.
    pending_split_applies: usize,
    /// Minimum number of interpreted evaluation steps required before
    /// adoption ([`TierPromotion::AfterSteps`]). Zero means "as soon as the
    /// compiled tier is ready".
    promotion_threshold: u64,
    /// Evaluation steps executed on the interpreted tier so far.
    interpreted_steps: u64,
    /// Evaluation iterations executed after adopting generated code.
    compiled_steps: u64,
    /// Interpreter count captured at the successful promotion boundary.
    promoted_after_interpreted_steps: Option<u64>,
    /// Scheduler safe-point polls made while the interpreter was active.
    safe_point_polls: u64,
    /// Safe-point polls deferred by a split evaluate/apply pair.
    split_apply_deferrals: u64,
    /// Safe-point polls deferred by the configured step threshold.
    threshold_deferrals: u64,
    /// Opt-in benchmark timing. Kept out of deterministic execution stats so
    /// normal simulations neither read the clock nor expose noisy state.
    execution_timing: Option<ActiveExecutionTiming>,
    compile_log: CompileLog,
    /// The compiled testbench's tick counter, when the program has one.
    ticks: Option<SignalRef>,
    #[cfg(hotspot_jit)]
    promoted_hotspot_stats: Option<super::HotspotStats>,
    design_profile: DesignProfile,
}

impl TieredBackend {
    #[cfg(all(test, target_arch = "x86_64", not(feature = "arm64-codegen")))]
    pub(crate) fn publish_scheduler_for_test(&mut self) {
        self.keep_incremental_for_test();
        let Phase::Interpreting(Some(interp)) = &mut self.phase else {
            panic!("scheduler test requires the mixed executor");
        };
        interp.publish_scheduler_for_test();
        assert!(self.runs_compiled_scheduler());
        assert!(!self.is_compiled(), "must not use whole-image promotion");
    }

    #[cfg(all(test, target_arch = "x86_64", not(feature = "arm64-codegen")))]
    pub(crate) fn interp_for_test(&mut self) -> &mut crate::backend::InterpBackend {
        self.keep_incremental_for_test();
        let Phase::Interpreting(Some(interp)) = &mut self.phase else {
            panic!("scheduler test requires the mixed executor");
        };
        interp
    }

    /// Stop the background whole-image compiler so a test that inspects the
    /// mixed executor keeps it for the rest of the run. Large designs
    /// escalate to whole-image code once it is ready, which would otherwise
    /// replace the executor under test at a timing-dependent safe point.
    /// Hotspot compilation is left running.
    #[cfg(all(test, target_arch = "x86_64", not(feature = "arm64-codegen")))]
    fn keep_incremental_for_test(&mut self) {
        if !matches!(self.phase, Phase::Interpreting(Some(_))) {
            return;
        }
        self.upgrade_receiver = None;
        self.cancel.cancel();
        self.derived_compile_request = None;
        if matches!(self.promotion, Promotion::Pending(_)) {
            self.promotion = Promotion::Disabled;
        }
    }

    /// Build a tiered simulation targeting this host's default compiled
    /// tier: ready to run immediately on the interpreter while the compiled
    /// tier is prepared in the background.
    ///
    /// The native tier observes cancellation (see
    /// [`TieredBackend::cancel_background_compilation`]); the Cranelift tier
    /// runs to completion once started.
    pub fn new(laid_out: &LaidOutProgram, options: &SimulatorOptions) -> Self {
        Self::build(laid_out, None, options)
    }

    /// [`Self::new`] for a program the caller shares: the background
    /// compiler keeps a reference to it instead of a copy of its own.
    pub(crate) fn new_shared(laid_out: &Arc<LaidOutProgram>, options: &SimulatorOptions) -> Self {
        Self::build(laid_out, Some(laid_out), options)
    }

    fn build(
        laid_out: &LaidOutProgram,
        shared: Option<&Arc<LaidOutProgram>>,
        options: &SimulatorOptions,
    ) -> Self {
        #[cfg(hotspot_jit)]
        if options.hotspot_jit
            && matches!(options.tier_promotion, crate::TierPromotion::Always)
            && !options.trace.native
            && !options.trace.mir
            && options.optimize_options.parallel_lanes() == 1
            && !options.native_force_support
            && options.signal_access.force_signals.is_empty()
            && !DesignProfile::analyze(laid_out).requires_whole_image_escalation()
        {
            let mut interpreting = options.clone();
            interpreting.tier_promotion = crate::TierPromotion::Never;
            let mut backend = Self::with_compiler(laid_out, &interpreting, |_, _, _| {
                unreachable!("incremental execution does not start a whole-image compiler")
            });
            if let Phase::Interpreting(Some(interp)) = &mut backend.phase {
                if let Err(error) = interp.enable_hotspots(options) {
                    backend.promotion = Promotion::Failed(error);
                } else {
                    interp.prepare_hotspot_scheduler(laid_out, shared);
                }
            }
            return backend;
        }

        if native_is_default_target() {
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            {
                // The baseline pipeline only pays off when the first stage
                // will actually be adopted and no codegen trace needs the
                // optimizing pipeline's diagnostics.
                if cfg!(all(target_arch = "x86_64", not(feature = "arm64-codegen")))
                    && !options.trace.native
                    && !options.trace.mir
                    && !matches!(
                        options.tier_promotion,
                        crate::simulator::TierPromotion::Never
                    )
                {
                    let mut baseline_options = options.clone();
                    baseline_options.x86_options.baseline = true;
                    let mut optimizing_options = options.clone();
                    optimizing_options.x86_options.baseline = false;
                    return Self::with_two_stage_compiler(
                        &shared
                            .cloned()
                            .unwrap_or_else(|| Arc::new(laid_out.clone())),
                        &baseline_options,
                        optimizing_options,
                        |laid_out, options, cancel| {
                            use crate::backend::native::{NativeBackend, SharedNativeCode};
                            // Safety: the image was produced in-process by the
                            // Celox compiler above.
                            let image = NativeBackend::compile_image_with_cancel(
                                laid_out, options, cancel,
                            )?;
                            let shared = Arc::new(unsafe {
                                SharedNativeCode::from_image_with_dpi(image, &options.dpi)?
                            });
                            Ok(CompiledCode::Native(shared))
                        },
                    );
                }
                Self::with_compiler(laid_out, options, |laid_out, options, cancel| {
                    use crate::backend::native::{NativeBackend, SharedNativeCode};
                    // Honor native/MIR tracing requests the same way the
                    // direct compile_native path does: capture the codegen
                    // trace and print it when output was requested.
                    let wants_native_trace = options.trace.native || options.trace.mir;
                    let image = if wants_native_trace {
                        let (image, native_trace) =
                            NativeBackend::compile_image_with_cancel_and_trace(
                                laid_out, options, cancel,
                            )?;
                        let compilation = crate::debug::CompilationTrace {
                            native_optimized_sir: Some(native_trace.optimized_sir),
                            mir: Some(native_trace.mir),
                            reactive_event_graph: Some(native_trace.reactive_graph),
                            native_state_layout: Some(native_trace.state_layout),
                            ..crate::debug::CompilationTrace::default()
                        };
                        if options.trace.output_to_stdout {
                            compilation.print();
                        }
                        image
                    } else {
                        NativeBackend::compile_image_with_cancel(laid_out, options, cancel)?
                    };
                    // Safety: the image was produced in-process by the Celox
                    // compiler above.
                    let shared = Arc::new(unsafe {
                        SharedNativeCode::from_image_with_dpi(image, &options.dpi)?
                    });
                    Ok(CompiledCode::Native(shared))
                })
            }
            #[cfg(not(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            )))]
            {
                unreachable!("native tier selected on a host without native support")
            }
        } else {
            #[cfg(not(feature = "cranelift"))]
            {
                Self::with_compiler(laid_out, options, |_, _, _| {
                    Err(
                        crate::CodegenError::message("Cranelift requires the cranelift feature")
                            .into(),
                    )
                })
            }
            #[cfg(feature = "cranelift")]
            Self::with_compiler(laid_out, options, |laid_out, options, _cancel| {
                let mut trace = crate::debug::CompilationTrace::default();
                let wants_codegen_trace = options.trace.pre_optimized_clif
                    || options.trace.post_optimized_clif
                    || options.trace.native;
                let shared = Arc::new(JitBackend::compile(
                    laid_out,
                    options,
                    wants_codegen_trace.then_some(&mut trace),
                )?);
                if options.trace.output_to_stdout {
                    trace.print();
                }
                Ok(CompiledCode::Cranelift(shared))
            })
        }
    }

    /// A deterministic scheduler test: either the ordinary native executor,
    /// or the real mixed executor after its scheduler has been published.
    #[cfg(all(test, target_arch = "x86_64", not(feature = "arm64-codegen")))]
    pub(crate) fn for_scheduler_test(
        laid_out: &LaidOutProgram,
        options: &SimulatorOptions,
    ) -> Result<Self, SimulatorError> {
        if options.hotspot_jit {
            let mut interpreting = options.clone();
            interpreting.tier_promotion = crate::TierPromotion::Never;
            let mut backend = Self::with_compiler(laid_out, &interpreting, |_, _, _| {
                unreachable!("scheduler test requires the mixed executor")
            });
            if let Phase::Interpreting(Some(interp)) = &mut backend.phase {
                interp.enable_hotspots(options)?;
                interp.prepare_hotspot_scheduler(laid_out, None);
            }
            Ok(backend)
        } else {
            let mut interpreting = options.clone();
            interpreting.tier_promotion = crate::TierPromotion::Never;
            let mut backend =
                Self::with_compiler(laid_out, &interpreting, |_, _, _| unreachable!());
            backend.phase = Phase::Compiled(CompiledTier::Native(Box::new(
                crate::NativeBackend::new(laid_out, options)?,
            )));
            backend.promotion = Promotion::Promoted;
            backend.derived_edges_live = true;
            Ok(backend)
        }
    }

    /// Build a tiered simulation with a custom compilation step.
    ///
    /// `compile` runs on the worker thread and receives the backend's
    /// cancellation token; a cancelled compile should return an error so the
    /// simulation stays on the interpreter. Tests use this to gate or stub
    /// compilation so promotion timing is deterministic.
    pub(crate) fn with_compiler<F>(
        laid_out: &LaidOutProgram,
        options: &SimulatorOptions,
        compile: F,
    ) -> Self
    where
        F: FnOnce(
                &LaidOutProgram,
                &SimulatorOptions,
                &CompileCancel,
            ) -> Result<CompiledCode, SimulatorError>
            + Send
            + 'static,
    {
        let mut interp = Box::new(
            InterpBackend::new(laid_out, options)
                .expect("interpreter construction cannot fail for a laid-out program"),
        );
        // Reserve promotion headroom before anything can observe the image:
        // the compiled tier's adoption may grow the image beyond the semantic
        // state (native spill/scratch arenas, Cranelift MemorySpilled plans),
        // and adoption must grow within this allocation so the live image
        // never moves. The slack covers the measured arena sizes with
        // margin; a design that outgrows it declines promotion (recorded via
        // `promotion_error`) instead of reallocating. Lane-partitioned
        // kernels give every lane its own arena, so the slack scales with
        // the lane count.
        if !matches!(
            options.tier_promotion,
            crate::simulator::TierPromotion::Never
        ) {
            let len = interp.image_word_len();
            let lanes = options.optimize_options.parallel_lanes() as usize;
            let slack = (len.max(1024) / 8).saturating_mul(lanes);
            #[cfg(hotspot_jit)]
            let hotspot_slack = if options.hotspot_jit {
                crate::backend::hotspot::SCRATCH_BYTES / 8
            } else {
                0
            };
            #[cfg(not(hotspot_jit))]
            let hotspot_slack = 0;
            interp.reserve_image_capacity(len + slack.max(1024) + hotspot_slack);
        }
        #[cfg(hotspot_jit)]
        if options.hotspot_jit
            && !matches!(
                options.tier_promotion,
                crate::simulator::TierPromotion::Never
            )
            && !options.trace.native
            && !options.trace.mir
            && options.optimize_options.parallel_lanes() == 1
            && !options.native_force_support
            && options.signal_access.force_signals.is_empty()
        {
            if let Err(error) = interp.enable_hotspots(options) {
                tracing::warn!("failed to enable hotspot JIT on tiered backend: {error}");
            } else {
                interp.prepare_hotspot_scheduler(laid_out, None);
            }
        }
        let events = interp
            .id_to_event_slice()
            .iter()
            .map(|ev| TieredEventRef {
                addr: ev.addr(),
                id: ev.id(),
            })
            .collect();

        let cancel = CompileCancel::new();
        let worker_cancel = cancel.clone();
        let compile_log = CompileLog::default();
        let worker_log = compile_log.clone();
        let ticks = testbench_ticks(laid_out, &interp);
        // A failed spawn keeps the interpreter as the permanent tier with the
        // reason recorded, matching the background-failure policy instead of
        // panicking the embedding application.
        let (promotion, compiler_worker) = if matches!(
            options.tier_promotion,
            crate::simulator::TierPromotion::Never
        ) {
            // Policy says interpreted-only: skip the worker entirely so a
            // never-promoted simulation pays nothing beyond the interpreter.
            (Promotion::Disabled, None)
        } else {
            let (sender, receiver) = mpsc::channel();
            let background_laid_out = laid_out.clone();
            let background_options = options.clone();
            match std::thread::Builder::new()
                .name("celox-jit-compile".to_string())
                .spawn(move || {
                    #[cfg(target_os = "linux")]
                    unsafe {
                        let thread = libc::syscall(libc::SYS_gettid) as libc::id_t;
                        libc::setpriority(libc::PRIO_PROCESS, thread, 10);
                    }
                    // The result is delivered through the channel instead of the
                    // join handle so safe-point polls never block on compilation.
                    // A panicked worker surfaces as a disconnected channel and
                    // keeps the simulation on the interpreter permanently.
                    let span = tracing::info_span!(
                        "celox.tiered.compile",
                        target = if native_is_default_target() {
                            "native"
                        } else {
                            "cranelift"
                        }
                    );
                    let result = span.in_scope(|| {
                        logged(&worker_log, TieredCompileKind::Single, || {
                            compile(&background_laid_out, &background_options, &worker_cancel)
                        })
                    });
                    let _ = sender.send(result);
                }) {
                Ok(handle) => (Promotion::Pending(receiver), Some(handle)),
                Err(error) => (
                    Promotion::Failed(SimulatorError::new(crate::SimulatorErrorKind::Codegen(
                        crate::CodegenError::message(format!(
                            "failed to spawn the background compiler thread: {error}"
                        )),
                    ))),
                    None,
                ),
            }
        };

        let promotion_threshold = match options.tier_promotion {
            crate::simulator::TierPromotion::AfterSteps(steps) => steps,
            _ => 0,
        };

        let design_profile = DesignProfile::analyze(laid_out);
        Self {
            phase: Phase::Interpreting(Some(interp)),
            promotion,
            events,
            cancel,
            compiler_worker,
            upgrade_receiver: None,
            derived_compile_request: None,
            derived_edges_live: false,
            pending_split_applies: 0,
            promotion_threshold,
            interpreted_steps: 0,
            compiled_steps: 0,
            promoted_after_interpreted_steps: None,
            safe_point_polls: 0,
            split_apply_deferrals: 0,
            threshold_deferrals: 0,
            execution_timing: None,
            compile_log,
            ticks,
            #[cfg(hotspot_jit)]
            promoted_hotspot_stats: None,
            design_profile,
        }
    }

    /// Build a tiered simulation whose native worker compiles a low-latency
    /// baseline image first (`baseline_options`) and then keeps compiling the
    /// optimizing image (`optimizing_options`) on the same worker thread.
    /// Single-edge derived units are omitted until a host requests them. An
    /// early request keeps the original two-stage build; a later request adds
    /// a final image after the optimized core is already available.
    ///
    /// The baseline result adopts exactly like a single-stage build; the
    /// optimizing result is later swapped in at a scheduler safe point. A
    /// failed baseline leaves the interpreter as the permanent tier, and a
    /// failed or cancelled optimizing stage keeps the baseline code.
    pub(crate) fn with_two_stage_compiler<F>(
        laid_out: &Arc<LaidOutProgram>,
        baseline_options: &SimulatorOptions,
        optimizing_options: SimulatorOptions,
        compile: F,
    ) -> Self
    where
        F: Fn(
                &LaidOutProgram,
                &SimulatorOptions,
                &CompileCancel,
            ) -> Result<CompiledCode, SimulatorError>
            + Send
            + 'static,
    {
        let mut interp = Box::new(
            InterpBackend::new(laid_out, baseline_options)
                .expect("interpreter construction cannot fail for a laid-out program"),
        );
        // Two-stage builds must fit both native images inside one
        // allocation. The baseline image omits the optimizing tier's
        // cross-unit cleanup, so its spill arenas run larger; reserve double
        // the single-stage slack before the image can be observed.
        let len = interp.image_word_len();
        let slack = len.max(1024) / 4;
        #[cfg(hotspot_jit)]
        let hotspot_slack = if baseline_options.hotspot_jit {
            crate::backend::hotspot::SCRATCH_BYTES / 8
        } else {
            0
        };
        #[cfg(not(hotspot_jit))]
        let hotspot_slack = 0;
        interp.reserve_image_capacity(len + slack.max(1024) + hotspot_slack);
        #[cfg(hotspot_jit)]
        if baseline_options.hotspot_jit
            && !matches!(
                baseline_options.tier_promotion,
                crate::simulator::TierPromotion::Never
            )
            && !baseline_options.trace.native
            && !baseline_options.trace.mir
            && baseline_options.optimize_options.parallel_lanes() == 1
            && !baseline_options.native_force_support
            && baseline_options.signal_access.force_signals.is_empty()
        {
            if let Err(error) = interp.enable_hotspots(baseline_options) {
                tracing::warn!("failed to enable hotspot JIT in two-stage tiered backend: {error}");
            } else {
                interp.prepare_hotspot_scheduler(laid_out, Some(laid_out));
            }
        }
        let events = interp
            .id_to_event_slice()
            .iter()
            .map(|ev| TieredEventRef {
                addr: ev.addr(),
                id: ev.id(),
            })
            .collect();

        let cancel = CompileCancel::new();
        let worker_cancel = cancel.clone();
        let compile_log = CompileLog::default();
        let worker_log = compile_log.clone();
        let ticks = testbench_ticks(laid_out, &interp);
        let (baseline_sender, baseline_receiver) = mpsc::channel();
        let (upgrade_sender, upgrade_receiver) = mpsc::channel();
        let (derived_compile_request, derived_receiver) =
            if laid_out.runtime.runtime_schema.derived_edges.is_empty() {
                (None, None)
            } else {
                let (sender, receiver) = mpsc::channel();
                (
                    Some(DerivedCompileRequest {
                        sent: false,
                        sender,
                    }),
                    Some(receiver),
                )
            };
        // The worker shares the program with whoever built it, and derives
        // the reduced programs of its stages one at a time: each is a copy
        // of the whole program, and a large design does not fit several.
        let background_laid_out = Arc::clone(laid_out);
        let wiring_clocks = !baseline_options.native_force_support
            && baseline_options.signal_access.force_signals.is_empty();
        let worker_baseline_options = baseline_options.clone();
        // A failed spawn keeps the interpreter as the permanent tier with the
        // reason recorded, matching the single-stage background-failure policy.
        let (promotion, compiler_worker) = match std::thread::Builder::new()
            .name("celox-jit-compile".to_string())
            .spawn(move || {
                #[cfg(target_os = "linux")]
                unsafe {
                    let thread = libc::syscall(libc::SYS_gettid) as libc::id_t;
                    libc::setpriority(libc::PRIO_PROCESS, thread, 10);
                }
                let span = tracing::info_span!("celox.tiered.compile", target = "native");
                span.in_scope(|| {
                    // The baseline image runs derived edges by walking their
                    // plans, and settles before each domain rather than
                    // compiling a fused copy of the settle per domain into
                    // the scheduler.
                    let first_tier_laid_out = crate::scheduler::without_fused_domains(
                        crate::derived_edge::without_units(&background_laid_out),
                        wiring_clocks,
                    );
                    let result = logged(&worker_log, TieredCompileKind::Baseline, || {
                        compile(
                            &first_tier_laid_out,
                            &worker_baseline_options,
                            &worker_cancel,
                        )
                    });
                    drop(first_tier_laid_out);
                    let ready = result.is_ok();
                    let delivered = baseline_sender.send(result).is_ok();
                    // A failed baseline leaves nothing to replace; the
                    // simulation stays interpreted and the worker retires.
                    if !delivered || !ready || worker_cancel.is_cancelled() {
                        return;
                    }
                    // Preserve the existing two-stage path when single-edge
                    // execution has already requested the specialized units.
                    // Coincident-edge hosts never request them, so publish an
                    // optimized core without paying for unused edge kernels.
                    let defer_edges = match derived_receiver.as_ref().map(|rx| rx.try_recv()) {
                        Some(Err(mpsc::TryRecvError::Empty)) => true,
                        Some(Err(mpsc::TryRecvError::Disconnected)) => return,
                        _ => false,
                    };
                    // The core without the derived-edge units, while it is
                    // being compiled.
                    let core = defer_edges
                        .then(|| crate::derived_edge::without_units(&background_laid_out));
                    let optimized: &LaidOutProgram = core.as_ref().unwrap_or(&background_laid_out);
                    // Without derived edges there are no units to defer.
                    let kind = if defer_edges || derived_receiver.is_none() {
                        TieredCompileKind::Optimizing
                    } else {
                        TieredCompileKind::DerivedEdges
                    };
                    let result = logged(&worker_log, kind, || {
                        compile(optimized, &optimizing_options, &worker_cancel)
                    });
                    let ready = result.is_ok();
                    let delivered = upgrade_sender
                        .send(result.map(|code| NativeUpgrade {
                            code,
                            derived_edges: !defer_edges,
                        }))
                        .is_ok();
                    if !defer_edges || !ready || !delivered || worker_cancel.is_cancelled() {
                        return;
                    }
                    // Only the full program is needed for a late edge request.
                    // Release the core IR copy before going idle.
                    drop(core);
                    // This wait consumes no CPU. Cancellation and Drop close
                    // the sender before joining, including when no host ever
                    // uses a single-edge specialized unit.
                    if derived_receiver.as_ref().unwrap().recv().is_err()
                        || worker_cancel.is_cancelled()
                    {
                        return;
                    }
                    let result = logged(&worker_log, TieredCompileKind::DerivedEdges, || {
                        compile(&background_laid_out, &optimizing_options, &worker_cancel)
                    });
                    let _ = upgrade_sender.send(result.map(|code| NativeUpgrade {
                        code,
                        derived_edges: true,
                    }));
                });
            }) {
            Ok(handle) => (Promotion::Pending(baseline_receiver), Some(handle)),
            Err(error) => (
                Promotion::Failed(SimulatorError::new(crate::SimulatorErrorKind::Codegen(
                    crate::CodegenError::message(format!(
                        "failed to spawn the background compiler thread: {error}"
                    )),
                ))),
                None,
            ),
        };

        let promotion_threshold = match baseline_options.tier_promotion {
            crate::simulator::TierPromotion::AfterSteps(steps) => steps,
            _ => 0,
        };

        let design_profile = DesignProfile::analyze(laid_out);
        Self {
            phase: Phase::Interpreting(Some(interp)),
            promotion,
            events,
            cancel,
            compiler_worker,
            upgrade_receiver: Some(upgrade_receiver),
            derived_compile_request,
            derived_edges_live: false,
            pending_split_applies: 0,
            promotion_threshold,
            interpreted_steps: 0,
            compiled_steps: 0,
            promoted_after_interpreted_steps: None,
            safe_point_polls: 0,
            split_apply_deferrals: 0,
            threshold_deferrals: 0,
            execution_timing: None,
            compile_log,
            ticks,
            #[cfg(hotspot_jit)]
            promoted_hotspot_stats: None,
            design_profile,
        }
    }

    /// Whether the compiled tier has been adopted.
    pub fn is_compiled(&self) -> bool {
        matches!(self.phase, Phase::Compiled(_))
    }

    /// Incremental entry counts. Whole-image promotion statistics describe the
    /// owning interpreter and do not count native calls inside mixed execution.
    #[cfg(hotspot_jit)]
    pub fn hotspot_stats(&self) -> Option<super::HotspotStats> {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.hotspot_stats(),
            _ => self.promoted_hotspot_stats.clone(),
        }
    }

    /// Return the static analysis profile of the design: (total instructions, requires whole-image escalation).
    pub fn design_profile(&self) -> (usize, bool) {
        (
            self.design_profile.total_instructions,
            self.design_profile.requires_whole_image_escalation(),
        )
    }

    /// Return deterministic lifecycle and execution measurements.
    pub fn execution_stats(&self) -> TieredExecutionStats {
        TieredExecutionStats {
            tier: if self.is_compiled() {
                TieredExecutionTier::Compiled
            } else {
                TieredExecutionTier::Interpreter
            },
            promotion: match &self.promotion {
                Promotion::Pending(_) => TieredPromotionStatus::Pending,
                Promotion::Failed(_) => TieredPromotionStatus::Failed,
                Promotion::Disabled => TieredPromotionStatus::Disabled,
                Promotion::Promoted => TieredPromotionStatus::Promoted,
            },
            interpreted_evaluations: self.interpreted_steps,
            compiled_evaluations: self.compiled_steps,
            promoted_after_interpreted_evaluations: self.promoted_after_interpreted_steps,
            safe_point_polls: self.safe_point_polls,
            split_apply_deferrals: self.split_apply_deferrals,
            threshold_deferrals: self.threshold_deferrals,
        }
    }

    /// Start measuring time to promotion for a benchmark workload.
    pub fn start_execution_timing(&mut self) {
        let tick = self.tick();
        self.execution_timing = Some(ActiveExecutionTiming {
            started_at: Instant::now(),
            promotion: self.is_compiled().then_some(TierTransition {
                elapsed: Duration::ZERO,
                tick,
            }),
            upgrade: None,
        });
    }

    /// Stop timing and return the observed tier transitions.
    pub fn finish_execution_timing(&mut self) -> Option<TieredExecutionTiming> {
        let finish_tick = self.tick();
        let mut compiles = [None; 3];
        if let Ok(log) = self.compile_log.lock() {
            for (slot, stage) in compiles.iter_mut().zip(log.iter()) {
                *slot = Some(*stage);
            }
        }
        self.execution_timing
            .take()
            .map(|timing| TieredExecutionTiming {
                promotion: timing.promotion,
                upgrade: timing.upgrade,
                finish_tick,
                compiles,
            })
    }

    /// The compiled testbench's tick, when one runs.
    fn tick(&self) -> Option<u64> {
        self.ticks.map(|ticks| self.get_as::<u64>(ticks))
    }

    /// Record a tier transition for opt-in timing.
    fn transition(&self) -> Option<TierTransition> {
        let timing = self.execution_timing.as_ref()?;
        Some(TierTransition {
            elapsed: timing.started_at.elapsed(),
            tick: self.tick(),
        })
    }

    /// Why promotion has not happened yet, for diagnostics.
    ///
    /// Returns `None` once running fully compiled or while background
    /// compilation is still in progress.
    pub fn promotion_error(&self) -> Option<&SimulatorError> {
        match &self.promotion {
            Promotion::Failed(error) => Some(error),
            _ => None,
        }
    }

    /// Request cancellation of background compilation.
    ///
    /// The native-tier worker unwinds at its next task boundary. Before the
    /// first promotion, the simulation stays interpreted and the reason is
    /// available through [`TieredBackend::promotion_error`]; after baseline
    /// adoption, cancellation keeps the last adopted code running without
    /// further upgrades and releases a worker waiting for a derived-edge request.
    /// A result that already reached the channel (or arrives afterwards from
    /// a tier that ignores the token) is rejected so cancellation is
    /// honored regardless of compiler timing. Returns whether a background
    /// compilation was still pending, so callers that only want to reclaim
    /// a finished worker can ignore the call cheaply.
    pub fn cancel_background_compilation(&mut self) -> bool {
        #[cfg(hotspot_jit)]
        let hotspot_cancelled = if let Phase::Interpreting(Some(interp)) = &mut self.phase {
            interp.cancel_hotspots()
        } else {
            false
        };
        #[cfg(not(hotspot_jit))]
        let hotspot_cancelled = false;
        let promotion_pending = matches!(self.promotion, Promotion::Pending(_));
        let upgrade_pending = self.upgrade_receiver.take().is_some();
        self.cancel.cancel();
        self.derived_compile_request = None;
        if promotion_pending {
            self.promotion = Promotion::Failed(super::compile_cancel::cancelled_error());
        }
        promotion_pending || upgrade_pending || hotspot_cancelled
    }

    /// Record one evaluation iteration against the currently active tier.
    /// Interpreter iterations also feed the [`TierPromotion::AfterSteps`]
    /// adoption threshold.
    fn count_evaluation_for_current_tier(&mut self) {
        if matches!(self.phase, Phase::Interpreting(Some(_))) {
            self.interpreted_steps = self.interpreted_steps.saturating_add(1);
        } else if matches!(self.phase, Phase::Compiled(_)) {
            self.compiled_steps = self.compiled_steps.saturating_add(1);
        }
    }

    /// Adopt the compiled tier if background compilation finished. Called at
    /// scheduler safe points (between evaluation phases) where no unit is
    /// mid-execution and the memory image can move atomically from the
    /// caller's perspective.
    fn maybe_promote(&mut self) {
        if !matches!(self.phase, Phase::Interpreting(Some(_))) {
            return;
        }
        self.safe_point_polls = self.safe_point_polls.saturating_add(1);
        // Never promote inside a split evaluate/apply pair: the compiled
        // apply phase must observe the interpreted evaluate-only results,
        // and adoption clears the sparse metadata they are tracked in.
        if self.pending_split_applies > 0 {
            self.split_apply_deferrals = self.split_apply_deferrals.saturating_add(1);
            return;
        }
        // Honor the minimum interpreted-step count so short simulations are
        // not disrupted by an adoption their remaining run cannot amortize.
        if self.interpreted_steps < self.promotion_threshold {
            self.threshold_deferrals = self.threshold_deferrals.saturating_add(1);
            return;
        }
        let Promotion::Pending(receiver) = &self.promotion else {
            return;
        };
        // Cancellation wins over a queued success: a compile that finished
        // before (or ignores) the flag must not adopt after the request.
        if self.cancel.is_cancelled() {
            self.promotion = Promotion::Failed(super::compile_cancel::cancelled_error());
            return;
        }
        let Ok(result) = receiver.try_recv() else {
            return;
        };

        let Ok(code) = result else {
            // Compilation errors keep the simulation on the interpreter;
            // the reason stays retrievable via promotion_error().
            self.promotion = match result {
                Err(error) => Promotion::Failed(error),
                Ok(_) => unreachable!("checked above"),
            };
            self.cancel_background_compilation();
            return;
        };
        // Adoption grows the transferred image within its existing allocation
        // so promotion never moves the live memory image (external views such
        // as zero-copy host buffers may still reference it). If the compiled
        // tier outgrew the reserved interpreter arena, decline promotion and
        // stay interpreted instead of reallocating.
        if let Phase::Interpreting(Some(interp)) = &mut self.phase {
            let required = code.required_image_words();
            if interp.image_word_capacity() < required {
                self.promotion = Promotion::Failed(SimulatorError::new(
                    crate::SimulatorErrorKind::Codegen(crate::CodegenError::message(format!(
                        "native image needs {required} words but the reserved interpreter \
                         image only has {} words of capacity; refusing to move the live \
                         memory image and staying interpreted",
                        interp.image_word_capacity()
                    ))),
                ));
                self.cancel_background_compilation();
                return;
            }
        }
        let mut adopted = None;
        if let Phase::Interpreting(slot) = &mut self.phase {
            if let Some(mut interp) = slot.take() {
                #[cfg(hotspot_jit)]
                {
                    self.promoted_hotspot_stats = interp.hotspot_stats();
                }
                let (memory, runtime_event_buffer, comb_capture_enabled, heap) =
                    interp.tier_transfer();
                drop(interp);
                adopted = Some(CompiledTier::adopt(
                    code,
                    memory,
                    runtime_event_buffer,
                    comb_capture_enabled,
                    heap,
                ));
            }
        }
        if let Some(mut compiled) = adopted {
            let promotion_span = tracing::info_span!(
                "celox.tiered.promote",
                interpreted_evaluations = self.interpreted_steps
            );
            let _entered = promotion_span.enter();
            // Clear sparse metadata so the compiled tier starts with clean
            // tracking; stale dirty bits from interpreted-tier evaluation
            // could confuse the compiled commit logic.
            let sparse_metas: Vec<_> = {
                let layout = compiled.layout();
                layout
                    .sparse_layouts
                    .values()
                    .map(|s| {
                        (
                            s.dirty_words_offset,
                            s.dirty_word_count * 8,
                            s.summary_words_offset,
                            s.summary_word_count * 8,
                        )
                    })
                    .collect()
            };
            unsafe {
                let base = compiled.memory_base_mut();
                for &(dwo, dwc, swo, swc) in &sparse_metas {
                    std::ptr::write_bytes(base.add(dwo), 0, dwc);
                    std::ptr::write_bytes(base.add(swo), 0, swc);
                }
            }
            self.phase = Phase::Compiled(compiled);
            // A second stage still to come means this is the baseline image.
            self.derived_edges_live = self.upgrade_receiver.is_none();
            self.promotion = Promotion::Promoted;
            self.promoted_after_interpreted_steps = Some(self.interpreted_steps);
            let transition = self.transition();
            if let Some(timing) = &mut self.execution_timing
                && timing.promotion.is_none()
            {
                timing.promotion = transition;
            }
            tracing::info!(tick = self.tick(), "tiered backend adopted generated code");
        }
    }

    /// Swap the adopted native image for the optimizing replacement when a
    /// baseline-pipeline build finishes an optimizing compilation stage.
    ///
    /// Like interpreter promotion, the replacement grows the live image
    /// within its existing allocation; the allocation never moves, so
    /// external views stay valid. Failure or cancellation keeps the
    /// last adopted code running for the rest of the simulation.
    fn maybe_upgrade(&mut self) {
        if self.cancel.is_cancelled() {
            self.upgrade_receiver = None;
            return;
        }
        if !matches!(self.phase, Phase::Compiled(_)) || self.pending_split_applies > 0 {
            return;
        }
        let Some(receiver) = self.upgrade_receiver.take() else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            // The optimizing stage retired without a result (worker died or
            // the baseline never adopted): stop polling; the baseline code
            // runs for the rest of the simulation.
            Err(mpsc::TryRecvError::Disconnected) => return,
            Err(mpsc::TryRecvError::Empty) => {
                self.upgrade_receiver = Some(receiver);
                return;
            }
        };
        let upgrade = match result {
            Ok(code) => code,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "tiered optimizing compilation failed; keeping baseline code"
                );
                return;
            }
        };
        if !upgrade.derived_edges {
            self.upgrade_receiver = Some(receiver);
        }
        let NativeUpgrade {
            code,
            derived_edges,
        } = upgrade;
        if derived_edges {
            self.derived_compile_request = None;
        }
        let required = code.required_image_words();
        if let Phase::Compiled(compiled) = &mut self.phase
            && compiled.memory_word_capacity() < required
        {
            tracing::warn!(
                required_words = required,
                "tiered optimizing image exceeds the reserved image; keeping baseline code"
            );
            return;
        }
        if let Phase::Compiled(compiled) = &mut self.phase
            && compiled.replace_native_code(code)
        {
            tracing::info!(
                derived_edges,
                tick = self.tick(),
                "tiered backend adopted optimizing native code"
            );
            self.derived_edges_live = derived_edges;
            let transition = self.transition();
            if let Some(timing) = &mut self.execution_timing
                && timing.upgrade.is_none()
            {
                timing.upgrade = transition;
            }
        }
    }
}

impl Drop for TieredBackend {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.derived_compile_request = None;
        if let Some(worker) = self.compiler_worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests;
