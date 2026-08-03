use celox::{Simulator, SimulatorBuilder};
use insta::assert_snapshot;

#[test]
fn test_combinational_loop_error_readability() {
    let code = r#"
        module Top (
            a: input logic,
            y: output logic
        ) {
            assign y = ~y & a;
        }
    "#;
    let res = SimulatorBuilder::new(code, "Top").build();

    assert!(res.is_err());
    let err = res.unwrap_err().to_string();
    assert_snapshot!(err);
}

#[test]
fn test_multiple_driver_error_readability() {
    let code = r#"
        module Top (
            a: input logic,
            y: output logic
        ) {
            assign y = a;
            assign y = ~a;
        }
    "#;
    let res = SimulatorBuilder::new(code, "Top").build();

    assert!(res.is_err());
    let err = res.unwrap_err().to_string();
    assert_snapshot!(err);
}

#[test]
fn test_multiple_errors_readability() {
    // Both `assign x = ~x & a` and `assign y = ~y & a` create self-loops.
    // Previously the Veryl analyzer reported both as unassign_variable warnings.
    // Now warnings pass through and the SIR scheduler detects the loops, but it
    // uses a fail-fast strategy (returns on the first unauthorized SCC), so only
    // one loop is reported. See scheduler.rs CombinationalLoop handling.
    let code = r#"
        module Top (
            a: input logic,
            x: output logic,
            y: output logic
        ) {
            assign x = ~x & a;
            assign y = ~y & a;
        }
    "#;
    let res = SimulatorBuilder::new(code, "Top").build();

    assert!(res.is_err());
    let err = res.unwrap_err().to_string();
    assert_snapshot!(err);
}

#[test]
fn test_call_non_function_error_readability() {
    let code = r#"
        module Top (
            a: input logic,
            y: output logic
        ) {
            assign y = a();
        }
    "#;
    let res = SimulatorBuilder::new(code, "Top").build();

    assert!(res.is_err());
    let err = res.unwrap_err().to_string();
    assert_snapshot!(err);
}

#[test]
fn test_top_not_found_error_readability() {
    let code = r#"
        module Foo (a: input logic, b: output logic) {
            assign b = a;
        }
    "#;
    let res = Simulator::builder(code, "NonExistentTop").build();

    assert!(res.is_err());
    let err = res.unwrap_err().to_string();
    assert_snapshot!(err);
}

#[test]
fn test_combinational_loop_sir_error_readability() {
    let code = r#"
        module Top (a: input logic, o: output logic) {
            var x: logic;
            var y: logic;
            var z: logic;
            assign x = y;
            assign y = z;
            assign z = x;
            assign o = x;
        }
    "#;
    let res = Simulator::builder(code, "Top").build();

    assert!(res.is_err());
    let err = res.unwrap_err().to_string();
    assert_snapshot!(err);
}

#[test]
fn test_sv_module_unsupported_error_readability() {
    let code = r#"
        module Top (
            i_clk  : input  logic,
            i_rst_n: input  logic,
            i_d    : input  logic,
            o_d    : output logic,
        ) {
            inst u0: $sv::delay (
                i_clk,
                i_rst_n,
                i_d,
                o_d,
            );
        }
    "#;
    let res = Simulator::builder(code, "Top").build();

    assert!(res.is_err());
    let err = res.unwrap_err().to_string();
    assert_snapshot!(err);
}

#[test]
fn test_mutable_for_bound_error_readability() {
    let code = r#"
        module Top {
            var limit: logic<8>;
            always_comb {
                limit = 4;
                for _i in 0..limit {
                    limit = 1;
                }
            }
        }
    "#;
    let error = Simulator::builder(code, "Top").build().unwrap_err();
    assert_snapshot!(error.to_string());
}

#[test]
fn test_elaborated_mutable_for_bound_error_readability() {
    let code = r#"
        module Counter (
            clk: input clock,
            count: output logic<8>,
        ) {
            always_ff {
                count += 1;
            }
        }

        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            var count: logic<8>;
            inst dut: Counter (clk, count);
            initial {
                for _i in 0..count {
                    clk.next();
                }
                $finish();
            }
        }
    "#;
    let error = Simulator::builder(code, "t").build().unwrap_err();
    assert_snapshot!(error.to_string());
}
