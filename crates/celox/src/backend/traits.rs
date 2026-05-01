use num_bigint::BigUint;

use crate::ir::{AbsoluteAddr, SignalRef};

use super::MemoryLayout;

// SimulatorErrorCode: on native it's defined in runtime.rs; on wasm32 we define it here.
#[cfg(not(target_arch = "wasm32"))]
#[allow(unused_imports)]
pub use super::runtime::SimulatorErrorCode;

#[cfg(target_arch = "wasm32")]
#[derive(Debug, Clone, Eq)]
#[allow(dead_code)]
pub enum SimulatorErrorCode {
    DetectedTrueLoop,
    DetectedTrueLoopCode(i64),
    DetectedTrueLoopAt {
        signals: Vec<String>,
    },
    Runtime {
        message: String,
        signals: Vec<String>,
    },
    InternalError,
    NotAnEvent(String),
}
#[cfg(target_arch = "wasm32")]
impl PartialEq for SimulatorErrorCode {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::DetectedTrueLoop, Self::DetectedTrueLoop)
            | (Self::DetectedTrueLoop, Self::DetectedTrueLoopCode(_))
            | (Self::DetectedTrueLoop, Self::DetectedTrueLoopAt { .. })
            | (Self::DetectedTrueLoopCode(_), Self::DetectedTrueLoop)
            | (Self::DetectedTrueLoopCode(_), Self::DetectedTrueLoopCode(_))
            | (Self::DetectedTrueLoopCode(_), Self::DetectedTrueLoopAt { .. })
            | (Self::DetectedTrueLoopAt { .. }, Self::DetectedTrueLoopCode(_))
            | (Self::DetectedTrueLoopAt { .. }, Self::DetectedTrueLoop)
            | (Self::DetectedTrueLoopAt { .. }, Self::DetectedTrueLoopAt { .. }) => true,
            (Self::InternalError, Self::InternalError) => true,
            (
                Self::Runtime {
                    message: a,
                    signals: sa,
                },
                Self::Runtime {
                    message: b,
                    signals: sb,
                },
            ) => a == b && sa == sb,
            (Self::NotAnEvent(a), Self::NotAnEvent(b)) => a == b,
            _ => false,
        }
    }
}
#[cfg(target_arch = "wasm32")]
impl std::fmt::Display for SimulatorErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DetectedTrueLoop | Self::DetectedTrueLoopCode(_) => {
                write!(f, "Detected True Loop")
            }
            Self::DetectedTrueLoopAt { signals } if signals.is_empty() => {
                write!(f, "Detected True Loop")
            }
            Self::DetectedTrueLoopAt { signals } => {
                write!(f, "Detected True Loop: {}", signals.join(", "))
            }
            Self::Runtime { message, signals } if signals.is_empty() => write!(f, "{message}"),
            Self::Runtime { message, signals } => {
                write!(f, "{}: {}", message, signals.join(", "))
            }
            Self::InternalError => write!(f, "Internal Error"),
            Self::NotAnEvent(name) => write!(f, "Not an event: {name}"),
        }
    }
}
#[cfg(target_arch = "wasm32")]
impl std::error::Error for SimulatorErrorCode {}

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
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub trait SimBackend {
    /// The event handle type produced by this backend.
    type Event: EventHandle;

    // ── evaluation ──────────────────────────────────────────────
    fn eval_comb(&mut self) -> Result<(), super::SimulatorErrorCode>;

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
    fn runtime_event_buffer_as_ptr(&self) -> (*const u8, usize);
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
