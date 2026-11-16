//! Replay host-written values into native wait snapshots before HDL execution.
use super::*;
use crate::simulator::native_io::ArenaHeader;
use std::mem::offset_of;

pub(crate) fn signals(schema: &celox_design::RuntimeSchema<AbsoluteAddr>) -> Vec<AbsoluteAddr> {
    let mut seen = HashSet::default();
    schema
        .wait_sites
        .iter()
        .flat_map(|site| site.terms.iter().map(|&(signal, _)| signal))
        .filter(|signal| seen.insert(*signal))
        .collect()
}

/// Retain the source snapshot offsets while removing terms the host did not
/// update. Counters and gates already observed host writes at the setter.
fn filtered(wait: &Wait, signal: AbsoluteAddr) -> Wait {
    let selected: Vec<_> = wait
        .terms
        .iter()
        .enumerate()
        .filter_map(|(index, &(term, _))| {
            (term == signal
                && wait.counters[index].is_none()
                && wait.gates.get(index).is_none_or(Option::is_none))
            .then_some(index)
        })
        .collect();
    let mut raw = wait.raw.clone();
    if let Some(raw) = &mut raw {
        raw.terms = selected.iter().map(|&index| raw.terms[index]).collect();
    }
    Wait {
        terms: selected.iter().map(|&index| wait.terms[index]).collect(),
        counters: vec![None; selected.len()],
        gates: vec![None; selected.len()],
        site_terms: wait.site_terms.clone(),
        alternatives: wait
            .alternatives
            .iter()
            .map(|wait| filtered(wait, signal))
            .collect(),
        raw,
        reactive: wait.reactive,
        sites: Vec::new(),
    }
}

pub(super) fn pending(g: &mut Generator<'_>) -> RegisterId {
    let pending = g.arena_header_load(offset_of!(ArenaHeader, host_entry));
    g.equals(pending, 1)
}

pub(super) fn replay(program: &LaidOutProgram, g: &mut Generator<'_>, checks: &[WaitCheck<'_>]) {
    let sources = signals(&program.runtime.runtime_schema);
    if sources.is_empty() {
        return;
    }
    let mut affected: HashMap<AbsoluteAddr, Vec<usize>> = HashMap::default();
    for (index, check) in checks.iter().enumerate() {
        let mut seen = HashSet::default();
        for &(signal, _) in &check.wait.terms {
            if seen.insert(signal) {
                affected.entry(signal).or_default().push(index);
            }
        }
    }
    let data = g.arena_header_load(offset_of!(ArenaHeader, host_values));
    let len = g.arena_header_load(offset_of!(ArenaHeader, host_words));
    let index = g.b.alloc_bit(64, false);
    let again = g.b.new_block_with(vec![index]);
    let dispatch = g.b.new_block();
    let done = g.b.new_block();
    let zero = g.constant(0, 64);
    g.b.seal_block(SIRTerminator::Jump(again, vec![zero]));
    g.b.switch_to_block(again);
    let more = g.binary(1, index, BinaryOp::LtU, len);
    g.b.seal_block(SIRTerminator::Branch {
        cond: more,
        true_block: (dispatch, Vec::new()),
        false_block: (done, Vec::new()),
    });
    g.b.switch_to_block(dispatch);
    let id = g.arena_load(data, index);
    let eight = g.constant(8, 64);
    let bytes = g.binary(64, index, BinaryOp::Mul, eight);
    let record = g.binary(64, data, BinaryOp::Add, bytes);
    let targets: Vec<_> = sources.iter().map(|_| g.b.new_block()).collect();
    let invalid = g.exit_block(SchedulerExit::Internal);
    g.dispatch_sites(id, &targets, invalid);
    for (signal, block) in sources.into_iter().zip(targets) {
        g.b.switch_to_block(block);
        let object = nba::writes::Object {
            signal,
            width: program.layout().widths[&signal],
            element: program
                .layout()
                .unpacked_arrays
                .get(&signal)
                .map(|layout| layout.element_width),
        };
        let words = object.width.div_ceil(64) * if g.keeps_unknowns(signal) { 2 } else { 1 };
        let size = g.constant((16 + words * 2) as u64, 64);
        let next = g.binary(64, index, BinaryOp::Add, size);
        let in_bounds = g.binary(1, next, BinaryOp::LeU, len);
        g.arena_require(in_bounds);
        let width = g.constant(object.width as u64, 64);
        let write = |g: &mut Generator<'_>, record| {
            if object.element.is_none() {
                nba::writes::write_whole(g, object, record);
            } else {
                nba::writes::write(g, object, object, Some(record), zero, width);
            }
        };
        write(g, record);
        for &check_index in affected.get(&signal).map_or(&[][..], Vec::as_slice) {
            let check = &checks[check_index];
            let wait = check.wait;
            if wait.raw.is_some() || !wait.alternatives.is_empty() {
                let filtered = filtered(wait, signal);
                let check = WaitCheck {
                    wait: &filtered,
                    ..*check
                };
                g.check_waits_at(&[check], false);
            } else {
                let mut selected = g.constant(0, 1);
                for (ordinal, &(term, _)) in wait.terms.iter().enumerate() {
                    if term != signal
                        || wait.counters[ordinal].is_some()
                        || wait.gates.get(ordinal).is_some_and(Option::is_some)
                    {
                        continue;
                    }
                    let active = if wait.terms.len() == 1 {
                        g.constant(1, 1)
                    } else {
                        let baseline = g.load(g.slots.baseline[check.index], SCHEDULER_FLAG_WIDTH);
                        let shift =
                            g.constant(u64::from(BASELINE_TERM_SHIFT), SCHEDULER_FLAG_WIDTH);
                        let term = g.binary(SCHEDULER_FLAG_WIDTH, baseline, BinaryOp::Shr, shift);
                        g.equals(term, ordinal as u64)
                    };
                    selected = g.binary(1, selected, BinaryOp::Or, active);
                }
                g.if_bit(selected, 0, |g| g.check_waits_at(&[*check], false));
            }
        }
        let offset = g.constant(((8 + words) * 8) as u64, 64);
        let restore = g.binary(64, record, BinaryOp::Add, offset);
        write(g, restore);
        g.b.seal_block(SIRTerminator::Jump(again, vec![next]));
    }
    g.b.switch_to_block(done);
    g.arena_header_store(offset_of!(ArenaHeader, host_words), zero);
}

