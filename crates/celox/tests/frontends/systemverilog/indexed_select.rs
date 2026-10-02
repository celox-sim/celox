use super::*;

// IEEE 1800-2023 11.5.1: the index operator and declaration direction
// independently determine the endpoints of the selected range.
sv_backends! {
    fn indexed_select_reads_and_writes_both_declaration_directions(sim) {
        @setup {
            let source = r#"
                module Top(input logic [15:0] data, input logic [3:0] replacement,
                           output logic [3:0] down_plus, down_minus, up_plus, up_minus,
                           output logic [15:0] down_written, output logic [0:15] up_written);
                    logic [0:15] up;
                    assign up = data;
                    assign down_plus = data[4 +: 4];
                    assign down_minus = data[7 -: 4];
                    assign up_plus = up[4 +: 4];
                    assign up_minus = up[7 -: 4];
                    always_comb begin
                        down_written = data;
                        down_written[4 +: 4] = replacement;
                        down_written[15 -: 4] = replacement;
                        up_written = data;
                        up_written[4 +: 4] = replacement;
                        up_written[15 -: 4] = replacement;
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("indexed_directions.sv"))], "Top");
        let data = sim.signal("data");
        let replacement = sim.signal("replacement");
        sim.modify(|io| { io.set(data, 0x1234u16); io.set(replacement, 0xau8); }).unwrap();
        assert_eq!(sim.get(sim.signal("down_plus")), 3u8.into());
        assert_eq!(sim.get(sim.signal("down_minus")), 3u8.into());
        assert_eq!(sim.get(sim.signal("up_plus")), 2u8.into());
        assert_eq!(sim.get(sim.signal("up_minus")), 2u8.into());
        assert_eq!(sim.get(sim.signal("down_written")), 0xa2a4u16.into());
        assert_eq!(sim.get(sim.signal("up_written")), 0x1a3au16.into());
    }

    fn indexed_select_parameterized_generate_and_ff(sim) {
        @setup {
            let source = r#"
                module Top #(parameter int WIDTH = 4)(input logic clk,
                    input logic [4*WIDTH-1:0] data, output logic [4*WIDTH-1:0] comb, q,
                    output logic [3:0] selected_size);
                    for (genvar i = 0; i < 4; i++) begin : lanes
                        assign comb[i*WIDTH +: WIDTH] = data[(3-i)*WIDTH +: WIDTH];
                        always_ff @(posedge clk) q[i*WIDTH +: WIDTH] <= data[i*WIDTH +: WIDTH];
                    end
                    localparam SELECTED_SIZE = $size(data[WIDTH +: WIDTH]);
                    assign selected_size = SELECTED_SIZE;
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("indexed_generate.sv"))], "Top");
        let data = sim.signal("data");
        sim.modify(|io| io.set(data, 0x1234u16)).unwrap();
        assert_eq!(sim.get(sim.signal("comb")), 0x4321u16.into());
        assert_eq!(sim.get(sim.signal("selected_size")), 4u8.into());
        sim.tick(sim.event("clk")).unwrap();
        assert_eq!(sim.get(sim.signal("q")), 0x1234u16.into());
        sim.modify(|io| io.set(data, 0xabcd_u16)).unwrap();
        assert_eq!(sim.get(sim.signal("q")), 0x1234u16.into());
        sim.tick(sim.event("clk")).unwrap();
        assert_eq!(sim.get(sim.signal("q")), 0xabcdu16.into());
    }

    fn veryl_emitted_parameterized_step_generate(sim) {
        @setup {
            let veryl = r#"
                module Child #(param WIDTH: u32 = 4) (
                    data: input logic<WIDTH * 4>, y: output logic<WIDTH * 4>,
                ) {
                    for i in 0..4: lanes {
                        assign y[i step WIDTH] = data[(3-i) step WIDTH];
                    }
                }
                module Top (data: input logic<16>, wide: output logic<16>, narrow: output logic<8>) {
                    inst wide_child: Child #(WIDTH: 4) (data, y: wide);
                    inst narrow_child: Child #(WIDTH: 2) (data: data[7:0], y: narrow);
                }
            "#;
            let emitted = celox_test_suite_veryl::emit::emit_veryl_sources(&[(veryl, Path::new("step_generate.veryl"))]);
            assert!(emitted.as_sv_sources().iter().any(|(source, _)| source.contains("+:")));
        }
        @build Simulator::from_sv_sources(emitted.as_sv_sources(), "Top");
        let data = sim.signal("data");
        sim.modify(|io| io.set(data, 0x12e4u16)).unwrap();
        assert_eq!(sim.get(sim.signal("wide")), 0x4e21u16.into());
        assert_eq!(sim.get(sim.signal("narrow")), 0x1bu8.into());
    }

    fn indexed_select_preserves_selected_and_cast_bases(sim) {
        @setup {
            let source = r#"
                module Top #(parameter logic [3:0] BASE = 4'b1010)(
                    input logic [15:0] data, input logic [3:0] replacement,
                    output logic [3:0] bit_base, range_base, cast_base, minus_base,
                    output logic [15:0] continuous_written, procedural_written);
                    typedef logic [3:0] nibble;
                    assign bit_base = data[BASE[0] +: 4];
                    assign range_base = data[BASE[1:0] +: 4];
                    assign cast_base = data[(nibble'(20) + 0) +: 4];
                    assign minus_base = data[(BASE[0] + 3) -: 4];
                    assign continuous_written[nibble'(4) +: 4] = replacement;
                    assign continuous_written[15:8] = data[15:8];
                    assign continuous_written[3:0] = data[3:0];
                    always_comb begin
                        procedural_written = data;
                        procedural_written[BASE[0] +: 4] = replacement;
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("indexed_base_expression.sv"))], "Top");
        let data = sim.signal("data");
        let replacement = sim.signal("replacement");
        for value in [0x00ffu16, 0x1234, 0xabcd] {
            sim.modify(|io| { io.set(data, value); io.set(replacement, 9u8); }).unwrap();
            assert_eq!(sim.get(sim.signal("bit_base")), (value & 15).into());
            assert_eq!(sim.get(sim.signal("range_base")), ((value >> 2) & 15).into());
            assert_eq!(sim.get(sim.signal("cast_base")), ((value >> 4) & 15).into());
            assert_eq!(sim.get(sim.signal("minus_base")), (value & 15).into());
            assert_eq!(sim.get(sim.signal("continuous_written")), ((value & 0xff0f) | 0x90).into());
            assert_eq!(sim.get(sim.signal("procedural_written")), ((value & 0xfff0) | 9).into());
        }
    }

    fn indexed_widths_preserve_parameter_selections(sim) {
        @setup {
            let source = r#"
                module Top(input logic [15:0] data, output logic [15:0] plus, minus, compound, cast_width, written);
                    localparam logic [3:0] W = 4'b1010;
                    typedef logic [3:0] nibble;
                    assign plus = data[0 +: W[1:0]];
                    assign minus = data[3 -: W[1:0]];
                    assign compound = data[0 +: (W[1:0] + 1)];
                    assign cast_width = data[0 +: nibble'(W[1:0])];
                    always_comb begin
                        written = data;
                        written[4 +: W[1:0]] = '1;
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("selected_widths.sv"))], "Top");
        let data = sim.signal("data");
        for value in [0x1234u16, 0xabcd, 0xffff] {
            sim.modify(|io| io.set(data, value)).unwrap();
            assert_eq!(sim.get(sim.signal("plus")), (value & 3).into());
            assert_eq!(sim.get(sim.signal("minus")), ((value >> 2) & 3).into());
            assert_eq!(sim.get(sim.signal("compound")), (value & 7).into());
            assert_eq!(sim.get(sim.signal("cast_width")), (value & 3).into());
            assert_eq!(sim.get(sim.signal("written")), (value | 0x30).into());
        }
    }

    fn indexed_compound_bases_fold_selected_operands(sim) {
        @setup {
            let source = r#"
                module Top(input logic [15:0] data, output logic [3:0] selected, concatenated, nested,
                    output logic [15:0] written);
                    localparam logic [3:0] BASE = 4'b1010;
                    localparam logic [3:0] IDX = 4'b0010;
                    assign nested = data[(BASE[IDX[0] +: 2] + 1) +: 4];
                    assign selected = data[(BASE[1:0] + 1) +: 4];
                    assign concatenated = data[({1'b0, BASE[1:0]} + 1) +: 4];
                    assign written[(BASE[1:0] + 1) +: 4] = 4'hf;
                    assign written[2:0] = data[2:0];
                    assign written[15:7] = data[15:7];
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("compound_bases.sv"))], "Top");
        let data = sim.signal("data");
        sim.modify(|io| io.set(data, 0x1234u16)).unwrap();
        assert_eq!(sim.get(sim.signal("selected")), 6u8.into());
        assert_eq!(sim.get(sim.signal("concatenated")), 6u8.into());
        assert_eq!(sim.get(sim.signal("nested")), 6u8.into());
        assert_eq!(sim.get(sim.signal("written")), 0x127cu16.into());
    }

    fn indexed_constants_respect_declared_parameter_indices(sim) {
        @setup {
            let source = r#"
                module Top(input logic [15:0] data, output logic [3:0] down, up,
                    output logic [15:0] selected_width, written);
                    localparam logic [7:4] BASE = 4'b0001;
                    localparam logic [4:7] UP = 4'b0001;
                    localparam logic [7:4] W = 4'b1010;
                    assign down = data[BASE[4] +: 4];
                    assign up = data[UP[7] +: 4];
                    assign selected_width = data[BASE[4] +: W[5:4]];
                    assign written[BASE[4] +: 4] = 4'hf;
                    assign written[0] = data[0];
                    assign written[15:5] = data[15:5];
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("parameter_indices.sv"))], "Top");
        let data = sim.signal("data");
        sim.modify(|io| io.set(data, 0x1234u16)).unwrap();
        assert_eq!(sim.get(sim.signal("down")), 10u8.into());
        assert_eq!(sim.get(sim.signal("up")), 10u8.into());
        assert_eq!(sim.get(sim.signal("selected_width")), 2u8.into());
        assert_eq!(sim.get(sim.signal("written")), 0x123eu16.into());
    }

    fn indexed_functions_preserve_selected_arguments(sim) {
        @setup {
            let source = r#"
                module Top(input logic [15:0] data, output logic [15:0] width,
                    nested_width, offset_width, output logic [3:0] base);
                    localparam logic [3:0] W = 4'b1011;
                    localparam logic [7:4] OFFSET = 4'b1011;
                    assign width = data[0 +: $clog2(W[1:0])];
                    assign nested_width = data[0 +: $clog2($countones({W[1:0], 2'b11}))];
                    assign offset_width = data[0 +: $clog2(OFFSET[5:4])];
                    assign base = data[$clog2(W[1:0]) +: 4];
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("indexed_functions.sv"))], "Top");
        let data = sim.signal("data");
        sim.modify(|io| io.set(data, 0x123fu16)).unwrap();
        assert_eq!(sim.get(sim.signal("width")), 3u8.into());
        assert_eq!(sim.get(sim.signal("nested_width")), 3u8.into());
        assert_eq!(sim.get(sim.signal("offset_width")), 3u8.into());
        assert_eq!(sim.get(sim.signal("base")), 15u8.into());
    }

    // IEEE 1800-2023 11.6.1: both ternary arms determine the result width.
    fn indexed_ternaries_preserve_both_arm_types(sim) {
        @setup {
            let source = r#"
                module Top(input logic [15:0] data, output logic [3:0] true_base,
                    false_base, output logic [15:0] width);
                    localparam logic [3:0] W = 4'b1011;
                    assign true_base = data[((1 ? W[1:0] : 8'b0) + 2'd1) +: 4];
                    assign false_base = data[((0 ? 8'b0 : W[1:0]) + 2'd1) +: 4];
                    assign width = data[0 +: ((1 ? W[1:0] : 8'b0) + 2'd1)];
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("indexed_ternaries.sv"))], "Top");
        let data = sim.signal("data");
        sim.modify(|io| io.set(data, 0x1234u16)).unwrap();
        assert_eq!(sim.get(sim.signal("true_base")), 3u8.into());
        assert_eq!(sim.get(sim.signal("false_base")), 3u8.into());
        assert_eq!(sim.get(sim.signal("width")), 4u8.into());
    }

    fn indexed_parameter_initializers_preserve_selections(sim) {
        @setup {
            let source = r#"
                module Top(output logic [3:0] plus, minus, compound, ascending, function_width,
                    default_q, specialized_q);
                    localparam logic [7:0] P = 8'hab;
                    localparam logic [4:11] UP = 8'hab;
                    localparam logic [3:0] Q = P[4 +: 4];
                    localparam logic [3:0] R = P[7 -: 4];
                    localparam logic [3:0] S = P[4 +: 4] + 4'd1;
                    localparam logic [3:0] T = UP[4 +: 4];
                    localparam logic [3:0] F = $clog2(P[4 +: 4]);
                    assign plus = Q;
                    assign minus = R;
                    assign compound = S;
                    assign ascending = T;
                    assign function_width = F;
                    Child default_child(.y(default_q));
                    Child #(.P(8'hcd)) specialized_child(.y(specialized_q));
                endmodule
                module Child #(parameter logic [7:0] P = 8'hab,
                    parameter logic [3:0] Q = P[4 +: 4])(output logic [3:0] y);
                    assign y = Q;
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("indexed_parameters.sv"))], "Top");
        assert_eq!(sim.get(sim.signal("plus")), 10u8.into());
        assert_eq!(sim.get(sim.signal("minus")), 10u8.into());
        assert_eq!(sim.get(sim.signal("compound")), 11u8.into());
        assert_eq!(sim.get(sim.signal("ascending")), 10u8.into());
        assert_eq!(sim.get(sim.signal("function_width")), 4u8.into());
        assert_eq!(sim.get(sim.signal("default_q")), 10u8.into());
        assert_eq!(sim.get(sim.signal("specialized_q")), 12u8.into());
    }

    fn indexed_parameter_initializers_use_assignment_width(sim) {
        @setup {
            let source = r#"
                module Top(output logic [7:0] sum, shifted, nested, mux,
                    output logic [3:0] narrow, implicit_value);
                    localparam logic [7:0] P = 8'hff;
                    localparam logic [7:0] Q = P[0 +: 4] + 4'd1;
                    localparam logic [7:0] R = P[0 +: 4] << 1;
                    localparam logic [7:0] S = (P[0 +: 4] + 4'd1) + 4'd1;
                    localparam logic [7:0] T = 1 ? P[0 +: 4] + 4'd1 : 4'd0;
                    localparam logic [3:0] N = P[0 +: 4] + 4'd1;
                    localparam I = P[0 +: 4] + 4'd1;
                    assign sum = Q;
                    assign shifted = R;
                    assign nested = S;
                    assign mux = T;
                    assign narrow = N;
                    assign implicit_value = I;
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("indexed_assignment_context.sv"))], "Top");
        assert_eq!(sim.get(sim.signal("sum")), 16u8.into());
        assert_eq!(sim.get(sim.signal("shifted")), 30u8.into());
        assert_eq!(sim.get(sim.signal("nested")), 17u8.into());
        assert_eq!(sim.get(sim.signal("mux")), 16u8.into());
        assert_eq!(sim.get(sim.signal("narrow")), 0u8.into());
        assert_eq!(sim.get(sim.signal("implicit_value")), 0u8.into());
    }

    fn indexed_parameter_initializers_resolve_enum_constants(sim) {
        @setup {
            let source = r#"
                module Top(output logic [3:0] base, width, arithmetic, enum_dependent);
                    localparam logic [7:0] P = 8'hab;
                    typedef enum logic [2:0] { E = 3'd4 } idx_t;
                    localparam logic [3:0] Q = P[E +: 4];
                    localparam logic [3:0] R = P[4 +: E];
                    localparam logic [3:0] S = P[E +: E] + 4'd1;
                    assign base = Q;
                    assign width = R;
                    assign arithmetic = S;
                    localparam logic [11:4] OFFSET_P = 8'hab;
                    localparam logic [3:0] OFFSET_Q = OFFSET_P[(E + 4) +: 4];
                    typedef enum logic [3:0] { F = OFFSET_Q } value_t;
                    assign enum_dependent = F;
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("indexed_enum_context.sv"))], "Top");
        assert_eq!(sim.get(sim.signal("base")), 10u8.into());
        assert_eq!(sim.get(sim.signal("width")), 10u8.into());
        assert_eq!(sim.get(sim.signal("arithmetic")), 11u8.into());
        assert_eq!(sim.get(sim.signal("enum_dependent")), 10u8.into());
    }

    fn indexed_parameter_ports_preserve_declared_ranges(sim) {
        @setup {
            let source = r#"
                module Top(output logic [3:0] down, up, specialized, header);
                    Child default_child(.down(down), .up(up), .header(header));
                    Child #(.P(8'hcd)) specialized_child(.down(specialized), .up(), .header());
                endmodule
                module Child #(parameter logic [11:4] P = 8'hab,
                    logic [4:11] U = 8'hab, parameter logic [3:0] H = P[8 +: 4])(
                    output logic [3:0] down, up, header);
                    localparam logic [3:0] Q = P[8 +: 4];
                    localparam logic [3:0] R = U[4 +: 4];
                    assign down = Q;
                    assign up = R;
                    assign header = H;
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("indexed_port_ranges.sv"))], "Top");
        assert_eq!(sim.get(sim.signal("down")), 10u8.into());
        assert_eq!(sim.get(sim.signal("up")), 10u8.into());
        assert_eq!(sim.get(sim.signal("specialized")), 12u8.into());
        assert_eq!(sim.get(sim.signal("header")), 10u8.into());
    }

    fn indexed_bases_resolve_size_queries(sim) {
        @setup {
            let source = r#"
                module Top(input logic [15:0] data, output logic [3:0] bits_base,
                    size_base, selected_base, type_base);
                    typedef logic [7:0] octet;
                    assign bits_base = data[$bits(data)/2 +: 4];
                    assign size_base = data[$size(data)/2 +: 4];
                    assign selected_base = data[$bits(data[7:0]) +: 4];
                    assign type_base = data[$bits(octet) +: 4];
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("indexed_size_queries.sv"))], "Top");
        let data = sim.signal("data");
        sim.modify(|io| io.set(data, 0xabcdu16)).unwrap();
        for name in ["bits_base", "size_base", "selected_base", "type_base"] {
            assert_eq!(sim.get(sim.signal(name)), 11u8.into());
        }
    }

    fn indexed_initializers_preserve_four_state_parameters(sim) {
        @setup {
            let source = r#"
                module Top(output logic [3:0] low, high, port_value,
                    output logic [7:0] sum);
                    localparam logic [7:0] P = 8'hxa;
                    localparam logic [11:4] Z = 8'haz;
                    localparam logic [3:0] Q = P[0 +: 4];
                    localparam logic [3:0] R = Z[8 +: 4];
                    localparam logic [7:0] S = P[0 +: 4] + 4'd6;
                    assign low = Q;
                    assign high = R;
                    assign sum = S;
                    Child child(.y(port_value));
                endmodule
                module Child #(parameter logic [11:4] P = 8'hxa,
                    parameter logic [3:0] Q = P[4 +: 4])(output logic [3:0] y);
                    assign y = Q;
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("indexed_four_state_parameters.sv"))], "Top");
        assert_eq!(sim.get(sim.signal("low")), 10u8.into());
        assert_eq!(sim.get(sim.signal("high")), 10u8.into());
        assert_eq!(sim.get(sim.signal("port_value")), 10u8.into());
        assert_eq!(sim.get(sim.signal("sum")), 16u8.into());
    }

    fn indexed_unsigned_bases_cross_zero(sim) {
        @setup {
            let source = r#"
                module Top(input logic [3:-4] data, output logic [3:0] minus, up_minus,
                    plus, wrapped_base, output logic [3:-4] written);
                    logic [-4:3] up;
                    assign up = data;
                    assign minus = data[32'd0 -: 4];
                    assign up_minus = up[32'd0 -: 4];
                    assign plus = data[(-3) +: 4];
                    assign wrapped_base = data[(2'd3 + 2'd1) +: 4];
                    always_comb begin
                        written = data;
                        written[32'd0 -: 4] = 4'hf;
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("indexed_unsigned.sv"))], "Top");
        let data = sim.signal("data");
        sim.modify(|io| io.set(data, 0xa5u8)).unwrap();
        assert_eq!(sim.get(sim.signal("minus")), 2u8.into());
        assert_eq!(sim.get(sim.signal("up_minus")), 4u8.into());
        assert_eq!(sim.get(sim.signal("plus")), 2u8.into());
        assert_eq!(sim.get(sim.signal("wrapped_base")), 10u8.into());
        assert_eq!(sim.get(sim.signal("written")), 0xbfu8.into());
    }

    fn indexed_select_multidimensional_packed_reads(sim) {
        @setup {
            let source = r#"
                module Top(input logic [1:0][0:7] data, output logic [3:0] plus, minus,
                           output logic [7:0] concat_slice);
                    assign plus = data[1][2 +: 4];
                    assign minus = data[0][5 -: 4];
                    assign concat_slice = {data[1], data[0]}[4 +: 8];
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("indexed_packed.sv"))], "Top");
        let data = sim.signal("data");
        sim.modify(|io| io.set(data, 0xa35cu16)).unwrap();
        assert_eq!(sim.get(sim.signal("plus")), 8u8.into());
        assert_eq!(sim.get(sim.signal("minus")), 7u8.into());
        assert_eq!(sim.get(sim.signal("concat_slice")), 0x35u8.into());
    }
}

