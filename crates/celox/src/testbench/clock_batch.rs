//! Execute an externally scheduled clock batch with the shared SIR scheduler.
//!
//! Veryl resumes its TB before clock edges at that timestamp. Only the already
//! collected edges enter this unit; periodic rearming remains with the caller.
use super::*;
use celox_design::{ProcessKind, SchedulerExit, SchedulerSlots};

pub(super) struct ClockBatch {
    unit: usize,
    slots: SchedulerSlots<SignalRef>,
    clocks: Vec<SignalRef>,
}

impl ClockBatch {
    #[cfg(test)]
    pub(super) fn count_slot(&self) -> SignalRef {
        self.slots.count
    }

    pub(super) fn new<B: SimBackend>(sim: &Simulator<B>) -> Option<Self> {
        let schema = &sim.program.runtime_schema;
        // Timed RTL processes need the runtime queue's scheduling state. The
        // caller checks all other observers before using the direct edge path.
        if sim.host_slot_work
            || !sim.host_watch.is_empty()
            || schema
                .processes
                .iter()
                .any(|p| p.kind != ProcessKind::Callable)
        {
            return None;
        }
        let scheduler = schema.scheduler.as_ref()?;
        let Some(unit) = scheduler.unit else {
            tracing::debug!(reason = ?scheduler.fallback.first(), "Veryl clock batches use the edge engine");
            return None;
        };
        tracing::debug!(
            unit,
            clocks = scheduler.clocks.len(),
            "Veryl compiled clock batches enabled"
        );
        Some(Self {
            unit,
            slots: scheduler
                .slots
                .clone()
                .map(|addr| sim.backend.resolve_signal(&addr)),
            clocks: scheduler
                .clocks
                .iter()
                .map(|addr| sim.backend.resolve_signal(addr))
                .collect(),
        })
    }

    pub(super) fn run<B: SimBackend>(
        &self,
        sim: &mut Simulator<B>,
        events: &[(B::Event, u8)],
        time: u64,
    ) -> Option<Result<(), String>> {
        // The shared scheduler rearms after a slot. At the last representable
        // timestamp the reference path owns the caller's overflow diagnostic.
        if time == u64::MAX || !sim.backend.runs_compiled_scheduler() {
            return None;
        }
        let mut armed = 0u64;
        let mut values = 0u64;
        for &(event, value) in events {
            let signal = sim.backend.resolve_signal(&event.addr());
            let bit = self.clocks.iter().position(|&clock| clock == signal)?;
            // A batch with conflicting moves must use the runtime edge engine.
            if armed & (1 << bit) != 0 {
                return None;
            }
            armed |= 1 << bit;
            values |= u64::from(value != 0) << bit;
        }
        let element = |mut signal: SignalRef, index: usize| {
            signal.offset += index * 8;
            signal.width = 64;
            signal
        };
        for bit in 0..self.clocks.len() {
            if armed & (1 << bit) != 0 {
                sim.backend.set(element(self.slots.clock_next, bit), time);
                // One slot only; the caller owns the actual clock periods.
                sim.backend.set(element(self.slots.clock_period, bit), 2u64);
            }
        }
        sim.backend.set(self.slots.limit, time);
        sim.backend.set(self.slots.exit, 0u8);
        sim.backend.set(self.slots.finish, 0u8);
        sim.backend.set(self.slots.dirty, u8::from(sim.dirty));
        sim.backend.set(self.slots.count, 0u64);
        sim.backend.set(self.slots.ready, 0u64);
        sim.backend.set(self.slots.clock_armed, armed);
        sim.backend.set(self.slots.clock_value, values);
        let result = sim
            .backend
            .run_process(self.unit)
            .map_err(|e| e.to_string());
        sim.backend.clear_triggered_bits();
        if let Err(error) = result {
            return Some(Err(error));
        }
        match SchedulerExit::from_code(sim.backend.get_as(self.slots.exit)) {
            Some(SchedulerExit::Limit) => {
                sim.dirty = false;
                Some(Ok(()))
            }
            // The event drain reports the finish record to the TB runner.
            Some(SchedulerExit::Finish) => {
                sim.dirty = true;
                Some(Ok(()))
            }
            exit => Some(Err(format!("clock batch scheduler exited with {exit:?}"))),
        }
    }
}
