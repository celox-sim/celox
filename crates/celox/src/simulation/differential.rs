//! [`SchedulerMode::Differential`]: every step is checked against a shadow
//! simulation of the same design that runs the reference loop.
//!
//! The primary runs its own loop, [`SimulationState::run_until_observed`]
//! or [`SimulationState::step_until_observed`], with a [`ShadowCheck`] as the
//! observer. Before each step the shadow takes the primary's design state,
//! scheduling state and undrained runtime events, so host calls between
//! steps need no mirroring. In a time slot suspended at `$stop` the
//! scheduling state cannot be copied: the shadow then takes the design state
//! and host changes, and continues the slot in lockstep.
//!
//! [`SchedulerMode::Differential`]: crate::SchedulerMode::Differential

use celox_runtime::{SimulationState, StepObserver};

use super::Simulation;
use crate::{
    RuntimeErrorCode, Simulator,
    backend::{EventHandle, SimBackend},
};

impl<B: SimBackend> Simulation<B> {
    /// In [`crate::SchedulerMode::Differential`], check every step against
    /// `shadow`, a simulation of the same design, or record why no step is
    /// checked.
    pub(crate) fn set_shadow(&mut self, shadow: Result<Box<Simulation<B>>, &'static str>) {
        match shadow {
            Ok(shadow) => self.shadow = Some(shadow),
            Err(reason) => self.unchecked = Some(reason),
        }
    }

    pub(super) fn mode(&self) -> crate::SchedulerMode {
        self.state.scheduler_stats().mode
    }

    /// The scheduler mode, the steps checked against a shadow and how each
    /// process kernel is scheduled.
    #[doc(hidden)]
    pub fn scheduler_stats(&self) -> crate::SchedulerStats {
        crate::SchedulerStats {
            native_service_calls: self.simulator.native.service_calls,
            checked_steps: self.checked_steps,
            unchecked: self.unchecked,
            ..self.state.scheduler_stats().clone()
        }
    }
}

/// Checks each step of a primary simulation against a shadow.
pub(super) struct ShadowCheck<'a, B: SimBackend> {
    shadow: &'a mut Simulation<B>,
    /// Compiled runs take one slot each; otherwise the shadow catches up
    /// with a longer run slot by slot.
    one_slot: bool,
    /// The time the step in progress started from.
    start: u64,
    /// Host settling can be a complete generated run at the current time.
    host_entry: bool,
    /// Steps checked.
    pub(super) checked: u64,
}

impl<'a, B: SimBackend> ShadowCheck<'a, B> {
    pub(super) fn new(shadow: &'a mut Simulation<B>, mode: crate::SchedulerMode) -> Self {
        Self {
            shadow,
            one_slot: mode != crate::SchedulerMode::DifferentialRuns,
            start: 0,
            host_entry: false,
            checked: 0,
        }
    }
}

impl<B: SimBackend> ShadowCheck<'_, B> {
    pub(super) fn after_finish(
        &mut self,
        state: &SimulationState<B>,
        primary: &mut Simulator<B>,
        result: &Result<(), RuntimeErrorCode>,
    ) -> Result<(), RuntimeErrorCode> {
        let shadow = &mut *self.shadow;
        let reference = shadow.state.finish(&mut shadow.simulator);
        self.checked += 1;
        let result = result.clone().map(|()| Some(state.time()));
        let reference = reference.map(|()| Some(shadow.state.time()));
        match difference(state, primary, shadow, &result, &reference) {
            None => Ok(()),
            Some(message) => Err(RuntimeErrorCode::Runtime {
                message: format!("finalization diverged from the reference: {message}"),
                signals: Vec::new(),
            }),
        }
    }
}

