#![cfg(feature = "systemverilog")]
//! `$past(e, k, gate)` is the sampled value of `e` at the `k`-th previous
//! tick of its clock at which `gate` held, and `$rose`, `$fell`, `$stable`
//! and `$changed` compare the sampled value with the one at the previous
//! tick. Before enough ticks, the past value is the default sampled value
//! (IEEE 1800-2023 16.9.3). The clock is inferred from an edge-triggered
//! process with one event (16.14.6).
use std::path::Path;

/// A clock rising at 1, 3, 5, 7, ... and the declarations of the tests.
const HEADER: &str = "`timescale 1ns/1ns\nmodule Top;\n  bit clk, en;\n  bit [7:0] a, p1, p2, g;\n  \
                      bit b; logic l; int rn, fn_, sn, cn, lr, ls;\n  always #1 clk = ~clk;\n";

macro_rules! past {
    ($module:ident, $backend:ident) => {
        mod $module {
            use super::*;

            fn simulation(body: &str) -> celox::Simulation<impl celox::SimBackend> {
                let source = format!("{HEADER}{body}\nendmodule");
                celox::Simulation::from_sv_sources(
                    vec![(source.as_str(), Path::new("past.sv"))],
                    "Top",
                )
                .$backend()
                .unwrap()
            }

            fn value(sim: &mut celox::Simulation<impl celox::SimBackend>, name: &str) -> u64 {
                let signal = sim.signal(name);
                sim.get(signal).try_into().unwrap()
            }

            /// `a` is 0, 1, 2, 3 at the ticks 1, 3, 5, 7.
            #[test]
            fn past_values_are_sampled_at_earlier_ticks() {
                let mut sim = simulation(
                    "  always @(posedge clk) begin p1 <= $past(a); p2 <= $past(a, 2); end\n  \
                     initial begin #2 a = 1; #2 a = 2; #2 a = 3; end",
                );
                sim.run_until(5).unwrap();
                assert_eq!((value(&mut sim, "p1"), value(&mut sim, "p2")), (1, 0));
                sim.run_until(7).unwrap();
                assert_eq!((value(&mut sim, "p1"), value(&mut sim, "p2")), (2, 1));
            }

            #[test]
            fn sampled_history_in_selects_uses_previous_ticks() {
                let mut sim = simulation(r#"
  bit i;
  bit [1:0] data = 2'b10;
  bit p, r, f, s, c;
  always @(posedge clk) begin
    p <= data[$past($sampled(i))];
    r <= data[$rose($sampled(i))];
    f <= data[$fell($sampled(i))];
    s <= data[$stable($sampled(i))];
    c <= data[$changed($sampled(i))];
  end
  initial begin #2 i = 1; #4 i = 0; end
"#);
                for (time, expected) in [
                    (1, [0, 0, 0, 1, 0]),
                    (3, [0, 1, 0, 0, 1]),
                    (5, [1, 0, 0, 1, 0]),
                    (7, [1, 0, 1, 0, 1]),
                ] {
                    sim.run_until(time).unwrap();
                    assert_eq!(["p", "r", "f", "s", "c"].map(|n| value(&mut sim, n)), expected, "time={time}");
                }
            }

            #[test]
            fn real_stability_uses_numeric_equality() {
                let mut sim = simulation(r#"
  real d;
  shortreal e;
  bit ds, dc, es, ec, nested;
  always @(posedge clk) begin
    ds <= $stable(d); dc <= $changed(d);
    es <= $stable(e); ec <= $changed(e);
    nested <= $stable($sampled(d + 0.0));
  end
  initial begin
    #2 d = 1.5; e = 1.5;
    #4 d = 0.0; e = 0.0;
    #2 d = -0.0; e = -0.0;
  end
"#);
                for (time, expected) in [(1, [1, 0]), (3, [0, 1]), (5, [1, 0]), (7, [0, 1]), (9, [1, 0])] {
                    sim.run_until(time).unwrap();
                    assert_eq!(["ds", "dc"].map(|n| value(&mut sim, n)), expected, "real time={time}");
                    assert_eq!(["es", "ec"].map(|n| value(&mut sim, n)), expected, "shortreal time={time}");
                    assert_eq!(value(&mut sim, "nested"), expected[0]);
                }
            }

            /// The gate's ticks: with `en` low at the tick of 5, `$past(a, 1,
            /// en)` at 7 is the value at 3.
            #[test]
            fn a_gate_skips_ticks() {
                let mut sim = simulation(
                    "  always @(posedge clk) g <= $past(a, 1, en);\n  \
                     initial begin en = 1; #2 a = 1; #2 a = 2; en = 0; #2 a = 3; en = 1; end",
                );
                sim.run_until(7).unwrap();
                assert_eq!(value(&mut sim, "g"), 1);
            }

            /// An omitted number of ticks is one.
            #[test]
            fn an_omitted_number_of_ticks_is_one() {
                let mut sim = simulation(
                    "  always @(posedge clk) g <= $past(a, , en);\n  \
                     initial begin en = 1; #2 a = 1; #2 a = 2; en = 0; #2 a = 3; en = 1; end",
                );
                sim.run_until(7).unwrap();
                assert_eq!(value(&mut sim, "g"), 1);
            }

            /// The gate is evaluated at the tick, before the body clears it:
            /// the tick of 3 takes `a` = 1 into the history.
            #[test]
            fn a_gate_is_evaluated_before_the_body() {
                let mut sim = simulation(
                    "  always @(posedge clk) begin g <= $past(a, 1, en); en = 0; end\n  \
                     initial begin #2 en = 1; a = 1; #2 en = 1; a = 2; end",
                );
                sim.run_until(5).unwrap();
                assert_eq!(value(&mut sim, "g"), 1);
            }

            /// The number of ticks is a constant expression.
            #[test]
            fn the_number_of_ticks_is_a_constant_expression() {
                let mut sim = simulation(
                    "  localparam int K = 1;\n  \
                     always @(posedge clk) begin p1 <= $past(a, K); p2 <= $past(a, K + 1); end\n  \
                     initial begin #2 a = 1; #2 a = 2; #2 a = 3; end",
                );
                sim.run_until(7).unwrap();
                assert_eq!((value(&mut sim, "p1"), value(&mut sim, "p2")), (2, 1));
            }


            /// `b` is 0, 1, 1, 0 at the ticks 1, 3, 5, 7; its value before
            /// the first tick is 0.
            #[test]
            fn value_changes_compare_with_the_previous_tick() {
                let mut sim = simulation(
                    "  always @(posedge clk) begin\n    \
                     if ($rose(b)) rn <= rn + 1;\n    \
                     if ($fell(b)) fn_ <= fn_ + 1;\n    \
                     if ($stable(b)) sn <= sn + 1;\n    \
                     if ($changed(b)) cn <= cn + 1;\n  \
                     end\n  \
                     initial begin #2 b = 1; #4 b = 0; end",
                );
                sim.run_until(7).unwrap();
                assert_eq!(
                    ["rn", "fn_", "sn", "cn"].map(|name| value(&mut sim, name)),
                    [1, 1, 2, 2]
                );
            }

            /// Case equality: in four-state simulation `l` is X before the
            /// first tick and at it, which is stable, and X to 1 is a rise.
            #[test]
            fn unknown_values_compare_with_case_equality() {
                let body = "  always @(posedge clk) begin\n    \
                            if ($stable(l)) ls <= ls + 1;\n    \
                            if ($rose(l)) lr <= lr + 1;\n  \
                            end\n  \
                            initial #2 l = 1;";
                let source = format!("{HEADER}{body}\nendmodule");
                let mut sim = celox::Simulation::from_sv_sources(
                    vec![(source.as_str(), Path::new("past.sv"))],
                    "Top",
                )
                .four_state(true)
                .$backend()
                .unwrap();
                sim.run_until(1).unwrap();
                assert_eq!((value(&mut sim, "ls"), value(&mut sim, "lr")), (1, 0));
                sim.run_until(3).unwrap();
                assert_eq!((value(&mut sim, "ls"), value(&mut sim, "lr")), (1, 1));
            }

            /// Before the first tick the past value is the declared value.
            #[test]
            fn the_first_tick_sees_the_declared_value() {
                let mut sim = simulation(
                    "  bit [7:0] d = 5;\n  \
                     always @(posedge clk) p1 <= $past(d);\n  \
                     initial #0 d = 6;",
                );
                sim.run_until(1).unwrap();
                assert_eq!(value(&mut sim, "p1"), 5);
            }

            /// With an asynchronous reset the body reads the reset, so the
            /// clock is inferred (IEEE 1800-2023 16.14.6); a run for the
            /// reset is no tick. `a` is 0, 1, 2 at the ticks 1, 3, 5, and the
            /// reset pulses low at 4: had that run shifted the history,
            /// `$past(a, 2)` would be 1 at 5.
            #[test]
            fn an_asynchronous_reset_is_no_tick() {
                let mut sim = simulation(
                    "  bit rst = 1;\n  \
                     always_ff @(posedge clk or negedge rst)\n    \
                     if (!rst) p2 <= p2 + 1; else p1 <= $past(a, 2);\n  \
                     initial begin #2 a = 1; #2 a = 2; rst = 0; #0 rst = 1; end",
                );
                sim.run_until(5).unwrap();
                assert_eq!((value(&mut sim, "p1"), value(&mut sim, "p2")), (0, 1));
                sim.run_until(7).unwrap();
                assert_eq!(value(&mut sim, "p1"), 1);
            }

            /// Multi-event processes count all four-state clock edges
            /// (IEEE 1800-2023 Table 9-2), including transitions into X/Z.
            #[test]
            fn four_state_clock_edges_advance_history() {
                for (edge, old, new) in [
                    ("posedge", "0", "1"),
                    ("posedge", "0", "x"),
                    ("posedge", "0", "z"),
                    ("posedge", "x", "1"),
                    ("posedge", "z", "1"),
                    ("negedge", "1", "0"),
                    ("negedge", "1", "x"),
                    ("negedge", "1", "z"),
                    ("negedge", "x", "0"),
                    ("negedge", "z", "0"),
                ] {
                    let source = format!(r#"`timescale 1ns/1ns
module Top;
  logic clk = 1'b{old};
  bit rst = 1;
  bit [7:0] a, p;
  always_ff @({edge} clk or negedge rst)
    if (!rst) p <= 0; else p <= $past(a);
  initial begin
    a = 11;
    #1 clk = 1'b{new};
    #1 a = 22; clk = 1'b{old};
    #1 clk = 1'b{new};
  end
endmodule"#);
                    let mut sim = celox::Simulation::from_sv_sources(
                        vec![(source.as_str(), Path::new("past.sv"))], "Top",
                    ).four_state(true).$backend().unwrap();
                    sim.run_until(3).unwrap();
                    assert_eq!(value(&mut sim, "p"), 11, "{edge} {old}->{new}");
                }
            }

            /// A function called by a clocked process infers its clock.
            #[test]
            fn subroutines_infer_the_clock_of_their_caller() {
                let mut sim = simulation(
                    "  function bit [7:0] previous();\n    \
                     return $past(a);\n  \
                     endfunction\n  \
                     always @(posedge clk) p1 <= previous();\n  \
                     initial begin #2 a = 1; #2 a = 2; #2 a = 3; end",
                );
                sim.run_until(7).unwrap();
                assert_eq!(value(&mut sim, "p1"), 2);
            }

            /// So does the default of an omitted argument.
            #[test]
            fn argument_defaults_infer_the_clock_of_their_caller() {
                let mut sim = simulation(
                    "  function bit [7:0] pass(bit [7:0] v = $past(a));\n    \
                     return v;\n  \
                     endfunction\n  \
                     always @(posedge clk) p1 <= pass();\n  \
                     initial begin #2 a = 1; #2 a = 2; #2 a = 3; end",
                );
                sim.run_until(7).unwrap();
                assert_eq!(value(&mut sim, "p1"), 2);
            }

            /// A subroutine's history is as deep as its own calls need, also
            /// beyond the caller's.
            #[test]
            fn subroutines_declare_their_own_history() {
                let mut sim = simulation(
                    "  function bit [7:0] deeper();\n    \
                     return $past(a, 2);\n  \
                     endfunction\n  \
                     always @(posedge clk) begin p1 <= $past(a); p2 <= deeper(); end\n  \
                     initial begin #2 a = 1; #2 a = 2; #2 a = 3; end",
                );
                sim.run_until(7).unwrap();
                assert_eq!((value(&mut sim, "p1"), value(&mut sim, "p2")), (2, 1));
            }

            /// Without one clock to infer, the functions are rejected.
            #[test]
            fn a_clock_must_be_inferable() {
                for item in [
                    "initial #1 p1 = $past(a);",
                    "always_comb p1 = $past(a);",
                    "always @(posedge clk or posedge en) p1 <= $past(a);",
                    "always @(posedge clk) begin #1; p1 = $past(a); end",
                ] {
                    let source = format!("{HEADER}  {item}\nendmodule");
                    let error = celox::Simulation::from_sv_sources(
                        vec![(source.as_str(), Path::new("past.sv"))],
                        "Top",
                    )
                    .$backend()
                    .err()
                    .expect("rejected");
                    let text = error.to_string();
                    assert!(
                        text.contains("whose clock it infers")
                            || text.contains("$sampled in combinational logic"),
                        "{item}: {text}"
                    );
                }
            }
        }
    };
}

past!(interpreter, build_interpreter);
#[cfg(feature = "cranelift")]
past!(cranelift, build_cranelift);
#[cfg(target_arch = "x86_64")]
past!(native, build_native);
#[cfg(feature = "wasm-runtime")]
past!(wasm, build_wasm);

/// A `Simulator` tick is a tick of the clock.
#[test]
fn simulator_ticks_are_clock_ticks() {
    let source = "module Top(input bit clk, input bit [7:0] a, output bit [7:0] p);\n  \
                  always_ff @(posedge clk) p <= $past(a);\n\
                  endmodule";
    let mut sim = celox::Simulator::from_sv_sources(vec![(source, Path::new("past.sv"))], "Top")
        .build()
        .unwrap();
    let (a, p, clk) = (sim.signal("a"), sim.signal("p"), sim.event("clk"));
    for value in 1..=3u8 {
        sim.modify(|io| io.set(a, value)).unwrap();
        sim.tick(clk).unwrap();
    }
    assert_eq!(sim.get_as::<u8>(p), 2);
}

/// Before the first tick, a past value is the declared value.
#[test]
fn a_first_simulator_tick_sees_the_declared_value() {
    let source = "module Top(input bit clk, output bit r);\n  \
                  bit d = 1;\n  \
                  always_ff @(posedge clk) begin r <= $past(d); d <= 0; end\n\
                  endmodule";
    let mut sim = celox::Simulator::from_sv_sources(vec![(source, Path::new("past.sv"))], "Top")
        .build()
        .unwrap();
    let (r, clk) = (sim.signal("r"), sim.event("clk"));
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_as::<u8>(r), 1);
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_as::<u8>(r), 1);
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_as::<u8>(r), 0);
}

