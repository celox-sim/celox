use celox::testbench::{compile_initial_testbench, run_compiled_testbench};
use celox::{BigUint, OptLevel, Simulator, TestResult};

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

all_backends! {

fn hierarchical_assignment_settles_logic_and_clocked_consumers(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @setup {
        let code = r#"
            module Child (clk: input clock, doubled: output logic<8>, sampled: output logic<8>) {
                #[allow(unassign_variable)]
                var q: logic<8>;
                #[allow(unassign_variable)]
                var scratch: logic<8>;
                assign doubled = q << 1;
                always_ff (clk) { sampled = q; }
            }
            #[test(Top)]
            module Top {
                inst clk: $tb::clock_gen;
                var doubled: logic<8>;
                var sampled: logic<8>;
                inst dut: Child (clk, doubled, sampled);
                initial {
                    dut.q = 8'h12;
                    $assert(doubled == 8'h24);
                    clk.next();
                    $assert(sampled == 8'h12);
                    dut.q = dut.q + 8'h03;
                    $assert(doubled == 8'h2a);
                    dut.q[3:0] = 4'h7;
                    $assert(doubled == 8'h2e);
                    clk.next();
                    $assert(sampled == 8'h17);
                    // A write-only child variable must survive DSE so the
                    // testbench's destination can still be bound.
                    dut.scratch = 8'h42;
                    $finish();
                }
            }
        "#;
    }
    @build Simulator::builder(code, "Top").opt_level(OptLevel::O2);
    let testbench = compile_initial_testbench(&sim).unwrap();
    assert_eq!(run_compiled_testbench(&mut sim, &testbench), TestResult::Pass);
    assert_eq!(sim.get(sim.signal("doubled")), 0x2eu32.into());
}

fn hierarchical_assignment_resolves_nested_instances_and_dynamic_selections(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "hierarchical_assignment::selections";
}

fn hierarchical_assignment_preserves_unselected_four_state_bits(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @setup {
        let code = r#"
            module Child (value: output logic<16>) {
                #[allow(unassign_variable)]
                var q: logic<16>;
                assign value = q;
            }
            #[test(Top)]
            module Top {
                var value: logic<16>;
                inst dut: Child (value);
                initial {
                    dut.q[7:0] = 8'hab;
                    $assert(value[7:0] == 8'hab);
                    $finish();
                }
            }
        "#;
    }
    @build Simulator::builder(code, "Top").four_state(true).opt_level(OptLevel::O2);
    let testbench = compile_initial_testbench(&sim).unwrap();
    assert_eq!(run_compiled_testbench(&mut sim, &testbench), TestResult::Pass);
    let (value, mask) = sim.get_four_state(sim.signal("value"));
    assert_eq!(value & BigUint::from(0xffu32), 0xabu32.into());
    assert_eq!(mask, 0xff00u32.into());
}

fn hierarchical_assignment_uses_function_result(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @setup {
        let code = r#"
            module Child (value: output logic<8>) {
                #[allow(unassign_variable)]
                var q: logic<8>;
                assign value = q;
            }
            #[test(Top)]
            module Top {
                var value: logic<8>;
                inst dut: Child (value);
                function increment (v: input logic<8>) -> logic<8> {
                    return v + 1;
                }
                initial {
                    dut.q = increment(8'h35);
                    $assert(value == 8'h36);
                    $finish();
                }
            }
        "#;
    }
    @build Simulator::builder(code, "Top").opt_level(OptLevel::O2);
    let testbench = compile_initial_testbench(&sim).unwrap();
    assert_eq!(run_compiled_testbench(&mut sim, &testbench), TestResult::Pass);
}

}

all_backends! {

fn hierarchical_assignment_to_disjoint_loop_state(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "hierarchical_assignment::disjoint_loop";
}

fn hierarchical_assignment_updates_register_state(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @setup {
        let code = r#"
            module Child (clk: input clock, q: output logic<8>) {
                always_ff (clk) { q = q + 1; }
            }
            #[test(Top)]
            module Top {
                inst clk: $tb::clock_gen;
                var q: logic<8>;
                inst dut: Child (clk, q);
                initial {
                    dut.q = 8'd5;
                    $assert(q == 5);
                    clk.next();
                    $assert(q == 6);
                    dut.q[3:0] = 4'd9;
                    clk.next();
                    $assert(q == 10);
                    $finish();
                }
            }
        "#;
    }
    @build Simulator::builder(code, "Top").opt_level(OptLevel::O2);
    let testbench = compile_initial_testbench(&sim).unwrap();
    assert_eq!(run_compiled_testbench(&mut sim, &testbench), TestResult::Pass);
}

}
