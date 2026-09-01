use std::{any::Any, sync::Arc};

use num_bigint::BigUint;

pub use crate::SimulatorErrorCode;
use crate::{AbsoluteAddr, MemoryLayout, RuntimeEventBuffer, SignalRef};

/// Marker trait for backend-specific event handles.
///
/// An event handle is an opaque reference to a compiled clock or
/// async-reset trigger. It is resolved once via
/// [`SimBackend::resolve_event`] and then passed to tick/eval methods
/// for zero-cost dispatch.
pub trait EventHandle: Copy + std::fmt::Debug {
    /// Numeric event identifier used for scheduling.
    fn id(&self) -> usize;

    /// The absolute address of the signal this event is bound to.
    fn addr(&self) -> AbsoluteAddr;
}

/// Abstraction over different simulation backends (JIT, WASM, etc.).
///
/// `Simulator<B>` is generic over this trait so that the same high-level
/// API works with any backend. `JitBackend` is the default.
pub trait SimBackend {
    /// The event handle type produced by this backend.
    type Event: EventHandle;

    // ── evaluation ──────────────────────────────────────────────
    fn eval_comb(&mut self) -> Result<(), SimulatorErrorCode>;

    /// Evaluate and apply a flip-flop domain for the given event.
    fn eval_apply_ff_at(&mut self, event: Self::Event) -> Result<(), SimulatorErrorCode>;

    /// Evaluate combinational logic and then evaluate/apply one flip-flop
    /// domain. Backends may override this to compile the two phases as one
    /// function; the default preserves the same ordering with two calls.
    fn eval_comb_apply_ff_at(&mut self, event: Self::Event) -> Result<(), SimulatorErrorCode> {
        self.eval_comb()?;
        self.eval_apply_ff_at(event)
    }

    /// Execute up to `count` identical fused ticks. The returned count is the
    /// number of iterations completed before a runtime event or error forced a
    /// return to the host. Backends without an in-function loop execute one
    /// iteration so the caller can preserve per-tick observation semantics.
    fn eval_comb_apply_ff_many_at(
        &mut self,
        event: Self::Event,
        count: u64,
    ) -> (u64, Result<(), SimulatorErrorCode>) {
        if count == 0 {
            return (0, Ok(()));
        }
        (1, self.eval_comb_apply_ff_at(event))
    }

    /// Evaluate FF domain without applying (for cascaded clocks).
    fn eval_only_ff_at(&mut self, event: Self::Event) -> Result<(), SimulatorErrorCode>;

    /// Apply (commit) an already-evaluated FF domain.
    fn apply_ff_at(&mut self, event: Self::Event) -> Result<(), SimulatorErrorCode>;

    // ── signal access ───────────────────────────────────────────
    fn resolve_signal(&self, addr: &AbsoluteAddr) -> SignalRef;
    fn resolve_event(&self, addr: &AbsoluteAddr) -> Self::Event;
    fn resolve_event_opt(&self, addr: &AbsoluteAddr) -> Option<Self::Event>;
    fn resolve_eval_only_event(&self, addr: &AbsoluteAddr) -> Option<Self::Event>;
    fn resolve_apply_event(&self, addr: &AbsoluteAddr) -> Option<Self::Event>;

    // ── get / set ───────────────────────────────────────────────
    fn set<T: Copy>(&mut self, signal: SignalRef, val: T);
    fn set_wide(&mut self, signal: SignalRef, val: BigUint);
    fn set_four_state(&mut self, signal: SignalRef, val: BigUint, mask: BigUint);
    fn get(&self, signal: SignalRef) -> BigUint;
    fn get_as<T: Default + Copy>(&self, signal: SignalRef) -> T;
    fn get_four_state(&self, signal: SignalRef) -> (BigUint, BigUint);

    // ── memory / layout ─────────────────────────────────────────
    fn memory_as_ptr(&self) -> (*const u8, usize);
    fn memory_as_mut_ptr(&mut self) -> (*mut u8, usize);
    /// Return an opaque owner that keeps the memory allocation alive.
    ///
    /// Host integrations that expose the raw memory outside Rust can retain
    /// this value until their external view is finalized. Backends without a
    /// separately owned stable allocation may keep the default `None`.
    /// When returning `Some`, the allocation and address reported by
    /// [`Self::memory_as_ptr`] and [`Self::memory_as_mut_ptr`] must remain valid
    /// until every clone of the owner has been dropped.
    fn memory_owner(&self) -> Option<Arc<dyn Any + Send + Sync>> {
        None
    }
    fn runtime_event_buffer_as_ptr(&self) -> (*const u8, usize);
    fn runtime_event_buffer(&self) -> Option<Arc<RuntimeEventBuffer>> {
        None
    }
    fn set_comb_capture_event_enabled(&mut self, _active_sites: &[bool]) {}
    fn stable_region_size(&self) -> usize;
    fn layout(&self) -> &MemoryLayout;

    // ── event enumeration ───────────────────────────────────────
    fn id_to_addr_slice(&self) -> &[AbsoluteAddr];
    fn id_to_event_slice(&self) -> &[Self::Event];
    fn num_events(&self) -> usize;

    // ── trigger bits (for Simulation edge detection) ────────────
    fn clear_triggered_bits(&mut self);
    fn mark_triggered_bit(&mut self, id: usize);
    fn get_triggered_bits(&self) -> bit_set::BitSet;
}