/// A nested `$past` would need a history of its own.
#[test]
fn nested_past_calls_are_rejected() {
    let source = "module Top(input bit clk, input bit a, output bit r);\n  \
                  always_ff @(posedge clk) r <= $past($past(a));\n\
                  endmodule";
    let error = celox::Simulator::from_sv_sources(vec![(source, Path::new("past.sv"))], "Top")
        .build()
        .unwrap_err();
    assert!(
        error.to_string().contains("calls a sampled value function"),
        "{error}"
    );
}

/// Operands whose value a history of their variables cannot hold, and
/// sampled value functions inside `$sampled`, are rejected; so is a call in
/// a subroutine that needs more history than its caller declared.
#[test]
fn operands_histories_cannot_hold_are_rejected() {
    for (body, message) in [
        (
            "always_ff @(posedge clk) t <= $past($time);",
            "calls a function",
        ),
        (
            "function bit f(); return a; endfunction\n  always_ff @(posedge clk) r <= $past(f());",
            "calls a function",
        ),
        (
            "always_ff @(posedge clk) r <= $sampled($past(a));",
            "`$sampled` of an operand that calls a sampled value function",
        ),
        (
            "function automatic bit gated(bit enable); return $past(a, 1, enable); endfunction\n  \
             always_ff @(posedge clk) r <= gated(1'b1);",
            "gate that reads the subroutine's arguments",
        ),
        (
            "function bit [7:0] previous(); return $past(a); endfunction\n  \
             always_ff @(posedge clk) r <= $past(previous());",
            "calls a function",
        ),
    ] {
        let source = format!(
            "module Top(input bit clk, input bit a, output bit r, output bit s, output time t);\n  {body}\nendmodule"
        );
        let Err(error) =
            celox::Simulator::from_sv_sources(vec![(source.as_str(), Path::new("past.sv"))], "Top")
                .build()
        else {
            panic!("{body} builds");
        };
        assert!(error.to_string().contains(message), "{body}: {error}");
    }
}

/// A history moves with a copy of the whole variable, which an unpacked
/// array has no form of.
#[test]
fn past_values_of_unpacked_arrays_are_rejected() {
    let source = "module Top(input bit clk, output bit [2:0] r);\n  \
                  bit [2:0] mem [0:3];\n  \
                  always_ff @(posedge clk) r <= $past(mem[3]);\n\
                  endmodule";
    let error = celox::Simulator::from_sv_sources(vec![(source, Path::new("past.sv"))], "Top")
        .build()
        .unwrap_err();
    assert!(
        error.to_string().contains("of an unpacked array"),
        "{error}"
    );
}
