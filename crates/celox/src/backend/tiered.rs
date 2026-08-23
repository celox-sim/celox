//! Tiered execution backend: interpret first, promote to compiled code.
//!
//! [`TieredBackend`] starts every simulation on the Tier-0 interpreter so
//! execution begins the moment the state layout is finalized, hiding the
//! code-generation latency behind the first simulated steps. A worker thread
//! compiles the program for the Cranelift JIT in the background; the next
//! scheduler safe point after completion adopts the compiled code and moves
//! the live memory image across without any translation, because both tiers
//! share the same packed layout ABI and event-buffer pointer conventions.
//!
//! Promotion is whole-program: once the compiled tier is adopted it is used
//! for the remainder of the simulation. The interpreter remains the permanent
//! fallback whenever background compilation fails.

#![cfg(feature = "host-runtime")]

use std::sync::Arc;
use std::sync::mpsc;

use num_bigint::BigUint;

use super::{
    EventHandle, MemoryLayout, RuntimeEventBuffer, SharedJitCode, SimBackend, SimulatorErrorCode,
};
use crate::backend::{InterpBackend, JitBackend};
use crate::{
    SimulatorError, SimulatorOptions,
    ir::{AbsoluteAddr, LaidOutProgram, SignalRef},
};

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
    Compiled(Box<JitBackend>),
}

enum Promotion {
    /// Background compilation still running; the receiver yields exactly one
    /// result when it finishes.
    Pending(mpsc::Receiver<Result<SharedJitCode, SimulatorError>>),
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
}

impl TieredBackend {
    /// Build a tiered simulation: ready to run immediately on the
    /// interpreter while the compiled tier is prepared in the background.
    pub fn new(laid_out: &LaidOutProgram, options: &SimulatorOptions) -> Self {
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
        std::thread::Builder::new()
            .name("celox-jit-compile".to_string())
            .spawn(move || {
                // The result is delivered through the channel instead of the
                // join handle so safe-point polls never block on compilation.
                // A panicked worker surfaces as a disconnected channel and
                // keeps the simulation on the interpreter permanently.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    JitBackend::compile(&background_laid_out, &background_options, None)
                }))
                .unwrap_or_else(|_| {
                    Err(SimulatorError::from(crate::RuntimeErrorCode::InternalError))
                });
                let _ = sender.send(result);
            })
            .expect("spawning the background compiler thread must succeed");

        Self {
            phase: Phase::Interpreting(Some(interp)),
            promotion: Promotion::Pending(receiver),
            events,
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

    /// Adopt the compiled tier if background compilation finished. Called at
    /// scheduler safe points (between evaluation phases) where no unit is
    /// mid-execution and the memory image can move atomically from the
    /// caller's perspective.
    fn maybe_promote(&mut self) {
        if !matches!(self.phase, Phase::Interpreting(Some(_))) {
            return;
        }
        let Promotion::Pending(receiver) = &self.promotion else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => self.handle_compile_result(result),
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                // The worker died before reporting; stay interpreted forever
                // rather than promoting with unknown state.
                self.promotion =
                    Promotion::Failed(SimulatorError::from(crate::RuntimeErrorCode::InternalError));
            }
        }
    }

    fn handle_compile_result(&mut self, result: Result<SharedJitCode, SimulatorError>) {
        let Ok(code) = result else {
            // Compilation errors keep the simulation on the interpreter;
            // the reason stays retrievable via promotion_error().
            self.promotion = match result {
                Err(error) => Promotion::Failed(error),
                Ok(_) => unreachable!("checked above"),
            };
            return;
        };

        let shared = Arc::new(code);
        let mut adopted = None;
        if let Phase::Interpreting(slot) = &mut self.phase {
            if let Some(mut interp) = slot.take() {
                let (memory, runtime_event_buffer, comb_capture_enabled) = interp.tier_transfer();
                drop(interp);
                adopted = Some(Box::new(JitBackend::adopt_shared_with_state(
                    shared,
                    memory,
                    runtime_event_buffer,
                    comb_capture_enabled,
                )));
            }
        }
        if let Some(jit) = adopted {
            self.phase = Phase::Compiled(jit);
            self.promotion = Promotion::Promoted;
        }
    }
}

impl SimBackend for TieredBackend {
    type Event = TieredEventRef;

