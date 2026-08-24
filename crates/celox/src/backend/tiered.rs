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
//! Promotion is whole-program: once the compiled tier is adopted it is used
//! for the remainder of the simulation. The interpreter remains the permanent
//! fallback whenever background compilation fails.
//!
//! Background compilation is cooperatively cancellable on the native tier:
//! dropping the backend (or calling
//! [`TieredBackend::cancel_background_compilation`]) flags the worker, which
//! unwinds at the next task boundary instead of finishing the image. The
//! Cranelift tier runs to completion once started.

#![cfg(feature = "host-runtime")]

use std::sync::Arc;
use std::sync::mpsc;

use num_bigint::BigUint;

use super::compile_cancel::CompileCancel;
use super::{
    EventHandle, MemoryLayout, RuntimeEventBuffer, SharedJitCode, SimBackend, SimulatorErrorCode,
};
#[cfg(any(
    target_arch = "x86_64",
    feature = "arm64-codegen",
    target_arch = "aarch64"
))]
use crate::backend::native::SharedNativeCode;
use crate::backend::{InterpBackend, JitBackend};
use crate::{
    SimulatorError, SimulatorOptions,
    ir::{AbsoluteAddr, LaidOutProgram, SignalRef},
};

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

/// The memory-layout mode for tiered execution.
///
/// Uses the selected compiled tier's required mode. On native hosts this is
/// ElementStrided (a native-tier optimization for unpacked arrays); the
/// interpreter implements matching strided addressing.
pub(crate) fn default_target_layout_mode() -> crate::backend::memory_layout::MemoryLayoutMode {
    if native_is_default_target() {
        crate::backend::memory_layout::MemoryLayoutMode::ElementStrided
    } else {
        crate::backend::memory_layout::MemoryLayoutMode::Packed
    }
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

/// The adopted compiled tier.
enum CompiledTier {
    Jit(Box<JitBackend>),
    #[cfg(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    ))]
    Native(Box<crate::backend::native::NativeBackend>),
}

impl CompiledTier {
    /// Bind compiled code to a live simulation state handed over by the
    /// interpreter during promotion.
    fn adopt(
        code: CompiledCode,
        memory: Vec<u64>,
        runtime_event_buffer: Arc<RuntimeEventBuffer>,
        comb_capture_enabled: Vec<u8>,
    ) -> Self {
        match code {
            CompiledCode::Cranelift(shared) => {
                Self::Jit(Box::new(JitBackend::adopt_shared_with_state(
                    shared,
                    memory,
                    runtime_event_buffer,
                    comb_capture_enabled,
                )))
            }
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            CompiledCode::Native(shared) => {
                // Native emission reserves trailing spill/scratch arena beyond
                // the semantic state, so grow the transferred image to the
                // size `NativeBackend::from_shared` would allocate; the extra
                // tail is fresh scratch the interpreter never touched.
                let target_words = shared.native_memory_size.div_ceil(8) + 1;
                let mut memory = memory;
                memory.resize(target_words, 0);
                Self::Native(Box::new(
                    crate::backend::native::NativeBackend::adopt_shared_with_state(
                        shared,
                        memory,
                        runtime_event_buffer,
                        comb_capture_enabled,
                    ),
                ))
            }
        }
    }

    fn layout(&self) -> &MemoryLayout {
        match self {
            Self::Jit(jit) => jit.layout(),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Self::Native(native) => native.layout(),
        }
    }

    fn memory_base_mut(&mut self) -> *mut u8 {
        match self {
            Self::Jit(jit) => jit.memory_as_mut_ptr().0,
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Self::Native(native) => native.memory_as_mut_ptr().0,
        }
    }

    fn eval_comb(&mut self) -> Result<(), SimulatorErrorCode> {
        match self {
            Self::Jit(jit) => jit.eval_comb(),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Self::Native(native) => native.eval_comb(),
        }
    }

    /// Resolve the tier's own event handle for a shared trigger id. Every
    /// backend indexes its event table by the same deterministic id space,
    /// so translation is an array lookup.
    fn eval_apply_ff_at(&mut self, id: usize) -> Result<(), SimulatorErrorCode> {
        match self {
            Self::Jit(jit) => {
                let event = jit.id_to_event_slice()[id];
                jit.eval_apply_ff_at(event)
            }
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Self::Native(native) => {
                let event = native.id_to_event_slice()[id];
                native.eval_apply_ff_at(event)
            }
        }
    }

    fn eval_comb_apply_ff_at(&mut self, id: usize) -> Result<(), SimulatorErrorCode> {
        match self {
            Self::Jit(jit) => {
                let event = jit.id_to_event_slice()[id];
                jit.eval_comb_apply_ff_at(event)
            }
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Self::Native(native) => {
                let event = native.id_to_event_slice()[id];
                native.eval_comb_apply_ff_at(event)
            }
        }
    }

    /// Execute up to `count` fused ticks. The Cranelift tier has no
    /// in-generated-code batch loop, so it completes one tick per call and
    /// lets the caller re-poll; the native tier runs the whole batch inside
    /// generated code when `native_tick_loop` is enabled.
    fn eval_comb_apply_ff_many_at(
        &mut self,
        id: usize,
        count: u64,
    ) -> (u64, Result<(), SimulatorErrorCode>) {
        match self {
            Self::Jit(jit) => (1, {
                let event = jit.id_to_event_slice()[id];
                jit.eval_comb_apply_ff_at(event)
            }),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Self::Native(native) => {
                native.eval_comb_apply_ff_many_at(native.id_to_event_slice()[id], count)
            }
        }
    }

    /// Resolve the phase-specific evaluate-only event for a shared trigger
    /// address and execute it.
    ///
    /// Evaluate-only and apply functions live in their own per-phase id
    /// spaces, so the combined `id_to_event_slice` entry must not be used
    /// here; the address resolves through each tier's phase-specific map.
    fn eval_only_ff_at_addr(&mut self, addr: &AbsoluteAddr) -> Result<(), SimulatorErrorCode> {
        match self {
            Self::Jit(jit) => {
                let event = jit
                    .resolve_eval_only_event(addr)
                    .unwrap_or_else(|| panic!("eval-only event not found for {addr:?}"));
                jit.eval_only_ff_at(event)
            }
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Self::Native(native) => {
                let event = native
                    .resolve_eval_only_event(addr)
                    .unwrap_or_else(|| panic!("eval-only event not found for {addr:?}"));
                native.eval_only_ff_at(event)
            }
        }
    }

    /// Resolve the phase-specific apply event for a shared trigger address
    /// and execute it.
    fn apply_ff_at_addr(&mut self, addr: &AbsoluteAddr) -> Result<(), SimulatorErrorCode> {
        match self {
            Self::Jit(jit) => {
                let event = jit
                    .resolve_apply_event(addr)
                    .unwrap_or_else(|| panic!("apply event not found for {addr:?}"));
                jit.apply_ff_at(event)
            }
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Self::Native(native) => {
                let event = native
                    .resolve_apply_event(addr)
                    .unwrap_or_else(|| panic!("apply event not found for {addr:?}"));
                native.apply_ff_at(event)
            }
        }
    }
}

/// Compiled code produced by the background worker, before it is bound to a
/// live simulation state at promotion time.
pub(crate) enum CompiledCode {
    Cranelift(Arc<SharedJitCode>),
    #[cfg(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    ))]
    Native(Arc<SharedNativeCode>),
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
    Promoted,
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
    /// Evaluate-only events whose apply phase has not run yet. Promotion is
    /// deferred while this is non-zero so the compiled apply phase always
    /// observes the interpreted evaluate-only results through an intact
    /// sparse-metadata image.
    pending_split_applies: usize,
}

impl TieredBackend {
    /// Build a tiered simulation targeting this host's default compiled
    /// tier: ready to run immediately on the interpreter while the compiled
    /// tier is prepared in the background.
    ///
    /// The native tier observes cancellation (see
    /// [`TieredBackend::cancel_background_compilation`]); the Cranelift tier
    /// runs to completion once started.
    pub fn new(laid_out: &LaidOutProgram, options: &SimulatorOptions) -> Self {
        if native_is_default_target() {
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            {
                Self::with_compiler(laid_out, options, |laid_out, options, cancel| {
                    use crate::backend::native::{NativeBackend, SharedNativeCode};
                    let image =
                        NativeBackend::compile_image_with_cancel(laid_out, options, cancel)?;
                    // Safety: the image was produced in-process by the Celox
                    // compiler above.
                    let shared = Arc::new(unsafe { SharedNativeCode::from_image(image)? });
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
        let interp = Box::new(
            InterpBackend::new(laid_out, options)
                .expect("interpreter construction cannot fail for a laid-out program"),
        );
        let events = interp
            .id_to_event_slice()
            .iter()
            .map(|ev| TieredEventRef {
                addr: ev.addr(),
                id: ev.id(),
            })
            .collect();

        let (sender, receiver) = mpsc::channel();
        let background_laid_out = laid_out.clone();
        let background_options = options.clone();
        let cancel = CompileCancel::new();
        let worker_cancel = cancel.clone();
        // A failed spawn keeps the interpreter as the permanent tier with the
        // reason recorded, matching the background-failure policy instead of
        // panicking the embedding application.
        let promotion = match std::thread::Builder::new()
            .name("celox-jit-compile".to_string())
            .spawn(move || {
                // The result is delivered through the channel instead of the
                // join handle so safe-point polls never block on compilation.
                // A panicked worker surfaces as a disconnected channel and
                // keeps the simulation on the interpreter permanently.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    compile(&background_laid_out, &background_options, &worker_cancel)
                }))
                .unwrap_or_else(|_| {
                    Err(SimulatorError::from(crate::RuntimeErrorCode::InternalError))
                });
                let _ = sender.send(result);
            }) {
            Ok(_handle) => Promotion::Pending(receiver),
            Err(error) => Promotion::Failed(SimulatorError::new(
                crate::SimulatorErrorKind::Codegen(crate::CodegenError::message(format!(
                    "failed to spawn the background compiler thread: {error}"
                ))),
            )),
        };

        Self {
            phase: Phase::Interpreting(Some(interp)),
            promotion,
            events,
            cancel,
            pending_split_applies: 0,
        }
    }

    /// Whether the compiled tier has been adopted.
    pub fn is_compiled(&self) -> bool {
        matches!(self.phase, Phase::Compiled(_))
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
    /// The native-tier worker unwinds at its next task boundary and the
    /// simulation stays on the interpreter permanently; the reason becomes
    /// retrievable through [`TieredBackend::promotion_error`]. A result that
    /// already reached the channel (or arrives afterwards, for example from
    /// a tier that ignores the token) is rejected so cancellation is
    /// honored regardless of compiler timing. Returns whether a background
    /// compilation was still pending, so callers that only want to reclaim
    /// a finished worker can ignore the call cheaply.
    pub fn cancel_background_compilation(&mut self) -> bool {
        let pending = matches!(self.promotion, Promotion::Pending(_));
        self.cancel.cancel();
        if pending {
            self.promotion = Promotion::Failed(super::compile_cancel::cancelled_error());
        }
        pending
    }

    /// Adopt the compiled tier if background compilation finished. Called at
    /// scheduler safe points (between evaluation phases) where no unit is
    /// mid-execution and the memory image can move atomically from the
    /// caller's perspective.
    fn maybe_promote(&mut self) {
        if !matches!(self.phase, Phase::Interpreting(Some(_))) {
            return;
        }
        // Never promote inside a split evaluate/apply pair: the compiled
        // apply phase must observe the interpreted evaluate-only results,
        // and adoption clears the sparse metadata they are tracked in.
        if self.pending_split_applies > 0 {
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
            return;
        };
        let mut adopted = None;
        if let Phase::Interpreting(slot) = &mut self.phase {
            if let Some(mut interp) = slot.take() {
                let (memory, runtime_event_buffer, comb_capture_enabled) = interp.tier_transfer();
                drop(interp);
                adopted = Some(CompiledTier::adopt(
                    code,
                    memory,
                    runtime_event_buffer,
                    comb_capture_enabled,
                ));
            }
        }
        if let Some(mut compiled) = adopted {
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
            self.promotion = Promotion::Promoted;
        }
    }
}

impl Drop for TieredBackend {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl SimBackend for TieredBackend {
    type Event = TieredEventRef;

    fn eval_comb(&mut self) -> Result<(), SimulatorErrorCode> {
        self.maybe_promote();
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.eval_comb(),
            Phase::Compiled(compiled) => compiled.eval_comb(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn eval_apply_ff_at(&mut self, event: TieredEventRef) -> Result<(), SimulatorErrorCode> {
        self.maybe_promote();
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.eval_apply_ff_at(
                super::interp::InterpEventRef::from_parts(event.addr(), event.id()),
            ),
            Phase::Compiled(compiled) => compiled.eval_apply_ff_at(event.id()),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn eval_comb_apply_ff_at(&mut self, event: TieredEventRef) -> Result<(), SimulatorErrorCode> {
        self.maybe_promote();
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.eval_comb_apply_ff_at(
                super::interp::InterpEventRef::from_parts(event.addr(), event.id()),
            ),
            Phase::Compiled(compiled) => compiled.eval_comb_apply_ff_at(event.id()),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn eval_comb_apply_ff_many_at(
        &mut self,
        event: TieredEventRef,
        count: u64,
    ) -> (u64, Result<(), SimulatorErrorCode>) {
        // Poll promotion once for the whole batch so an adopted tier runs
        // every remaining iteration inside generated code.
        if count == 0 {
            return (0, Ok(()));
        }
        self.maybe_promote();
        match &mut self.phase {
            Phase::Interpreting(Some(_)) => (1, self.eval_comb_apply_ff_at(event)),
            Phase::Compiled(compiled) => compiled.eval_comb_apply_ff_many_at(event.id(), count),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn eval_only_ff_at(&mut self, event: TieredEventRef) -> Result<(), SimulatorErrorCode> {
        self.maybe_promote();
        let result = match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.eval_only_ff_at(
                super::interp::InterpEventRef::from_parts(event.addr(), event.id()),
            ),
            Phase::Compiled(compiled) => compiled.eval_only_ff_at_addr(&event.addr()),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        };
        // The paired apply phase must run on the same tier; defer promotion
        // until it completes.
        self.pending_split_applies = self.pending_split_applies.saturating_add(1);
        result
    }

    fn apply_ff_at(&mut self, event: TieredEventRef) -> Result<(), SimulatorErrorCode> {
        // Never promote between the evaluate-only and apply phases.
        self.maybe_promote();
        let result = match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.apply_ff_at(
                super::interp::InterpEventRef::from_parts(event.addr(), event.id()),
            ),
            Phase::Compiled(compiled) => compiled.apply_ff_at_addr(&event.addr()),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        };
        self.pending_split_applies = self.pending_split_applies.saturating_sub(1);
        result
    }

    fn resolve_signal(&self, addr: &AbsoluteAddr) -> SignalRef {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.resolve_signal(addr),
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.resolve_signal(addr),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.resolve_signal(addr),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn resolve_event(&self, addr: &AbsoluteAddr) -> TieredEventRef {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => {
                let ev = interp.resolve_event(addr);
                TieredEventRef {
                    addr: ev.addr(),
                    id: ev.id(),
                }
            }
            Phase::Compiled(CompiledTier::Jit(jit)) => {
                let ev = jit.resolve_event(addr);
                TieredEventRef {
                    addr: ev.addr(),
                    id: ev.id(),
                }
            }
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => {
                let ev = native.resolve_event(addr);
                TieredEventRef {
                    addr: ev.addr(),
                    id: ev.id(),
                }
            }
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn resolve_event_opt(&self, addr: &AbsoluteAddr) -> Option<TieredEventRef> {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => {
                interp.resolve_event_opt(addr).map(|ev| TieredEventRef {
                    addr: ev.addr(),
                    id: ev.id(),
                })
            }
            Phase::Compiled(CompiledTier::Jit(jit)) => {
                jit.resolve_event_opt(addr).map(|ev| TieredEventRef {
                    addr: ev.addr(),
                    id: ev.id(),
                })
            }
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => {
                native.resolve_event_opt(addr).map(|ev| TieredEventRef {
                    addr: ev.addr(),
                    id: ev.id(),
                })
            }
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn resolve_eval_only_event(&self, addr: &AbsoluteAddr) -> Option<TieredEventRef> {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => {
                interp
                    .resolve_eval_only_event(addr)
                    .map(|ev| TieredEventRef {
                        addr: ev.addr(),
                        id: ev.id(),
                    })
            }
            Phase::Compiled(CompiledTier::Jit(jit)) => {
                jit.resolve_eval_only_event(addr).map(|ev| TieredEventRef {
                    addr: ev.addr(),
                    id: ev.id(),
                })
            }
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native
                .resolve_eval_only_event(addr)
                .map(|ev| TieredEventRef {
                    addr: ev.addr(),
                    id: ev.id(),
                }),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn resolve_apply_event(&self, addr: &AbsoluteAddr) -> Option<TieredEventRef> {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => {
                interp.resolve_apply_event(addr).map(|ev| TieredEventRef {
                    addr: ev.addr(),
                    id: ev.id(),
                })
            }
            Phase::Compiled(CompiledTier::Jit(jit)) => {
                jit.resolve_apply_event(addr).map(|ev| TieredEventRef {
                    addr: ev.addr(),
                    id: ev.id(),
                })
            }
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => {
                native.resolve_apply_event(addr).map(|ev| TieredEventRef {
                    addr: ev.addr(),
                    id: ev.id(),
                })
            }
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn set<T: Copy>(&mut self, signal: SignalRef, value: T) {
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.set(signal, value),
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.set(signal, value),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.set(signal, value),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn set_wide(&mut self, signal: SignalRef, value: BigUint) {
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.set_wide(signal, value),
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.set_wide(signal, value),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.set_wide(signal, value),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn set_four_state(&mut self, signal: SignalRef, value: BigUint, mask: BigUint) {
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.set_four_state(signal, value, mask),
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.set_four_state(signal, value, mask),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => {
                native.set_four_state(signal, value, mask)
            }
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn get(&self, signal: SignalRef) -> BigUint {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.get(signal),
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.get(signal),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.get(signal),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn get_as<T: Default + Copy>(&self, signal: SignalRef) -> T {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.get_as(signal),
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.get_as(signal),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.get_as(signal),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn get_four_state(&self, signal: SignalRef) -> (BigUint, BigUint) {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.get_four_state(signal),
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.get_four_state(signal),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.get_four_state(signal),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn memory_as_ptr(&self) -> (*const u8, usize) {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.memory_as_ptr(),
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.memory_as_ptr(),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.memory_as_ptr(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn memory_as_mut_ptr(&mut self) -> (*mut u8, usize) {
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.memory_as_mut_ptr(),
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.memory_as_mut_ptr(),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.memory_as_mut_ptr(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn runtime_event_buffer_as_ptr(&self) -> (*const u8, usize) {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.runtime_event_buffer_as_ptr(),
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.runtime_event_buffer_as_ptr(),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.runtime_event_buffer_as_ptr(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn runtime_event_buffer(&self) -> Option<Arc<RuntimeEventBuffer>> {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.runtime_event_buffer(),
            Phase::Compiled(CompiledTier::Jit(jit)) => Some(jit.runtime_event_buffer().clone()),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.runtime_event_buffer(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn set_comb_capture_event_enabled(&mut self, active_sites: &[bool]) {
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => {
                interp.set_comb_capture_event_enabled(active_sites)
            }
            Phase::Compiled(CompiledTier::Jit(jit)) => {
                jit.set_comb_capture_event_enabled(active_sites)
            }
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => {
                native.set_comb_capture_event_enabled(active_sites)
            }
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn stable_region_size(&self) -> usize {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.stable_region_size(),
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.stable_region_size(),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.stable_region_size(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn layout(&self) -> &MemoryLayout {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.layout(),
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.layout(),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.layout(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn id_to_addr_slice(&self) -> &[AbsoluteAddr] {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.id_to_addr_slice(),
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.id_to_addr_slice(),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.id_to_addr_slice(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn id_to_event_slice(&self) -> &[TieredEventRef] {
        &self.events
    }

    fn num_events(&self) -> usize {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.num_events(),
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.num_events(),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.num_events(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn clear_triggered_bits(&mut self) {
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.clear_triggered_bits(),
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.clear_triggered_bits(),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.clear_triggered_bits(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn mark_triggered_bit(&mut self, id: usize) {
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.mark_triggered_bit(id),
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.mark_triggered_bit(id),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.mark_triggered_bit(id),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn get_triggered_bits(&self) -> bit_set::BitSet {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.get_triggered_bits(),
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.get_triggered_bits(),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.get_triggered_bits(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::NativeBackend;
    use crate::{CodegenError, RuntimeEvent, Simulator, SimulatorBuilder, SimulatorErrorKind};
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Two-stage pipeline: `q` trails `d` by two clock edges.
    const PIPELINE: &str = r#"
module Top (
    clk: input clock,
    rst: input reset,
    d: input logic<8>,
    q: output logic<8>,
) {
    var stage1: logic<8>;
    var stage2: logic<8>;
    always_ff (clk, rst) {
        if_reset {
            stage1 = 0;
            stage2 = 0;
        } else {
            stage1 = d;
            stage2 = stage1;
        }
    }
    assign q = stage2;
}
"#;

    struct Gate(Arc<AtomicBool>);

    impl Gate {
        fn closed() -> Self {
            Self(Arc::new(AtomicBool::new(false)))
        }

        fn open(&self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    /// Compile through the Cranelift tier behind a gate. The native tier is
    /// covered end-to-end by the public `build_tiered` integration tests on
    /// native hosts; gating lets these unit tests pin promotion timing.
    fn build_gated(gate: &Gate) -> Simulator<TieredBackend> {
        let worker_gate = gate.0.clone();
        SimulatorBuilder::<Simulator>::new(PIPELINE, "Top")
            .build_tiered_with_compiler(move |laid_out, options, _cancel| {
                while !worker_gate.load(Ordering::Acquire) {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                {
                    let image = NativeBackend::compile_image(laid_out, options)?;
                    let shared = unsafe { SharedNativeCode::from_image(image)? };
                    Ok(CompiledCode::Native(Arc::new(shared)))
                }
            })
            .unwrap()
    }

    /// Drive the pipeline through a reset and an input sweep, recording `q`
    /// after every tick. Identical inputs must produce identical outputs no
    /// matter when (or whether) promotion lands, because both tiers are
    /// bit-exact.
    fn drive(sim: &mut Simulator<TieredBackend>, inputs: &[u8]) -> Vec<u8> {
        let clk = sim.event("clk");
        let rst = sim.signal("rst");
        let d = sim.signal("d");

        sim.modify(|io| {
            io.set(rst, 0u8);
            io.set(d, inputs[0]);
        })
        .unwrap();
        sim.tick(clk).unwrap();
        sim.tick(clk).unwrap();
        sim.modify(|io| io.set(rst, 1u8)).unwrap();

        let mut observed = Vec::new();
        for &value in inputs {
            sim.modify(|io| io.set(d, value)).unwrap();
            sim.tick(clk).unwrap();
            sim.tick(clk).unwrap();
            observed.push(sim.get_as::<u8>(sim.signal("q")));
        }
        observed
    }

    fn reference_outputs(inputs: &[u8]) -> Vec<u8> {
        // Each iteration ticks twice before sampling, which fully absorbs
        // the pipeline's two-cycle latency: the sampled output equals the
        // current input.
        inputs.to_vec()
    }

    #[test]
    fn unpacked_array_works_across_promotion_on_strided_layout() {
        // On native-default hosts this runs the interpreter against an
        // element-strided layout, exercising strided element addressing and
        // plane-sized state before and after promotion.
        let code = r#"
module Top (
    clk: input clock,
    we: input logic,
    waddr: input logic<3>,
    wdata: input logic<8>,
    raddr: input logic<3>,
    q: output logic<8>,
) {
    var mem: logic<8>[8];
    always_ff (clk) {
        if we {
            mem[waddr] = wdata;
        }
    }
    assign q = mem[raddr];
}
"#;
        let gate = Gate::closed();
        let worker_gate = gate.0.clone();
        let mut sim: Simulator<TieredBackend> = SimulatorBuilder::<Simulator>::new(code, "Top")
            .build_tiered_with_compiler(move |laid_out, options, _cancel| {
                while !worker_gate.load(Ordering::Acquire) {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                {
                    let image = NativeBackend::compile_image(laid_out, options)?;
                    let shared = unsafe { SharedNativeCode::from_image(image)? };
                    Ok(CompiledCode::Native(Arc::new(shared)))
                }
            })
            .unwrap();
        let clk = sim.event("clk");
        let we = sim.signal("we");
        let waddr = sim.signal("waddr");
        let wdata = sim.signal("wdata");
        let raddr = sim.signal("raddr");
        let q = sim.signal("q");

        // Fill every element while still interpreted.
        for i in 0..8u8 {
            sim.modify(|io| {
                io.set(we, 1u8);
                io.set(waddr, i);
                io.set(wdata, i * 7 + 1);
            })
            .unwrap();
            sim.tick(clk).unwrap();
        }
        sim.modify(|io| io.set(we, 0u8)).unwrap();

        let read_back = |sim: &mut Simulator<TieredBackend>, index: u8| -> u8 {
            sim.modify(|io| io.set(raddr, index)).unwrap();
            sim.get_as::<u8>(q)
        };

        // Verify interpreted results...
        for i in 0..8u8 {
            assert_eq!(read_back(&mut sim, i), i * 7 + 1);
        }

        // ...release the worker and promote...
        gate.open();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !sim.is_compiled() && std::time::Instant::now() < deadline {
            sim.tick(clk).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(sim.is_compiled());

        // ...and confirm the whole array survived the memory handoff.
        for i in 0..8u8 {
            assert_eq!(read_back(&mut sim, i), i * 7 + 1, "element {i}");
        }

        // Writes keep working on the compiled tier too.
        sim.modify(|io| {
            io.set(we, 1u8);
            io.set(waddr, 3u8);
            io.set(wdata, 250u8);
        })
        .unwrap();
        sim.tick(clk).unwrap();
        sim.modify(|io| io.set(we, 0u8)).unwrap();
        assert_eq!(read_back(&mut sim, 3), 250u8);
    }

    /// Four-state dynamically-indexed array through native promotion.
    ///
    /// Exercises element-strided addressing, sparse commit, and mask plane
    /// handling across the tier boundary.
    #[test]
    fn four_state_unpacked_array_planes_initialize_and_survive_promotion() {
        let code = r#"
module Top (
    clk: input clock,
    we: input logic,
    waddr: input logic<2>,
    wdata: input logic<8>,
    raddr: input logic<2>,
    q: output logic<8>,
) {
    var mem: logic<8>[4];
    always_ff (clk) {
        if we {
            mem[waddr] = wdata;
        }
    }
    assign q = mem[raddr];
}
"#;
        let gate = Gate::closed();
        let worker_gate = gate.0.clone();
        let mut sim: Simulator<TieredBackend> = SimulatorBuilder::<Simulator>::new(code, "Top")
            .four_state(true)
            .build_tiered_with_compiler(move |laid_out, options, _cancel| {
                while !worker_gate.load(Ordering::Acquire) {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                let image = NativeBackend::compile_image(laid_out, options)?;
                let shared = unsafe { SharedNativeCode::from_image(image)? };
                Ok(CompiledCode::Native(Arc::new(shared)))
            })
            .unwrap();
        let clk = sim.event("clk");
        let we = sim.signal("we");
        let waddr = sim.signal("waddr");
        let wdata = sim.signal("wdata");
        let raddr = sim.signal("raddr");
        let q = sim.signal("q");

        // Write all elements on the interpreted tier.
        for i in 0..4u8 {
            sim.modify(|io| {
                io.set(we, 1u8);
                io.set(waddr, i);
                io.set(wdata, i * 60 + 5);
            })
            .unwrap();
            sim.tick(clk).unwrap();
        }
        sim.modify(|io| io.set(we, 0u8)).unwrap();

        // Verify interpreted-tier self-consistency.
        for i in 0..4u8 {
            sim.modify(|io| io.set(raddr, i)).unwrap();
            assert_eq!(sim.get_as::<u8>(q), i * 60 + 5);
        }

        // Promote to native tier.
        gate.open();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !sim.is_compiled() && std::time::Instant::now() < deadline {
            sim.tick(clk).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(sim.is_compiled());

        // Verify all elements survived promotion and are readable on native.
        for i in 0..4u8 {
            sim.modify(|io| io.set(raddr, i)).unwrap();
            assert_eq!(
                sim.get_as::<u8>(q),
                i * 60 + 5,
                "element {i} after native promotion"
            );
        }

        // Writes keep working on the native tier.
        sim.modify(|io| {
            io.set(we, 1u8);
            io.set(waddr, 2u8);
            io.set(wdata, 200u8);
        })
        .unwrap();
        sim.tick(clk).unwrap();
        sim.modify(|io| io.set(we, 0u8)).unwrap();
        sim.modify(|io| io.set(raddr, 2u8)).unwrap();
        assert_eq!(sim.get_as::<u8>(q), 200u8, "native-tier write");
    }

    /// Compare Packed-mode interpreter vs ElementStrided-mode interpreter
    /// (no promotion). If these differ, the strided addressing formula is
    /// wrong independent of any tier transition.
    #[test]
    fn strided_interp_matches_packed_interp_for_four_state_array() {
        let code = r#"
module Top (
    clk: input clock,
    we: input logic,
    waddr: input logic<2>,
    wdata: input logic<8>,
    raddr: input logic<2>,
    q: output logic<8>,
) {
    var mem: logic<8>[4];
    always_ff (clk) {
        if we {
            mem[waddr] = wdata;
        }
    }
    assign q = mem[raddr];
}
"#;
        // Reference: Packed-mode interpreter (build_interpreter always uses Packed).
        // NOTE: build_interpreter is pub so we can call it from integration tests,
        // but from unit tests inside the crate we use the builder directly.
        let mut packed_sim = SimulatorBuilder::<Simulator>::new(code, "Top")
            .four_state(true)
            .build_interpreter()
            .unwrap();

        // Strided: ElementStrided interpreter via gated tiered (never promotes).
        let gate = Gate::closed();
        let worker_gate = gate.0.clone();
        let mut strided_sim: Simulator<TieredBackend> =
            SimulatorBuilder::<Simulator>::new(code, "Top")
                .four_state(true)
                .build_tiered_with_compiler(move |laid_out, options, _cancel| {
                    while !worker_gate.load(Ordering::Acquire) {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    {
                        let image = NativeBackend::compile_image(laid_out, options)?;
                        let shared = unsafe { SharedNativeCode::from_image(image)? };
                        Ok(CompiledCode::Native(Arc::new(shared)))
                    }
                })
                .unwrap();

        let packed_clk = packed_sim.event("clk");
        let strided_clk = strided_sim.event("clk");
        let packed_we = packed_sim.signal("we");
        let strided_we = strided_sim.signal("we");
        let packed_waddr = packed_sim.signal("waddr");
        let strided_waddr = strided_sim.signal("waddr");
        let packed_wdata = packed_sim.signal("wdata");
        let strided_wdata = strided_sim.signal("wdata");
        let packed_raddr = packed_sim.signal("raddr");
        let strided_raddr = strided_sim.signal("raddr");

        // Write identical data to both simulators.
        for i in 0..4u8 {
            // Drive packed
            packed_sim
                .modify(|io| {
                    io.set(packed_we, 1u8);
                    io.set(packed_waddr, i);
                    io.set(packed_wdata, i * 60 + 5);
                })
                .unwrap();
            packed_sim.tick(packed_clk).unwrap();
            // Drive strided
            strided_sim
                .modify(|io| {
                    io.set(strided_we, 1u8);
                    io.set(strided_waddr, i);
                    io.set(strided_wdata, i * 60 + 5);
                })
                .unwrap();
            strided_sim.tick(strided_clk).unwrap();
        }
        packed_sim.modify(|io| io.set(packed_we, 0u8)).unwrap();
        strided_sim.modify(|io| io.set(strided_we, 0u8)).unwrap();

        // Read back and compare.
        for i in 0..4u8 {
            packed_sim.modify(|io| io.set(packed_raddr, i)).unwrap();
            strided_sim.modify(|io| io.set(strided_raddr, i)).unwrap();
            let pv = packed_sim.get_as::<u8>(packed_sim.signal("q"));
            let sv = strided_sim.get_as::<u8>(strided_sim.signal("q"));
            assert_eq!(
                pv, sv,
                "element {i} diverges between Packed and ElementStrided interp"
            );
        }
    }

    #[test]
    fn outputs_match_reference_when_promotion_never_happens() {
        let inputs: Vec<u8> = (10..40).collect();
        let gate = Gate::closed();
        let mut sim = build_gated(&gate);
        let observed = drive(&mut sim, &inputs);
        assert_eq!(observed, reference_outputs(&inputs));
        assert!(!sim.is_compiled());
        assert!(sim.promotion_error().is_none(), "gate still closed");
    }

    #[test]
    fn outputs_match_reference_when_promotion_lands_mid_run() {
        let inputs: Vec<u8> = (10..40).collect();
        let gate = Gate::closed();
        let mut sim = build_gated(&gate);

        let clk = sim.event("clk");
        let rst = sim.signal("rst");
        let d = sim.signal("d");
        sim.modify(|io| {
            io.set(rst, 0u8);
            io.set(d, inputs[0]);
        })
        .unwrap();
        sim.tick(clk).unwrap();
        sim.tick(clk).unwrap();
        sim.modify(|io| io.set(rst, 1u8)).unwrap();

        // Run the first half strictly interpreted.
        let mut observed = Vec::new();
        for &value in &inputs[..inputs.len() / 2] {
            sim.modify(|io| io.set(d, value)).unwrap();
            sim.tick(clk).unwrap();
            sim.tick(clk).unwrap();
            observed.push(sim.get_as::<u8>(sim.signal("q")));
        }

        // Release the worker; the next safe points adopt the compiled tier.
        gate.open();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !sim.is_compiled() && std::time::Instant::now() < deadline {
            // Hold the input steady so any extra ticks are unobservable.
            sim.tick(clk).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(sim.is_compiled());
        assert!(sim.promotion_error().is_none());

        for &value in &inputs[inputs.len() / 2..] {
            sim.modify(|io| io.set(d, value)).unwrap();
            sim.tick(clk).unwrap();
            sim.tick(clk).unwrap();
            observed.push(sim.get_as::<u8>(sim.signal("q")));
        }

        assert_eq!(observed, reference_outputs(&inputs));
    }

    #[test]
    fn compilation_failure_keeps_the_interpreter() {
        let mut sim: Simulator<TieredBackend> = SimulatorBuilder::<Simulator>::new(PIPELINE, "Top")
            .build_tiered_with_compiler(|_, _, _| {
                Err(SimulatorError::from(crate::RuntimeErrorCode::InternalError))
            })
            .unwrap();

        let inputs: Vec<u8> = (10..20).collect();
        let observed = drive(&mut sim, &inputs);
        assert_eq!(observed, reference_outputs(&inputs));
        assert!(!sim.is_compiled());

        // The worker reports its failure through the channel; keep polling
        // safe points until it lands (thread start may lag under load).
        let clk = sim.event("clk");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while sim.promotion_error().is_none() && std::time::Instant::now() < deadline {
            sim.tick(clk).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(sim.promotion_error().is_some());
    }

    #[test]
    fn runtime_events_stay_continuous_across_promotion() {
        let code = r#"
module Top (
    clk: input clock,
    cnt: output logic<8>,
) {
    var c: logic<8>;
    always_ff (clk) {
        c = c + 1;
        $display("tick %0d", c);
    }
    assign cnt = c;
}
"#;
        let gate = Gate::closed();
        let worker_gate = gate.0.clone();
        let mut sim: Simulator<TieredBackend> = SimulatorBuilder::<Simulator>::new(code, "Top")
            .build_tiered_with_compiler(move |laid_out, options, _cancel| {
                while !worker_gate.load(Ordering::Acquire) {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                {
                    let image = NativeBackend::compile_image(laid_out, options)?;
                    let shared = unsafe { SharedNativeCode::from_image(image)? };
                    Ok(CompiledCode::Native(Arc::new(shared)))
                }
            })
            .unwrap();
        let clk = sim.event("clk");

        // Five interpreted ticks emit tick 0..4 (display observes the
        // pre-increment value).
        for _ in 0..5 {
            sim.tick(clk).unwrap();
        }
        let pre = sim.drain_runtime_events();
        assert_eq!(pre.len(), 5);
        for (index, event) in pre.iter().enumerate() {
            let RuntimeEvent::Display { message } = event else {
                panic!("unexpected event {event:?}");
            };
            assert_eq!(message.as_str(), format!("tick {index}"));
        }

        gate.open();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !sim.is_compiled() && std::time::Instant::now() < deadline {
            sim.tick(clk).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(sim.is_compiled());
        // Discard the unbounded wait-loop events.
        let _ = sim.drain_runtime_events();

        // Eight compiled-tier ticks continue the counter seamlessly.
        const POST_TICKS: u32 = 8;
        for _ in 0..POST_TICKS {
            sim.tick(clk).unwrap();
        }
        let base = sim
            .get_as::<u8>(sim.signal("cnt"))
            .wrapping_sub(POST_TICKS as u8);
        let post = sim.drain_runtime_events();
        assert_eq!(post.len(), POST_TICKS as usize);
        for (index, event) in post.iter().enumerate() {
            let RuntimeEvent::Display { message } = event else {
                panic!("unexpected event {event:?}");
            };
            assert_eq!(
                message.as_str(),
                format!("tick {}", base.wrapping_add(index as u8))
            );
        }
    }

    /// Explicit cancellation unwinds background compilation and leaves the
    /// interpreter as the permanent tier, with the cancellation retrievable
    /// through `promotion_error`.
    #[test]
    fn cancel_background_compilation_stays_on_the_interpreter() {
        let mut sim: Simulator<TieredBackend> = SimulatorBuilder::<Simulator>::new(PIPELINE, "Top")
            .build_tiered_with_compiler(|laid_out, options, cancel| {
                while !cancel.is_cancelled() {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                let image = NativeBackend::compile_image_with_cancel(laid_out, options, cancel)?;
                let shared = unsafe { SharedNativeCode::from_image(image)? };
                Ok(CompiledCode::Native(Arc::new(shared)))
            })
            .unwrap();

        assert!(sim.cancel_background_compilation());

        let clk = sim.event("clk");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while sim.promotion_error().is_none() && std::time::Instant::now() < deadline {
            sim.tick(clk).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(!sim.is_compiled());
        match sim.promotion_error() {
            Some(error) => assert!(matches!(
                error.kind(),
                SimulatorErrorKind::Codegen(CodegenError::Cancelled)
            )),
            None => panic!("cancellation was not reported through promotion_error"),
        }

        // The interpreted tier keeps producing correct results afterwards.
        let inputs: Vec<u8> = (0..8).collect();
        assert_eq!(drive(&mut sim, &inputs), reference_outputs(&inputs));
    }

    /// Dropping the simulator flags the worker so a blocked compilation
    /// unwinds instead of running to completion.
    #[test]
    fn drop_cancels_pending_background_compilation() {
        let saw_cancel = Arc::new(AtomicBool::new(false));
        let worker_saw_cancel = Arc::clone(&saw_cancel);
        let sim: Simulator<TieredBackend> = SimulatorBuilder::<Simulator>::new(PIPELINE, "Top")
            .build_tiered_with_compiler(move |_laid_out, _options, cancel| {
                while !cancel.is_cancelled() {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                worker_saw_cancel.store(true, Ordering::SeqCst);
                Err(SimulatorError::new(SimulatorErrorKind::Codegen(
                    CodegenError::Cancelled,
                )))
            })
            .unwrap();
        drop(sim);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !saw_cancel.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(saw_cancel.load(Ordering::Acquire));
    }
}