#[cfg(all(test, target_arch = "x86_64", not(feature = "arm64-codegen")))]
mod tests {
    use crate::{SchedulerMode, Simulation};

    fn check_host_active_order<B: crate::SimBackend>(
        mut sim: Simulation<B>,
        mode: SchedulerMode,
        prepare: impl FnOnce(&mut B),
    ) {
        let a = sim.signal("a");
        let order = sim.signal("order");
        let clk = sim.signal("clk");
        sim.modify(|io| io.set(a, 0u32)).unwrap();
        sim.run_until(1).unwrap();
        prepare(&mut sim.simulator.backend);
        let before = sim.scheduler_stats();
        let mut previous = 0u32;
        for value in [1u32, 4, 2] {
            sim.modify(|io| {
                io.set(a, value);
                io.set(order, 0u32);
                io.set(clk, 1u8);
            })
            .unwrap();
            sim.run_until(1).unwrap();
            assert_eq!(
                sim.get(sim.signal("seen")),
                (previous * 3 + 1).into(),
                "{mode:?}"
            );
            assert_eq!(
                sim.get(order),
                12u32.into(),
                "raw waiter precedes derived waiter: {mode:?}"
            );
            assert_eq!(sim.get(sim.signal("derived")), (value * 3 + 1).into());
            assert_eq!(
                sim.get(sim.signal("q")),
                0u32.into(),
                "Active writes cancel the host clock edge before FF evaluation"
            );
            previous = value;
        }
        if mode == SchedulerMode::Compiled {
            let after = sim.scheduler_stats();
            assert_eq!(after.reference_steps, before.reference_steps, "{after:?}");
            assert!(after.compiled_slots > before.compiled_slots);
        }
    }

    #[test]
    fn host_awakened_active_processes_precede_combinational_settling() {
        let source = r#"module Top(input logic [31:0] a);
            wire [31:0] derived = a * 3 + 1;
            int seen = 0, order = 0, q = 0;
            bit clk = 0;
            always_ff @(posedge clk) q <= q + 1;
            initial forever begin @(derived); order = order * 10 + 2; end
            initial forever begin @(a); seen = derived; order = order * 10 + 1; clk = 0; end
        endmodule"#;
        for four_state in [false, true] {
            for mode in [SchedulerMode::Reference, SchedulerMode::Compiled] {
                let make = || {
                    Simulation::from_sv_sources(
                        vec![(source, std::path::Path::new("host-active-before-settle.sv"))],
                        "Top",
                    )
                    .four_state(four_state)
                    .scheduler_mode(mode)
                };
                check_host_active_order(make().build_native().unwrap(), mode, |_| {});
                check_host_active_order(
                    make().hotspot_jit(true).build_tiered().unwrap(),
                    mode,
                    |backend| {
                        if mode == SchedulerMode::Compiled {
                            backend.publish_scheduler_for_test();
                        }
                    },
                );
            }
        }
    }