impl<B: SimBackend> StepObserver<Simulator<B>> for ShadowCheck<'_, B> {
    fn one_slot(&self) -> bool {
        self.one_slot
    }

    fn before_step(&mut self, state: &SimulationState<B>, primary: &mut Simulator<B>) {
        self.start = state.time();
        self.host_entry = state.host_pending();
        let shadow = &mut *self.shadow;
        shadow.simulator.mirror(primary);
        if state.is_suspended() {
            shadow.state.set_host_pending(state.host_pending());
            return;
        }
        // Pending events take the shadow's own handles, which run the
        // shadow's compiled code.
        let events = shadow.simulator.backend.id_to_event_slice();
        shadow
            .state
            .restore(&state.snapshot(), |event| events.get(event.id()).copied())
            .unwrap_or_else(|_| unreachable!("the shadow has the same events"));
    }

    fn after_step(
        &mut self,
        state: &SimulationState<B>,
        primary: &mut Simulator<B>,
        limit: u64,
        result: &Result<Option<u64>, RuntimeErrorCode>,
    ) -> Result<(), RuntimeErrorCode> {
        let shadow = &mut *self.shadow;
        // A generated host-input run may end after settling the current
        // timestamp. The reference's step normally continues to the next
        // event, so stop it at the same legal boundary for this comparison.
        // Compare the complete state and pending queues there as usual.
        let limit = if self.host_entry && matches!(result, Ok(Some(time)) if *time == self.start) {
            limit.min(self.start)
        } else {
            limit
        };
        let mut reference = shadow.state.step_until(&mut shadow.simulator, limit);
        // A compiled run of several slots: the shadow runs them one by one,
        // up to the last slot of the run, or to the slot a failed run ended
        // in, where the reference must fail the same way.
        let last = match result {
            Ok(Some(time)) => Some(*time),
            Ok(None) => None,
            Err(_) => Some(state.time()),
        };
        if let Some(time) = last {
            while let Ok(Some(reached)) = reference {
                if reached >= time
                    || shadow.state.is_finished()
                    || shadow.state.is_stopped()
                    || shadow
                        .state
                        .next_event_time()
                        .is_none_or(|next| next > time)
                {
                    break;
                }
                reference = shadow.state.step_until(&mut shadow.simulator, limit);
            }
        }
        self.checked += 1;
        match difference(state, primary, shadow, result, &reference) {
            None => Ok(()),
            Some(difference) => Err(RuntimeErrorCode::Runtime {
                message: format!(
                    "the scheduler diverged from the reference in the step from time {}: \
                     {difference}",
                    self.start
                ),
                signals: Vec::new(),
            }),
        }
    }
}

/// What differs between a primary simulation and `shadow` after a step with
/// results `primary_result` and `reference`, if anything.
fn difference<B: SimBackend>(
    state: &SimulationState<B>,
    primary: &mut Simulator<B>,
    shadow: &mut Simulation<B>,
    primary_result: &Result<Option<u64>, RuntimeErrorCode>,
    reference: &Result<Option<u64>, RuntimeErrorCode>,
) -> Option<String> {
    let result = |result: &Result<Option<u64>, RuntimeErrorCode>| match result {
        Ok(time) => format!("{time:?}"),
        Err(error) => format!("error: {error}"),
    };
    let (returned, reference) = (result(primary_result), result(reference));
    if returned != reference {
        return Some(format!(
            "the step returned {returned}, the reference {reference}"
        ));
    }
    let (key, reference) = (state.schedule_key(), shadow.state.schedule_key());
    if key != reference {
        return Some(format!(
            "scheduling state {key:?}, the reference {reference:?}"
        ));
    }
    // The compiled scheduler's slots hold nothing between runs, and the
    // settle's partition flags only what is left to recompute.
    let excluded: Vec<_> = primary
        .program
        .runtime_schema
        .scheduler
        .iter()
        .flat_map(|scheduler| scheduler.slots.iter())
        .map(|address| {
            let signal = primary.backend.resolve_signal(address);
            signal.offset..signal.offset + celox_state_layout::get_byte_size(signal.width)
        })
        .chain(
            primary
                .backend
                .layout()
                .settle
                .as_ref()
                .map(|settle| settle.flags()),
        )
        .collect();
    if let Some(offset) = first_difference(&primary.backend, &shadow.simulator.backend, &excluded) {
        let signal = primary
            .named_signals()
            .into_iter()
            .find(|named| {
                let start = named.signal.offset;
                (start..start + named.signal.width.div_ceil(8)).contains(&offset)
            })
            .map_or_else(|| "no named signal".to_string(), |named| named.name);
        return Some(format!("state byte {offset} ({signal})"));
    }
    if !primary
        .backend
        .heap()
        .same_contents(shadow.simulator.backend.heap())
        && (state.is_suspended()
            || !live_heap(primary).same_contents(&live_heap(&shadow.simulator)))
    {
        return Some("the runtime heap (strings or objects)".to_string());
    }
    let (events, reference) = (
        primary.peek_runtime_events(),
        shadow.simulator.peek_runtime_events(),
    );
    if events != reference {
        return Some(format!(
            "runtime events {events:?}, the reference {reference:?}"
        ));
    }
    None
}

