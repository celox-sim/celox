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

// Keep the common single-word batch allocation-free. Wider scheduler schemas
// use the same operations on every clock bit, including zeroing stale high bits.
trait ClockMask: Default {
    fn contains(&self, bit: usize) -> bool;
    fn insert(&mut self, bit: usize);
    fn write<B: SimBackend>(self, backend: &mut B, signal: SignalRef);
}

impl ClockMask for u64 {
    fn contains(&self, bit: usize) -> bool {
        self & (1 << bit) != 0
    }

    fn insert(&mut self, bit: usize) {
        *self |= 1 << bit;
    }

    fn write<B: SimBackend>(self, backend: &mut B, signal: SignalRef) {
        backend.set(signal, self);
    }
}

impl ClockMask for BigUint {
    fn contains(&self, bit: usize) -> bool {
        self.bit(bit as u64)
    }

    fn insert(&mut self, bit: usize) {
        self.set_bit(bit as u64, true);
    }

    fn write<B: SimBackend>(self, backend: &mut B, signal: SignalRef) {
        backend.set_wide(signal, self);
    }
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
        if sim.slot_end.work
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
        if self.clocks.len() <= u64::BITS as usize {
            self.run_masked::<B, u64>(sim, events, time)
        } else {
            self.run_masked::<B, BigUint>(sim, events, time)
        }
    }

    fn run_masked<B: SimBackend, M: ClockMask>(
        &self,
        sim: &mut Simulator<B>,
        events: &[(B::Event, u8)],
        time: u64,
    ) -> Option<Result<(), String>> {
        let mut armed = M::default();
        let mut values = M::default();
        for &(event, value) in events {
            let signal = sim.backend.resolve_signal(&event.addr());
            let bit = self.clocks.iter().position(|&clock| clock == signal)?;
            // A batch with conflicting moves must use the runtime edge engine.
            if armed.contains(bit) {
                return None;
            }
            armed.insert(bit);
            if value != 0 {
                values.insert(bit);
            }
        }
        let element = |mut signal: SignalRef, index: usize| {
            signal.offset += index * 8;
            signal.width = 64;
            signal
        };
        for bit in 0..self.clocks.len() {
            if armed.contains(bit) {
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
        armed.write(&mut sim.backend, self.slots.clock_armed);
        values.write(&mut sim.backend, self.slots.clock_value);
        let result = sim
            .run_process_checked(self.unit)
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

#[cfg(all(test, feature = "systemverilog"))]
mod tests {
    use super::*;

    fn check<B: SimBackend>(mut sim: Simulator<B>, count: usize) {
        let batch = ClockBatch::new(&sim).expect("compiled host clock batch");
        assert_eq!(batch.clocks.len(), count);
        // Select by scheduler position, so the fixture cannot accidentally
        // exercise only low bits when the frontend's event ordering changes.
        let events: Vec<_> = sim
            .program
            .runtime_schema
            .scheduler
            .as_ref()
            .unwrap()
            .clocks
            .iter()
            .map(|addr| sim.backend.resolve_event(addr))
            .collect();
        let high = count - 2;
        let last = count - 1;
        let mut levels = vec![0u8; count];
        let mut expected = vec![0u8; count];
        for (time, moves) in [
            vec![(0, 1)],
            vec![(high, 1), (last, 1)],
            vec![(0, 0), (high, 0)],
            vec![(0, 1)],
            vec![(high, 1), (0, 0)],
        ]
        .into_iter()
        .enumerate()
        {
            let edges: Vec<_> = moves
                .iter()
                .map(|&(bit, value)| (events[bit], value))
                .collect();
            batch
                .run(&mut sim, &edges, time as u64)
                .expect("must execute a compiled batch")
                .unwrap();
            for (bit, value) in moves {
                if levels[bit] == 0 && value == 1 {
                    expected[bit] += 1;
                }
                levels[bit] = value;
            }
            for (&clock, &level) in batch.clocks.iter().zip(&levels) {
                assert_eq!(sim.backend.get_as::<u8>(clock), level);
            }
        }
        // A duplicate high clock must keep the complete batch on the edge
        // engine, without applying the first move or aliasing a low clock.
        assert!(
            batch
                .run(&mut sim, &[(events[last], 0), (events[last], 1)], 5)
                .is_none()
        );
        for (&clock, &level) in batch.clocks.iter().zip(&levels) {
            assert_eq!(sim.backend.get_as::<u8>(clock), level);
        }
        // Map expected counters through each input's actual scheduler slot.
        for index in 0..count {
            let clock = sim.signal(&format!("c{index}"));
            let bit = batch
                .clocks
                .iter()
                .position(|signal| *signal == clock)
                .unwrap();
            let output = sim.signal(&format!("q{index}"));
            assert_eq!(sim.get_as::<u8>(output), expected[bit], "q{index}");
        }
    }

    #[test]
    fn host_batches_preserve_clock_masks_across_word_boundaries() {
        for count in [70, 64] {
            let mut source = String::from("module Top(");
            for index in 0..count {
                if index != 0 {
                    source.push(',');
                }
                source.push_str(&format!("input bit c{index}, output bit [7:0] q{index}"));
            }
            source.push_str(");\n");
            for index in 0..count {
                source.push_str(&format!(
                    "always_ff @(posedge c{index}) q{index} <= q{index} + 1;\n"
                ));
            }
            source.push_str("endmodule");
            for four_state in [false, true] {
                let build = || {
                    Simulator::from_sv_sources(
                        vec![(source.as_str(), std::path::Path::new("wide-host-batch.sv"))],
                        "Top",
                    )
                    .four_state(four_state)
                    .emit_triggers()
                };
                check(build().build_native().unwrap(), count);
                #[cfg(feature = "cranelift")]
                check(build().build_cranelift().unwrap(), count);
                #[cfg(feature = "wasm-runtime")]
                check(build().build_wasm().unwrap(), count);
            }
        }
    }
}
