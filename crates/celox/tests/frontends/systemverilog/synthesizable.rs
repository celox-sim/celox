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

// A constant function selects the bits of its variables by their declared
// indices (IEEE 1800-2023 7.4.1, 11.5.1, 13.4.3), as the same call at run time
// does: of an ascending range, as fpnew's format masks `logic [0:N-1]`, and of
// a range whose right bound is not zero, by bit-selects, part-selects and
// indexed part-selects that read and write.
#[test]
fn constant_functions_select_declared_indices() {
    let source = r#"
        module Top;
            function automatic int unsigned loop_index(logic [0:3] cfg);
                int unsigned r = 0;
                for (int i = 0; i < 4; i++) if (cfg[i]) r = r + (1 << i);
                return r;
            endfunction
            function automatic int unsigned const_index(logic [0:3] cfg);
                return {cfg[0], cfg[1], cfg[2], cfg[3]};
            endfunction
            function automatic int unsigned part(logic [0:3] cfg);
                return cfg[1:2];
            endfunction
            function automatic int unsigned indexed(logic [0:3] cfg, int i);
                return cfg[i +: 2];
            endfunction
            function automatic int unsigned shifted(logic [3:0] x);
                logic [7:4] v;
                v = x;
                return v[7:6];
            endfunction
            function automatic int unsigned write_ascending();
                logic [0:3] v;
                v = '0;
                v[0] = 1'b1;
                v[2:3] = 2'b01;
                return v;
            endfunction
            function automatic int unsigned write_shifted();
                logic [7:4] v;
                v = '0;
                v[4] = 1'b1;
                v[7] = 1'b1;
                return v;
            endfunction
            function automatic int unsigned max_width(logic [0:3] cfg);
                int unsigned r = 0;
                for (int i = 0; i < 4; i++) if (cfg[i] && (8 << i) > r) r = 8 << i;
                return r;
            endfunction
            localparam int unsigned A = loop_index(4'b0011);
            localparam int unsigned B = const_index(4'b0011);
            localparam int unsigned C = part(4'b0011);
            localparam int unsigned D = indexed(4'b0011, 2);
            localparam int unsigned E = shifted(4'b1001);
            localparam int unsigned F = write_ascending();
            localparam int unsigned G = write_shifted();
            localparam int unsigned W = max_width(4'b0011);
            logic [W-1:0] widest;
            initial begin
                $display("%0d %0d %0d %0d %0d %0d %0d %0d", A, B, C, D, E, F, G, $bits(widest));
                $display("%0d %0d %0d %0d %0d %0d %0d %0d", loop_index(4'b0011),
                    const_index(4'b0011), part(4'b0011), indexed(4'b0011, 2), shifted(4'b1001),
                    write_ascending(), write_shifted(), max_width(4'b0011));
            end
        endmodule
    "#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("declared_indices.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    sim.run_until(1).unwrap();
    let displayed: Vec<_> = sim
        .drain_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            celox::RuntimeEvent::Display { message } => Some(String::from_utf8(message).unwrap()),
            _ => None,
        })
        .collect();
    assert_eq!(displayed, ["12 3 1 3 2 9 9 64"; 2]);
}

// A cast of a division by a parameter is a constant (IEEE 1800-2023 6.24.1,
// 11.2.1) where a constant is required: a parameter, as fpnew's
// `SIMD_WIDTH = unsigned'(Width/NUM_LANES)`, or the width of an indexed
// part-select.
#[test]
fn constant_casts_of_divisions_by_parameters() {
    let source = r#"
        `timescale 1ns/1ns
        module Top #(parameter int unsigned Width = 64, parameter int unsigned NUM_LANES = 4) ();
            localparam int unsigned SIMD_WIDTH = unsigned'(Width/NUM_LANES);
            localparam int HALF = signed'(Width / (NUM_LANES / 2));
            logic [Width-1:0] d = 64'h0123_4567_89ab_cdef;
            logic [SIMD_WIDTH-1:0] lane;
            assign lane = d[16 +: unsigned'(Width / NUM_LANES)];
            initial #1 $display("%0d %0d %0d %h", SIMD_WIDTH, HALF, $bits(lane), lane);
        endmodule
    "#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("casts.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    sim.run_until(2).unwrap();
    let displayed: Vec<_> = sim
        .drain_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            celox::RuntimeEvent::Display { message } => Some(String::from_utf8(message).unwrap()),
            _ => None,
        })
        .collect();
    assert_eq!(displayed, ["16 32 16 89ab"]);
}
