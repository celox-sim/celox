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
    /// Steps checked.
    pub(super) checked: u64,
}

impl<'a, B: SimBackend> ShadowCheck<'a, B> {
    pub(super) fn new(shadow: &'a mut Simulation<B>, mode: crate::SchedulerMode) -> Self {
        Self {
            shadow,
            one_slot: mode != crate::SchedulerMode::DifferentialRuns,
            start: 0,
            checked: 0,
        }
    }
}

impl<B: SimBackend> StepObserver<Simulator<B>> for ShadowCheck<'_, B> {
    fn one_slot(&self) -> bool {
        self.one_slot
    }

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
    // The compiled scheduler's slots hold nothing between runs.
    let scheduler: Vec<_> = primary
        .program
        .runtime_schema
        .scheduler
        .iter()
        .flat_map(|scheduler| scheduler.slots.iter())
        .map(|address| {
            let signal = primary.backend.resolve_signal(address);
            signal.offset..signal.offset + celox_state_layout::get_byte_size(signal.width)
        })
        .collect();
    if let Some(offset) = first_difference(&primary.backend, &shadow.simulator.backend, &scheduler)
    {
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
    let (primary, shadow) = (bytes(primary), bytes(shadow));
    primary
        .iter()
        .zip(&shadow)
        .enumerate()
        .map(|(index, pair)| (range.start + index, pair))
        .find(|(offset, (a, b))| a != b && !excluded.iter().any(|r| r.contains(offset)))
        .map(|(offset, _)| offset)
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
    fn the_compiled_scheduler_runs_delay_only_kernels() {
        let source = r#"
            `timescale 1ns/1ns
            module Top(output logic [7:0] count);
                logic clk = 1'b0;
                always #5 clk = ~clk;
                initial count = 8'd0;
                always_ff @(posedge clk) count <= count + 8'd1;
            endmodule
        "#;
        for mode in [
            SchedulerMode::Compiled,
            SchedulerMode::Differential,
            SchedulerMode::DifferentialRuns,
            SchedulerMode::Reference,
        ] {
            let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top")
                .scheduler_mode(mode)
                .build()
                .unwrap();
            sim.run_until(1000).unwrap();
            let count = sim.signal("count");
            assert_eq!(sim.get(count), 100u8.into(), "{mode:?}");
            let stats = sim.scheduler_stats();
            if mode == SchedulerMode::Reference {
                assert_eq!(stats.compiled_slots, 0);
            } else {
                assert!(stats.compiled_slots > 0, "{mode:?}: {stats:?}");
            }
            match mode {
                SchedulerMode::Differential => {
                    assert!(stats.checked_steps >= stats.compiled_slots)
                }
                // One run takes all the slots up to the target.
                SchedulerMode::DifferentialRuns => {
                    assert!(stats.checked_steps < stats.compiled_slots, "{stats:?}")
                }
                _ => {}
            }
        }
    }

    /// A kernel whose writes no event domain listens to: the scheduler
    /// runs its slots without any domain to dispatch.
    #[test]
    fn the_compiled_scheduler_runs_kernels_without_event_writes() {
        let source = r#"
            `timescale 1ns/1ns
            module Top(output logic [7:0] seen);
                logic tick = 1'b0;
                logic [7:0] count = 8'd0;
                always #5 begin
                    tick = ~tick;
                    count = count + 8'd1;
                end
                assign seen = count + {7'd0, tick};
            endmodule
        "#;
        for mode in [SchedulerMode::Differential, SchedulerMode::DifferentialRuns] {
            let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top")
                .scheduler_mode(mode)
                .build()
                .unwrap();
            sim.run_until(52).unwrap();
            let seen = sim.signal("seen");
            assert_eq!(sim.get(seen), 10u8.into(), "{mode:?}");
            assert!(sim.scheduler_stats().compiled_slots > 0, "{mode:?}");
        }
    }

    const MODES: [SchedulerMode; 3] = [
        SchedulerMode::Compiled,
        SchedulerMode::Differential,
        SchedulerMode::DifferentialRuns,
    ];

    fn build_in(source: &str, mode: SchedulerMode) -> Simulation {
        Simulation::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top")
            .scheduler_mode(mode)
            .build()
            .unwrap()
    }

    /// A `$finish` in a domain the scheduler inlines, with two sites.
    #[test]
    fn compiled_slots_end_at_a_finish_in_a_domain() {
        let source = r#"
            `timescale 1ns/1ns
            module Top(input logic err, output logic [7:0] n);
                logic clk = 1'b0;
                always #5 clk = ~clk;
                initial n = 8'd0;
                always_ff @(posedge clk) begin
                    n <= n + 8'd1;
                    if (n == 8'd3) $finish;
                    if (err) $finish;
                end
            endmodule
        "#;
        let mut reference = build_in(source, SchedulerMode::Reference);
        reference.run_until(100).unwrap();
        assert!(reference.is_finished());
        for mode in MODES {
            let mut sim = build_in(source, mode);
            sim.run_until(100).unwrap();
            assert!(sim.is_finished(), "{mode:?}");
            assert_eq!(sim.time(), reference.time(), "{mode:?}");
            let n = sim.signal("n");
            assert_eq!(sim.get(n), reference.get(n), "{mode:?}");
            assert!(sim.scheduler_stats().compiled_slots > 0, "{mode:?}");
        }
    }

    /// A compiled kernel's `$finish` leaves the state the reference leaves:
    /// a kernel due in the same slot that did not run yet is still ready,
    /// and a state file taken afterwards is the same.
    #[test]
    fn compiled_slots_end_at_a_kernel_finish_as_the_reference_does() {
        let clock = "always #5 clk = ~clk;";
        for finish in ["initial #27 $finish;", "initial #30 $finish;"] {
            for order in [[finish, clock], [clock, finish]] {
                let source = format!(
                    "`timescale 1ns/1ns
                    module Top(output logic [7:0] count);
                        logic clk = 1'b0;
                        {}
                        {}
                        initial count = 8'd0;
                        always_ff @(posedge clk) count <= count + 8'd1;
                    endmodule",
                    order[0], order[1]
                );
                let mut reference = build_in(&source, SchedulerMode::Reference);
                reference.run_until(100).unwrap();
                let expected = reference.save_state().unwrap();
                for mode in MODES {
                    let mut sim = build_in(&source, mode);
                    sim.run_until(100).unwrap();
                    assert!(sim.is_finished(), "{mode:?} {order:?}");
                    assert!(sim.scheduler_stats().compiled_slots > 0, "{mode:?}");
                    assert_eq!(sim.save_state().unwrap(), expected, "{mode:?} {order:?}");
                    assert!(sim.checkpoint().is_ok());
                }
            }
        }
    }

    /// A kernel may wake at the last representable time.
    #[test]
    fn compiled_kernels_may_wake_at_the_last_time() {
        let source = r#"
            `timescale 1fs/1fs
            module Top(output logic [7:0] x);
                initial begin
                    x = 8'd0;
                    #10 x = 8'd1;
                    #(64'd18446744073709551605) x = 8'd2;
                end
            endmodule
        "#;
        for mode in MODES {
            let mut sim = build_in(source, mode);
            sim.run_until(100).unwrap();
            let x = sim.signal("x");
            assert_eq!(sim.get(x), 1u8.into(), "{mode:?}");
            assert_eq!(sim.next_event_time(), Some(u64::MAX), "{mode:?}");
            sim.run_until(u64::MAX).unwrap();
            assert_eq!(sim.get(x), 2u8.into(), "{mode:?}");
        }
    }

    /// Run `source` to `end` in every mode and compare the state files with
    /// the reference's; at least one slot must run compiled.
    fn agrees_with_the_reference(source: &str, end: u64) {
        let mut reference = build_in(source, SchedulerMode::Reference);
        reference.run_until(end).unwrap();
        let expected = reference.save_state().unwrap();
        for mode in MODES {
            let mut sim = build_in(source, mode);
            sim.run_until(end).unwrap();
            assert_eq!(sim.save_state().unwrap(), expected, "{mode:?}");
            let stats = sim.scheduler_stats();
            assert!(stats.compiled_slots > 0, "{mode:?}: {stats:?}");
        }
    }

    /// A kernel waiting for a clock edge writes what the domain of that edge
    /// samples, blocking and nonblocking.
    #[test]
    fn clock_kernels_run_in_compiled_slots() {
        for assign in ["=", "<="] {
            let source = format!(
                "`timescale 1ns/1ns
                module Top(output logic [7:0] sum);
                    logic clk = 1'b0;
                    logic [7:0] stim = 8'd0;
                    logic [7:0] r = 8'd0;
                    always #5 clk = ~clk;
                    initial forever @(posedge clk) stim {assign} stim + 8'd3;
                    always_ff @(posedge clk) r <= r + stim;
                    assign sum = r;
                endmodule"
            );
            agrees_with_the_reference(&source, 1000);
        }
    }

    /// Kernels woken in the middle of a pass, declared before and after the
    /// clock kernels, and waits for falling and any edges.
    #[test]
    fn clock_kernels_wake_in_the_reference_order() {
        let source = r#"
            `timescale 1ns/1ns
            module Top(output logic [7:0] seen);
                logic a = 1'b0;
                logic b = 1'b0;
                logic [7:0] log = 8'd0;
                logic [7:0] acc = 8'd0;
                initial forever @(posedge a) log = log * 8'd3 + 8'd1;
                always #5 a = ~a;
                initial forever @(negedge a) log = log * 8'd5 + 8'd2;
                always #5 b = ~b;
                initial forever @(b) log = log * 8'd7 + 8'd3;
                initial forever @(posedge b) acc <= acc + log;
                assign seen = log + acc;
            endmodule
        "#;
        agrees_with_the_reference(source, 200);
    }

    /// A clock kernel's `$finish` ends a compiled slot as the reference's.
    #[test]
    fn clock_kernels_finish_compiled_slots() {
        let source = r#"
            `timescale 1ns/1ns
            module Top(output logic [7:0] n);
                logic clk = 1'b0;
                always #5 clk = ~clk;
                initial begin
                    n = 8'd0;
                    forever @(posedge clk) begin
                        n = n + 8'd1;
                        if (n == 8'd7) $finish;
                    end
                end
            endmodule
        "#;
        agrees_with_the_reference(source, 1000);
    }

    /// Compiled kernels declared after more than 64 others keep their own
    /// bits in the ready mask.
    #[test]
    fn compiled_kernels_after_many_processes() {
        let mut source = String::from(
            "`timescale 1ns/1ns\nmodule Top(output logic [7:0] n);\nlogic clk = 1'b0;\n",
        );
        for index in 0..66 {
            source.push_str(&format!(
                "logic d{index} = 1'b0;\ninitial #0 d{index} = 1'b1;\n"
            ));
        }
        source.push_str(
            "always #5 clk = ~clk;\n\
             initial begin n = 8'd0; forever @(posedge clk) n = n + 8'd1; end\n\
             endmodule\n",
        );
        agrees_with_the_reference(&source, 200);
    }

    /// A kernel that waits and then loops without suspending yields now and
    /// then, and keeps the reference loop.
    #[test]
    fn waiting_kernels_that_yield_fall_back() {
        let reasons = |source: &str| -> Vec<String> {
            build_in(source, SchedulerMode::Compiled)
                .scheduler_stats()
                .kernels
                .into_iter()
                .filter_map(|kernel| match kernel {
                    crate::KernelSchedule::Reference(reason) => Some(reason),
                    crate::KernelSchedule::Compiled => None,
                })
                .collect()
        };
        // A loop that does not suspend yields now and then.
        let spinning = r#"
            `timescale 1ns/1ns
            module Top(output logic q);
                logic clk = 1'b0;
                always #5 clk = ~clk;
                initial begin q = 1'b0; @(posedge clk); forever q <= ~q; end
            endmodule
        "#;
        assert_eq!(
            reasons(spinning),
            ["the process suspends other than for one edge"]
        );
    }

    /// A nonblocking site in a loop runs several times in one run of its
    /// kernel. A loop the frontend keeps yields now and then, and the
    /// copies of one it unrolls queue their values: either way the kernel
    /// keeps the reference loop, and every mode leaves its state.
    #[test]
    fn nonblocking_sites_in_loops_keep_the_reference_loop() {
        for (bound, reason) in [
            ("n", "the process suspends other than for one edge"),
            ("2", "a nonblocking site queues its values"),
        ] {
            for tail in ["", "$finish;"] {
                let source = format!(
                    "`timescale 1ns/1ns
                    module Top(output logic [7:0] q);
                        logic clk = 1'b0;
                        int n = 2;
                        always #5 clk = ~clk;
                        initial begin
                            q = 8'd0;
                            forever begin
                                @(posedge clk);
                                for (int i = 0; i < {bound}; i++) q <= q + 8'(i) + 8'd1;
                                {tail}
                            end
                        end
                    endmodule"
                );
                let kernels = build_in(&source, SchedulerMode::Compiled)
                    .scheduler_stats()
                    .kernels;
                assert!(
                    kernels.contains(&crate::KernelSchedule::Reference(reason.into())),
                    "{bound} {tail}: {kernels:?}"
                );
                let mut reference = build_in(&source, SchedulerMode::Reference);
                reference.run_until(100).unwrap();
                let expected = reference.save_state().unwrap();
                for mode in MODES {
                    let mut sim = build_in(&source, mode);
                    sim.run_until(100).unwrap();
                    assert_eq!(
                        sim.save_state().unwrap(),
                        expected,
                        "{bound} {tail} {mode:?}"
                    );
                }
            }
        }
    }

    /// A nonblocking update into what a stateful combinational process, a
    /// latch, is sensitive to bumps its occurrence counter when the
    /// reference publishes it: the kernel keeps the reference loop, and
    /// every mode leaves the reference's state and records.
    #[test]
    fn nonblocking_updates_into_probed_signals_keep_the_reference_loop() {
        let source = r#"
            `timescale 1ns/1ns
            module Top(output logic [7:0] n);
                logic clk = 1'b0;
                logic [7:0] q = 8'd0;
                always #5 clk = ~clk;
                logic [7:0] l;
                initial forever @(posedge clk) q <= q + 8'd1;
                always_latch if (q[0]) l = q;
                assign n = l;
            endmodule
        "#;
        let kernels = build_in(source, SchedulerMode::Compiled)
            .scheduler_stats()
            .kernels;
        // The frontend queues this site's values, and a staged one would
        // keep the reference loop too (`Plan::new`).
        assert!(
            matches!(kernels[1], crate::KernelSchedule::Reference(_)),
            "{kernels:?}"
        );
        let displays = |sim: &mut Simulation| {
            sim.drain_runtime_events()
                .into_iter()
                .filter(|event| matches!(event, crate::RuntimeEvent::Display { .. }))
                .count()
        };
        let mut reference = build_in(source, SchedulerMode::Reference);
        reference.run_until(100).unwrap();
        let expected = (reference.save_state().unwrap(), displays(&mut reference));
        for mode in MODES {
            let mut sim = build_in(source, mode);
            sim.run_until(100).unwrap();
            let state = sim.save_state().unwrap();
            assert_eq!((state, displays(&mut sim)), expected, "{mode:?}");
        }
    }

    /// What the host does before a run.
    #[derive(Clone, Copy)]
    enum Host {
        Clock(&'static str, u64, u64),
        Set(&'static str, u8),
        Schedule(&'static str, u64, u64),
    }

    fn drive<B: crate::SimBackend>(sim: &mut Simulation<B>, actions: &[Host]) {
        for &action in actions {
            match action {
                Host::Clock(port, period, delay) => sim.add_clock(port, period, delay),
                Host::Set(port, value) => {
                    let signal = sim.signal(port);
                    sim.modify(|io| io.set(signal, value)).unwrap();
                }
                Host::Schedule(port, time, value) => sim.schedule(port, time, value).unwrap(),
            }
        }
    }

    /// Run `source`, whose kernels all compile, in every mode on the native
    /// and Cranelift backends: before each run until `end`, the host does
    /// `actions`, and afterwards the state file is the reference's. At least
    /// one slot must run compiled.
    fn host_driven_agrees(source: &str, runs: &[(u64, &[Host])]) {
        let kernels = build_in(source, SchedulerMode::Compiled)
            .scheduler_stats()
            .kernels;
        assert!(
            kernels
                .iter()
                .all(|kernel| *kernel == crate::KernelSchedule::Compiled),
            "{kernels:?}"
        );
        let mut reference = build_in(source, SchedulerMode::Reference);
        let mut expected = Vec::new();
        for &(end, actions) in runs {
            drive(&mut reference, actions);
            reference.run_until(end).unwrap();
            expected.push(reference.save_state().unwrap());
        }
        fn check<B: crate::SimBackend>(
            mut sim: Simulation<B>,
            runs: &[(u64, &[Host])],
            expected: &[crate::StateFile],
            label: &str,
        ) {
            for (run, &(end, actions)) in runs.iter().enumerate() {
                drive(&mut sim, actions);
                sim.run_until(end).unwrap();
                assert_eq!(
                    sim.save_state().unwrap(),
                    expected[run],
                    "{label} run {run}"
                );
            }
            let stats = sim.scheduler_stats();
            assert!(stats.compiled_slots > 0, "{label}: {stats:?}");
        }
        for mode in MODES {
            check(
                build_in(source, mode),
                runs,
                &expected,
                &format!("{mode:?}"),
            );
            let cranelift = Simulation::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top")
                .scheduler_mode(mode)
                .build_cranelift()
                .unwrap();
            check(cranelift, runs, &expected, &format!("cranelift {mode:?}"));
        }
    }

    /// Kernels waiting for a host clock run in compiled slots with the
    /// domain of their edge, blocking and nonblocking.
    #[test]
    fn host_clocks_run_in_compiled_slots() {
        for assign in ["=", "<="] {
            let source = format!(
                "`timescale 1ns/1ns
                module Top(input logic clk, output logic [7:0] sum);
                    logic [7:0] stim = 8'd0;
                    logic [7:0] r = 8'd0;
                    initial forever @(posedge clk) stim {assign} stim + 8'd3;
                    always_ff @(posedge clk) r <= r + stim;
                    assign sum = r;
                endmodule"
            );
            host_driven_agrees(&source, &[(1000, &[Host::Clock("clk", 10, 5)])]);
        }
    }

    /// Host clocks with coinciding edges, odd and short periods, a kernel
    /// clock in the same slots, and waits for falling and any edges.
    #[test]
    fn coincident_host_clocks_run_in_compiled_slots() {
        let source = r#"
            `timescale 1ns/1ns
            module Top(input logic a, input logic b, input logic c, output logic [7:0] seen);
                logic g = 1'b0;
                logic [7:0] log = 8'd0;
                logic [7:0] acc = 8'd0;
                logic [7:0] ra = 8'd0;
                logic [7:0] rb = 8'd0;
                logic [7:0] rg = 8'd0;
                logic [7:0] rc = 8'd0;
                initial forever @(posedge a) log = log * 8'd3 + 8'd1;
                always #3 g = ~g;
                initial forever @(negedge b) log = log * 8'd5 + 8'd2;
                initial forever @(c) acc <= acc + log;
                always_ff @(posedge a) ra <= ra + log;
                always_ff @(negedge b) rb <= rb + acc;
                always_ff @(posedge g) rg <= rg + ra + rb;
                always_ff @(posedge c) rc <= rc + log;
                assign seen = log + acc + rg + rc;
            endmodule
        "#;
        for (a, b, c) in [(10, 6, 15), (7, 3, 1), (2, 5, 9)] {
            host_driven_agrees(
                source,
                &[(
                    300,
                    &[
                        Host::Clock("a", a, 1),
                        Host::Clock("b", b, 3),
                        Host::Clock("c", c, 2),
                    ],
                )],
            );
        }
    }

    /// Host writes, of a clocked signal too, a clock added later, a one-shot
    /// event on a clocked signal and a clock added twice to one event
    /// between runs.
    #[test]
    fn host_clocks_agree_across_host_changes() {
        let source = r#"
            `timescale 1ns/1ns
            module Top(input logic clk, input logic clk2, input logic go, input logic [7:0] k,
                       output logic [7:0] q);
                logic [7:0] n = 8'd0;
                logic [7:0] m = 8'd0;
                logic [7:0] r = 8'd0;
                logic [7:0] r2 = 8'd0;
                logic [7:0] boost = 8'd0;
                initial forever @(posedge clk) n = n + k + boost;
                initial @(posedge go) boost = 8'd5;
                initial forever @(posedge clk2) m <= m + n;
                always_ff @(posedge clk) r <= r + n + m;
                always_ff @(posedge clk2) r2 <= r2 ^ m;
                assign q = r + r2;
            endmodule
        "#;
        host_driven_agrees(
            source,
            &[
                (100, &[Host::Set("k", 1), Host::Clock("clk", 10, 5)]),
                (
                    200,
                    &[
                        Host::Set("k", 3),
                        Host::Set("go", 1),
                        Host::Clock("clk2", 7, 2),
                    ],
                ),
                (300, &[Host::Schedule("clk", 253, 1)]),
                (400, &[Host::Clock("clk2", 4, 1)]),
                (500, &[Host::Set("clk", 1)]),
                (600, &[Host::Set("clk", 0), Host::Set("clk2", 1)]),
            ],
        );
    }

    /// A kernel's `$finish` in a slot with a host clock edge, and a state
    /// file and a checkpoint taken between runs and restored.
    #[test]
    fn host_clocks_finish_and_restore_as_the_reference() {
        let source = r#"
            `timescale 1ns/1ns
            module Top(input logic clk, input logic clk2, output logic [7:0] n);
                logic [7:0] r = 8'd0;
                initial begin
                    n = 8'd0;
                    forever @(posedge clk) begin
                        n = n + 8'd1;
                        if (n == 8'd40) $finish;
                    end
                end
                logic [7:0] q = 8'd0;
                always_ff @(posedge clk2) r <= r + n;
                always_ff @(posedge clk) q <= q ^ r;
            endmodule
        "#;
        let clocks = [Host::Clock("clk", 10, 5), Host::Clock("clk2", 3, 1)];
        host_driven_agrees(source, &[(100, &clocks), (1000, &[])]);
        // A state file and a checkpoint taken in the middle continue as the
        // reference does.
        let mut reference = build_in(source, SchedulerMode::Reference);
        drive(&mut reference, &clocks);
        reference.run_until(1000).unwrap();
        assert!(reference.is_finished());
        let expected = reference.save_state().unwrap();
        for mode in MODES {
            let mut sim = build_in(source, mode);
            drive(&mut sim, &clocks);
            sim.run_until(133).unwrap();
            let state = sim.save_state().unwrap();
            let checkpoint = sim.checkpoint().unwrap();
            sim.run_until(1000).unwrap();
            assert_eq!(sim.save_state().unwrap(), expected, "{mode:?}");
            sim.restore(&checkpoint).unwrap();
            sim.run_until(1000).unwrap();
            assert_eq!(sim.save_state().unwrap(), expected, "{mode:?} checkpoint");
            let mut loaded = build_in(source, mode);
            loaded.load_state(&state).unwrap();
            loaded.run_until(1000).unwrap();
            assert_eq!(
                loaded.save_state().unwrap(),
                expected,
                "{mode:?} state file"
            );
        }
    }

    /// The next edge of a host clock that overflows the simulation time
    /// fails the run as the reference's, after the clocks before it in the
    /// slot re-armed.
    #[test]
    fn host_clock_overflow_fails_as_the_reference() {
        let source = r#"
            `timescale 1ns/1ns
            module Top(input logic a, input logic b, output logic [7:0] n);
                logic [7:0] ra = 8'd0;
                logic [7:0] rb = 8'd0;
                initial begin n = 8'd0; forever @(posedge b) n = n + 8'd1; end
                always_ff @(posedge a) ra <= ra + n;
                always_ff @(posedge b) rb <= rb + ra;
            endmodule
        "#;
        let run = |mode: SchedulerMode| {
            let mut sim = build_in(source, mode);
            sim.add_clock("a", 4, u64::MAX - 3);
            sim.add_clock("b", 10, u64::MAX - 3);
            let error = sim.run_until(u64::MAX).unwrap_err();
            (error.to_string(), sim.save_state().unwrap(), sim)
        };
        let (expected_error, expected, _) = run(SchedulerMode::Reference);
        assert!(
            expected_error.contains("overflows simulation time"),
            "{expected_error}"
        );
        for mode in MODES {
            let (error, state, sim) = run(mode);
            assert_eq!(error, expected_error, "{mode:?}");
            assert_eq!(state, expected, "{mode:?}");
            assert!(sim.scheduler_stats().compiled_slots > 0, "{mode:?}");
        }
    }

    /// Events only the host writes and no clock drives, a reset and a clock
    /// input: runs skip the clock code, host writes of them between runs
    /// reach their domains in the reference loop, and a `$finish` in the
    /// middle of a slot leaves their baselines as the reference's.
    #[test]
    fn unclocked_host_events_agree() {
        for finish in ["", "initial #47 $finish;"] {
            let source = format!(
                "`timescale 1ns/1ns
                module Top(input logic rst_n, input logic clk2, output logic [7:0] q);
                    logic clk = 1'b0;
                    logic [7:0] q2 = 8'd0;
                    always #5 clk = ~clk;
                    {finish}
                    always_ff @(posedge clk or negedge rst_n)
                        if (!rst_n) q <= 8'd0; else q <= q + 8'd1;
                    always_ff @(posedge clk2) q2 <= q2 + q;
                endmodule"
            );
            host_driven_agrees(
                &source,
                &[
                    (100, &[Host::Set("rst_n", 1), Host::Set("clk2", 1)]),
                    (200, &[Host::Set("rst_n", 0), Host::Set("clk2", 0)]),
                    (300, &[Host::Set("rst_n", 1), Host::Set("clk2", 1)]),
                ],
            );
        }
    }

    /// Combinational logic reads what each writer changes, on slots where
    /// no domain triggers: a kernel's blocking write, a published
    /// nonblocking value, a host clock's level and a host input written
    /// between runs, besides a domain's registers. Each change reaches the
    /// combinational outputs in the state file as in the reference.
    #[test]
    fn settles_follow_every_write() {
        let source = r#"
            `timescale 1ns/1ns
            module Top(input logic h, input logic [7:0] k, output logic [7:0] y);
                logic clk = 1'b0;
                logic [7:0] stim = 8'd0;
                logic [7:0] staged = 8'd0;
                logic [7:0] r = 8'd0;
                logic [7:0] rh = 8'd0;
                logic [7:0] w;
                always #5 clk = ~clk;
                initial forever @(negedge clk) stim = stim + 8'd1;
                initial forever @(negedge clk) staged <= staged + 8'd2;
                always_ff @(posedge clk) r <= r + w;
                always_ff @(posedge h) rh <= rh + 8'd1;
                assign w = stim * 8'd3 + staged + k + {7'd0, h};
                assign y = w ^ r ^ rh;
            endmodule
        "#;
        host_driven_agrees(
            source,
            &[
                (100, &[Host::Set("k", 1)]),
                (200, &[Host::Clock("h", 7, 3)]),
                (300, &[Host::Set("k", 9)]),
                (400, &[]),
            ],
        );
    }

    /// A clock on an event a kernel drives, or added twice to one event,
    /// stays with the reference loop and bounds the compiled runs.
    #[test]
    fn other_host_clocks_bound_compiled_runs() {
        let source = r#"
            `timescale 1ns/1ns
            module Top(input logic h, output logic [7:0] n);
                logic clk = 1'b0;
                logic [7:0] r = 8'd0;
                logic [7:0] r2 = 8'd0;
                always #5 clk = ~clk;
                initial begin n = 8'd0; forever @(posedge clk) n = n + 8'd1; end
                always_ff @(posedge clk) r <= r + n;
                always_ff @(posedge h) r2 <= r2 ^ n;
            endmodule
        "#;
        host_driven_agrees(source, &[(300, &[Host::Clock("clk", 6, 2)])]);
        host_driven_agrees(
            source,
            &[(300, &[Host::Clock("h", 20, 2), Host::Clock("h", 20, 7)])],
        );
    }

    /// A compiled run of several slots that fails in a later one: the
    /// shadow catches up to that slot and fails the same way, so every mode
    /// reports the reference's error and leaves its state.
    #[test]
    fn compiled_runs_that_fail_after_several_slots_agree() {
        let source = r#"
            `timescale 1fs/1fs
            module Top(output logic [7:0] x);
                logic clk = 1'b0;
                logic [7:0] r = 8'd0;
                always #5 clk = ~clk;
                always_ff @(posedge clk) r <= r + 8'd1;
                initial begin
                    x = 8'd0;
                    #30 x = 8'd1;
                    #(64'd18446744073709551605) x = 8'd2;
                end
            endmodule
        "#;
        let run = |mode: SchedulerMode| {
            let mut sim = build_in(source, mode);
            let error = sim.run_until(100).unwrap_err().to_string();
            (error, sim.time(), sim.save_state().unwrap(), sim)
        };
        let (error, time, state, _) = run(SchedulerMode::Reference);
        assert!(error.contains("delay overflows simulation time"), "{error}");
        assert_eq!(time, 30);
        for mode in MODES {
            let (mode_error, mode_time, mode_state, sim) = run(mode);
            assert_eq!(mode_error, error, "{mode:?}");
            assert_eq!(mode_time, time, "{mode:?}");
            assert_eq!(mode_state, state, "{mode:?}");
            assert!(sim.scheduler_stats().compiled_slots > 1, "{mode:?}");
        }
    }

    /// A delay-only kernel pulses the signal another kernel waits on within
    /// one activation, with two stores or in a loop: the frontend counts the
    /// signal's occurrences, the waiting kernel wakes on every pulse, and a
    /// design that counts occurrences keeps the reference loop in every
    /// mode.
    #[test]
    fn pulses_within_one_activation_wake_as_the_reference() {
        for pulse in [
            "a = 1'b1; a = 1'b0;",
            "for (int i = 0; i < 2; i++) a = ~a;",
            "for (int i = 0; i < k; i++) a = ~a;",
        ] {
            let source = format!(
                "`timescale 1ns/1ns
                module Top(output logic [7:0] n);
                    logic a = 1'b0;
                    int k = 2;
                    always #5 begin {pulse} end
                    initial begin n = 8'd0; forever @(posedge a) n = n + 8'd1; end
                endmodule"
            );
            let counted =
                crate::KernelSchedule::Reference("the design counts event occurrences".into());
            let kernels = build_in(&source, SchedulerMode::Compiled)
                .scheduler_stats()
                .kernels;
            assert!(
                kernels.iter().all(|kernel| *kernel == counted),
                "{pulse}: {kernels:?}"
            );
            let mut reference = build_in(&source, SchedulerMode::Reference);
            reference.run_until(100).unwrap();
            let n = reference.signal("n");
            assert_eq!(reference.get(n), 20u8.into(), "{pulse}");
            let expected = reference.save_state().unwrap();
            for mode in MODES {
                let mut sim = build_in(&source, mode);
                sim.run_until(100).unwrap();
                assert_eq!(sim.save_state().unwrap(), expected, "{pulse} {mode:?}");
            }
        }
    }

    #[test]
    fn differential_mode_detects_object_rng_divergence_before_randomize() {
        use crate::backend::SimBackend as _;
        let source = r#"
            module Top(output bit done);
                timeunit 1ps / 1ps;
                class C; rand int value; endclass
                C c;
                initial begin
                    c = new;
                    #5 done = 1;
                    #5 void'(c.randomize());
                end
            endmodule
        "#;
        let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("rng.sv"))], "Top")
            .scheduler_mode(SchedulerMode::Differential)
            .build()
            .unwrap();
        sim.run_until(0).unwrap();
        let object = sim.get(sim.signal("c")).to_u32_digits()[0];
        use celox_runtime::StepObserver as _;
        let mut check =
            super::ShadowCheck::new(sim.shadow.as_mut().unwrap(), SchedulerMode::Differential);
        check.before_step(&sim.state, &mut sim.simulator);
        let result = sim.state.step_until(&mut sim.simulator, 5);
        // Inject an extra draw after the primary step: before_step mirrors the
        // heap, so a mutation between run_until calls would be overwritten.
        sim.simulator.backend.heap_mut().random_object(object);
        let error = check
            .after_step(&sim.state, &mut sim.simulator, 5, &result)
            .unwrap_err()
            .to_string();
        assert!(error.contains("runtime heap"), "{error}");
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
