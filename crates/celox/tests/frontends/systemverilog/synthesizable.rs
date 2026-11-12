//! Constructs commonly found in synthesizable RTL, checked against a software model.

use super::*;

sv_backends! {

    fn instance_array_elements_are_numbered_by_declared_index(sim) {
        @setup {
            let source = r#"
                module Inv(input logic [3:0] a, output logic [3:0] y);
                    assign y = ~a;
                endmodule
                module Top(input logic [7:0] a, output logic [7:0] y);
                    Inv u[3:2](.a(a), .y(y));
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("nz.sv"))], "Top");
        let hierarchy = sim.named_hierarchy();
        let (_, elements) = hierarchy
            .children
            .iter()
            .find(|(name, _)| name == "u")
            .expect("instance array `u`");
        let indices: Vec<_> = elements.iter().map(|element| element.index).collect();
        assert_eq!(indices, [2, 3]);
    }

    fn single_element_instance_arrays_are_indexed_in_the_hierarchy(sim) {
        @setup {
            let source = r#"
                module Inv(input logic [3:0] a, output logic [3:0] y);
                    assign y = ~a;
                endmodule
                module Top(input logic [3:0] a, output logic [3:0] y);
                    Inv u[0:0](.a(a), .y(y));
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("one.sv"))], "Top");
        let hierarchy = sim.named_hierarchy();
        let (_, elements) = hierarchy
            .children
            .iter()
            .find(|(name, _)| name == "u")
            .expect("instance array `u`");
        assert_eq!(elements.len(), 1);
        assert!(elements[0].indexed);
        assert_eq!(elements[0].index, 0);
    }

}

#[test]
fn task_timing_control_is_rejected() {
    let source = "module Top(input logic [7:0] a, output logic [7:0] y); \
        task automatic t(input logic [7:0] x, output logic [7:0] z); #5 z = x; endtask \
        always_comb t(a, y); endmodule";
    let error = Simulator::from_sv_sources(vec![(source, Path::new("task_delay.sv"))], "Top")
        .build_cranelift()
        .expect_err("a task with a delay must be rejected")
        .to_string();
    // IEEE 1800-2023 9.2.2.2: always_comb may not suspend.
    assert!(
        error.contains("timed task call in an always_comb body"),
        "{error}"
    );
}

#[test]
fn unsupported_constructs_name_their_tracking_issue() {
    {
        let (source, issue) = (
            "module Top(input logic a, b, output logic y); tran g(y, a); endmodule",
            457,
        );
        let error = Simulator::from_sv_sources(vec![(source, Path::new("tracking.sv"))], "Top")
            .build_cranelift()
            .expect_err("the construct must be rejected")
            .to_string();
        assert!(
            error.contains(&format!("[tracking issue #{issue}]")),
            "{error}"
        );
    }
}

// `default_nettype none` disables implicit nets (IEEE 1800-2023 22.8), so an
// undeclared continuous assignment target stays an error.
#[test]
fn default_nettype_none_rejects_undeclared_assignment_targets() {
    let source = "`default_nettype none
        module Top(input logic a, output logic y);
            assign x = a;
            assign y = x;
        endmodule
        `default_nettype wire";
    let error = Simulator::from_sv_sources(vec![(source, Path::new("none.sv"))], "Top")
        .build_cranelift()
        .expect_err("an undeclared target under `default_nettype none` is rejected")
        .to_string();
    assert!(error.contains("undeclared `x`"), "{error}");
}
