#![allow(
    clippy::disallowed_types,
    reason = "test-only: reads generated-code state through atomics, as the runtime does"
)]
use super::*;

const GATED_COUNTER: &str = "module Top (clk: input clock, en: input logic, count: output logic<8>, plain: output logic<8>) {
            let gated: '_ clock = clk & en;
            always_ff (gated) { count += 1; }
            always_ff (clk) { plain += 1; }
        }";

/// A design with `clocks` clock domains whose flip-flops all read
/// one combinational value.
#[cfg(feature = "cranelift")]
fn many_clock_design(clocks: usize) -> String {
    let names = |i: usize| (b'a' + i as u8) as char;
    let mut ports = String::new();
    let mut ffs = String::new();
    for i in 0..clocks {
        let d = names(i);
        ports.push_str(&format!(
            "clk{i}: input '{d} clock, q{i}: output '{d} logic<8>,\n"
        ));
        let next = if i == 0 {
            "x + 1".to_string()
        } else {
            format!("q{} + x", i - 1)
        };
        ffs.push_str(&format!("always_ff (clk{i}) {{ q{i} = {next}; }}\n"));
    }
    format!(
        "module Top ({ports} din: input 'a logic<8>) {{
                    var x: 'a logic<8>;
                    assign x = din ^ q0;
                    unsafe (cdc) {{ {ffs} }}
                }}"
    )
}

/// Combined comb-then-FF ticks give the same results whether each
/// clock has its own merged program (few clocks) or all share the
/// one combinational program (many clocks).
#[cfg(feature = "cranelift")]
#[test]
fn combined_ticks_match_across_backends_with_shared_comb() {
    fn run<B: SimBackend>(mut sim: Simulator<B>, clocks: usize) -> Vec<BigUint> {
        let events: Vec<_> = (0..clocks).map(|i| sim.event(&format!("clk{i}"))).collect();
        let outputs: Vec<_> = (0..clocks).map(|i| sim.signal(&format!("q{i}"))).collect();
        let din = sim.signal("din");
        let mut seen = Vec::new();
        for step in 0..80u64 {
            let value = (step * 37 + 11) as u8;
            sim.modify(|io| io.set(din, value)).unwrap();
            let event = events[(step * 7 % clocks as u64) as usize];
            sim.tick_deferred_comb(event).unwrap();
            if step % 3 == 0 {
                sim.tick_deferred_comb(events[((step + 1) % clocks as u64) as usize])
                    .unwrap();
            }
            for &output in &outputs {
                seen.push(sim.get(output));
            }
        }
        seen
    }
    use celox_frontend_core::symbolic::assembly::MERGED_COMB_APPLY_EVENTS;
    for clocks in [2, MERGED_COMB_APPLY_EVENTS + 2] {
        let code = many_clock_design(clocks);
        let builder = || Simulator::builder(&code, "Top");
        let reference = run(builder().build_interpreter().unwrap(), clocks);
        let cranelift = builder().build_cranelift().unwrap();
        let shared = cranelift
            .backend
            .id_to_event_slice()
            .iter()
            .all(|event| event.comb_apply_func.is_none());
        assert_eq!(shared, clocks > MERGED_COMB_APPLY_EVENTS, "{clocks} clocks");
        assert_eq!(run(cranelift, clocks), reference, "{clocks} clocks");
        assert_eq!(
            run(builder().build().unwrap(), clocks),
            reference,
            "{clocks} clocks"
        );
    }
}

#[test]
fn signal_copy_accepts_an_aliased_four_state_view() {
    let mut sim = Simulator::builder("module Top (data: input logic<32>) {}", "Top")
        .four_state(true)
        .build_interpreter()
        .unwrap();
    let signal = sim.try_child_signal(&[], "data").unwrap();
    let value = BigUint::from(0x1234u32);
    let mask = BigUint::from(0x00ffu32);
    sim.backend
        .set_four_state(signal, value.clone(), mask.clone());
    celox_runtime::copy_signal(&mut sim.backend, signal, signal);
    assert_eq!(sim.backend.get_four_state(signal), (value, mask));
}

