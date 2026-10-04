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

    fn indexed_initializers_accept_casts_around_selections(sim) {
        @setup {
            let source = r#"
                module Top(input logic [15:0] data, output int integer_value, nested_value, function_value,
                    output logic [3:0] alias_value, output logic [7:0] arithmetic,
                    output logic [15:0] cast_width);
                    typedef logic [3:0] nibble;
                    localparam logic [7:0] P = 8'hab;
                    localparam int Q = int'(P[0 +: 4]);
                    localparam nibble R = nibble'(P[4 +: 4]);
                    localparam int S = int'(nibble'(P[4 +: 4]));
                    localparam logic [7:0] T = 8'(P[0 +: 4]) + 8'd5;
                    localparam int F = $clog2(int'(P[0 +: 4]));
                    assign integer_value = Q;
                    assign alias_value = R;
                    assign nested_value = S;
                    assign arithmetic = T;
                    assign function_value = F;
                    assign cast_width = data[0 +: int'(P[4 +: 4])];
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("indexed_initializer_casts.sv"))], "Top");
        let data = sim.signal("data");
        sim.modify(|io| io.set(data, 0x1234u16)).unwrap();
        assert_eq!(sim.get(sim.signal("integer_value")), 11u8.into());
        assert_eq!(sim.get(sim.signal("alias_value")), 10u8.into());
        assert_eq!(sim.get(sim.signal("nested_value")), 10u8.into());
        assert_eq!(sim.get(sim.signal("arithmetic")), 16u8.into());
        assert_eq!(sim.get(sim.signal("function_value")), 4u8.into());
        assert_eq!(sim.get(sim.signal("cast_width")), 0x234u16.into());
    }

    fn indexed_size_queries_preserve_parameter_dimensions(sim) {
        @setup {
            let source = r#"
                module Top #(parameter logic [1:0][3:0] P = 8'hab,
                    parameter logic [2:1][7:4] OFFSET = 8'hab,
                    parameter logic [1:2][4:7] UP = 8'hab)(input logic [15:0] data,
                    output logic [3:0] size_base, bits_base, offset_base, up_base, outer_base, whole_base,
                    output logic [7:0] size_width, bits_width);
                    assign size_base = data[$size(P[0]) +: 4];
                    assign bits_base = data[$bits(P[0]) +: 4];
                    assign offset_base = data[$size(OFFSET[1]) +: 4];
                    assign up_base = data[$bits(UP[2]) +: 4];
                    assign outer_base = data[$size(P) +: 4];
                    assign whole_base = data[$bits(P) +: 4];
                    assign size_width = data[0 +: $size(P[0])];
                    assign bits_width = data[0 +: $bits(OFFSET[1])];
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("indexed_parameter_size_queries.sv"))], "Top");
        let data = sim.signal("data");
        sim.modify(|io| io.set(data, 0x1234u16)).unwrap();
        for name in ["size_base", "bits_base", "offset_base", "up_base"] {
            assert_eq!(sim.get(sim.signal(name)), 3u8.into());
        }
        assert_eq!(sim.get(sim.signal("outer_base")), 13u8.into());
        assert_eq!(sim.get(sim.signal("whole_base")), 2u8.into());
        assert_eq!(sim.get(sim.signal("size_width")), 4u8.into());
        assert_eq!(sim.get(sim.signal("bits_width")), 4u8.into());
    }

    fn indexed_widths_accept_replication_concatenations(sim) {
        @setup {
            let source = r#"
                module Top(input logic [15:0] data, output logic [15:0] simple,
                    parts, nested, selected_count);
                    localparam logic [3:0] COUNT = 4'b1010;
                    assign simple = data[0 +: {2{1'b1}}];
                    assign parts = data[0 +: {2{1'b0, 1'b1}}];
                    assign nested = data[0 +: {2{{2{1'b1}}}}];
                    assign selected_count = data[0 +: {COUNT[1:0]{1'b1}}];
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("indexed_replication_widths.sv"))], "Top");
        let data = sim.signal("data");
        sim.modify(|io| io.set(data, 0x123fu16)).unwrap();
        assert_eq!(sim.get(sim.signal("simple")), 7u8.into());
        assert_eq!(sim.get(sim.signal("parts")), 31u8.into());
        assert_eq!(sim.get(sim.signal("nested")), 0x123fu16.into());
        assert_eq!(sim.get(sim.signal("selected_count")), 7u8.into());
    }

    fn indexed_constants_work_in_ordinary_selection_indices(sim) {
        @setup {
            let source = r#"
                module Top(input logic [7:0] data, input logic replacement,
                    output logic direct_bit, compound_bit, concat_bit, repeated_bit,
                    output logic [1:0] range_value, repeated_value,
                    output logic [7:0] net_written, procedural_written);
                    localparam logic [7:4] P = 4'b1010;
                    assign direct_bit = data[P[4 +: 2]];
                    assign compound_bit = data[P[4 +: 2] + 1];
                    assign concat_bit = {4'b0, data}[P[4 +: 2]];
                    assign repeated_bit = {2{data}}[P[4 +: 2]];
                    assign range_value = data[P[4 +: 2] + 1:P[4 +: 2]];
                    assign repeated_value = {P[4 +: 2]{1'b1}};
                    assign net_written[1:0] = data[1:0];
                    assign net_written[P[4 +: 2]] = replacement;
                    assign net_written[7:3] = data[7:3];
                    always_comb begin
                        procedural_written = data;
                        procedural_written[P[4 +: 2]] = replacement;
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("indexed_ordinary_indices.sv"))], "Top");
        let data = sim.signal("data");
        let replacement = sim.signal("replacement");
        sim.modify(|io| { io.set(data, 0xa4u8); io.set(replacement, false); }).unwrap();
        for name in ["direct_bit", "concat_bit", "repeated_bit"] {
            assert_eq!(sim.get(sim.signal(name)), 1u8.into());
        }
        assert_eq!(sim.get(sim.signal("compound_bit")), 0u8.into());
        assert_eq!(sim.get(sim.signal("range_value")), 1u8.into());
        assert_eq!(sim.get(sim.signal("repeated_value")), 3u8.into());
        for name in ["net_written", "procedural_written"] {
            assert_eq!(sim.get(sim.signal(name)), 0xa0u8.into());
        }
    }

    fn indexed_runtime_replications_preserve_attached_selections(sim) {
        @setup {
            let source = r#"
                module Top(input logic [7:0] data, output logic [15:0] plus, minus,
                    output logic [7:0] ordinary, output logic bit_value);
                    assign plus = {2{data}}[4 +: 12];
                    assign minus = {2{data}}[15 -: 12];
                    assign ordinary = {2{data}}[11:4];
                    assign bit_value = {2{data}}[8];
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("indexed_runtime_replication.sv"))], "Top");
        let data = sim.signal("data");
        sim.modify(|io| io.set(data, 0xabu8)).unwrap();
        assert_eq!(sim.get(sim.signal("plus")), 0xabau16.into());
        assert_eq!(sim.get(sim.signal("minus")), 0xabau16.into());
        assert_eq!(sim.get(sim.signal("ordinary")), 0xbau8.into());
        assert_eq!(sim.get(sim.signal("bit_value")), 1u8.into());
    }

    fn indexed_generate_parameters_retain_local_metadata(sim) {
        @setup {
            let source = r#"
                module Top #(parameter logic [3:0] BASE = 4'd2)(input logic [15:0] data,
                    output logic [3:0] down, up, masked, derived, outer,
                    output logic [7:0] loop_value);
                    if (1) begin : selected
                        localparam logic [7:4] BASE = 4'b0001;
                        localparam logic [4:7] UP = 4'b0001;
                        localparam logic [7:4] MASK = 4'bx001;
                        localparam logic [3:0] Q = MASK[4 +: 3];
                        assign down = data[BASE[4] +: 4];
                        assign up = data[UP[7] +: 4];
                        assign masked = data[MASK[4] +: 4];
                        assign derived = data[Q +: 4];
                    end
                    for (genvar i = 0; i < 2; i++) begin : lanes
                        localparam logic [7:4] LOCAL_BASE = i + 1;
                        assign loop_value[i*4 +: 4] = data[LOCAL_BASE[4 +: 4] +: 4];
                    end
                    assign outer = data[BASE +: 4];
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("indexed_generate_metadata.sv"))], "Top");
        let data = sim.signal("data");
        sim.modify(|io| io.set(data, 0x1234u16)).unwrap();
        for name in ["down", "up", "masked", "derived"] {
            assert_eq!(sim.get(sim.signal(name)), 10u8.into());
        }
        assert_eq!(sim.get(sim.signal("outer")), 13u8.into());
        assert_eq!(sim.get(sim.signal("loop_value")), 0xdau8.into());
    }

    fn indexed_constants_lower_selected_concatenations(sim) {
        @setup {
            let source = r#"
                module Top(input logic [15:0] data, output logic [15:0] range_width,
                    indexed_width, repeated_width, repeated_indexed_width, bit_width,
                    output logic [3:0] parameter_value);
                    localparam logic [3:0] Q = {4'b0011, 4'b0000}[4 +: 4];
                    assign range_width = data[0 +: {4'b0011, 4'b0000}[7:4]];
                    assign indexed_width = data[0 +: {4'b0011, 4'b0000}[4 +: 4]];
                    assign repeated_width = data[0 +: {2{4'b0011}}[3:0]];
                    assign repeated_indexed_width = data[0 +: {2{4'b0011}}[7 -: 4]];
                    assign bit_width = data[0 +: {1'b0, 1'b1}[0]];
                    assign parameter_value = Q;
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("indexed_selected_concatenations.sv"))], "Top");
        let data = sim.signal("data");
        sim.modify(|io| io.set(data, 0x123fu16)).unwrap();
        for name in ["range_width", "indexed_width", "repeated_width", "repeated_indexed_width"] {
            assert_eq!(sim.get(sim.signal(name)), 7u8.into());
        }
        assert_eq!(sim.get(sim.signal("bit_width")), 1u8.into());
        assert_eq!(sim.get(sim.signal("parameter_value")), 3u8.into());
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

    fn runtime_indexed_reads_follow_the_declared_direction(sim) {
        @setup {
            let source = r#"
                module Top(input logic [31:0] down, input logic [0:31] up, input logic [4:0] i,
                           output logic bit_down, bit_up,
                           output logic [7:0] down_plus, down_minus, up_plus, up_minus);
                    assign bit_down = down[i];
                    assign bit_up = up[i];
                    assign down_plus = down[i +: 8];
                    assign down_minus = down[i -: 8];
                    assign up_plus = up[i +: 8];
                    assign up_minus = up[i -: 8];
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("runtime_indexed_reads.sv"))], "Top");
        let (down, up, i) = (sim.signal("down"), sim.signal("up"), sim.signal("i"));
        let value = 0xaabb_ccddu32;
        sim.modify(|io| { io.set(down, value); io.set(up, value); }).unwrap();
        let bit = |position: u32| u8::from(value >> position & 1 != 0);
        let byte = |low: u32| ((value >> low) & 0xff) as u8;
        // An ascending range numbers its bits from the most-significant end.
        for index in 7..=24u32 {
            sim.modify(|io| io.set(i, index as u8)).unwrap();
            assert_eq!(sim.get(sim.signal("bit_down")), bit(index).into(), "down[{index}]");
            assert_eq!(sim.get(sim.signal("bit_up")), bit(31 - index).into(), "up[{index}]");
            assert_eq!(sim.get(sim.signal("down_plus")), byte(index).into(), "down[{index} +: 8]");
            assert_eq!(sim.get(sim.signal("down_minus")), byte(index - 7).into(), "down[{index} -: 8]");
            assert_eq!(sim.get(sim.signal("up_plus")), byte(24 - index).into(), "up[{index} +: 8]");
            assert_eq!(sim.get(sim.signal("up_minus")), byte(31 - index).into(), "up[{index} -: 8]");
        }
        for (index, expected) in [(0u32, bit(0)), (31, bit(31))] {
            sim.modify(|io| io.set(i, index as u8)).unwrap();
            assert_eq!(sim.get(sim.signal("bit_down")), expected.into(), "down[{index}]");
        }
    }

    fn runtime_indexed_base_may_select_a_parameter_bit(sim) {
        @setup {
            let source = r#"
                module Top #(parameter logic [3:0] BASE = 4'b1010)(
                    input logic [15:0] data, input logic index, output logic [3:0] y);
                    assign y = data[BASE[index] +: 4];
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("selected_runtime_base.sv"))], "Top");
        let (data, index) = (sim.signal("data"), sim.signal("index"));
        sim.modify(|io| { io.set(data, 0xabcdu16); io.set(index, 0u8); }).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 0xdu8.into());
        sim.modify(|io| io.set(index, 1u8)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 0x6u8.into());
    }

    fn runtime_indexed_writes_keep_unselected_bits_and_clip_overhang(sim) {
        @setup {
            let source = r#"
                module Top(input logic [2:0] i, input logic replace, input logic [7:0] base,
                           output logic [7:0] bit_write, plus_write, minus_write, filled);
                    always_comb begin
                        bit_write = base;
                        bit_write[i] = 1'b0;
                        plus_write = base;
                        plus_write[i +: 2] = 2'b10;
                        minus_write = base;
                        minus_write[i -: 2] = 2'b10;
                        filled = '0;
                        filled[i +: 2] = 2'b11;
                        if (replace) filled = '1;
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("runtime_indexed_writes.sv"))], "Top");
        let (i, replace, base) = (sim.signal("i"), sim.signal("replace"), sim.signal("base"));
        // Write `value` into the bits `low..low + width`; bits outside 0..8 are dropped.
        let write = |start: u8, low: i32, width: i32, value: u8| {
            (0..width).fold(start, |bits, offset| {
                let position = low + offset;
                if !(0..8).contains(&position) {
                    return bits;
                }
                let mask = 1u8 << position;
                if value >> offset & 1 != 0 { bits | mask } else { bits & !mask }
            })
        };
        let initial = 0xa5u8;
        sim.modify(|io| { io.set(base, initial); io.set(replace, 0u8); }).unwrap();
        for index in 0..8i32 {
            sim.modify(|io| io.set(i, index as u8)).unwrap();
            assert_eq!(sim.get(sim.signal("bit_write")), write(initial, index, 1, 0).into(), "bit_write i={index}");
            assert_eq!(sim.get(sim.signal("plus_write")), write(initial, index, 2, 0b10).into(), "plus_write i={index}");
            assert_eq!(sim.get(sim.signal("minus_write")), write(initial, index - 1, 2, 0b10).into(), "minus_write i={index}");
            // A conditional write after a runtime-positioned one is merged by the
            // analyzer's value tracking, which only follows selections that lie
            // fully inside the vector.
            if index <= 6 {
                assert_eq!(sim.get(sim.signal("filled")), write(0, index, 2, 0b11).into(), "filled i={index}");
            }
        }
        sim.modify(|io| io.set(replace, 1u8)).unwrap();
        assert_eq!(sim.get(sim.signal("filled")), 0xffu8.into());
    }

    fn runtime_indexed_ff_write_updates_only_the_selected_slice(sim) {
        @setup {
            let source = r#"
                module Top(input logic clk, input logic [1:0] i, input logic [7:0] v,
                           output logic [31:0] q);
                    always_ff @(posedge clk) q[i*8 +: 8] <= v;
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("runtime_indexed_ff.sv"))], "Top");
        let (i, v) = (sim.signal("i"), sim.signal("v"));
        for (index, value) in [(0u8, 0x11u8), (2, 0x22), (3, 0x33), (1, 0x44)] {
            sim.modify(|io| { io.set(i, index); io.set(v, value); }).unwrap();
            sim.tick(sim.event("clk")).unwrap();
        }
        assert_eq!(sim.get(sim.signal("q")), 0x3322_4411u32.into());
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
    for width in ["0", "-1", "width", "{2{1'b0}}", "{0{1'b1}}"] {
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
