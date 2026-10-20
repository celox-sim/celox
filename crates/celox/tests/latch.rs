#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {
    fn transparent_chain(sim) {
        @case "latch::transparent_chain";
    }
    fn partial_ranges(sim) {
        @case "latch::partial_ranges";
    }
    fn four_state_enable(sim) {
        @case "latch::four_state_enable";
    }
    fn dynamic_partial_writes(sim) {
        @case "latch::dynamic_partial_writes";
    }
    fn unpacked_array_input(sim) {
        @case "latch::unpacked_array_input";
    }
    fn initial_drives_latch(sim) {
        @omit_veryl;
        @omit_sv;
        @setup { let source = include_str!("../testdata/veryl/latch_initial.veryl"); }
        @build celox::SimulatorBuilder::new(source, "Top").allow_inferred_latches(true);
        let program = celox::testbench::compile_initial_testbench(&sim).unwrap();
        assert_eq!(celox::testbench::run_compiled_testbench_to_finish(&mut sim, &program), celox::TestResult::Pass);
    }
}

#[test]
fn saved_latch_state_is_required_and_restored_across_optimization_levels() {
    let source = r#"module Top(en: input logic, d: input logic<8>, q: output logic<8>) {
        always_comb { if en { q = d; } }
    }"#;
    let mut sim = celox::SimulatorBuilder::new(source, "Top")
        .allow_inferred_latches(true)
        .opt_level(celox::OptLevel::O0)
        .build_interpreter()
        .unwrap();
    let en = sim.signal("en");
    let d = sim.signal("d");
    sim.modify(|io| {
        io.set(en, 1u8);
        io.set(d, 73u8);
    })
    .unwrap();
    sim.eval_comb().unwrap();
    sim.modify(|io| {
        io.set(en, 0u8);
        io.set(d, 19u8);
    })
    .unwrap();
    let file = sim.save_state().unwrap();
    assert_eq!(
        file.objects
            .iter()
            .find(|object| object.path == "q")
            .unwrap()
            .role,
        celox::StateRole::State
    );
    let mut restored = celox::SimulatorBuilder::new(source, "Top")
        .allow_inferred_latches(true)
        .opt_level(celox::OptLevel::O2)
        .build_cranelift()
        .unwrap();
    restored.load_state(&file).unwrap();
    let q = restored.signal("q");
    assert_eq!(restored.get(q), 73u8.into());
    restored.eval_comb().unwrap();
    assert_eq!(restored.get(q), 73u8.into());
}

#[test]
fn inference_option_does_not_suppress_explicit_arithmetic_feedback() {
    let source =
        "module Top(en: input logic, q: output logic<8>) { always_comb { if en { q += 1; } } }";
    let error = celox::SimulatorBuilder::new(source, "Top")
        .allow_inferred_latches(true)
        .build_interpreter()
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("CombinationalLoop") || error.contains("combinational loop"),
        "{error}"
    );
}

#[test]
fn inference_option_preserves_authorized_continuous_loop_detection() {
    let source = r#"module Top(a: input logic, q: output logic) {
        var v: logic<2>;
        assign v[0] = ~v[1] & a;
        assign v[1] = v[0];
        assign q = v[0];
    }"#;
    let mut sim = celox::SimulatorBuilder::new(source, "Top")
        .allow_inferred_latches(true)
        .true_loop((vec![], vec!["v".into()]), (vec![], vec!["v".into()]), 10)
        .build_interpreter()
        .unwrap();
    let a = sim.signal("a");
    sim.modify(|io| io.set(a, 1u8)).unwrap();
    assert_eq!(
        sim.eval_comb().unwrap_err(),
        celox::RuntimeErrorCode::DetectedTrueLoop
    );
}

#[test]
fn initial_change_return_activates_a_veryl_previous_value_latch() {
    let source = r#"
module Latch(en: input logic, d: input logic<8>, q: output logic<8>) {
    var held: logic<8>;
    always_comb {
        if en {
            q = held;
            held = d;
        }
    }
}
#[test(Top)] module Top {
    inst clk: $tb::clock_gen;
    var en: logic;
    var d: logic<8>;
    var q: logic<8>;
    inst dut: Latch(en, d, q);
    initial {
        en = 1; d = 0;
        clk.next(1);
        d = 7;
        clk.next(1);
        $assert(q == 8'd0, "before pulse q=%0d", q);
        d = 8; d = 7;
        clk.next(1);
        // The compiled testbench may settle between these two writes.
        $assert(q == 8'd7 || q == 8'd8, "after pulse q=%0d", q);
        $finish();
    }
}
"#;
    for four_state in [false, true] {
        for level in [celox::OptLevel::O0, celox::OptLevel::O2] {
            let mut sim = celox::SimulatorBuilder::new(source, "Top")
                .allow_inferred_latches(true)
                .four_state(four_state)
                .opt_level(level)
                .build_interpreter()
                .unwrap();
            let program = celox::testbench::compile_initial_testbench(&sim).unwrap();
            assert_eq!(
                celox::testbench::run_compiled_testbench_to_finish(&mut sim, &program),
                celox::TestResult::Pass,
                "four_state={four_state}, {level:?}",
            );
        }
    }
}
