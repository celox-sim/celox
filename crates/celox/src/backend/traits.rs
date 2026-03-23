use num_bigint::BigUint;

use crate::ir::{AbsoluteAddr, SignalRef};

use super::MemoryLayout;

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
    fn eval_comb(&mut self) -> Result<(), super::SimulatorErrorCode>;

    /// Execute a full eval-apply-ff cycle for the given event and then
    /// evaluate combinational logic in a single merged call.
    /// Returns `Err` if the backend does not support merged calls.
    fn eval_apply_ff_and_comb(&mut self, event: Self::Event) -> Result<(), super::SimulatorErrorCode>;

    /// Evaluate and apply a flip-flop domain for the given event.
    fn eval_apply_ff_at(&mut self, event: Self::Event) -> Result<(), super::SimulatorErrorCode>;

    /// Evaluate FF domain without applying (for cascaded clocks).
    fn eval_only_ff_at(&mut self, event: Self::Event) -> Result<(), super::SimulatorErrorCode>;

    /// Apply (commit) an already-evaluated FF domain.
    fn apply_ff_at(&mut self, event: Self::Event) -> Result<(), super::SimulatorErrorCode>;

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