/// Compare the heap reachable from the same stable state that `first_difference`
/// checks. FF working slots can retain intermediate handles that an inlined
/// scheduler never stores. Those are not design state between completed slots.
/// Collect only copies: the real heaps, allocation cursors and GC cadence are
/// unchanged. `same_contents` still checks future handle allocation and the
/// contents (including RNG state) of every reachable object.
fn live_heap<B: SimBackend>(sim: &Simulator<B>) -> celox_heap::Heap {
    let mut heap = sim.backend.heap().clone();
    let slots = sim.handle_slots();
    let roots = sim.handle_roots();
    heap.collect(
        slots
            .iter()
            .zip(roots)
            .filter(|((offset, _), _)| *offset < sim.backend.layout().total_size)
            .map(|(_, root)| root)
            .chain(sim.heap.process_roots.iter().copied()),
    );
    heap
}

/// The first byte of design state outside `excluded` that differs between
/// two backends of the same design.
fn first_difference<B: SimBackend>(
    primary: &B,
    shadow: &B,
    excluded: &[std::ops::Range<usize>],
) -> Option<usize> {
    let range = celox_state_layout::STATE_HEADER_SIZE..primary.layout().total_size;
    let bytes = |backend: &B| {
        let (ptr, len) = backend.memory_as_ptr();
        assert!(range.end <= len, "state exceeds the memory image");
        // Safety: the backend exposes `len` readable bytes at `ptr`.
        let memory = unsafe { std::slice::from_raw_parts(ptr, len) };
        memory[range.clone()].to_vec()
    };
    let primary_layout = primary.layout();
    let (primary, shadow) = (bytes(primary), bytes(shadow));
    // Native and interpreted stores may preserve different unused bits in a
    // physical slot. Compare every declared stable value/mask bit, including
    // anonymous process state, but not alignment or per-element padding.
    let layout = primary_layout;
    layout
        .offsets
        .iter()
        .flat_map(|(address, &base)| {
            let width = layout.widths[address];
            let (elements, stride, bits) = layout.unpacked_arrays.get(address).map_or(
                (1, layout.plane_size(address), width),
                |array| {
                    (
                        array.element_count,
                        array.element_stride,
                        array.element_width,
                    )
                },
            );
            let planes = if layout.four_state && layout.is_4states[address] {
                2
            } else {
                1
            };
            (0..planes).flat_map(move |plane| {
                (0..elements).flat_map(move |element| {
                    (0..bits.div_ceil(8)).map(move |byte| {
                        let offset =
                            base + plane * layout.plane_size(address) + element * stride + byte;
                        let mask = ((1u16 << (bits - byte * 8).min(8)) - 1) as u8;
                        (offset, mask)
                    })
                })
            })
        })
        .filter(|&(offset, mask)| {
            (primary[offset - range.start] ^ shadow[offset - range.start]) & mask != 0
                && !excluded.iter().any(|r| r.contains(&offset))
        })
        .map(|(offset, _)| offset)
        .min()
}

#[cfg(all(test, feature = "systemverilog"))]
mod tests;
