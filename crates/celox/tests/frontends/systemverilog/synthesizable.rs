//! Constructs commonly found in synthesizable RTL, checked against a software model.

use super::*;

sv_backends! {
    fn enum_members_without_values_follow_their_predecessor(sim) {
        @case "synthesizable::enum_members_without_values_follow_their_predecessor";
    }

    fn always_star_and_edge_sensitive_always_match_the_systemverilog_keywords(sim) {
        @case "synthesizable::always_star_and_edge_sensitive_always_match_the_systemverilog_keywords";
    }

    fn net_declaration_assignment_drives_the_net(sim) {
        @case "synthesizable::net_declaration_assignment_drives_the_net";
    }

    fn assigning_the_function_name_returns_its_last_value(sim) {
        @setup {
            let source = r#"
                module Top(input logic [7:0] a, input logic [1:0] sel,
                           output logic [7:0] clamped, doubled, picked);
                    function automatic logic [7:0] clamp(input logic [7:0] x);
                        clamp = x;
                        if (x > 8'd10) clamp = 8'd10;
                    endfunction
                    function automatic logic [7:0] twice_plus_one(input logic [7:0] x);
                        logic [7:0] t;
                        t = x * 2;
                        twice_plus_one = t + 1;
                    endfunction
                    function automatic logic [7:0] pick(input logic [1:0] s);
                        case (s)
                            0: pick = 8'd10;
                            1: pick = 8'd20;
                            default: pick = 8'd30;
                        endcase
                    endfunction
                    assign clamped = clamp(a);
                    always_comb doubled = twice_plus_one(a);
                    assign picked = pick(sel);
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("function_name_return.sv"))], "Top");
        let (a, sel) = (sim.signal("a"), sim.signal("sel"));
        for (value, select) in [(3u8, 0u8), (50, 1), (100, 2), (10, 3)] {
            sim.modify(|io| { io.set(a, value); io.set(sel, select); }).unwrap();
            assert_eq!(sim.get(sim.signal("clamped")), value.min(10).into());
            assert_eq!(sim.get(sim.signal("doubled")), value.wrapping_mul(2).wrapping_add(1).into());
            let picked = match select { 0 => 10u8, 1 => 20, _ => 30 };
            assert_eq!(sim.get(sim.signal("picked")), picked.into());
        }
    }

    fn casez_and_casex_treat_wildcard_bits_as_dont_care(sim) {
        @case "synthesizable::casez_and_casex_treat_wildcard_bits_as_dont_care";
    }

    fn inside_matches_values_ranges_and_wildcards(sim) {
        @setup {
            let source = r#"
                module Top(input logic [3:0] a, output logic y, output logic [1:0] z);
                    assign y = a inside {4'd0, [4'd9:4'd10], 4'b11??};
                    always_comb begin
                        if (a inside {[4'd0:4'd3]}) z = 1;
                        else if (a inside {4'd8, 4'd9}) z = 2;
                        else z = 0;
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("inside.sv"))], "Top");
        let a = sim.signal("a");
        for value in 0u8..16 {
            sim.modify(|io| io.set(a, value)).unwrap();
            let y = value == 0 || (9..=10).contains(&value) || value >> 2 == 0b11;
            assert_eq!(sim.get(sim.signal("y")), u8::from(y).into(), "a={value}");
            let z = if value <= 3 { 1u8 } else if value == 8 || value == 9 { 2 } else { 0 };
            assert_eq!(sim.get(sim.signal("z")), z.into(), "a={value}");
        }
    }

    fn positional_ports_and_parameters_bind_in_declaration_order(sim) {
        @setup {
            let source = r#"
                module Shift #(parameter W = 4, parameter S = 1)(
                    input logic [W-1:0] a, input logic en, output logic [W-1:0] y, output logic z);
                    assign y = en ? a << S : a;
                    assign z = ^a;
                endmodule
                module Top(input logic [7:0] a, input logic en, output logic [7:0] y,
                           output logic z);
                    Shift #(8, 2) u(a, en, y, z);
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("positional.sv"))], "Top");
        let (a, en) = (sim.signal("a"), sim.signal("en"));
        for (value, enable) in [(3u8, 1u8), (64, 1), (0xa5, 0), (0xff, 1)] {
            sim.modify(|io| { io.set(a, value); io.set(en, enable); }).unwrap();
            let expected = if enable != 0 { value << 2 } else { value };
            assert_eq!(sim.get(sim.signal("y")), expected.into(), "a={value} en={enable}");
            assert_eq!(
                sim.get(sim.signal("z")),
                u8::try_from(value.count_ones() & 1).unwrap().into()
            );
        }
    }

    fn instance_arrays_broadcast_and_slice_connections(sim) {
        @case "synthesizable::instance_arrays_broadcast_and_slice_connections";
    }

    fn instance_arrays_with_ascending_range(sim) {
        @case "synthesizable::instance_arrays_with_ascending_range";
    }

    fn instance_arrays_with_non_zero_based_range(sim) {
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
        let a = sim.signal("a");
        for value in [0x3cu8, 0xa5, 0xff, 0x01] {
            sim.modify(|io| io.set(a, value)).unwrap();
            assert_eq!(sim.get(sim.signal("y")), (!value).into(), "a={value}");
            // The leftmost element, `u[3]`, takes the most significant slice;
            // elements are addressed by their declared index.
            let high = sim.child_signal(&[("u", 3)], "a");
            let low = sim.child_signal(&[("u", 2)], "a");
            assert_eq!(sim.get(high), (value >> 4).into(), "a={value}");
            assert_eq!(sim.get(low), (value & 0xf).into(), "a={value}");
        }
        let hierarchy = sim.named_hierarchy();
        let (_, elements) = hierarchy
            .children
            .iter()
            .find(|(name, _)| name == "u")
            .expect("instance array `u`");
        let indices: Vec<_> = elements.iter().map(|element| element.index).collect();
        assert_eq!(indices, [2, 3]);
    }

    fn instance_array_unpacked_connection_descending_to_descending(sim) {
        @case "synthesizable::instance_array_unpacked_connection_descending_to_descending";
    }

    fn instance_array_unpacked_connection_descending_to_ascending(sim) {
        @case "synthesizable::instance_array_unpacked_connection_descending_to_ascending";
    }

    fn instance_array_unpacked_connection_ascending_to_descending(sim) {
        @case "synthesizable::instance_array_unpacked_connection_ascending_to_descending";
    }

    fn instance_array_unpacked_connection_offset_ranges(sim) {
        @case "synthesizable::instance_array_unpacked_connection_offset_ranges";
    }

    fn typedef_unpacked_arrays_are_not_instance_arrays(sim) {
        @case "synthesizable::typedef_unpacked_arrays_are_not_instance_arrays";
    }

    fn instance_arrays_reject_a_fill_literal_tie_off(sim) {
        @case "synthesizable::instance_arrays_reject_a_fill_literal_tie_off";
    }

    fn instance_arrays_reject_an_unsized_constant_tie_off(sim) {
        @case "synthesizable::instance_arrays_reject_an_unsized_constant_tie_off";
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

    fn exponentiation_works_in_constant_expressions(sim) {
        @case "synthesizable::exponentiation_works_in_constant_expressions";
    }

    fn packages_provide_types_parameters_functions_and_enums(sim) {
        @case "synthesizable::packages_provide_types_parameters_functions_and_enums";
    }

    fn struct_assignment_patterns_follow_the_target_layout(sim) {
        @case "synthesizable::struct_assignment_patterns_follow_the_target_layout";
    }

    fn function_output_and_inout_arguments_are_written_back(sim) {
        @case "synthesizable::function_output_and_inout_arguments_are_written_back";
    }

    fn tasks_without_timing_write_their_outputs(sim) {
        @case "synthesizable::tasks_without_timing_write_their_outputs";
    }

    fn break_and_continue_end_unrolled_loop_iterations(sim) {
        @setup {
            let source = r#"
                module Top(input logic clk, input logic [8:0] a, output logic [7:0] first,
                           even, lead, rows, q);
                    always_comb begin
                        first = 8'hff;
                        for (int i = 0; i < 8; i++) begin
                            if (a[i]) begin first = i[7:0]; break; end
                        end
                        even = 0;
                        for (int i = 0; i < 8; i++) begin
                            if (i[0]) continue;
                            even = even + a[i];
                        end
                        lead = 0;
                        for (int i = 0; i < 8; i++) begin
                            if (a[i]) break;
                            lead = lead + 1;
                        end
                        rows = 0;
                        for (int r = 0; r < 3; r++) begin
                            for (int c = 0; c < 3; c++) begin
                                if (!a[r*3+c]) break;
                                rows = rows + 1;
                            end
                        end
                    end
                    always_ff @(posedge clk) begin
                        q <= 8'hff;
                        for (int i = 0; i < 8; i++) begin
                            if (a[i]) begin q <= i[7:0]; break; end
                        end
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("jumps.sv"))], "Top");
        let a = sim.signal("a");
        for value in [0u16, 0x1ff, 0x0a8, 0x005, 0x080, 0x1b7] {
            sim.modify(|io| io.set(a, value)).unwrap();
            let bit = |i: u32| (value >> i) & 1;
            let first = (0..8).find(|i| bit(*i) != 0).map_or(0xff, |i| i as u8);
            let even = (0..8).step_by(2).map(bit).sum::<u16>() as u8;
            let lead = (0..8).take_while(|i| bit(*i) == 0).count() as u8;
            let rows: u8 = (0..3)
                .map(|r| (0..3).take_while(|c| bit(r * 3 + c) != 0).count() as u8)
                .sum();
            assert_eq!(sim.get(sim.signal("first")), first.into(), "a={value:#x}");
            assert_eq!(sim.get(sim.signal("even")), even.into(), "a={value:#x}");
            assert_eq!(sim.get(sim.signal("lead")), lead.into(), "a={value:#x}");
            assert_eq!(sim.get(sim.signal("rows")), rows.into(), "a={value:#x}");
            sim.tick(sim.event("clk")).unwrap();
            assert_eq!(sim.get(sim.signal("q")), first.into(), "a={value:#x}");
        }
    }

    fn type_parameters_are_bound_by_instantiations(sim) {
        @case "synthesizable::type_parameters_are_bound_by_instantiations";
    }

    fn block_locals_and_dependent_assignments_accumulate(sim) {
        @setup {
            let source = r#"
                module Top(input logic [7:0] a, output logic [3:0] ones, output logic [7:0] chain,
                           output logic [7:0] swapped);
                    always_comb begin
                        logic [3:0] count;
                        count = 0;
                        for (int i = 0; i < 8; i++) count = count + a[i];
                        ones = count;
                    end
                    always_comb begin
                        chain = a;
                        chain = chain + 1;
                        chain = chain * 2;
                        chain = chain + 3;
                    end
                    always_comb begin : swap
                        logic [3:0] hi, lo;
                        hi = a[7:4];
                        lo = a[3:0];
                        swapped = {lo, hi};
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("accumulate.sv"))], "Top");
        let a = sim.signal("a");
        for value in [0u8, 1, 7, 0x5a, 0xff] {
            sim.modify(|io| io.set(a, value)).unwrap();
            assert_eq!(sim.get(sim.signal("ones")), u8::try_from(value.count_ones()).unwrap().into());
            let chain = value.wrapping_add(1).wrapping_mul(2).wrapping_add(3);
            assert_eq!(sim.get(sim.signal("chain")), chain.into());
            assert_eq!(sim.get(sim.signal("swapped")), value.rotate_left(4).into());
        }
    }
}

#[test]
fn block_local_names_may_not_shadow_other_signals() {
    for source in [
        "module Top(input logic a, output logic t); \
         always_comb begin logic t; t = a; end endmodule",
        "module Top(input logic a, output logic y, z); \
         always_comb begin logic t; t = a; y = t; end \
         always_comb begin logic t; t = ~a; z = t; end endmodule",
    ] {
        let error = Simulator::from_sv_sources(vec![(source, Path::new("shadow.sv"))], "Top")
            .build_cranelift()
            .expect_err("a shadowing block-local name must be rejected")
            .to_string();
        assert!(
            error.contains("duplicate internal signal")
                || error.contains("duplicate port or signal name"),
            "{error}"
        );
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
    assert!(error.contains("Unsupported"), "{error}");
}
