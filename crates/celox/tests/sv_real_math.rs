#![cfg(feature = "systemverilog")]
use std::path::Path;

macro_rules! math_backend {
    ($module:ident, $backend:ident) => {
        mod $module {
            use super::*;
            #[test]
            fn real_math_ieee_domains_zero_precision_and_large_hypot() {
                let source = r#"module Top(input real x, input shortreal narrow_input, output bit ok);
                  localparam real NAN_VALUE = $sqrt(-1.0);
                  localparam real INF_VALUE = $exp(1000.0);
                  localparam real UNKNOWN_FORMAL = $sqrt(8'hx9);
                  localparam int NAN_AS_INT = int'($sqrt(-1.0));
                  localparam int INF_AS_INT = int'($exp(1000.0));
                  real a, b;
                  logic [63:0] bits;
                  always_comb begin
                    a = $hypot(x * 1.0e308, x * 1.0e308);
                    b = $ln(narrow_input);
                    ok = a > 1.414213562373094e308 && a < 1.414213562373096e308;
                    ok &= b > 0.6931471805599452 && b < 0.6931471805599454;
                    ok &= NAN_VALUE != NAN_VALUE && INF_VALUE > 1.0e308;
                    ok &= NAN_AS_INT == 0 && INF_AS_INT == 0;
                    ok &= UNKNOWN_FORMAL == 3.0 && $sqrt(8'hz9) == UNKNOWN_FORMAL;
                    ok &= int'($sqrt(-x)) == NAN_AS_INT && int'($exp(1000.0*x)) == INF_AS_INT;
                    bits = $realtobits($sqrt(-x));
                    ok &= bits[62:52] == 11'h7ff && bits[51:0] != 0;
                    ok &= $realtobits($sin(-0.0*x)) == 64'h8000000000000000;
                    ok &= $realtobits($sqrt(-0.0*x)) == 64'h8000000000000000;
                    ok &= $ln(0.0*x) < -1.0e308 && $exp(1000.0*x) > 1.0e308;
                    ok &= $floor(-2.25*x) == -3.0 && $ceil(-2.25*x) == -2.0;
                    ok &= $atan2(x,-x) > 2.35 && $atan2(x,-x) < 2.36;
                  end
                endmodule"#;
                for four_state in [false, true] {
                    let mut sim = celox::Simulator::from_sv_sources(
                        vec![(source, Path::new("real_math_ieee.sv"))],
                        "Top",
                    )
                    .four_state(four_state)
                    .$backend()
                    .unwrap();
                    let x = sim.signal("x");
                    let narrow_input = sim.signal("narrow_input");
                    sim.modify(|io| {
                        io.set(x, 1.0f64.to_bits());
                        io.set(narrow_input, 2.0f32.to_bits());
                    })
                    .unwrap();
                    assert_eq!(sim.get(sim.signal("ok")), 1u8.into());
                }
            }

            #[test]
            fn constant_math_converts_only_in_integer_assignment_contexts() {
                let source = r#"module Top(output bit ok);
                  localparam int WIDTH = $ceil(3.1) + $floor(1.9);
                  localparam IMPLICIT = $sqrt(2.0);
                  localparam real EXPLICIT = $sqrt(8.0);
                  localparam int FROM_REAL = EXPLICIT;
                  localparam int CAST = int'($sqrt(8.0));
                  localparam logic [7:0] WRAP = 8'($pow(2.0, 40.0));
                  localparam int CHOSEN = $ln($exp(0.25)) ? 7 : 9;
                  localparam logic [127:0] MARKER = 128'h80000000000000000000000000000001;
                  logic [WIDTH-1:0] data;
                  initial begin
                    ok = $bits(data) == 5 && FROM_REAL == 3 && CAST == 3 && WRAP == 0;
                    ok &= IMPLICIT > 1.414 && IMPLICIT < 1.415 && CHOSEN == 7;
                    ok &= MARKER[127] && MARKER[0] && MARKER[126:1] == 0;
                  end
                endmodule"#;
                let mut sim = celox::Simulation::from_sv_sources(
                    vec![(source, Path::new("real_math_contexts.sv"))],
                    "Top",
                )
                .$backend()
                .unwrap();
                sim.run_until(0).unwrap();
                assert_eq!(sim.get(sim.signal("ok")), 1u8.into());
            }

            #[test]
            fn math_actuals_with_side_effects_execute_once() {
                let source = r#"module Top(input bit clk, output bit ok);
                  int calls;
                  real value;
                  function automatic real actual(); calls++; return 1.0; endfunction
                  always_ff @(posedge clk) begin
                    calls = 0; value = $ln(actual());
                    ok = calls == 1 && value == 0.0;
                    calls = 0; value = $pow(actual(), actual());
                    ok &= calls == 2 && value == 1.0;
                    calls = 0; value = $atan2(actual(), actual());
                    ok &= calls == 2 && value > 0.785 && value < 0.786;
                    calls = 0; value = $hypot(actual(), actual());
                    ok &= calls == 2 && value > 1.414 && value < 1.415;
                  end
                endmodule"#;
                let mut sim = celox::Simulator::from_sv_sources(
                    vec![(source, Path::new("real_math_effects.sv"))],
                    "Top",
                )
                .$backend()
                .unwrap();
                sim.tick(sim.event("clk")).unwrap();
                assert_eq!(sim.get(sim.signal("ok")), 1u8.into());
            }
        }
    };
}
math_backend!(interpreter, build_interpreter);
math_backend!(cranelift, build);
#[cfg(target_arch = "x86_64")]
math_backend!(native, build_native);
math_backend!(wasm, build_wasm);

#[test]
fn real_math_results_are_rejected_in_integral_only_bounds_and_counts() {
    for expression in ["1.5", "$floor(2.5)", "$ceil(2.1)"] {
        for declaration in [
            format!("logic [{expression}:0] value;"),
            format!("logic [7:0] value [0:{expression}];"),
            format!("logic [31:0] value; initial value = {{{expression}{{1'b1}}}};"),
        ] {
            let source = format!("module Top; {declaration} endmodule");
            assert!(
                celox::Simulation::from_sv_sources(
                    vec![(&source, Path::new("real_math_integral_only.sv"))],
                    "Top",
                )
                .build_interpreter()
                .is_err(),
                "{declaration}"
            );
        }
    }
}
