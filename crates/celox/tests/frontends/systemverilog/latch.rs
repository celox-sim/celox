use super::*;

sv_backends! {
    fn task_call_sensitivity(sim) { @case "latch::task_call_sensitivity"; }
    fn star_complete_declaration_startup(sim) { @case "latch::star_complete_declaration_startup"; }
    fn star_unpacked_select_sensitivity(sim) { @case "latch::star_unpacked_select_sensitivity"; }
    fn star_select_sensitivity(sim) { @case "latch::star_select_sensitivity"; }
    fn explicit_latch(sim) { @case "latch::explicit_latch"; }
    fn classic_star_latch(sim) { @case "latch::classic_star_latch"; }
    fn classic_star_declaration_startup(sim) { @case "latch::classic_star_declaration_startup"; }
    fn classic_star_initial_startup(sim) { @case "latch::classic_star_initial_startup"; }
    fn classic_star_continuous_startup(sim) { @case "latch::classic_star_continuous_startup"; }
    fn classic_star_hierarchy_declaration_startup(sim) { @case "latch::classic_star_hierarchy_declaration_startup"; }
    fn guarded_activation(sim) { @case "latch::guarded_activation"; }
    fn four_state_latch(sim) { @case "latch::four_state_latch"; }
    fn hierarchy_same_activation_change_return(sim) { @case "latch::hierarchy_same_activation_change_return"; }
    fn same_activation_change_return(sim) { @case "latch::same_activation_change_return"; }
    fn timed_initial_drives_latch(sim) { @case "latch::timed_initial_drives_latch"; }
}

#[test]
fn canonical_nonblocking_latch_has_an_explicit_unsupported_diagnostic() {
    let source =
        "module Top(input logic en,d, output logic q); always_latch if(en) q<=d; endmodule";
    let error = Simulator::from_sv_sources(vec![(source, Path::new("nba_latch.sv"))], "Top")
        .build_interpreter()
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("nonblocking assignment inside always_latch"),
        "{error}"
    );
}

#[test]
fn guarded_activation_at_o0_and_o2() {
    let case = celox_test_suite::sv::case("latch::guarded_activation").unwrap();
    let design = case.design();
    for level in [celox::OptLevel::O0, celox::OptLevel::O2] {
        let source = &design.sources[0];
        let mut sim =
            Simulator::from_sv_sources(vec![(source.text.as_str(), source.path.as_path())], "Top")
                .allow_inferred_latches(true)
                .opt_level(level)
                .build_interpreter()
                .unwrap();
        let seed = sim.signal("seed");
        let en = sim.signal("en");
        let trigger = sim.signal("trigger");
        let unrelated = sim.signal("unrelated");
        let q = sim.signal("q");
        sim.modify(|io| {
            io.set(seed, 1u8);
            io.set(en, 0u8);
            io.set(trigger, 0u8);
        })
        .unwrap();
        assert_eq!(sim.get(q), 0u8.into());
        sim.modify(|io| {
            io.set(seed, 0u8);
            io.set(en, 1u8);
        })
        .unwrap();
        assert_eq!(sim.get(q), 1u8.into());
        for value in [1u8, 0, 1] {
            sim.eval_comb().unwrap();
            sim.modify(|io| io.set(unrelated, value)).unwrap();
            assert_eq!(sim.get(q), 1u8.into());
        }
        sim.modify(|io| io.set(trigger, 1u8)).unwrap();
        assert_eq!(sim.get(q), 2u8.into());
        let checkpoint = sim.save_state().unwrap();
        sim.modify(|io| io.set(trigger, 0u8)).unwrap();
        assert_eq!(sim.get(q), 3u8.into());
        sim.load_state(&checkpoint).unwrap();
        sim.eval_comb().unwrap();
        assert_eq!(sim.get(q), 2u8.into());
    }
}

const MIXED_TOP: &str = "module Top(en: input logic, d: input logic<8>, q: output logic<8>) { inst dut: $sv::Latch(en,d,q); }";
const MIXED_DUT: &str = "module Latch(input logic en, input logic [7:0] d, output logic [7:0] q); always_comb if(en) q=d; endmodule";

