//! Parameters of unpacked array type (IEEE 1800-2023 6.20.2).
use crate::*;

fn sorted_lines(source: &str, four_state: bool) -> Vec<String> {
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("array_parameters.sv"))], "top")
            .four_state(four_state)
            .build()
            .unwrap();
    sim.run_until(10).unwrap();
    let mut lines: Vec<_> = sim
        .drain_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            celox::RuntimeEvent::Display { message } => {
                Some(String::from_utf8_lossy(&message).into_owned())
            }
            _ => None,
        })
        .collect();
    lines.sort();
    lines
}

/// The value of an array parameter is an expression of the array type: a
/// conditional whose condition is a constant selects one of its operands
/// whole (11.4.11), and another array parameter gives its value.
#[test]
fn array_parameter_values_are_array_expressions() {
    let source = r#"`timescale 1ns/1ns
module leaf #(parameter int h_p = 0) ();
  localparam int els_lp[4:0] = (h_p == 0) ? '{2, 2, 2, 2, 3} : '{4, 4, 4, 4, 4};
  localparam int copy_lp[4:0] = els_lp;
  localparam int nested_lp[1:0] = (h_p == 2) ? '{7, 8} : ((h_p == 1) ? '{5, 6} : '{1, 0});
  initial #1 $display("%0d: %0d %0d / %0d %0d / %0d %0d", h_p, els_lp[0], els_lp[4],
                      copy_lp[0], copy_lp[4], nested_lp[1], nested_lp[0]);
endmodule
module top;
  leaf #(.h_p(0)) a ();
  leaf #(.h_p(1)) b ();
  leaf #(.h_p(2)) c ();
  initial #2 $finish;
endmodule"#;
    for four_state in [false, true] {
        assert_eq!(
            sorted_lines(source, four_state),
            [
                "0: 3 2 / 3 2 / 1 0",
                "1: 4 4 / 4 4 / 5 6",
                "2: 4 4 / 4 4 / 7 8"
            ],
            "four_state={four_state}"
        );
    }
}

/// Every operand of a conditional array value is a value of the parameter,
/// also the one its condition does not select (11.4.11).
#[test]
fn conditional_array_parameter_value_checks_the_operand_it_does_not_select() {
    for value in ["(1) ? '{1, 2} : '{1, 2, 3}", "(0) ? wide_lp : '{1, 2}"] {
        let source = format!(
            "module top; localparam int wide_lp[2:0] = '{{1, 2, 3}}; \
             localparam int v_lp[1:0] = {value}; endmodule"
        );
        assert!(
            celox::Simulation::from_sv_sources(vec![(&source, Path::new("arms.sv"))], "top")
                .build()
                .is_err(),
            "{value}"
        );
    }
}