    #[test]
    fn host_input_pulses_and_iff_gates_remain_native() {
        let source = r#"`timescale 1ns/1ns
module Top(input logic [72:0] a, b, input bit enabled);
    int wakes = 0, qualified = 0;
    logic [72:0] seen = 0;
    initial forever begin @(a or b); wakes++; seen = a; end
    initial forever begin @(posedge a[0] iff enabled); qualified++; end
endmodule"#;
        for four_state in [false, true] {
            for hotspot in [false, true] {
                for start in [0, 2] {
                    let make = |mode| {
                        Simulation::from_sv_sources(
                            vec![(source, std::path::Path::new("native-host-input.sv"))],
                            "Top",
                        )
                        .four_state(four_state)
                        .hotspot_jit(hotspot)
                        .scheduler_mode(mode)
                        .build_scheduler_test()
                        .unwrap()
                    };
                    let mut native = make(SchedulerMode::Compiled);
                    let mut reference = make(SchedulerMode::Reference);
                    for sim in [&mut native, &mut reference] {
                        let a = sim.signal("a");
                        let b = sim.signal("b");
                        let enabled = sim.signal("enabled");
                        sim.modify(|io| {
                            io.set(a, 0u8);
                            io.set(b, 0u8);
                            io.set(enabled, 0u8);
                        })
                        .unwrap();
                        sim.run_until(start).unwrap();
                    }
                    let before = native.scheduler_stats().reference_steps;
                    for batch in 0..4 {
                        for sim in [&mut native, &mut reference] {
                            let enabled = sim.signal("enabled");
                            let a = sim.signal("a");
                            let b = sim.signal("b");
                            sim.modify(|io| {
                                io.set(enabled, (batch % 2) as u8);
                                io.set(a, 1u8);
                                io.set(a, 0u8);
                                io.set(enabled, 1u8);
                                io.set_four_state(
                                    b,
                                    0u8.into(),
                                    num_bigint::BigUint::from(1u8) << 72,
                                );
                                io.set_four_state(b, 1u8.into(), 0u8.into());
                            })
                            .unwrap();
                            sim.run_until(start).unwrap();
                        }
                        assert_eq!(
                            native.save_state().unwrap(),
                            reference.save_state().unwrap(),
                            "batch={batch}, four_state={four_state}, hotspot={hotspot}, start={start}"
                        );
                        let saved = native.save_state().unwrap();
                        native.load_state(&saved).unwrap();
                    }
                    assert_eq!(native.scheduler_stats().reference_steps, before);
                    assert_eq!(native.get(native.signal("wakes")), 4u8.into());
                }
            }
        }
    }