#[test]
fn rejects_nonpositive_and_runtime_indexed_widths() {
    for width in ["0", "-1", "width"] {
        let source = format!(
            "module Top(input logic [7:0] data, input int width, output logic [7:0] y); assign y = data[0 +: {width}]; endmodule"
        );
        let error =
            Simulator::from_sv_sources(vec![(&source, Path::new("invalid_width.sv"))], "Top")
                .build_cranelift()
                .expect_err("invalid indexed width must be rejected")
                .to_string();
        assert!(error.contains("indexed part-select"), "{error}");
    }
}

#[test]
fn rejects_runtime_selected_bases() {
    let source = r#"
        module Top #(parameter logic [3:0] BASE = 4'b1010)(
            input logic [15:0] data, input logic index, output logic [3:0] y);
            assign y = data[BASE[index] +: 4];
        endmodule
    "#;
    let error =
        Simulator::from_sv_sources(vec![(source, Path::new("runtime_selected_base.sv"))], "Top")
            .build_cranelift()
            .expect_err("selected runtime base must be rejected")
            .to_string();
    assert!(error.contains("indexed part-select"), "{error}");
}

#[test]
fn rejects_selected_widths_that_are_nonpositive() {
    for width in ["W[0]", "$clog2(W[0])", "W[1:0] - 2", "int'(W[1:0]) - 3"] {
        let source = format!(
            "module Top(input logic [15:0] data, output logic [15:0] y); localparam logic [3:0] W = 4'b1010; assign y = data[0 +: ({width})]; endmodule"
        );
        let error = Simulator::from_sv_sources(
            vec![(&source, Path::new("invalid_selected_width.sv"))],
            "Top",
        )
        .build_cranelift()
        .expect_err("selected width must be positive")
        .to_string();
        assert!(error.contains("indexed part-select"), "{error}");
    }
}

#[test]
fn rejects_indexed_selections_in_unlowered_constant_contexts() {
    for declaration in [
        "logic [P[4 +: 4]-1:0] y; assign y = '0;",
        "typedef enum logic [3:0] { E = P[4 +: 4] } nibble; nibble y; assign y = E;",
    ] {
        let source =
            format!("module Top; localparam logic [7:0] P = 8'hab; {declaration} endmodule");
        Simulator::from_sv_sources(vec![(&source, Path::new("unlowered_constant.sv"))], "Top")
            .build_cranelift()
            .expect_err("unlowered indexed constants must be rejected");
    }
}

#[test]
fn rejects_unresolved_indexed_parameter_initializers_after_collection() {
    let source = "module Top; localparam logic [7:0] P = 8'hab; localparam logic [3:0] Q = P[MISSING +: 4]; endmodule";
    Simulator::from_sv_sources(
        vec![(source, Path::new("unresolved_indexed_parameter.sv"))],
        "Top",
    )
    .build_cranelift()
    .expect_err("an unused unresolved indexed initializer must be rejected");
}