#[test]
fn signal_copy_marks_the_destination_for_vcd_tracking() {
    use celox_runtime::backend::SimBackend as _;
    let dir = tempfile::tempdir().unwrap();
    let mut sim = Simulator::builder(
        "module Top (a: input logic<8>, b: output logic<8>) {}",
        "Top",
    )
    .vcd(dir.path().join("copy.vcd"))
    .build_interpreter()
    .unwrap();
    let a = sim.try_child_signal(&[], "a").unwrap();
    let b = sim.try_child_signal(&[], "b").unwrap();
    let group =
        |signal: celox_runtime::SignalRef| signal.offset / celox_state_layout::TRACE_GROUP_BYTES;
    let mut groups = Vec::new();
    assert!(sim.backend.take_vcd_activity(&mut groups));
    sim.backend.set(a, 0xa5u8);
    assert!(sim.backend.take_vcd_activity(&mut groups));
    assert!(groups.contains(&group(a)), "{groups:?}");
    celox_runtime::copy_signal(&mut sim.backend, a, b);
    assert_eq!(sim.backend.get(b), BigUint::from(0xa5u8));
    assert!(sim.backend.take_vcd_activity(&mut groups));
    assert!(groups.contains(&group(b)), "{groups:?}");
}

/// Batched testbench ticks end each tick's slot in a design with
/// sampled values: a register capturing `$sampled(count)` follows
/// the preceding tick.
#[test]
fn batched_testbench_ticks_end_each_slot() {
    let source = "module Top(input bit clk, output bit [7:0] count, output bit [7:0] seen);\n  \
                          always_ff @(posedge clk) begin count <= count + 1; seen <= $sampled(count); end\n\
                          endmodule";
    let mut sim = Simulator::from_sv_sources(vec![(source, std::path::Path::new("t.sv"))], "Top")
        .build()
        .unwrap();
    let clk = sim.event("clk");
    sim.dirty = true;
    let mut done = 0;
    while done < 3 {
        let (ran, result) = sim.tick_deferred_comb_many(clk, 3 - done);
        result.unwrap();
        done += ran;
    }
    let seen = sim.signal("seen");
    assert_eq!(sim.get(seen), BigUint::from(2u8));
}

#[test_case::test_case(false; "two_state")]
#[test_case::test_case(true; "four_state")]
fn cached_coincident_edges_match_fresh_plans_after_level_and_gate_changes(four_state: bool) {
    let source = "module Top(input logic aclk, bclk, cclk, en,
                output logic [15:0] a, b, c, g);
                wire gated = aclk & en;
                always @(posedge aclk) a <= a + 1;
                always @(posedge bclk) b <= a;
                always @(posedge cclk) c <= b;
                always @(posedge gated) g <= g + 1;
            endmodule";
    let build = || {
        Simulator::from_sv_sources(vec![(source, std::path::Path::new("cache.sv"))], "Top")
            .four_state(four_state)
            .build()
            .unwrap()
    };
    let mut cached = build();
    let mut fresh = build();
    for sim in [&mut cached, &mut fresh] {
        for name in ["a", "b", "c", "g", "aclk", "bclk", "cclk", "en"] {
            let signal = sim.signal(name);
            sim.set_four_state(signal, BigUint::from(0u8), BigUint::from(0u8));
        }
    }
    let mut random = 13u32;
    for step in 0..256 {
        random = random.wrapping_mul(1664525).wrapping_add(1013904223);
        for (sim, reuse) in [(&mut cached, true), (&mut fresh, false)] {
            let enable = sim.signal("en");
            sim.modify(|io| io.set(enable, (step % 3 == 0) as u8))
                .unwrap();
            if four_state && step % 7 == 0 {
                let clock = sim.signal("aclk");
                sim.set_four_state(clock, BigUint::from(0u8), BigUint::from(1u8));
            }
            if !reuse {
                sim.derived.coincident_edges = Default::default();
            }
            let mut moves: Vec<_> = ["aclk", "bclk", "cclk"]
                .into_iter()
                .enumerate()
                .map(|(bit, name)| {
                    (
                        sim.event(name),
                        if random & (1 << bit) == 0 {
                            celox_runtime::Edge::Falling
                        } else {
                            celox_runtime::Edge::Rising
                        },
                    )
                })
                .collect();
            if step % 2 == 0 {
                moves.reverse();
            }
            // Duplicate signals and transitions to the existing level
            // must not change which domains belong to the cached plan.
            if step % 5 == 0 {
                moves.push((moves[0].0, moves[0].1.opposite()));
            }
            sim.edges(&moves).unwrap();
        }
        for name in ["a", "b", "c", "g", "aclk", "bclk", "cclk"] {
            let left = cached.signal(name);
            let right = fresh.signal(name);
            assert_eq!(
                cached.get_four_state(left),
                fresh.get_four_state(right),
                "step {step}: {name}"
            );
        }
        assert_eq!(cached.dirty, fresh.dirty);
    }
}

