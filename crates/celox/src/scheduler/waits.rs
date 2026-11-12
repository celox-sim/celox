//! General event-control dispatch. Compact scalar waits remain a fast path;
//! every other site keeps its own snapshot layout and uses its full site id.
use super::*;

impl Generator<'_> {
    pub(super) fn dispatch_sites(
        &mut self,
        site: RegisterId,
        targets: &[celox_sir::BlockId],
        invalid: celox_sir::BlockId,
    ) {
        let site_mask = self.constant(!celox_design::PROCESS_LEVEL_WAIT, SCHEDULER_TIME_WIDTH);
        let site = self.binary(SCHEDULER_TIME_WIDTH, site, BinaryOp::And, site_mask);
        let limit = self.constant(targets.len() as u64, SCHEDULER_TIME_WIDTH);
        let valid = self.binary(1, site, BinaryOp::LtU, limit);
        let dispatch = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: valid,
            true_block: (dispatch, Vec::new()),
            false_block: (invalid, Vec::new()),
        });
        self.b.switch_to_block(dispatch);
        let bits = usize::BITS - targets.len().saturating_sub(1).leading_zeros();
        self.dispatch_site_bytes(
            site,
            targets,
            bits.max(1).div_ceil(8) as usize * 8 - 8,
            invalid,
        );
    }

    fn dispatch_site_bytes(
        &mut self,
        site: RegisterId,
        targets: &[celox_sir::BlockId],
        shift: usize,
        invalid: celox_sir::BlockId,
    ) {
        let selector = self.b.alloc_bit(8, false);
        self.b.emit(SIRInstruction::Slice(selector, site, shift, 8));
        let mut cases = Vec::new();
        let mut children = Vec::new();
        for (value, group) in targets.chunks(1usize << shift).enumerate() {
            let target = if shift == 0 {
                group[0]
            } else {
                self.b.new_block()
            };
            cases.push(SIRSwitchCase {
                value: value.into(),
                target,
            });
            if shift > 0 {
                children.push((target, group));
            }
        }
        self.b.seal_block(SIRTerminator::Switch {
            selector,
            cases,
            default: invalid,
        });
        for (block, group) in children {
            self.b.switch_to_block(block);
            self.dispatch_site_bytes(site, group, shift - 8, invalid);
        }
    }

    pub(super) fn check_wait_site(
        &mut self,
        process: usize,
        delay: AbsoluteAddr,
        wait: &Wait,
        waiting: RegisterId,
        settled: bool,
    ) -> RegisterId {
        let result = self.b.alloc_bit(1, false);
        let done = self.b.new_block_with(vec![result]);
        let dispatch = self.b.new_block();
        let zero = self.constant(0, 1);
        self.b.seal_block(SIRTerminator::Branch {
            cond: waiting,
            true_block: (dispatch, Vec::new()),
            false_block: (done, vec![zero]),
        });
        self.b.switch_to_block(dispatch);
        let site = self.load(delay, SCHEDULER_TIME_WIDTH);
        let targets: Vec<_> = wait
            .alternatives
            .iter()
            .map(|_| self.b.new_block())
            .collect();
        let invalid = self.exit_block(SchedulerExit::Internal);
        let sites: Vec<_> = wait
            .site_terms
            .iter()
            .map(|&index| targets[index])
            .collect();
        self.dispatch_sites(site, &sites, invalid);
        for (alternative, block) in wait.alternatives.iter().zip(targets) {
            self.b.switch_to_block(block);
            let occurred = self.check_raw_wait(process, alternative, waiting, settled);
            self.b.seal_block(SIRTerminator::Jump(done, vec![occurred]));
        }
        self.b.switch_to_block(done);
        result
    }
}

