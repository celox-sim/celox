//! [`SchedulerMode::Differential`]: every step is checked against a shadow
//! simulation of the same design that runs the reference loop.
//!
//! Before each step the shadow takes the primary's design state, scheduling
//! state and undrained runtime events, so host calls between steps need no
//! mirroring. A step suspended at `$stop` cannot be copied; the two then
//! continue in lockstep from their last equal state.
//!
//! [`SchedulerMode::Differential`]: crate::SchedulerMode::Differential

use super::Simulation;
use crate::{
    RuntimeErrorCode,
    backend::{EventHandle, SimBackend},
};

impl<B: SimBackend> Simulation<B> {
    /// Check every step against `shadow`, a simulation of the same design.
    pub(crate) fn set_shadow(&mut self, shadow: Option<Box<Simulation<B>>>) {
        self.shadow = shadow;
    }

    /// One step of the scheduler, checked against the shadow.
    pub(super) fn differential_step_until(
        &mut self,
        shadow: &mut Simulation<B>,
        end_time: u64,
    ) -> Result<Option<u64>, RuntimeErrorCode> {
        if !self.state.is_suspended() {
            shadow.simulator.mirror(&mut self.simulator);
            let events = self.simulator.backend.id_to_event_slice();
            shadow
                .state
                .restore(&self.state.snapshot(), |event| {
                    events.get(event.id()).copied()
                })
                .unwrap_or_else(|_| unreachable!("the shadow has the same events"));
        }
        let start = self.state.time();
        let primary = self.state.step_until(&mut self.simulator, end_time);
        let reference = shadow.state.step_until(&mut shadow.simulator, end_time);
        self.state.note_checked_step();
        if let Some(difference) = self.difference(shadow, &primary, &reference) {
            return Err(RuntimeErrorCode::Runtime {
                message: format!(
                    "the scheduler diverged from the reference in the step from time {start}: \
                     {difference}"
                ),
                signals: Vec::new(),
            });
        }
        primary
    }

    /// What differs between this simulation and `shadow` after a step with
    /// results `primary` and `reference`, if anything.
    fn difference(
        &mut self,
        shadow: &mut Simulation<B>,
        primary: &Result<Option<u64>, RuntimeErrorCode>,
        reference: &Result<Option<u64>, RuntimeErrorCode>,
    ) -> Option<String> {
        let result = |result: &Result<Option<u64>, RuntimeErrorCode>| match result {
            Ok(time) => format!("{time:?}"),
            Err(error) => format!("error: {error}"),
        };
        let (primary, reference) = (result(primary), result(reference));
        if primary != reference {
            return Some(format!(
                "the step returned {primary}, the reference {reference}"
            ));
        }
        let (key, reference) = (self.state.schedule_key(), shadow.state.schedule_key());
        if key != reference {
            return Some(format!(
                "scheduling state {key:?}, the reference {reference:?}"
            ));
        }
        if let Some(offset) = first_difference(&self.simulator.backend, &shadow.simulator.backend) {
            let signal = self
                .simulator
                .named_signals()
                .into_iter()
                .find(|named| {
                    let start = named.signal.offset;
                    (start..start + named.signal.width.div_ceil(8)).contains(&offset)
                })
                .map_or_else(|| "no named signal".to_string(), |named| named.name);
            return Some(format!("state byte {offset} ({signal})"));
        }
        let (events, reference) = (
            self.simulator.peek_runtime_events(),
            shadow.simulator.peek_runtime_events(),
        );
        if events != reference {
            return Some(format!(
                "runtime events {events:?}, the reference {reference:?}"
            ));
        }
        None
    }

    /// The scheduler mode, the steps checked against a shadow and how each
    /// process kernel is scheduled.
    #[doc(hidden)]
    pub fn scheduler_stats(&self) -> crate::SchedulerStats {
        self.state.scheduler_stats().clone()
    }
}

/// The first byte of design state that differs between two backends of the
/// same design.
fn first_difference<B: SimBackend>(primary: &B, shadow: &B) -> Option<usize> {
    let range = celox_state_layout::STATE_HEADER_SIZE..primary.layout().total_size;
    let bytes = |backend: &B| {
        let (ptr, len) = backend.memory_as_ptr();
        assert!(range.end <= len, "state exceeds the memory image");
        // Safety: the backend exposes `len` readable bytes at `ptr`.
        let memory = unsafe { std::slice::from_raw_parts(ptr, len) };
        memory[range.clone()].to_vec()
    };
    let (primary, shadow) = (bytes(primary), bytes(shadow));
    primary
        .iter()
        .zip(&shadow)
        .position(|(a, b)| a != b)
        .map(|index| range.start + index)
}

#[cfg(all(test, feature = "systemverilog"))]
mod tests {
    use std::path::Path;

    use crate::{SchedulerMode, Simulation};

    const SOURCE: &str = r#"
        `timescale 1ns/1ns
        module Top(output logic [7:0] count, output logic flag);
            logic clk = 1'b0;
            always #5 clk = ~clk;
            initial begin
                count = 8'd0;
                flag = $test$plusargs("fast");
                forever @(posedge clk) count <= count + (flag ? 8'd2 : 8'd1);
            end
        endmodule
    "#;

    fn build(mode: SchedulerMode) -> Simulation {
        Simulation::from_sv_sources(vec![(SOURCE, Path::new("top.sv"))], "Top")
            .scheduler_mode(mode)
            .build()
            .unwrap()
    }

    #[test]
    fn differential_mode_checks_every_step_against_the_reference() {
        let mut sim = build(SchedulerMode::Differential);
        sim.run_until(100).unwrap();
        // A host write between runs reaches the shadow too.
        let count = sim.signal("count");
        sim.modify(|io| io.set(count, 100u8)).unwrap();
        sim.run_until(200).unwrap();
        let mut reference = build(SchedulerMode::Reference);
        reference.run_until(100).unwrap();
        reference.modify(|io| io.set(count, 100u8)).unwrap();
        reference.run_until(200).unwrap();
        assert_eq!(sim.get(count), reference.get(count));
        let stats = sim.scheduler_stats();
        assert_eq!(stats.mode, SchedulerMode::Differential);
        assert!(stats.checked_steps > 0);
        assert_eq!(reference.scheduler_stats().checked_steps, 0);
    }

    #[test]
    fn differential_mode_reports_a_diverging_step() {
        let mut sim = build(SchedulerMode::Differential);
        // The shadow sees other plusargs, which a copy of the state does not
        // carry, so its first step computes another flag.
        sim.shadow.as_mut().unwrap().simulator.plusargs =
            celox_runtime::plusargs::Plusargs(vec!["+fast".to_string()]);
        let error = sim.run_until(100).unwrap_err().to_string();
        assert!(
            error.contains("diverged from the reference in the step from time 0"),
            "{error}"
        );
    }
}
