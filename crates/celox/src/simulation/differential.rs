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

    /// The scheduler mode, the steps checked against a shadow and how each
    /// process kernel is scheduled.
    #[doc(hidden)]
    pub fn scheduler_stats(&self) -> crate::SchedulerStats {
        crate::SchedulerStats {
            checked_steps: self.checked_steps,
            unchecked: self.unchecked,
            ..self.state.scheduler_stats().clone()
        }
    }
}

/// Checks each step of a primary simulation against a shadow.
pub(super) struct ShadowCheck<'a, B: SimBackend> {
    shadow: &'a mut Simulation<B>,
    /// The time the step in progress started from.
    start: u64,
    /// Steps checked.
    pub(super) checked: u64,
}

impl<'a, B: SimBackend> ShadowCheck<'a, B> {
    pub(super) fn new(shadow: &'a mut Simulation<B>) -> Self {
        Self {
            shadow,
            start: 0,
            checked: 0,
        }
    }
}

impl<B: SimBackend> StepObserver<Simulator<B>> for ShadowCheck<'_, B> {
    fn before_step(&mut self, state: &SimulationState<B>, primary: &mut Simulator<B>) {
        self.start = state.time();
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
        let reference = shadow.state.step_until(&mut shadow.simulator, limit);
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
    if let Some(offset) = first_difference(&primary.backend, &shadow.simulator.backend) {
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
    {
        return Some("the string heap".to_string());
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
    fn differential_mode_mirrors_host_writes_in_a_stopped_slot() {
        let source = r#"
            `timescale 1ns/1ns
            module Top(input logic [7:0] a, output logic [7:0] seen);
                logic [7:0] y;
                assign y = a + 8'd1;
                initial begin
                    seen = 8'd0;
                    #5 $stop;
                    seen = y;
                end
            endmodule
        "#;
        let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top")
            .scheduler_mode(SchedulerMode::Differential)
            .build()
            .unwrap();
        sim.run_until(10).unwrap();
        assert!(sim.is_stopped());
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 3u8)).unwrap();
        sim.run_until(10).unwrap();
        let seen = sim.signal("seen");
        assert_eq!(sim.get(seen), 4u8.into());
        assert!(sim.scheduler_stats().checked_steps >= 2);
    }

    #[cfg(any(
        all(target_arch = "x86_64", not(feature = "arm64-codegen")),
        all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
    ))]
    #[test]
    fn the_shadow_runs_its_own_compiled_events() {
        use crate::backend::{EventHandle as _, SimBackend as _};
        let source = r#"
            module Top(input logic clk, output logic [7:0] count);
                initial count = 8'd0;
                always_ff @(posedge clk) count <= count + 8'd1;
            endmodule
        "#;
        let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top")
            .scheduler_mode(SchedulerMode::Differential)
            .build_native()
            .unwrap();
        sim.add_clock("clk", 10, 5);
        sim.run_until(100).unwrap();
        let shadow = sim.shadow.as_ref().unwrap();
        let own = shadow.simulator.backend.id_to_event_slice();
        let primary = sim.simulator.backend.id_to_event_slice();
        let events = shadow
            .state
            .export_schedule(&shadow.simulator.backend)
            .events;
        assert!(!events.is_empty());
        for event in events {
            let id = event.event_ref.id();
            assert_eq!(event.event_ref.func as usize, own[id].func as usize);
            assert_ne!(event.event_ref.func as usize, primary[id].func as usize);
        }
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