#[test]
fn mixed_hierarchy_inference_remains_opt_in() {
    let builder = || {
        Simulator::from_mixed_sources(
            vec![(MIXED_TOP, Path::new("top.veryl"))],
            vec![(MIXED_DUT, Path::new("latch.sv"))],
            "Top",
        )
    };
    let error = builder().build_interpreter().unwrap_err().to_string();
    assert!(
        error.contains("latch inference inside always_comb"),
        "{error}"
    );
    let mut sim = builder()
        .allow_inferred_latches(true)
        .build_interpreter()
        .unwrap();
    let en = sim.signal("en");
    let d = sim.signal("d");
    let q = sim.signal("q");
    sim.modify(|io| {
        io.set(en, 1u8);
        io.set(d, 29u8);
    })
    .unwrap();
    assert_eq!(sim.get(q), 29u8.into());
    sim.modify(|io| {
        io.set(en, 0u8);
        io.set(d, 63u8);
    })
    .unwrap();
    assert_eq!(sim.get(q), 29u8.into());
}

#[test]
fn inferred_latch_option_does_not_admit_continuous_feedback() {
    let source = "module Top(input logic en, output logic q); assign q=q^en; endmodule";
    let error =
        Simulator::from_sv_sources(vec![(source, Path::new("continuous_feedback.sv"))], "Top")
            .allow_inferred_latches(true)
            .build_interpreter()
            .unwrap_err()
            .to_string();
    assert!(
        error.contains("latch inference") || error.contains("CombinationalLoop"),
        "{error}"
    );
}

#[test]
fn veryl_initial_testbench_drives_an_sv_latch() {
    let source = include_str!("../../../testdata/veryl/latch_initial.veryl")
        .replace("inst dut: Latch", "inst dut: $sv::Latch");
    let mut sim = Simulator::from_mixed_sources(
        vec![(source.as_str(), Path::new("initial.veryl"))],
        vec![(MIXED_DUT, Path::new("latch.sv"))],
        "Top",
    )
    .allow_inferred_latches(true)
    .build_interpreter()
    .unwrap();
    let testbench = celox::testbench::compile_initial_testbench(&sim).unwrap();
    assert_eq!(
        celox::testbench::run_compiled_testbench_to_finish(&mut sim, &testbench),
        celox::TestResult::Pass,
    );
}

#[test]
fn wildcard_latch_observes_the_first_direct_host_write_in_both_state_modes() {
    let source = "module Top(input logic en, input logic [7:0] d, output logic [7:0] q); always @* if(en) q=d; endmodule";
    for four_state in [false, true] {
        let mut sim =
            Simulator::from_sv_sources(vec![(source, Path::new("first_write.sv"))], "Top")
                .four_state(four_state)
                .build_interpreter()
                .unwrap();
        let en = sim.signal("en");
        let d = sim.signal("d");
        let q = sim.signal("q");
        // No read or eval before these writes: startup must already be armed.
        sim.set(en, 1u8);
        sim.set(d, 7u8);
        assert_eq!(sim.get(q), 7u8.into());
        sim.set(en, 0u8);
        sim.set(d, 99u8);
        assert_eq!(sim.get(q), 7u8.into());
    }
}

#[test]
fn wildcard_parallel_startup_at_o0_and_o2() {
    // Keep q as a top port so O2 PreserveTopPorts retains its observable store.
    let source = "module Top(output logic [7:0] q); wire en=1; wire [7:0] d=7; initial q=0; always @* if(en) q=d; endmodule";
    for level in [celox::OptLevel::O0, celox::OptLevel::O2] {
        let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("startup.sv"))], "Top")
            .threads(4)
            .parallel_partition(celox::ParallelPartition::Always)
            .opt_level(level)
            .build()
            .unwrap();
        let q = sim.signal("q");
        assert_eq!(sim.get(q), 7u8.into(), "{level:?}");
    }
}

#[test]
fn latch_policy_preserves_compilation_diagnostic_overrides() {
    let top = "module Top(input logic en, input logic [7:0] d, output logic [7:0] q); timeunit 1ns / 1ps; Latch dut(en,d,q); endmodule";
    for level in [celox::DiagnosticLevel::Warn, celox::DiagnosticLevel::Allow] {
        let mut policy = celox::DiagnosticPolicy::default();
        policy.set("mixed_timescale", level);
        let mut sim = Simulator::from_sv_sources(
            vec![
                (top, Path::new("top.sv")),
                (MIXED_DUT, Path::new("latch.sv")),
            ],
            "Top",
        )
        .diagnostic_policy(policy)
        .allow_inferred_latches(true)
        .build_interpreter()
        .unwrap();
        assert_eq!(
            sim.warnings().len(),
            usize::from(level == celox::DiagnosticLevel::Warn)
        );
        let en = sim.signal("en");
        let d = sim.signal("d");
        let q = sim.signal("q");
        sim.modify(|io| {
            io.set(en, 1u8);
            io.set(d, 23u8);
        })
        .unwrap();
        assert_eq!(sim.get(q), 23u8.into());
        sim.modify(|io| {
            io.set(en, 0u8);
            io.set(d, 99u8);
        })
        .unwrap();
        assert_eq!(sim.get(q), 23u8.into());
    }
}