#[test]
fn edges_leave_a_finished_simulation_unchanged() {
    let mut sim = Simulator::builder(GATED_COUNTER, "Top").build().unwrap();
    let (clk, en) = (sim.event("clk"), sim.signal("en"));
    sim.modify(|io| io.set(en, 1u8)).unwrap();
    sim.posedge(clk).unwrap();
    let (count, plain) = (sim.signal("count"), sim.signal("plain"));
    assert_eq!(sim.get(count), BigUint::from(1u8));
    sim.negedge(clk).unwrap();
    sim.frozen = true;
    sim.posedge(clk).unwrap();
    sim.edges(&[(clk, celox_runtime::Edge::Rising)]).unwrap();
    sim.tick(clk).unwrap();
    let clk_signal = sim.signal("clk");
    assert_eq!(sim.get(clk_signal), BigUint::from(0u8));
    assert_eq!(sim.get(count), BigUint::from(1u8));
    assert_eq!(sim.get(plain), BigUint::from(1u8));
}

#[test]
fn edges_write_the_clock_through_the_host_watch() {
    for design in [
        GATED_COUNTER,
        "module Top (clk: input clock, plain: output logic<8>) {
                always_ff (clk) { plain += 1; }
            }",
    ] {
        let mut sim = Simulator::builder(design, "Top").build().unwrap();
        let clk = sim.event("clk");
        let clk_signal = sim.signal("clk");
        let waited = [clk_signal.offset].into_iter().collect();
        sim.host_watch = celox_runtime::HostWatch::new([], waited);
        sim.posedge(clk).unwrap();
        sim.edges(&[(clk, celox_runtime::Edge::Falling)]).unwrap();
        // Already low: no write to observe.
        sim.negedge(clk).unwrap();
        let levels: Vec<_> = sim
            .host_watch
            .log()
            .iter()
            .map(|(signal, (value, _))| (signal.offset, value.clone()))
            .collect();
        assert_eq!(
            levels,
            [
                (clk_signal.offset, BigUint::from(1u8)),
                (clk_signal.offset, BigUint::from(0u8)),
            ],
            "{design}"
        );
    }
}

#[test]
fn edges_that_run_nothing_leave_comb_stale_only_when_comb_reads_the_clock() {
    for (seen, comb_read) in [("1'b1", false), ("clk", true)] {
        let source = format!(
            "module Top (clk: input clock, count: output logic<8>, seen: output logic) {{
                        always_ff (clk) {{ count += 1; }}
                        assign seen = {seen};
                    }}"
        );
        // The interpreter requests the generic full-settle fallback.
        // Native wiring-only cones are checked in native_clock_aliases.
        let mut sim = Simulator::builder(&source, "Top")
            .build_interpreter()
            .unwrap();
        let clk = sim.event("clk");
        sim.posedge(clk).unwrap();
        assert!(!sim.dirty);
        // Moving `clk` without running a domain leaves combinational
        // logic stale only when it reads `clk`.
        sim.negedge(clk).unwrap();
        assert_eq!(sim.dirty, comb_read, "{seen}");
        sim.tick(clk).unwrap();
        assert_eq!(sim.dirty, comb_read, "{seen}");
        let seen_signal = sim.signal("seen");
        let expected = if comb_read { 0u8 } else { 1 };
        assert_eq!(sim.get(seen_signal), BigUint::from(expected));
        let count = sim.signal("count");
        assert_eq!(sim.get(count), BigUint::from(2u8));
    }
}