    fn eval_comb(&mut self) -> Result<(), SimulatorErrorCode> {
        self.maybe_promote();
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.eval_comb(),
            Phase::Compiled(jit) => jit.eval_comb(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn eval_apply_ff_at(&mut self, event: TieredEventRef) -> Result<(), SimulatorErrorCode> {
        self.maybe_promote();
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.eval_apply_ff_at(
                super::interp::InterpEventRef::from_parts(event.addr(), event.id()),
            ),
            Phase::Compiled(jit) => jit.eval_apply_ff_at(jit.id_to_event_slice()[event.id()]),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn eval_comb_apply_ff_at(&mut self, event: TieredEventRef) -> Result<(), SimulatorErrorCode> {
        self.maybe_promote();
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.eval_comb_apply_ff_at(
                super::interp::InterpEventRef::from_parts(event.addr(), event.id()),
            ),
            Phase::Compiled(jit) => jit.eval_comb_apply_ff_at(jit.id_to_event_slice()[event.id()]),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn eval_only_ff_at(&mut self, event: TieredEventRef) -> Result<(), SimulatorErrorCode> {
        self.maybe_promote();
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.eval_only_ff_at(
                super::interp::InterpEventRef::from_parts(event.addr(), event.id()),
            ),
            Phase::Compiled(jit) => jit.eval_only_ff_at(jit.id_to_event_slice()[event.id()]),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn apply_ff_at(&mut self, event: TieredEventRef) -> Result<(), SimulatorErrorCode> {
        self.maybe_promote();
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.apply_ff_at(
                super::interp::InterpEventRef::from_parts(event.addr(), event.id()),
            ),
            Phase::Compiled(jit) => jit.apply_ff_at(jit.id_to_event_slice()[event.id()]),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn resolve_signal(&self, addr: &AbsoluteAddr) -> SignalRef {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.resolve_signal(addr),
            Phase::Compiled(jit) => jit.resolve_signal(addr),
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
            Phase::Compiled(jit) => {
                let ev = jit.resolve_event(addr);
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
            Phase::Compiled(jit) => jit.resolve_event_opt(addr).map(|ev| TieredEventRef {
                addr: ev.addr(),
                id: ev.id(),
            }),
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
            Phase::Compiled(jit) => jit.resolve_eval_only_event(addr).map(|ev| TieredEventRef {
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
            Phase::Compiled(jit) => jit.resolve_apply_event(addr).map(|ev| TieredEventRef {
                addr: ev.addr(),
                id: ev.id(),
            }),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn set<T: Copy>(&mut self, signal: SignalRef, value: T) {
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.set(signal, value),
            Phase::Compiled(jit) => jit.set(signal, value),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn set_wide(&mut self, signal: SignalRef, value: BigUint) {
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.set_wide(signal, value),
            Phase::Compiled(jit) => jit.set_wide(signal, value),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn set_four_state(&mut self, signal: SignalRef, value: BigUint, mask: BigUint) {
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.set_four_state(signal, value, mask),
            Phase::Compiled(jit) => jit.set_four_state(signal, value, mask),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn get(&self, signal: SignalRef) -> BigUint {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.get(signal),
            Phase::Compiled(jit) => jit.get(signal),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn get_as<T: Default + Copy>(&self, signal: SignalRef) -> T {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.get_as(signal),
            Phase::Compiled(jit) => jit.get_as(signal),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn get_four_state(&self, signal: SignalRef) -> (BigUint, BigUint) {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.get_four_state(signal),
            Phase::Compiled(jit) => jit.get_four_state(signal),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn memory_as_ptr(&self) -> (*const u8, usize) {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.memory_as_ptr(),
            Phase::Compiled(jit) => jit.memory_as_ptr(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn memory_as_mut_ptr(&mut self) -> (*mut u8, usize) {
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.memory_as_mut_ptr(),
            Phase::Compiled(jit) => jit.memory_as_mut_ptr(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn runtime_event_buffer_as_ptr(&self) -> (*const u8, usize) {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.runtime_event_buffer_as_ptr(),
            Phase::Compiled(jit) => jit.runtime_event_buffer_as_ptr(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn runtime_event_buffer(&self) -> Option<Arc<RuntimeEventBuffer>> {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.runtime_event_buffer(),
            Phase::Compiled(jit) => Some(jit.runtime_event_buffer().clone()),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn set_comb_capture_event_enabled(&mut self, active_sites: &[bool]) {
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => {
                interp.set_comb_capture_event_enabled(active_sites)
            }
            Phase::Compiled(jit) => jit.set_comb_capture_event_enabled(active_sites),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn stable_region_size(&self) -> usize {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.stable_region_size(),
            Phase::Compiled(jit) => jit.stable_region_size(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn layout(&self) -> &MemoryLayout {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.layout(),
            Phase::Compiled(jit) => jit.layout(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn id_to_addr_slice(&self) -> &[AbsoluteAddr] {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.id_to_addr_slice(),
            Phase::Compiled(jit) => jit.id_to_addr_slice(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn id_to_event_slice(&self) -> &[TieredEventRef] {
        &self.events
    }

    fn num_events(&self) -> usize {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.num_events(),
            Phase::Compiled(jit) => jit.num_events(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn clear_triggered_bits(&mut self) {
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.clear_triggered_bits(),
            Phase::Compiled(jit) => jit.clear_triggered_bits(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn mark_triggered_bit(&mut self, id: usize) {
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.mark_triggered_bit(id),
            Phase::Compiled(jit) => jit.mark_triggered_bit(id),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn get_triggered_bits(&self) -> bit_set::BitSet {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.get_triggered_bits(),
            Phase::Compiled(jit) => jit.get_triggered_bits(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }
}