impl Generator<'_> {
    /// Count a compact one-bit host-clock write with the same ordering as the
    /// runtime calendar. Shared storage aliases use the writer's physical value.
    pub(super) fn count_host_clock(
        &mut self,
        schema: &celox_design::RuntimeSchema<AbsoluteAddr>,
        signal: AbsoluteAddr,
        old: RegisterId,
        new: RegisterId,
        due: RegisterId,
    ) {
        let storage = |address| self.shared.get(&address).copied().unwrap_or(address);
        let target = storage(signal);
        let mut counters: Vec<(CounterKind, AbsoluteAddr, Option<AbsoluteAddr>)> = schema
            .watches
            .iter()
            .filter(|watch| {
                storage(watch.signal) == target
                    && watch.changes.is_some()
                    && watch.rises.is_some()
                    && watch.falls.is_some()
            })
            .flat_map(|watch| {
                watch
                    .counters()
                    .map(|(kind, counter)| (kind, counter, None))
            })
            .collect();
        for site in &schema.wait_sites {
            for (&(signal, kind), gate) in site.terms.iter().zip(&site.gates) {
                if storage(signal) == target
                    && let Some(gate) = gate
                    && let Some(counted) = gate.counters
                {
                    counters.push((kind, counted.all, None));
                    counters.push((kind, counted.gated, Some(gate.signal)));
                }
            }
        }
        for (kind, counter, gate) in counters {
            let occurred = match kind {
                CounterKind::Changes => self.binary(1, old, BinaryOp::Ne, new),
                CounterKind::Rises => self.level_edge(true, old, new),
                CounterKind::Falls => self.level_edge(false, old, new),
            };
            let mut occurred = self.binary(1, occurred, BinaryOp::And, due);
            if let Some(gate) = gate {
                let level = self.signal_level(gate);
                let enabled = self.equals(level, 1);
                occurred = self.binary(1, occurred, BinaryOp::And, enabled);
            }
            let before = self.load(counter, 32);
            let delta = self.widen(occurred, 32);
            let after = self.binary(32, before, BinaryOp::Add, delta);
            self.store(counter, 32, after);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{KernelSchedule, SchedulerMode, SimBackend, Simulation};

    fn check<B: SimBackend>(mut sim: Simulation<B>, until: u64) {
        sim.run_until(1).unwrap();
        let before = sim.scheduler_stats();
        assert!(
            before
                .kernels
                .iter()
                .all(|kernel| *kernel == KernelSchedule::Compiled),
            "{before:?}"
        );
        // Cross several host boundaries while different wait sites are armed.
        // Handover must use the active site's layout, including after site 255.
        for stop in 2..=until {
            sim.run_until(stop).unwrap();
        }
        let after = sim.scheduler_stats();
        assert!(after.compiled_slots > before.compiled_slots, "{after:?}");
        assert_eq!(after.reference_steps, before.reference_steps, "{after:?}");
        assert!(after.checked_steps > 0, "{after:?}");
        let count = sim.signal("count");
        assert!(sim.get(count) > 0u8.into());
    }

    fn check_backends(source: &str, until: u64) {
        for four_state in [false, true] {
            let build = || {
                Simulation::from_sv_sources(
                    vec![(source, std::path::Path::new("general-waits.sv"))],
                    "Top",
                )
                .four_state(four_state)
                .scheduler_mode(SchedulerMode::DifferentialRuns)
            };
            check(build().build_native().unwrap(), until);
            #[cfg(feature = "cranelift")]
            check(build().build_cranelift().unwrap(), until);
            #[cfg(feature = "wasm-runtime")]
            check(build().build_wasm().unwrap(), until);
        }
    }

    #[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
    #[test]
    fn opaque_dependencies_use_generated_scans() {
        extern "C" fn increment(value: i32) -> i32 {
            value.wrapping_add(1)
        }
        for body in [
            "int raw = 0; always #3 raw = increment(data); wire [31:0] observed = raw + 1;",
            "int observed = 0; initial forever begin @(data); observed = increment(data); end",
            "int observed = 0; always @(posedge clk) observed = increment(data);",
        ] {
            let source = format!(
                r#"`timescale 1ns/1ns
module Top;
    import "DPI-C" function int increment(input int value);
    bit clk = 0;
    int data = 0, count = 0;
    always #1 clk = ~clk;
    always #2 data = data + 1;
    {body}
    initial forever begin @(observed); count = count + 1; end
endmodule"#
            );
            for (four_state, hotspot) in
                [(false, false), (true, false), (false, true), (true, true)]
            {
                let build = Simulation::from_sv_sources(
                    vec![(source.as_str(), std::path::Path::new("opaque-waits.sv"))],
                    "Top",
                )
                .four_state(four_state)
                .hotspot_jit(hotspot)
                .scheduler_mode(SchedulerMode::DifferentialRuns);
                // SAFETY: the imported signature is int increment(int).
                let build = unsafe { build.dpi_function("increment", increment as *const ()) };
                check(build.build_scheduler_test().unwrap(), 24);
            }
        }
    }

    #[test]
    fn different_event_lists_stay_compiled() {
        check_backends(
            r#"`timescale 1ns/1ns
module Top;
    logic [7:0] a = 0;
    logic [72:0] b = 0;
    int count = 0;
    always #2 a = a + 1;
    always #3 b = b + 1;
    initial forever begin
        @(posedge a); count = count + 1;
        @(a or b); count = count + 1;
        @(negedge a or negedge b); count = count + 1;
    end
endmodule"#,
            48,
        );
    }

    #[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
    #[test]
    fn event_writing_waits_use_generated_waves() {
        for body in [
            "initial forever begin @(posedge root); generated = ~generated; end",
            "initial forever begin @(posedge root); generated = 1; generated = 0; end",
            "initial forever begin @(posedge root); generated = ~generated; end\n\
             always @(negedge root) generated = ~generated;",
        ] {
            let source = format!(
                r#"`timescale 1ns/1ns
module Top;
    bit root = 0;
    logic generated = 0;
    int count = 0;
    always #1 root = ~root;
    {body}
    always_ff @(posedge generated) count <= count + 1;
endmodule"#
            );
            for (four_state, hotspot) in
                [(false, false), (true, false), (false, true), (true, true)]
            {
                let simulation = Simulation::from_sv_sources(
                    vec![(
                        source.as_str(),
                        std::path::Path::new("event-writing-waits.sv"),
                    )],
                    "Top",
                )
                .four_state(four_state)
                .hotspot_jit(hotspot)
                .scheduler_mode(SchedulerMode::DifferentialRuns)
                .build_scheduler_test()
                .unwrap();
                check(simulation, 24);
            }
        }
    }

    #[test]
    fn full_wait_site_index_stays_compiled() {
        let mut source = String::from(
            r#"`timescale 1ns/1ns
module Top;
    logic a = 0;
    logic b = 0;
    int count = 0;
    always #1 a = ~a;
    always #2 b = ~b;
    initial forever begin
"#,
        );
        for index in 0..260 {
            source.push_str(if index % 2 == 0 {
                "@(posedge a); count = count + 1;\n"
            } else {
                "@(negedge b); count = count + 1;\n"
            });
        }
        source.push_str("end\nendmodule");
        check_backends(&source, 540);
    }

    #[test]
    fn conditional_event_lists_stay_compiled() {
        // The accepted pulses have disappeared, and the gates are false,
        // before a process boundary. The other pulses must remain rejected
        // even though their gates become true afterwards (LRM 9.4.2.3).
        check_backends(
            r#"`timescale 1ns/1ns
module Top;
    logic a = 0, b = 0, ena = 0, enb = 0;
    bit phase = 0;
    int count = 0;
    always #2 begin
        phase = ~phase;
        ena = phase; a = 1; a = 0; ena = ~phase;
    end
    always #3 begin
        enb = 1; b = 1; b = 0; enb = 0;
    end
    initial forever begin
        @(posedge a iff ena); count = count + 1;
        @(negedge b iff enb or posedge a iff ena); count = count + 1;
    end
endmodule"#,
            48,
        );
    }

    #[test]
    fn settled_conditional_events_stay_compiled() {
        check_backends(
            r#"`timescale 1ns/1ns
module Top;
    logic [7:0] data = 0;
    logic enable = 0;
    wire [7:0] observed = data + 1;
    wire gate = enable && data[0];
    int count = 0;
    always #2 begin
        data = data + 1;
        enable = data[1] ? 1'bx : 1'b1;
    end
    always @(observed iff gate) count = count + 1;
endmodule"#,
            48,
        );
    }

    #[test]
    fn inert_wait_does_not_force_other_processes_to_reference() {
        check_backends(
            r#"`timescale 1ns/1ns
module Top;
    int count = 0;
    always #1 count = count + 1;
    initial wait (0);
endmodule"#,
            12,
        );
    }

    #[test]
    fn array_event_expressions_stay_compiled() {
        for width in [13, 65] {
            let source = format!(
                r#"`timescale 1ns/1ns
module Top;
    logic [{high}:0] words [0:2];
    bit [1:0] index = 0;
    int count = 0;
    initial begin words[0] = 0; words[1] = 0; words[2] = 0; end
    always #2 begin
        words[index] = words[index] + 1;
        index = index == 2 ? 0 : index + 1;
    end
    always @(words) count = count + 1;
endmodule"#,
                high = width - 1
            );
            check_backends(&source, 24);
        }
    }

    #[test]
    fn more_than_compact_term_count_stays_compiled() {
        let mut source = String::from("`timescale 1ns/1ns\nmodule Top;\nint count = 0;\n");
        for index in 0..65 {
            source.push_str(&format!("logic c{index} = 0;\n"));
        }
        source.push_str("always #1 begin\n");
        for index in 0..65 {
            source.push_str(&format!("c{index} = ~c{index};\n"));
        }
        source.push_str("end\ninitial forever begin\n");
        for index in 0..65 {
            source.push_str(&format!("@(c{index}); count = count + 1;\n"));
        }
        source.push_str("end\nendmodule");
        check_backends(&source, 70);
    }
}

#[cfg(test)]
mod restored_and_scheduled_gates {
    use crate::{BaselineRecord, SchedulerMode, Simulation};

    #[test]
    fn restored_legacy_iff_counters_enter_compiled_waits() {
        let source = r#"`timescale 1ns/1ns
module Top;
 bit clk=0, en=1;
 int count=0;
 always #2 clk=!clk;
 initial forever begin @(posedge clk iff en); count++; end
endmodule"#;
        for four_state in [false, true] {
            let make = |mode| {
                Simulation::from_sv_sources(
                    vec![(source, std::path::Path::new("restored-iff-counter.sv"))],
                    "Top",
                )
                .four_state(four_state)
                .scheduler_mode(mode)
                .build_native()
                .unwrap()
            };
            let mut native = make(SchedulerMode::Compiled);
            let mut reference = make(SchedulerMode::Reference);
            reference.run_until(5).unwrap();
            let mut state = reference.save_state().unwrap();
            let mut converted = 0;
            for waiter in &mut state.schedule.as_mut().unwrap().processes.waiters {
                for baseline in &mut waiter.baselines {
                    if let BaselineRecord::Gated {
                        gated, value, mask, ..
                    } = baseline
                    {
                        *baseline = BaselineRecord::Counter {
                            seen: *gated,
                            value: value.clone(),
                            mask: mask.clone(),
                        };
                        converted += 1;
                    }
                }
            }
            assert!(converted > 0);
            native.load_state(&state).unwrap();
            reference.load_state(&state).unwrap();
            let before = native.scheduler_stats().reference_steps;
            for end in [6, 9, 14, 22] {
                native.run_until(end).unwrap();
                reference.run_until(end).unwrap();
                assert_eq!(
                    native.save_state().unwrap(),
                    reference.save_state().unwrap()
                );
            }
            assert_eq!(native.scheduler_stats().reference_steps, before);
            assert!(native.scheduler_stats().compiled_slots > 0);
        }
    }

    #[test]
    fn periodic_clock_batches_count_iff_at_each_write() {
        for reverse in [false, true] {
            let ports = if reverse {
                "input logic en, clk"
            } else {
                "input logic clk, en"
            };
            let source = format!(
                r#"`timescale 1ns/1ns
module Top({ports});
 int count=0,clock_edges=0,gate_edges=0;
 always_ff @(posedge clk) clock_edges<=clock_edges+1;
 always_ff @(posedge en) gate_edges<=gate_edges+1;
 initial forever begin @(posedge clk iff en); count++; end
endmodule"#
            );
            for four_state in [false, true] {
                for gate_delay in [1, 3] {
                    let make = |mode| {
                        Simulation::from_sv_sources(
                            vec![(
                                source.as_str(),
                                std::path::Path::new("periodic-iff-batch.sv"),
                            )],
                            "Top",
                        )
                        .four_state(four_state)
                        .scheduler_mode(mode)
                        .build_native()
                        .unwrap()
                    };
                    let mut native = make(SchedulerMode::Compiled);
                    let mut reference = make(SchedulerMode::Reference);
                    for sim in [&mut native, &mut reference] {
                        let clk = sim.signal("clk");
                        let en = sim.signal("en");
                        sim.modify(|io| {
                            io.set(clk, 0u8);
                            io.set(en, 0u8);
                        })
                        .unwrap();
                        sim.run_until(0).unwrap();
                        sim.add_clock("clk", 4, 1);
                        sim.add_clock("en", 4, gate_delay);
                    }
                    let before = native.scheduler_stats().reference_steps;
                    for end in [1, 3, 5, 9, 13, 21] {
                        native.run_until(end).unwrap();
                        reference.run_until(end).unwrap();
                        assert_eq!(
                            native.save_state().unwrap(),
                            reference.save_state().unwrap(),
                            "reverse={reverse},four_state={four_state},gate_delay={gate_delay},end={end}"
                        );
                    }
                    assert_eq!(native.scheduler_stats().reference_steps, before);
                    assert!(native.scheduler_stats().compiled_slots > 0);
                }
            }
        }
    }
}