#[test]
fn change_return_activation_at_o0_and_o2() {
    for name in [
        "latch::same_activation_change_return",
        "latch::hierarchy_same_activation_change_return",
    ] {
        let case = celox_test_suite::sv::case(name).unwrap();
        let design = case.design();
        // The cases are four-state: two-state mode runs `always @*` at time zero.
        for level in [celox::OptLevel::O0, celox::OptLevel::O2] {
            let mut sim = celox::Simulation::from_sv_sources(
                design
                    .sources
                    .iter()
                    .map(|source| (source.text.as_str(), source.path.as_path()))
                    .collect(),
                "Top",
            )
            .four_state(true)
            .opt_level(level)
            .build_interpreter()
            .unwrap();
            sim.run_until(10_000)
                .unwrap_or_else(|error| panic!("{name}, {level:?}: {error}"));
            assert!(sim.is_finished(), "{name}, {level:?}");
        }
    }
}

#[test]
fn veryl_initial_change_return_activates_an_sv_latch() {
    let top = r#"
#[test(Top)] module Top {
    inst clk: $tb::clock_gen;
    var en: logic;
    var d: logic;
    var q: logic<8>;
    inst dut: $sv::Latch(en, d, q);
    initial {
        en = 0; d = 0;
        clk.next(1);
        en = 1;
        clk.next(1);
        $assert(q == 8'd1);
        d = 1; d = 0;
        clk.next(1);
        $assert(q == 8'd2);
        $finish();
    }
}
"#;
    let dut = "module Latch(input wire en, d, output wire [7:0] q); logic [7:0] held=0; assign q=held; always @* if(en && !d) held=held+1; endmodule";
    for four_state in [false, true] {
        let mut sim = Simulator::from_mixed_sources(
            vec![(top, Path::new("initial_pulse.veryl"))],
            vec![(dut, Path::new("latch.sv"))],
            "Top",
        )
        .four_state(four_state)
        .build_interpreter()
        .unwrap();
        let testbench = celox::testbench::compile_initial_testbench(&sim).unwrap();
        assert_eq!(
            celox::testbench::run_compiled_testbench_to_finish(&mut sim, &testbench),
            celox::TestResult::Pass,
            "four_state={four_state}",
        );
    }
}

#[test]
fn sv_initial_change_return_activates_a_veryl_previous_value_latch() {
    let veryl = "module Latch(en: input logic, d: input logic<8>, q: output logic<8>) { var held: logic<8>; always_comb { if en { q = held; held = d; } } }";
    let sv = r#"
module Top;
    logic en = 1;
    logic [7:0] d = 0;
    logic [7:0] q;
    prj_Latch dut(en, d, q);
    initial begin
        #1; d = 7;
        #1; if (q !== 0) $fatal(1, "before pulse q=%0d", q);
        d = 8; d = 7;
        #1; if (q !== 7 && q !== 8) $fatal(1, "after pulse q=%0d", q);
        $finish;
    end
endmodule
"#;
    for four_state in [false, true] {
        for level in [celox::OptLevel::O0, celox::OptLevel::O2] {
            let mut sim =
                celox::Simulation::from_sources(vec![(veryl, Path::new("latch.veryl"))], "Top")
                    .with_sv_sources(vec![(sv, Path::new("initial_pulse.sv"))])
                    .allow_inferred_latches(true)
                    .four_state(four_state)
                    .opt_level(level)
                    .build_interpreter()
                    .unwrap();
            sim.run_until(10_000)
                .unwrap_or_else(|error| panic!("four_state={four_state}, {level:?}: {error}"));
            assert!(sim.is_finished());
        }
    }
}

#[test]
fn sliced_latch_change_return_does_not_count_other_bits() {
    // Icarus expands always_* constant selects to the whole vector, so keep
    // exact range sensitivity in the owning frontend's regression tests.
    let source = r#"
module Top;
    logic [1:0] bus = 0;
    logic [7:0] q = 0;
    always_latch if (!bus[0]) q = q + 1;
    initial begin
        #1; if (q !== 1) $fatal(1, "startup");
        bus[1] = 1; bus[1] = 0;
        #1; if (q !== 1) $fatal(1, "unrelated slice");
        bus[0] = 1; bus[0] = 0;
        #1; if (q !== 2) $fatal(1, "selected slice");
        $finish;
    end
endmodule
"#;
    for four_state in [false, true] {
        for level in [celox::OptLevel::O0, celox::OptLevel::O2] {
            let mut sim = celox::Simulation::from_sv_sources(
                vec![(source, Path::new("slice_occurrences.sv"))],
                "Top",
            )
            .four_state(four_state)
            .opt_level(level)
            .build_interpreter()
            .unwrap();
            sim.run_until(10_000)
                .unwrap_or_else(|error| panic!("four_state={four_state}, {level:?}: {error}"));
            assert!(sim.is_finished());
        }
    }
}

#[test]
fn procedural_copy_does_not_forward_change_return() {
    // Celox evaluates this procedural copy after the initial kernel yields.
    // Other simulators may legally interleave the two processes.
    let source = r#"
module Top;
    logic a = 0, w;
    logic [7:0] q = 0;
    always_comb w = a;
    always @* if (!w) q = 7;
    initial begin
        #1; q = 0; a = 1; a = 0;
        #1; if (q !== 0) $fatal(1, "procedural copy must execute before it can trigger downstream");
        $finish;
    end
endmodule
"#;
    for four_state in [false, true] {
        for level in [celox::OptLevel::O0, celox::OptLevel::O2] {
            let mut sim = celox::Simulation::from_sv_sources(
                vec![(source, Path::new("procedural_copy.sv"))],
                "Top",
            )
            .four_state(four_state)
            .opt_level(level)
            .build_interpreter()
            .unwrap();
            sim.run_until(10_000)
                .unwrap_or_else(|error| panic!("four_state={four_state}, {level:?}: {error}"));
            assert!(sim.is_finished());
        }
    }
}

#[test]
fn star_select_sensitivity_differs_from_latch_ranges() {
    // IEEE 1800-2023 9.4.2.2 adds identifiers for @*, while 9.2.2.2.1
    // uses longest static prefixes for always_comb/always_latch.
    let source = r#"
module Top;
    logic [3:0] bus = 0;
    logic q_bit = 0, q_part = 0;
    logic [7:0] latch_bit = 0, latch_part = 0;
    always @* if (!bus[0]) q_bit = 1;
    always @* if (bus[1:0] == 0) q_part = 1;
    always_latch if (!bus[0]) latch_bit = latch_bit + 1;
    always_latch if (bus[1:0] == 0) latch_part = latch_part + 1;
    initial begin
        #1; q_bit = 0; q_part = 0;
        bus[3] = 1;
        #1;
        if (q_bit !== 1 || q_part !== 1) $fatal(1, "wildcard whole-variable sensitivity");
        if (latch_bit !== 1 || latch_part !== 1) $fatal(1, "latch longest-static-prefix sensitivity");
        $finish;
    end
endmodule
"#;
    for four_state in [false, true] {
        for level in [celox::OptLevel::O0, celox::OptLevel::O2] {
            let mut sim = celox::Simulation::from_sv_sources(
                vec![(source, Path::new("star_select_contrast.sv"))],
                "Top",
            )
            .four_state(four_state)
            .opt_level(level)
            .build_interpreter()
            .unwrap();
            sim.run_until(10_000)
                .unwrap_or_else(|error| panic!("four_state={four_state}, {level:?}: {error}"));
            assert!(sim.is_finished());
        }
    }
}

#[test]
fn task_call_sensitivity_in_both_state_modes_at_o0_and_o2() {
    let case = celox_test_suite::sv::case("latch::task_call_sensitivity").unwrap();
    let design = case.design();
    for four_state in [false, true] {
        for level in [celox::OptLevel::O0, celox::OptLevel::O2] {
            let mut sim = celox::Simulation::from_sv_sources(
                design
                    .sources
                    .iter()
                    .map(|source| (source.text.as_str(), source.path.as_path()))
                    .collect(),
                "Top",
            )
            .four_state(four_state)
            .opt_level(level)
            .build_interpreter()
            .unwrap();
            sim.run_until(10_000)
                .unwrap_or_else(|error| panic!("four_state={four_state}, {level:?}: {error}"));
            assert!(sim.is_finished());
        }
    }
}
