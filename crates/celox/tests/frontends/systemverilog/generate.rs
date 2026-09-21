use super::*;

sv_backends! {
    fn preserves_generate_localparam_types_in_child_overrides(sim) {
        @setup {
            let sv = r#"
                module Child #(parameter P=0)(output logic [7:0] y);
                    localparam WIDTH = $bits(P);
                    localparam NEGATIVE = (P < 0);
                    assign y = WIDTH | (NEGATIVE << 6);
                endmodule
                module Top(output logic [31:0] y);
                    for (genvar i=0; i<2; i++) begin : g
                        localparam logic [3+i:0] P = '1;
                        localparam logic signed [3+i:0] Q = '1;
                        Child #(.P(P)) u(.y(y[16*i+7:16*i]));
                        Child #(.P(Q)) s(.y(y[16*i+15:16*i+8]));
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("generate_override_types.sv"))], "Top");
        sim.modify(|_| {}).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 0x45054404u32.into());
    }

    fn treats_unknown_generate_truth_as_false(sim) {
        @setup {
            let sv = r#"
                module Top #(parameter logic P = 1'bx)(output logic [4:0] y);
                    if (1'bx) begin initial $fatal; end
                    if (P) begin initial $fatal; end
                    else assign y[0] = 1;
                    if (1'bz) begin initial $fatal; end
                    else assign y[1] = 1;
                    if (2'b1x) assign y[2] = 1;
                    else begin initial $fatal; end
                    if (1) begin : g
                        localparam logic [1:0] X = 2'b0x;
                        if (X) begin initial $fatal; end
                        else assign y[3] = 1;
                        for (genvar i=0; X; i++) begin initial $fatal; end
                        for (genvar i=0; (i==0) ? 2'b1z : X; i++) begin
                            assign y[4] = 1;
                        end
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("generate_unknown_truth.sv"))], "Top").four_state(true);
        sim.modify(|_| {}).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 31u8.into());
    }

    fn resolves_generate_signal_size_parameter_dependencies(sim) {
        @setup {
            let sv = r#"
                module Top(input logic [15:0] a, output logic [15:0] y);
                    logic [1:0] data;
                    assign data = 0;
                    for (genvar i=0; i<2; i++) begin : g
                        localparam W = $bits(data);
                        localparam S = $size(data);
                        logic [W-1:0] copy;
                        logic [N-1:0] data;
                        localparam N = 8;
                        assign data = a[8*i+7:8*i];
                        assign copy = data;
                        if (W == 8 && S == 8) assign y[8*i+7:8*i] = copy;
                        else begin initial $fatal; end
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("generate_signal_size.sv"))], "Top");
        let a = sim.signal("a");
        for value in [0u16, 0x1234, 0xfedc] {
            sim.modify(|io| io.set(a, value)).unwrap();
            assert_eq!(sim.get(sim.signal("y")), value.into());
        }
    }

    fn resolves_generate_function_names_lexically(sim) {
        @setup {
            let sv = r#"
                module Top(input logic clk, input logic a, output logic [5:0] y);
                    function automatic logic f(input logic x); return x; endfunction
                    function automatic logic outer(input logic x); return f(x); endfunction
                    if (1) begin : first
                        function automatic logic f(input logic x); return ~x; endfunction
                        function automatic logic wrapper(input logic x); return f(x); endfunction
                        assign y[0] = wrapper(a);
                        always_comb y[1] = f(a);
                        always_ff @(posedge clk) y[2] <= f(a);
                        if (1) begin : nested
                            assign y[3] = wrapper(a);
                        end
                        assign y[4] = outer(a);
                    end
                    if (1) begin : second
                        function automatic logic f(input logic x); return x; endfunction
                        assign y[5] = f(a);
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("generate_function_scopes.sv"))], "Top");
        let a = sim.signal("a");
        let clk = sim.event("clk");
        for value in 0u8..2 {
            sim.modify(|io| io.set(a, value)).unwrap();
            sim.tick(clk).unwrap();
            assert_eq!(sim.get(sim.signal("y")), (if value == 0 { 15u8 } else { 48u8 }).into());
        }
    }

    fn resolves_forward_generate_localparam_dependencies(sim) {
        @setup {
            let sv = r#"
                module Top(input logic clk, output logic [7:0] y, output logic [3:0] q);
                    localparam B = 99;
                    if (1) begin : constants
                        localparam logic [WIDTH-1:0] A = B + 1;
                        localparam B = C + 1, C = 1;
                        localparam WIDTH = 4;
                        logic [A:0] tmp;
                        assign tmp = A;
                        assign y[3:0] = tmp;
                        always_ff @(posedge clk) q <= A;
                    end
                    for (genvar i = 0; i < 2; i++) begin : lanes
                        localparam A = B;
                        localparam B = C;
                        localparam C = i + 1;
                        assign y[5+2*i:4+2*i] = A;
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("generate_forward_parameters.sv"))], "Top");
        sim.modify(|_| {}).unwrap();
        sim.tick(sim.event("clk")).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 0x93u8.into());
        assert_eq!(sim.get(sim.signal("q")), 3u8.into());
    }

    fn elaborates_generate_lanes_with_local_state_and_instances(sim) {
        @setup {
            let sv = r#"
                module Child #(parameter INDEX = 0)(input logic a, output logic y);
                    assign y = a ^ (INDEX % 2);
                endmodule
                module Top(input logic clk, rst_n, input logic [3:0] a, output logic [3:0] y);
                    for (genvar i = 0; i < 4; i++) begin : lanes
                        localparam INDEX = i;
                        logic tmp;
                        logic q;
                        Child #(.INDEX(INDEX)) child(.a(a[i]), .y(tmp));
                        always_ff @(posedge clk or negedge rst_n)
                            if (!rst_n) q <= 0;
                            else q <= tmp;
                        assign y[i] = q;
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("generate_lanes.sv"))], "Top");
        let clk = sim.event("clk");
        let rst_n = sim.signal("rst_n");
        let a = sim.signal("a");
        let y = sim.signal("y");
        sim.modify(|io| { io.set(rst_n, 0u8); io.set(a, 0u8); }).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(y), 0u8.into());
        sim.modify(|io| { io.set(rst_n, 1u8); io.set(a, 3u8); }).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(y), 9u8.into());
        sim.modify(|io| io.set(a, 12u8)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(y), 6u8.into());
    }

    fn elaborates_nested_generate_and_case_branches(sim) {
        @setup {
            let sv = r#"
                module Top(input logic [3:0] a, output logic [3:0] y);
                    for (genvar row = 1; row >= 0; row--) begin : rows
                        for (genvar col = 0; col < 2; col++) begin : columns
                            localparam INDEX = row * 2 + col;
                            case (INDEX)
                                0, 3: begin : selected
                                    logic tmp;
                                    assign tmp = ~a[INDEX];
                                    assign y[INDEX] = tmp;
                                end
                                default: begin : selected
                                    logic tmp;
                                    always_comb tmp = a[INDEX];
                                    assign y[INDEX] = tmp;
                                end
                            endcase
                        end
                    end
                    for (genvar skipped = 0; skipped < 0; skipped++) begin
                        initial $fatal;
                    end
                    if (0) begin
                        initial $fatal;
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("nested_generate.sv"))], "Top");
        let a = sim.signal("a");
        let y = sim.signal("y");
        for value in 0u8..16 {
            sim.modify(|io| io.set(a, value)).unwrap();
            assert_eq!(sim.get(y), (value ^ 9).into());
        }
    }

    fn specializes_generate_scopes_for_each_child_parameter(sim) {
        @setup {
            let sv = r#"
                module Child #(parameter MODE = 0)(input logic a, output logic y);
                    case (MODE)
                        0: begin : branch
                            logic tmp;
                            assign tmp = a;
                            assign y = tmp;
                        end
                        1: begin : branch
                            logic tmp;
                            assign tmp = ~a;
                            assign y = tmp;
                        end
                        default: begin
                            initial $fatal;
                        end
                    endcase
                endmodule
                module Top(input logic a, output logic [1:0] y);
                    for (genvar i = 0; i < 2; i++) begin : lanes
                        Child #(.MODE(i)) child(.a(a), .y(y[i]));
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("specialized_generate.sv"))], "Top");
        let a = sim.signal("a");
        let y = sim.signal("y");
        sim.modify(|io| io.set(a, 0u8)).unwrap();
        assert_eq!(sim.get(y), 2u8.into());
        sim.modify(|io| io.set(a, 1u8)).unwrap();
        assert_eq!(sim.get(y), 1u8.into());
    }

    fn elaborates_generate_declaration_widths_and_shadowing(sim) {
        @setup {
            let sv = r#"
                module Top(input logic [3:0] a, output logic [3:0] y);
                    logic tmp;
                    assign tmp = 1'b0;
                    for (genvar i = 0; i < 4; i++) begin : lanes
                        logic [i:0] tmp;
                        assign tmp = a[i:0];
                        assign y[i] = tmp[i];
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("generate_widths.sv"))], "Top");
        let a = sim.signal("a");
        let y = sim.signal("y");
        for value in 0u8..16 {
            sim.modify(|io| io.set(a, value)).unwrap();
            assert_eq!(sim.get(y), value.into());
        }
    }

    fn preserves_generate_constant_scopes_and_masks(sim) {
        @setup {
            let sv = r#"
                module Child(input logic a, output logic y); assign y = a; endmodule
                module Top(output logic [3:0] y);
                    localparam logic MASK = 0;
                    for (genvar i = 0; i < 2; i++) begin : lanes
                        localparam logic MASK = 1'bx;
                        if (i == 0) begin
                            assign y[i] = (MASK === 1'bx);
                        end else begin
                            Child child(.a(MASK === 1'bx), .y(y[i]));
                        end
                    end
                    if (1) begin : first
                        logic tmp;
                        assign tmp = 1'b0;
                        assign y[2] = tmp;
                    end
                    if (1) begin : second
                        logic tmp;
                        assign tmp = 1'b1;
                        assign y[3] = tmp;
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("generate_constants.sv"))], "Top").four_state(true);
        sim.modify(|_| {}).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 11u8.into());
    }

    fn connects_generated_outputs_to_ascending_packed_slices(sim) {
        @setup {
            let sv = r#"
                module Child #(parameter VALUE=0)(output logic [1:0] y); assign y = VALUE; endmodule
                module Top(output logic [0:7] y);
                    wire [0:7] bus;
                    for (genvar i=0; i<4; i++) begin : lanes
                        Child #(.VALUE(i)) child(.y(bus[2*i:2*i+1]));
                    end
                    assign y = bus;
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("generate_slices.sv"))], "Top");
        sim.modify(|_| {}).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 0x1bu8.into());
    }

    fn selects_case_generate_with_four_state_parameter_and_local_labels(sim) {
        @setup {
            let sv = r#"
                module Top #(parameter logic [3:0] SELECT=4'bxz01)(output logic y);
                    if (1) begin : scope
                        localparam logic [3:0] LABEL=4'bxz01;
                        case (SELECT)
                            LABEL: assign y = 1;
                            default: initial $fatal;
                        endcase
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("generate_case_masks.sv"))], "Top").four_state(true);
        sim.modify(|_| {}).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 1u8.into());
    }

    fn uses_one_width_and_sign_context_for_generate_case(sim) {
        @setup {
            // Verilator also selects the default branch: the unsigned label
            // prevents sign extension of the four-bit selector to eight bits.
            let sv = r#"
                module Top(output logic y);
                    case (4'shf)
                        8'shff: assign y = 1;
                        8'h00: assign y = 1;
                        default: assign y = 0;
                    endcase
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("generate_case_context.sv"))], "Top");
        sim.modify(|_| {}).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 0u8.into());
    }
}