#[test]
fn comb_fatal_records_do_not_mask_unrelated_backend_errors() {
    let mut sim = Simulator::builder(
        r#"module Top {
                    always_comb {
                        $display("before");
                        $assert(1'd0, "fatal");
                    }
                }"#,
        "Top",
    )
    .build_interpreter()
    .unwrap();
    let (result, events) = sim.eval_comb_capturing_events();
    assert!(matches!(
        result,
        Err(RuntimeErrorCode::DetectedTrueLoopCode(-2))
    ));
    assert!(sim.check_comb_backend_result(result).is_ok());
    assert!(sim.fatal_comb_capture_error(&events).is_some());

    // These are the raw outcomes another parallel lane can report while
    // the fatal assertion's record is present in the shared buffer.
    for error in [
        RuntimeErrorCode::InternalError,
        RuntimeErrorCode::DetectedTrueLoop,
        RuntimeErrorCode::DetectedTrueLoopCode(1),
        RuntimeErrorCode::DetectedTrueLoopCode(2000),
        RuntimeErrorCode::Runtime {
            message: "fatal".to_string(),
            signals: Vec::new(),
        },
    ] {
        let expected = sim.decorate_runtime_error(error.clone());
        assert_eq!(sim.check_comb_backend_result(Err(error)), Err(expected));
    }
    // A fatal status remains identifiable after losing its record.
    assert!(
        sim.check_comb_backend_result(Err(RuntimeErrorCode::DetectedTrueLoopCode(
            celox_runtime::comb_fatal_code(1)
        )),)
            .is_ok()
    );
    // A display site, missing site, or internal failure is not fatal.
    for code in [
        celox_runtime::comb_fatal_code(0),
        celox_runtime::comb_fatal_code(99),
        i64::MIN,
    ] {
        assert!(
            sim.check_comb_backend_result(Err(RuntimeErrorCode::DetectedTrueLoopCode(code)),)
                .is_err()
        );
    }
}

#[test]
fn comb_fatal_status_survives_overwritten_event_records() {
    use celox_state_layout::{
        RUNTIME_EVENT_HEADER_SIZE, RUNTIME_EVENT_SLOT_ARG_COUNT_OFFSET,
        RUNTIME_EVENT_SLOT_SEQ_OFFSET, RUNTIME_EVENT_SLOT_SITE_OFFSET,
    };
    let mut sim = Simulator::builder(
        r#"module Top {
                    always_comb {
                        $display("before");
                        $assert(1'd0, "fatal");
                    }
                }"#,
        "Top",
    )
    .build_interpreter()
    .unwrap();
    let (result, _) = sim.eval_comb_capturing_events();
    let buffer = sim.backend.runtime_event_buffer().unwrap();
    let layout = sim.backend.layout();
    let start = sim.runtime_event_write_seq();
    // Model the other lanes that finish publishing display records
    // after the fatal lane has returned. The simulator is idle here.
    for seq in start..start + layout.runtime_event_capacity as u64 {
        let slot = seq as usize & (layout.runtime_event_capacity - 1);
        let base = RUNTIME_EVENT_HEADER_SIZE + slot * layout.runtime_event_slot_size;
        unsafe {
            let ptr = buffer.as_mut_ptr();
            ptr.add(base + RUNTIME_EVENT_SLOT_SITE_OFFSET)
                .cast::<u64>()
                .write(0);
            ptr.add(base + RUNTIME_EVENT_SLOT_ARG_COUNT_OFFSET)
                .cast::<u64>()
                .write(0);
            (*ptr
                .add(base + RUNTIME_EVENT_SLOT_SEQ_OFFSET)
                .cast::<AtomicU64>())
            .store(seq, Ordering::Release);
            (*ptr.cast::<AtomicU64>()).store(seq + 1, Ordering::Release);
        }
    }
    let events = sim.peek_backend_runtime_events_from(0);
    assert!(matches!(
        events.first(),
        Some(RawRuntimeEvent::Missed { count: 2 })
    ));
    assert!(sim.fatal_comb_capture_error(&events).is_none());
    assert!(sim.check_comb_backend_result(result.clone()).is_ok());
    assert!(matches!(sim.decorate_runtime_error(result.unwrap_err()),
                RuntimeErrorCode::Fatal { message, .. } if message.contains("overwritten")));
    let drained = sim.drain_runtime_events();
    assert!(matches!(
        drained.first(),
        Some(RuntimeEvent::Missed { count: 2 })
    ));
    assert_eq!(
        drained.len(),
        1 + sim.backend.layout().runtime_event_capacity
    );
    assert!(sim.drain_runtime_events().is_empty());
}