    #[test]
    fn host_driven_orphan_and_gated_domains_remain_native() {
        for clock in ["data[0]", "data[0] & enable"] {
            let source = format!(
                r#"`timescale 1ns/1ns
module Top(input logic [1:0] data, input bit enable);
    wire clock = {clock};
    int q = 0, wakes = 0;
    always_ff @(posedge clock) q <= q + 1;
    initial forever begin @(clock); wakes++; end
endmodule"#
            );
            for four_state in [false, true] {
                let make = |mode| {
                    Simulation::from_sv_sources(
                        vec![(
                            source.as_str(),
                            std::path::Path::new("native-host-domain.sv"),
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
                    let data = sim.signal("data");
                    let enable = sim.signal("enable");
                    sim.modify(|io| {
                        io.set(data, 0u8);
                        io.set(enable, 1u8);
                    })
                    .unwrap();
                    sim.run_until(2).unwrap();
                }
                let before = native.scheduler_stats().reference_steps;
                assert_eq!(
                    native.save_state().unwrap(),
                    reference.save_state().unwrap()
                );
                // Initial settling can wake an X-valued derived clock. Check
                // the additional transitions caused by the host stimulus.
                let initial_wakes = native.get(native.signal("wakes"));
                let expected: [(u8, u8); 5] = if clock == "data[0]" {
                    [(1, 1), (1, 2), (2, 3), (2, 3), (2, 4)]
                } else {
                    [(1, 1), (1, 2), (1, 2), (2, 3), (2, 4)]
                };
                for ((data_value, enable_value), (edges, wakes)) in
                    [(1u8, 1u8), (0, 1), (1, 0), (1, 1), (0, 1)]
                        .into_iter()
                        .zip(expected)
                {
                    for sim in [&mut native, &mut reference] {
                        let data = sim.signal("data");
                        let enable = sim.signal("enable");
                        sim.modify(|io| {
                            io.set(data, data_value);
                            io.set(enable, enable_value);
                        })
                        .unwrap();
                        sim.run_until(2).unwrap();
                    }
                    assert_eq!(
                        native.save_state().unwrap(),
                        reference.save_state().unwrap(),
                        "clock={clock}, data={data_value}, enable={enable_value}"
                    );
                    assert_eq!(native.get(native.signal("q")), edges.into());
                    assert_eq!(
                        native.get(native.signal("wakes")),
                        &initial_wakes + wakes,
                        "clock={clock}, four_state={four_state}, data={data_value}, enable={enable_value}"
                    );
                }
                assert_eq!(native.scheduler_stats().reference_steps, before);
            }
        }
    }

    #[test]
    fn host_write_and_due_clock_count_only_actual_iff_transitions() {
        let source = r#"`timescale 1ns/1ns
module Top(input bit clk, enabled);
    int q = 0, wakes = 0;
    always_ff @(posedge clk) q <= q + 1;
    initial forever begin @(posedge clk iff enabled); wakes++; end
endmodule"#;
        for four_state in [false, true] {
            let make = |mode| {
                Simulation::from_sv_sources(
                    vec![(source, std::path::Path::new("native-host-due-clock.sv"))],
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
                sim.run_until(2).unwrap();
                let clk = sim.signal("clk");
                let enabled = sim.signal("enabled");
                sim.modify(|io| {
                    io.set(enabled, 1u8);
                    io.set(clk, 1u8);
                })
                .unwrap();
                // The first periodic write repeats the host's high level at
                // this timestamp. It must not count another gated occurrence.
                sim.add_clock("clk", 4, 2);
            }
            let before = native.scheduler_stats().reference_steps;
            for end in [2, 4, 6, 10] {
                native.run_until(end).unwrap();
                reference.run_until(end).unwrap();
                assert_eq!(
                    native.save_state().unwrap(),
                    reference.save_state().unwrap()
                );
                if end == 2 {
                    assert_eq!(native.get(native.signal("q")), 1u8.into());
                    assert_eq!(native.get(native.signal("wakes")), 1u8.into());
                }
            }
            assert_eq!(native.scheduler_stats().reference_steps, before);
        }
    }

    #[test]
    fn host_async_reset_without_a_periodic_clock_remains_native() {
        let source = r#"`timescale 1ns/1ns
module Top(input bit rst_n, clk2);
 bit clk = 0;
 int q = 0, q2 = 0;
 always #5 clk = !clk;
 always_ff @(posedge clk or negedge rst_n)
     if (!rst_n) q <= 0; else q <= q + 1;
 always_ff @(posedge clk2) q2 <= q2 + q;
endmodule"#;
        for four_state in [false, true] {
            for hotspot in [false, true] {
                let make = |mode| {
                    Simulation::from_sv_sources(
                        vec![(source, std::path::Path::new("native-host-reset.sv"))],
                        "Top",
                    )
                    .four_state(four_state)
                    .hotspot_jit(hotspot)
                    .scheduler_mode(mode)
                    .build_scheduler_test()
                    .unwrap()
                };
                let mut native = make(SchedulerMode::Compiled);
                let mut reference = make(SchedulerMode::Reference);
                native.run_until(0).unwrap();
                reference.run_until(0).unwrap();
                let before = native.scheduler_stats().reference_steps;
                for (start, value) in [(0, 1u8), (100, 0), (200, 1)] {
                    for sim in [&mut native, &mut reference] {
                        let reset = sim.signal("rst_n");
                        let clock = sim.signal("clk2");
                        sim.modify(|io| {
                            io.set(reset, value);
                            io.set(clock, value);
                        })
                        .unwrap();
                    }
                    for end in [start, start + 100] {
                        native.run_until(end).unwrap();
                        reference.run_until(end).unwrap();
                        assert_eq!(
                            native.save_state().unwrap(),
                            reference.save_state().unwrap()
                        );
                    }
                }
                assert_eq!(native.scheduler_stats().reference_steps, before);
            }
        }
    }
}
