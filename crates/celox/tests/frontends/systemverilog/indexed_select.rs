use super::*;

// IEEE 1800-2023 11.5.1: the index operator and declaration direction
// independently determine the endpoints of the selected range.
sv_backends! {
    fn indexed_select_reads_and_writes_both_declaration_directions(sim) {
        @case "indexed_select::indexed_select_reads_and_writes_both_declaration_directions";
    }

    fn indexed_select_parameterized_generate_and_ff(sim) {
        @case "indexed_select::indexed_select_parameterized_generate_and_ff";
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
            let emitted = celox_test_suite::veryl::emit::emit_veryl_sources(&[(veryl, Path::new("step_generate.veryl"))]);
            assert!(emitted.as_sv_sources().iter().any(|(source, _)| source.contains("+:")));
        }
        @build Simulator::from_sv_sources(emitted.as_sv_sources(), "Top");
        let data = sim.signal("data");
        sim.modify(|io| io.set(data, 0x12e4u16)).unwrap();
        assert_eq!(sim.get(sim.signal("wide")), 0x4e21u16.into());
        assert_eq!(sim.get(sim.signal("narrow")), 0x1bu8.into());
    }

    fn indexed_select_preserves_selected_and_cast_bases(sim) {
        @case "indexed_select::indexed_select_preserves_selected_and_cast_bases";
    }

    fn indexed_widths_preserve_parameter_selections(sim) {
        @case "indexed_select::indexed_widths_preserve_parameter_selections";
    }

    fn indexed_compound_bases_fold_selected_operands(sim) {
        @case "indexed_select::indexed_compound_bases_fold_selected_operands";
    }

    fn indexed_constants_respect_declared_parameter_indices(sim) {
        @case "indexed_select::indexed_constants_respect_declared_parameter_indices";
    }

    fn indexed_functions_preserve_selected_arguments(sim) {
        @case "indexed_select::indexed_functions_preserve_selected_arguments";
    }

    // IEEE 1800-2023 11.6.1: both ternary arms determine the result width.
    fn indexed_ternaries_preserve_both_arm_types(sim) {
        @case "indexed_select::indexed_ternaries_preserve_both_arm_types";
    }

    fn indexed_parameter_initializers_preserve_selections(sim) {
        @case "indexed_select::indexed_parameter_initializers_preserve_selections";
    }

    fn indexed_parameter_initializers_use_assignment_width(sim) {
        @case "indexed_select::indexed_parameter_initializers_use_assignment_width";
    }

    fn indexed_parameter_initializers_resolve_enum_constants(sim) {
        @case "indexed_select::indexed_parameter_initializers_resolve_enum_constants";
    }

    fn indexed_parameter_ports_preserve_declared_ranges(sim) {
        @case "indexed_select::indexed_parameter_ports_preserve_declared_ranges";
    }

    fn indexed_bases_resolve_size_queries(sim) {
        @case "indexed_select::indexed_bases_resolve_size_queries";
    }

    fn indexed_initializers_preserve_four_state_parameters(sim) {
        @case "indexed_select::indexed_initializers_preserve_four_state_parameters";
    }

    fn indexed_initializers_accept_casts_around_selections(sim) {
        @case "indexed_select::indexed_initializers_accept_casts_around_selections";
    }

    fn indexed_size_queries_preserve_parameter_dimensions(sim) {
        @case "indexed_select::indexed_size_queries_preserve_parameter_dimensions";
    }

    fn indexed_widths_accept_replication_concatenations(sim) {
        @case "indexed_select::indexed_widths_accept_replication_concatenations";
    }

    fn indexed_constants_work_in_ordinary_selection_indices(sim) {
        @case "indexed_select::indexed_constants_work_in_ordinary_selection_indices";
    }

    fn indexed_runtime_replications_preserve_attached_selections(sim) {
        @case "indexed_select::indexed_runtime_replications_preserve_attached_selections";
    }

    fn indexed_generate_parameters_retain_local_metadata(sim) {
        @case "indexed_select::indexed_generate_parameters_retain_local_metadata";
    }

    fn indexed_constants_lower_selected_concatenations(sim) {
        @case "indexed_select::indexed_constants_lower_selected_concatenations";
    }

    fn indexed_unsigned_bases_cross_zero(sim) {
        @case "indexed_select::indexed_unsigned_bases_cross_zero";
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
        @case "indexed_select::runtime_indexed_base_may_select_a_parameter_bit";
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
        @case "indexed_select::runtime_indexed_ff_write_updates_only_the_selected_slice";
    }

    fn indexed_select_multidimensional_packed_reads(sim) {
        @case "indexed_select::indexed_select_multidimensional_packed_reads";
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