#[test]
fn generate_function_call_preserves_definition_site_bindings() {
    let source = r#"
        module Top(input logic clk, input logic [1:0] a, output logic [3:0] y);
            function automatic logic f(input logic x); return a[0] ^ x; endfunction
            if (1) begin : g
                logic [1:0] a;
                logic x;
                assign a = 2'b10;
                assign x = 0;
                function automatic logic local_value(); return a[1]; endfunction
                always_comb y[0] = f(x);
                assign y[1] = local_value();
                always_comb y[2] = local_value();
                always_ff @(posedge clk) y[3] <= f(x);
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("generate_function_binding.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let a = sim.signal("a");
    let clk = sim.event("clk");
    for value in 0u8..4 {
        sim.modify(|io| io.set(a, value)).unwrap();
        sim.tick(clk).unwrap();
        let expected = if value & 1 != 0 { 15u8 } else { 6u8 };
        assert_eq!(sim.get(sim.signal("y")), expected.into());
    }
}

#[test]
fn rejects_unqualified_generate_function_calls_outside_their_scope() {
    for call in [
        "assign y = f(a);",
        "if (1) begin : sibling assign y = f(a); end",
    ] {
        let source = format!(
            r#"
            module Top(input logic a, output logic y);
                if (1) begin : g
                    function automatic logic f(input logic x); return x; endfunction
                end
                {call}
            endmodule
        "#
        );
        assert!(
            Simulator::from_sv_sources(
                vec![(&source, Path::new("generate_function_visibility.sv"))],
                "Top"
            )
            .build_cranelift()
            .is_err(),
            "accepted out-of-scope call: {call}"
        );
    }
}
