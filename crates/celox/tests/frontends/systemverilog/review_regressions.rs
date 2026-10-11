use super::*;

fn cranelift_build_error(source: &str) -> String {
    match Simulator::from_sv_sources(vec![(source, Path::new("review.sv"))], "Top")
        .build_cranelift()
    {
        Ok(_) => panic!("unsupported SystemVerilog unexpectedly compiled:\n{source}"),
        Err(error) => error.to_string(),
    }
}

fn four_state_cranelift_build_error(source: &str) -> String {
    match Simulator::from_sv_sources(vec![(source, Path::new("review.sv"))], "Top")
        .four_state(true)
        .build_cranelift()
    {
        Ok(_) => panic!("unsupported SystemVerilog unexpectedly compiled:\n{source}"),
        Err(error) => error.to_string(),
    }
}

#[test]
fn reads_previous_values_before_later_writes_in_always_comb() {
    // A variable written by an always_comb block is not in its implicit
    // sensitivity list (IEEE 1800-2023 9.2.2.2.1): a read before the write
    // observes the value from the previous evaluation.
    let source = r#"
        module Top(input logic b, output logic a, output logic c);
            always_comb begin
                c = a;
                a = b;
            end
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("read_before_write.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    let b = sim.signal("b");
    let a = sim.signal("a");
    let c = sim.signal("c");
    sim.modify(|io| io.set(b, 1u8)).unwrap();
    assert_eq!(sim.get(a), 1u8.into());
    assert_eq!(sim.get(c), 0u8.into());
    sim.modify(|io| io.set(b, 0u8)).unwrap();
    assert_eq!(sim.get(c), 1u8.into());
}

#[test]
fn reads_previous_slice_values_before_later_writes_in_always_comb() {
    let source = r#"
        module Top(input logic a, output logic [1:0] y);
            always_comb begin
                y[0] = y[1];
                y[1] = a;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("slice_read_before_write.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let a = sim.signal("a");
    let y = sim.signal("y");
    sim.modify(|io| io.set(a, 1u8)).unwrap();
    assert_eq!(sim.get(y), 0b10u8.into());
    sim.modify(|io| io.set(a, 0u8)).unwrap();
    assert_eq!(sim.get(y), 0b01u8.into());
}

#[test]
fn rejects_self_reads_hidden_in_comb_function_calls() {
    let error = cranelift_build_error(
        r#"
        module Top(input logic c, output logic x);
            function automatic logic read_x();
                return x;
            endfunction
            always_comb begin
                if (c)
                    x = read_x();
                else
                    x = 1'b0;
            end
        endmodule
        "#,
    );
    assert!(
        error.contains("latch inference inside always_comb"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_block_local_declarations_inside_always_comb() {
    let error = cranelift_build_error(
        r#"
        module Top(input logic c, a, b, output logic t, y);
            always_comb begin
                t = 1'b0;
                if (c) begin
                    logic t;
                    t = a;
                    y = t;
                end else begin
                    y = b;
                end
            end
        endmodule
        "#,
    );
    // The local `t` would be hoisted onto the output port `t`.
    assert!(
        error.contains("duplicate port or signal name `t`"),
        "unexpected error: {error}"
    );
}

#[test]
fn substitutes_prior_comb_values_into_function_bodies() {
    let source = r#"
        module Top(input logic c, output logic x, y);
            function automatic logic read_x();
                return x;
            endfunction
            always_comb begin
                x = 1'b0;
                y = read_x();
                if (c)
                    x = 1'b1;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("blocking_read_inside_function.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let c = sim.signal("c");
    let x = sim.signal("x");
    let y = sim.signal("y");
    sim.modify(|io| io.set(c, 1u8)).unwrap();
    assert_eq!(sim.get(x), 1u8.into());
    assert_eq!(sim.get(y), 0u8.into());
}

#[test]
fn substitutes_blocking_values_inside_dynamic_select_indices() {
    let source = r#"
        module Top(
            input logic c, a, b,
            input logic [7:0] lut[2],
            output logic x,
            output logic [7:0] y
        );
            always_comb begin
                x = a;
                y = lut[x];
                if (c)
                    x = b;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("blocking_dynamic_select_index.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let c = sim.signal("c");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let lut = sim.signal("lut");
    let x = sim.signal("x");
    let y = sim.signal("y");
    sim.modify(|io| {
        io.set(c, 0u8);
        io.set(a, 1u8);
        io.set(b, 0u8);
        io.set(lut, 0xaa55u16);
    })
    .unwrap();
    assert_eq!(sim.get(x), 1u8.into());
    assert_eq!(sim.get(y), 0xaau8.into());
    sim.modify(|io| {
        io.set(c, 1u8);
        io.set(a, 0u8);
        io.set(b, 1u8);
    })
    .unwrap();
    assert_eq!(sim.get(x), 1u8.into());
    assert_eq!(sim.get(y), 0x55u8.into());
}

#[test]
fn substitutes_blocking_values_inside_dynamic_write_indices() {
    let source = r#"
        module Top(
            input logic c,
            input logic [1:0] a,
            input logic [1:0] d,
            output logic [3:0] y
        );
            logic bits[4];
            logic [1:0] index;
            always_comb begin
                bits = '0;
                index = a;
                bits[index] = 1'b1;
                if (c)
                    index = d;
            end
            assign y = {bits[3], bits[2], bits[1], bits[0]};
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("blocking_dynamic_write_index.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let c = sim.signal("c");
    let a = sim.signal("a");
    let d = sim.signal("d");
    let y = sim.signal("y");
    sim.modify(|io| {
        io.set(c, false);
        io.set(a, 1u8);
        io.set(d, 3u8);
    })
    .unwrap();
    assert_eq!(sim.get(y), 0b0010u8.into());
    sim.modify(|io| {
        io.set(c, true);
        io.set(a, 0u8);
        io.set(d, 2u8);
    })
    .unwrap();
    assert_eq!(sim.get(y), 0b0001u8.into());
}

#[test]
fn rejects_inline_enum_ports_instead_of_scalarizing_them() {
    let error = cranelift_build_error(
        r#"
        module Top(input enum { A, B, C } state, output logic y);
            assign y = (state == C);
        endmodule
        "#,
    );
    assert!(error.contains("enum port"), "unexpected error: {error}");
}

#[test]
fn rejects_non_integral_ports_instead_of_scalarizing_them() {
    let error = cranelift_build_error(
        r#"
        module Top(input real value, output logic y);
            assign y = 1'b0;
        endmodule
        "#,
    );
    assert!(
        error.contains("unsupported port data type"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_internal_signals_that_shadow_ports() {
    let error = cranelift_build_error(
        r#"
        module Top(output logic y);
            logic y;
            assign y = 1'b1;
        endmodule
        "#,
    );
    assert!(
        error.contains("duplicate port or signal name `y`"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_overlapping_combinational_variable_drivers() {
    let error = cranelift_build_error(
        r#"
        module Top(input logic [1:0] a, b, output wire [2:0] y);
            logic [2:0] value;
            assign value[1:0] = a;
            always_comb value[2:1] = b;
            assign y = value;
        endmodule
        "#,
    );
    assert!(
        error.contains("multiple variable drivers for `value`"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_combinational_and_ff_variable_drivers() {
    let error = cranelift_build_error(
        r#"
        module Top(input logic clk, a, b, output wire y);
            logic q;
            assign q = a;
            always_ff @(posedge clk) q <= b;
            assign y = q;
        endmodule
        "#,
    );
    assert!(
        error.contains("multiple variable drivers for `q`"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_overlapping_always_ff_variable_drivers() {
    let error = cranelift_build_error(
        r#"
        module Top(input logic clk, a, b, output wire [1:0] y);
            logic [1:0] q;
            always_ff @(posedge clk) q[0] <= a;
            always_ff @(posedge clk) q <= {a, b};
            assign y = q;
        endmodule
        "#,
    );
    assert!(
        error.contains("multiple variable drivers for `q`"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_variables_also_written_through_output_arguments() {
    // A called subroutine writes its output actual for the calling process
    // (IEEE 1800-2023 9.2.2.2), so `q` has two always_ff drivers.
    let error = cranelift_build_error(
        r#"
        module Top(input logic clk, input logic [3:0] d, output logic [1:0] x,
                   output wire [3:0] y);
            function automatic logic [1:0] pick(input logic [3:0] prior,
                                                output logic [3:0] after);
                after = prior + 4'd1;
                return 2'd1;
            endfunction
            logic [3:0] q;
            always_ff @(posedge clk) x <= pick(q, q);
            always_ff @(posedge clk) q <= d;
            assign y = q;
        endmodule
        "#,
    );
    assert!(
        error.contains("multiple variable drivers for `q`"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_variables_also_written_by_called_subroutine_bodies() {
    let error = cranelift_build_error(
        r#"
        module Top(input logic clk, input logic [3:0] d, output wire [3:0] y);
            logic [3:0] q;
            task automatic bump();
                q = q + 4'd1;
            endtask
            always_ff @(posedge clk) bump();
            always_ff @(posedge clk) q <= d;
            assign y = q;
        endmodule
        "#,
    );
    assert!(
        error.contains("multiple variable drivers for `q`"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_variables_also_written_by_calls_in_target_indices() {
    // The index of `x[...]` calls `pick`, which writes `q` through an output.
    let error = cranelift_build_error(
        r#"
        module Top(input logic clk, input logic [3:0] d, output logic [3:0] x,
                   output wire [3:0] y);
            logic [3:0] q;
            function automatic logic [1:0] pick(output logic [3:0] o);
                o = 4'd5;
                return 2'd1;
            endfunction
            always_ff @(posedge clk) x[pick(q)] <= 1'b1;
            always_ff @(posedge clk) q <= d;
            assign y = q;
        endmodule
        "#,
    );
    assert!(
        error.contains("multiple variable drivers for `q`"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_variables_also_written_by_default_argument_calls() {
    // `f()` evaluates the default of its omitted argument, which writes `q`.
    let error = cranelift_build_error(
        r#"
        module Top(input logic clk, input logic [3:0] d, output logic [1:0] x,
                   output wire [3:0] y);
            logic [3:0] q;
            function automatic logic side_effect(output logic [3:0] o);
                o = 4'd5;
                return 1'b1;
            endfunction
            function automatic logic [1:0] f(input logic a = side_effect(q));
                return {1'b0, a};
            endfunction
            always_ff @(posedge clk) x <= f();
            always_ff @(posedge clk) q <= d;
            assign y = q;
        endmodule
        "#,
    );
    assert!(
        error.contains("multiple variable drivers for `q`"),
        "unexpected error: {error}"
    );
}

#[test]
fn accepts_a_variable_written_only_through_one_process_calls() {
    let source = r#"
        module Top(input logic clk, output logic [1:0] x, output wire [3:0] y);
            function automatic logic [1:0] pick(input logic [3:0] prior,
                                                output logic [3:0] after);
                after = prior + 4'd1;
                return 2'd1;
            endfunction
            logic [3:0] q;
            initial q = 4'd0;
            always_ff @(posedge clk) x <= pick(q, q);
            assign y = q;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("one_writer.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let y = sim.signal("y");
    let clk = sim.event("clk");
    for count in 1u8..=3 {
        sim.tick(clk).unwrap();
        assert_eq!(sim.get_as::<u8>(y), count);
    }
}

#[test]
fn rejects_child_outputs_that_multiply_drive_a_variable() {
    let error = cranelift_build_error(
        r#"
        module Source(output logic y); assign y = 1'b1; endmodule
        module Top(output wire out);
            logic w;
            Source a(.y(w));
            Source b(.y(w));
            assign out = w;
        endmodule
        "#,
    );
    assert!(
        error.contains("multiple variable drivers for `w`"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_child_outputs_connected_to_input_ports() {
    let error = cranelift_build_error(
        r#"
        module Child(output logic y); assign y = 1'b1; endmodule
        module Top(input logic a); Child child(.y(a)); endmodule
        "#,
    );
    assert!(
        error.contains("write to input port `a`"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_writes_to_input_ports() {
    let error = cranelift_build_error(
        r#"
        module Top(input logic a, b, output wire y);
            always_comb a = b;
            assign y = a;
        endmodule
        "#,
    );
    assert!(
        error.contains("write to input port `a`"),
        "unexpected error: {error}"
    );
}

#[test]
fn supports_generate_locals_that_shadow_parameters() {
    let source = r#"
        module Top #(parameter P = 0) (output wire y);
            if (1) begin : g
                logic P;
                always_comb P = 1'b1;
                assign y = P;
            end
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("generate_shadow.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn rejects_single_branch_generate_locals_instead_of_leaking_them() {
    let error = cranelift_build_error(
        r#"
        module Top(output logic y);
            if (1) begin : g
                logic tmp;
                assign tmp = 1'b1;
            end
            assign y = tmp;
        endmodule
        "#,
    );
    assert!(
        error.contains("combinational expression assigned to `y`"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_parameters_that_collide_with_ports_or_signals() {
    for source in [
        r#"
            module Top #(parameter A = 1) (input logic A, output logic y);
                assign y = A;
            endmodule
        "#,
        r#"
            module Top #(parameter A = 1) (output logic y);
                logic A;
                assign y = A;
            endmodule
        "#,
    ] {
        let error = cranelift_build_error(source);
        assert!(
            error.contains("parameter name collides with port or signal `A`"),
            "unexpected error: {error}"
        );
    }
}

#[test]
fn rejects_generate_local_typedefs_instead_of_leaking_them() {
    let error = cranelift_build_error(
        r#"
        module Top(output wire y);
            if (0) begin : g
                typedef logic [7:0] T;
            end
            T value;
            assign y = value[0];
        endmodule
        "#,
    );
    assert!(
        error.contains("combinational expression assigned to `y`"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_duplicate_function_argument_names() {
    let error = cranelift_build_error(
        r#"
        module Top(output wire y);
            function logic f(input logic a, input logic a);
                return a;
            endfunction
            assign y = f(1'b0, 1'b1);
        endmodule
        "#,
    );
    assert!(
        error.contains("duplicate function argument `a`"),
        "unexpected error: {error}"
    );
}

#[test]
fn merges_function_formals_updated_in_conditional_branches() {
    let source = r#"
        module Top(input logic sel, value, output logic y);
            function automatic logic force_one(input logic s, input logic v);
                if (s) v = 1'b1;
                return v;
            endfunction
            assign y = force_one(sel, value);
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("function_formal.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    let sel = sim.signal("sel");
    let value = sim.signal("value");
    let y = sim.signal("y");
    sim.modify(|io| {
        io.set(sel, 0u8);
        io.set(value, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(y), 0u8.into());
    sim.modify(|io| io.set(sel, 1u8)).unwrap();
    assert_eq!(sim.get(y), 1u8.into());
}

#[test]
fn merges_function_formals_updated_in_case_branches() {
    let source = r#"
        module Top(input logic sel, value, output logic y);
            function automatic logic force_one(input logic s, v);
                case (s)
                    1'b1: v = 1'b1;
                    default: ;
                endcase
                return v;
            endfunction
            assign y = force_one(sel, value);
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("function_formal_case.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    let sel = sim.signal("sel");
    let value = sim.signal("value");
    let y = sim.signal("y");
    sim.modify(|io| {
        io.set(sel, 0u8);
        io.set(value, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(y), 0u8.into());
    sim.modify(|io| io.set(value, 1u8)).unwrap();
    assert_eq!(sim.get(y), 1u8.into());
    sim.modify(|io| {
        io.set(sel, 1u8);
        io.set(value, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(y), 1u8.into());
}

#[test]
fn rejects_dropped_unrepresentable_function_call_actuals() {
    let error = cranelift_build_error(
        r#"
        module Top(input logic a, b, output logic y);
            function automatic logic f(input logic value);
                return value;
            endfunction
            assign y = f(a ** b, b);
        endmodule
        "#,
    );
    assert!(
        error.contains("combinational expression") || error.contains("function call"),
        "unexpected error: {error}"
    );
}

#[test]
fn preserves_selected_parameter_constants_in_hierarchy_glue() {
    let source = r#"
        module Child(input logic a, output logic y); assign y = a; endmodule
        module Top(output logic y);
            parameter logic [3:0] P = 4'h5;
            Child child(.a(P[0]), .y(y));
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("selected_parameter_glue.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn lowers_only_the_reachable_parameter_specialization() {
    let source = r#"
        module Child #(parameter ENABLE = 1) (
            input logic a,
            input logic b,
            output logic y
        );
            if (ENABLE) assign y = a ** b;
            else assign y = 1'b0;
        endmodule
        module Top(output logic y);
            Child #(.ENABLE(0)) child(.a(1'b0), .b(1'b0), .y(y));
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("reachable_specialization.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0u8.into());
}

#[test]
fn initializes_four_state_variables_to_unknown() {
    let source = r#"
        module Top(output wire logic_is_x, output wire bit_is_zero);
            logic four_state;
            bit two_state;
            assign logic_is_x = (four_state === 1'bx);
            assign bit_is_zero = (two_state === 1'b0);
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("variable_initial_state.sv"))],
        "Top",
    )
    .four_state(true)
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("logic_is_x")), 1u8.into());
    assert_eq!(sim.get(sim.signal("bit_is_zero")), 1u8.into());
}

#[test]
fn evaluates_sized_arithmetic_and_logical_right_shift_parameters() {
    let source = r#"
        module Top(output logic wraps, output logic logical_shift);
            localparam WRAPS = ((8'hff + 8'h01) == 8'h00);
            localparam LOGICAL_SHIFT = ((8'shfe >> 1) == 8'h7f);
            assign wraps = WRAPS;
            assign logical_shift = LOGICAL_SHIFT;
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("sized_constants.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    assert_eq!(sim.get(sim.signal("wraps")), 1u8.into());
    assert_eq!(sim.get(sim.signal("logical_shift")), 1u8.into());
}

#[test]
fn wraps_signed_division_and_preserves_oob_parameter_selects() {
    let source = r#"
        module Top(output logic division_wraps, output logic oob_is_unknown);
            parameter logic [3:0] P = 4'b0000;
            parameter OOB = P[4];
            assign division_wraps = ((8'sh80 / 8'shff) == 8'sh80);
            assign oob_is_unknown = (OOB === 1'bx);
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("constant_edge_cases.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    assert_eq!(sim.get(sim.signal("division_wraps")), 1u8.into());
    assert_eq!(sim.get(sim.signal("oob_is_unknown")), 1u8.into());
}

#[test]
fn permits_disjoint_continuous_net_drivers() {
    let source = r#"
        module Top(output wire [1:0] y);
            assign y[0] = 1'b0;
            assign y[1] = 1'b1;
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("disjoint_net_drivers.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 2u8.into());
}

#[test]
fn initializes_partially_driven_internal_nets_to_z() {
    let source = r#"
        module Top(output logic undriven_bits_are_z);
            wire [7:0] w;
            assign w[0] = 1'b0;
            assign undriven_bits_are_z = (w[7:1] === 7'bzzzzzzz);
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("partial_internal_net.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    assert_eq!(sim.get(sim.signal("undriven_bits_are_z")), 1u8.into());
}

#[test]
fn preserves_implicit_net_ranges_and_computed_select_dependencies() {
    let source = r#"
        module Top(input logic [7:0] a, b,
                   output logic [7:0] copied,
                   output logic carry);
            wire [7:0] w;
            assign w = a;
            assign copied = w;
            function automatic logic high_bit(
                input logic [7:0] lhs,
                input logic [7:0] rhs
            );
                logic [7:0] sum;
                sum = lhs + rhs;
                return sum[7];
            endfunction
            assign carry = high_bit(a, b);
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("net_range_and_select_deps.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    sim.set(sim.signal("a"), 0x7eu8);
    sim.set(sim.signal("b"), 1u8);
    assert_eq!(sim.get(sim.signal("copied")), 0x7eu8.into());
    assert_eq!(sim.get(sim.signal("carry")), 0u8.into());
    sim.set(sim.signal("a"), 0x7fu8);
    assert_eq!(sim.get(sim.signal("copied")), 0x7fu8.into());
    assert_eq!(sim.get(sim.signal("carry")), 1u8.into());
}

#[test]
fn initializes_shadowing_function_locals_to_unknown() {
    let source = r#"
        module Top(output logic local_is_unknown);
            logic tmp;
            assign tmp = 1'b1;
            function automatic logic f();
                logic tmp;
                return tmp;
            endfunction
            assign local_is_unknown = (f() === 1'bx);
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("function_local_shadow.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    assert_eq!(sim.get(sim.signal("local_is_unknown")), 1u8.into());
}

#[test]
fn preserves_declared_width_for_parameter_logical_right_shifts() {
    let source = r#"
        module Top(output logic [7:0] shifted, output logic negation_matches);
            parameter logic signed [7:0] P = -2;
            parameter logic [7:0] Q = P >> 1;
            parameter NEGATED = -8'd1;
            parameter NEGATION_MATCHES = (NEGATED == 8'hff);
            assign shifted = Q;
            assign negation_matches = NEGATION_MATCHES;
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("typed_parameter_ops.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    assert_eq!(sim.get(sim.signal("shifted")), 0x7fu8.into());
    assert_eq!(sim.get(sim.signal("negation_matches")), 1u8.into());
}

#[test]
fn applies_unsigned_coercion_to_typed_parameter_comparisons() {
    let source = r#"
        module Top(output logic y, output logic all_ones);
            parameter logic signed [7:0] P = -1;
            if (P < 8'h01) assign y = 1'b0;
            else assign y = 1'b1;
            if (&P) assign all_ones = 1'b1;
            else assign all_ones = 1'b0;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("typed_parameter_compare.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
    assert_eq!(sim.get(sim.signal("all_ones")), 1u8.into());
}

#[test]
fn compile_sv_to_sir_forwards_parameter_overrides() {
    let source = r#"
        module Top #(parameter ENABLE_UNSUPPORTED = 0)
                   (input logic clk, d, output logic q);
            if (ENABLE_UNSUPPORTED) begin
                always_ff @(posedge clk) fork q <= d; join
            end else begin
                assign q = d;
            end
        endmodule
    "#;
    let error = celox::compile_sv_to_sir(
        &[(source, Path::new("compile_sv_override.sv"))],
        "Top",
        &[],
        &[],
        false,
        &celox::TraceOptions::default(),
        None,
        None,
        None,
        None,
        &[("ENABLE_UNSUPPORTED".to_string(), 1)],
        &celox::OptimizeOptions::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("fork-join block"), "{error}");
}

#[test]
fn coerces_ir_parameter_constants_to_their_declared_widths() {
    let source = r#"
        module Top #(
            parameter logic [3:0] W = 5'd16
        ) (output logic [W:0] y);
            assign y = '1;
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("typed_parameter.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn validates_net_drivers_after_parameter_specialization() {
    let error = cranelift_build_error(
        r#"
        module Driver(output logic y); assign y = 1'b1; endmodule
        module Sink(input logic a); endmodule
        module Child #(parameter ENABLE = 1) (output logic y);
            wire w;
            if (ENABLE) Driver driver(.y(w));
            Sink sink(.a(w));
            assign y = 1'b0;
        endmodule
        module Top(output logic y);
            Child #(.ENABLE(0)) child(.y(y));
        endmodule
        "#,
    );
    assert!(
        error.contains("undriven net declaration `w`"),
        "unexpected error: {error}"
    );
}

#[test]
fn applies_unsigned_coercion_to_mixed_signed_constant_comparisons() {
    let source = r#"
        module Top(output logic y);
            localparam FLAG = (8'shff < 8'h01);
            assign y = FLAG;
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("constant_compare.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0u8.into());
}

#[test]
fn does_not_count_child_inputs_as_net_drivers() {
    let error = cranelift_build_error(
        r#"
        module Sink(input logic a, output logic y); assign y = a; endmodule
        module Top(output logic y);
            wire w;
            Sink sink(.a(w), .y(y));
        endmodule
        "#,
    );
    assert!(
        error.contains("undriven net declaration `w`"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_top_level_localparam_overrides() {
    let source = r#"
        module Top(output logic y);
            localparam LOCKED = 0;
            assign y = LOCKED;
        endmodule
    "#;
    let error = Simulator::from_sv_sources(vec![(source, Path::new("localparam.sv"))], "Top")
        .param("LOCKED", 1)
        .build_cranelift()
        .expect_err("localparam override must be rejected")
        .to_string();
    assert!(
        error.contains("localparam override `LOCKED`"),
        "unexpected error: {error}"
    );

    let child_override = r#"
        module Child(output logic y);
            localparam LOCKED = 0;
            assign y = LOCKED;
        endmodule
        module Top(output logic y);
            Child #(.LOCKED(1)) child(.y(y));
        endmodule
    "#;
    let error = cranelift_build_error(child_override);
    assert!(
        error.contains("localparam override `LOCKED`"),
        "unexpected child override error: {error}"
    );
}

#[test]
fn merges_always_ff_processes_with_the_same_trigger() {
    let source = r#"
        module Top(input logic clk, input logic a, input logic b,
                   output logic qa, output logic qb);
            always_ff @(posedge clk) qa <= a;
            always_ff @(posedge clk) qb <= b;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("shared_ff.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let clk = sim.event("clk");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let qa = sim.signal("qa");
    let qb = sim.signal("qb");
    sim.modify(|io| {
        io.set(a, 1u8);
        io.set(b, 1u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(qa), 1u8.into());
    assert_eq!(sim.get(qb), 1u8.into());
}

#[test]
fn builds_whole_unpacked_array_always_ff_assignment() {
    let source = r#"
        module Top(
            input logic clk,
            input logic [7:0] d [2],
            output logic [7:0] q [2]
        );
            always_ff @(posedge clk) q <= d;
        endmodule
    "#;
    Simulator::from_sv_sources(
        vec![(source, Path::new("whole_array_ff_assignment.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
}

#[test]
fn connects_whole_unpacked_array_ports() {
    let source = r#"
        module Child(
            input logic [7:0] input_values[2],
            output logic [7:0] output_values[2]
        );
            assign output_values = input_values;
        endmodule
        module Top(
            input logic [7:0] values[2],
            output logic [7:0] result[2]
        );
            Child child(
                .input_values(values),
                .output_values(result)
            );
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("whole_array_ports.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    let values = sim.signal("values");
    let result = sim.signal("result");
    sim.modify(|io| io.set(values, 0x3412u16)).unwrap();
    assert_eq!(sim.get(result), 0x3412u16.into());
}

#[test]
fn preserves_named_zero_cast_width() {
    let source = r#"
        module Top(output logic [8:0] y);
            typedef logic [7:0] byte_t;
            assign y = {1'b1, byte_t'(0)};
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("named_zero_cast.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0x100u16.into());
}

#[test]
fn ignores_loop_substituted_out_of_range_array_writes() {
    // A write through an index outside the declared range is ignored
    // (IEEE 1800-2023 7.4.6).
    let source = r#"
        module Top(input logic clk, output logic [7:0] y);
            logic [7:0] a[0:1][0:2];
            always_ff @(posedge clk) begin
                a[0][2] <= 8'h11;
                for (int i = 2; i < 4; i++) a[0][i] <= 8'hff;
            end
            assign y = a[0][2];
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("out_of_range_loop_write.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    sim.tick(sim.event("clk")).unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0xffu8.into());
}

#[test]
fn coerces_sized_loop_initializers_to_signed_int() {
    let source = r#"
        module Top(input logic clk, output logic q);
            always_ff @(posedge clk) begin
                for (int i = 32'hffff_ffff; i < 0; i++) q <= 1'b1;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("signed_loop_initializer.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    sim.tick(sim.event("clk")).unwrap();
    assert_eq!(sim.get(sim.signal("q")), 1u8.into());
}

#[test]
fn preserves_outer_loop_index_types_during_nested_unrolling() {
    let source = r#"
        module Top(input logic clk, output logic [1:0] q);
            always_ff @(posedge clk) begin
                q <= 2'b00;
                for (int i = -1; i < 0; i++) begin
                    for (int j = 0; j < (i < 32'd1); j++) q[j] <= 1'b1;
                end
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("nested_signed_loop_index.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    sim.tick(sim.event("clk")).unwrap();
    assert_eq!(sim.get(sim.signal("q")), 0u8.into());
}

#[test]
fn rejects_out_of_range_packed_array_element_indices() {
    let error = cranelift_build_error(
        r#"
        module Top(input logic [7:0] a[0:1], output logic y);
            assign y = a[0][8];
        endmodule
        "#,
    );
    assert!(
        error.contains("index 2 of `a` outside its declared range"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_out_of_range_packed_array_part_select_bounds() {
    let error = cranelift_build_error(
        r#"
        module Top(input logic [7:0] a[0:1], output logic [1:0] y);
            assign y = a[0][9:8];
        endmodule
        "#,
    );
    assert!(
        error.contains("part-select of `a` outside its declared range"),
        "unexpected error: {error}"
    );
}

#[test]
fn preserves_signedness_in_loop_conditions() {
    let source = r#"
        module Top(input logic clk, output logic q, output logic [1:0] result);
            always_ff @(posedge clk) begin
                q <= 1'b0;
                for (int i = -1; i < 32'd1; i++) result[i + 1] <= 1'b1;
            end
        endmodule
        "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("signed_loop_condition.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    sim.tick(sim.event("clk")).unwrap();
    assert_eq!(sim.get(sim.signal("result")), 0u8.into());
}

#[test]
fn scopes_extra_loop_index_declarations() {
    // `J` declared by the loop shadows the parameter inside the loop.
    let source = r#"
        module Top #(parameter J = 1) (input logic clk, output logic [1:0] q);
            always_ff @(posedge clk) begin
                q <= 2'b00;
                for (int i = 0, J = 0; i < 1; i++) q[J] <= 1'b1;
            end
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("loop_declarations.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    sim.tick(sim.event("clk")).unwrap();
    assert_eq!(sim.get(sim.signal("q")), 0b01u8.into());
}

#[test]
fn connects_child_outputs_to_unpacked_array_elements() {
    let source = r#"
        module Child(
            input logic [7:0] input_value,
            output logic [7:0] output_value
        );
            assign output_value = input_value;
        endmodule
        module Top(
            input logic [7:0] a,
            input logic [7:0] b,
            output logic [7:0] values[2]
        );
            Child first(.input_value(a), .output_value(values[0]));
            Child second(.input_value(b), .output_value(values[1]));
        endmodule
        "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("array_output_connection.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let values = sim.signal("values");
    sim.modify(|io| {
        io.set(a, 0x12u8);
        io.set(b, 0x34u8);
    })
    .unwrap();
    assert_eq!(sim.get(values), 0x3412u16.into());
}

#[test]
fn connects_child_outputs_to_unpacked_array_ranges() {
    let source = r#"
        module Child(output logic [15:0] output_value);
            assign output_value = 16'h1234;
        endmodule
        module Top(
            output logic [7:0] values[4],
            output logic [15:0] value
        );
            Child child(.output_value(values[1:0]));
            assign value = values[1:0];
        endmodule
        "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("array_output_range_connection.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("value")), 0x1234u16.into());
}

#[test]
fn connects_runtime_selected_unpacked_array_elements_to_child_inputs() {
    let source = r#"
        module Child(
            input logic [7:0] input_value,
            output logic [7:0] output_value
        );
            assign output_value = input_value;
        endmodule
        module Top(
            input logic [1:0] sel,
            input logic [7:0] values[4],
            output logic [7:0] value
        );
            Child child(.input_value(values[sel]), .output_value(value));
        endmodule
        "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("array_input_dynamic_connection.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let sel = sim.signal("sel");
    let values = sim.signal("values");
    let value = sim.signal("value");
    sim.modify(|io| {
        io.set(sel, 2u8);
        io.set(values, 0x44332211u32);
    })
    .unwrap();
    assert_eq!(sim.get(value), 0x33u8.into());
}

#[test]
fn rejects_runtime_selected_unpacked_array_elements_to_child_outputs() {
    let source = r#"
        module Child(output logic [7:0] output_value);
            assign output_value = 8'h5a;
        endmodule
        module Top(
            input logic [1:0] sel,
            output var logic [7:0] values[4],
            output logic [7:0] value
        );
            Child child(.output_value(values[sel]));
            assign value = values[sel];
        endmodule
        "#;
    let error = cranelift_build_error(source);
    assert!(
        error.contains("systemverilog output port lvalue connection"),
        "{error}"
    );
}

#[test]
fn rejects_runtime_selected_packed_subselects_to_child_outputs() {
    let source = r#"
        module Child(
            input logic [3:0] input_value,
            output logic [3:0] output_value
        );
            assign output_value = input_value;
        endmodule
        module Top(
            input logic [1:0] sel,
            input logic [3:0] input_value,
            output var logic [7:0] values[4],
            output logic [3:0] value
        );
            Child child(.input_value(input_value), .output_value(values[sel][3:0]));
            assign value = values[sel][3:0];
        endmodule
        "#;
    let error = cranelift_build_error(source);
    assert!(
        error.contains("systemverilog output port lvalue connection"),
        "{error}"
    );
}

#[test]
fn rejects_runtime_selected_child_outputs_to_nets() {
    let error = cranelift_build_error(
        r#"
        module Child(output logic [7:0] output_value);
            assign output_value = 8'h5a;
        endmodule
        module Top(input logic [1:0] sel, output logic [7:0] value);
            wire [7:0] values[4];
            Child child(.output_value(values[sel]));
            assign value = values[0];
        endmodule
        "#,
    );
    assert!(
        error.contains("undriven net declaration `values`")
            || error.contains("systemverilog output port lvalue connection"),
        "unexpected error: {error}"
    );
}

#[test]
fn reads_unpacked_array_elements_at_runtime_indices() {
    let source = r#"
        module Top(
            input logic clk,
            input logic [1:0] sel,
            input logic [7:0] values[4],
            output logic [7:0] combinational,
            output logic [7:0] registered
        );
            assign combinational = values[sel];
            always_ff @(posedge clk) registered <= values[sel];
        endmodule
        "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("dynamic_array_index.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    let sel = sim.signal("sel");
    let values = sim.signal("values");
    let combinational = sim.signal("combinational");
    let registered = sim.signal("registered");
    sim.modify(|io| {
        io.set(sel, 2u8);
        io.set(values, 0x44332211u32);
    })
    .unwrap();
    assert_eq!(sim.get(combinational), 0x33u8.into());
    sim.tick(sim.event("clk")).unwrap();
    assert_eq!(sim.get(registered), 0x33u8.into());
}

#[test]
fn returns_unknown_for_invalid_runtime_array_read_indices() {
    let source = r#"
        module Top(
            input bit clk,
            input logic [2:0] sel,
            input logic [7:0] values[4],
            output logic [7:0] combinational,
            output logic [7:0] registered
        );
            assign combinational = values[sel];
            always_ff @(posedge clk) registered <= values[sel];
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("dynamic_array_invalid_read.sv"))],
        "Top",
    )
    .four_state(true)
    .build_cranelift()
    .unwrap();
    let sel = sim.signal("sel");
    let values = sim.signal("values");
    let combinational = sim.signal("combinational");
    let registered = sim.signal("registered");
    sim.modify(|io| {
        io.set(values, 0x44332211u32);
        io.set_four_state(sel, BigUint::default(), BigUint::from(0b111u8));
    })
    .unwrap();
    let all_x = (BigUint::from(0xffu16), BigUint::from(0xffu16));
    assert_eq!(sim.get_four_state(combinational), all_x);
    sim.tick(sim.event("clk")).unwrap();
    assert_eq!(sim.get_four_state(registered), all_x);
    sim.modify(|io| io.set(sel, 5u8)).unwrap();
    assert_eq!(sim.get_four_state(combinational), all_x);
}

#[test]
fn returns_zero_for_invalid_runtime_two_state_array_read_indices() {
    let source = r#"
        module Top(
            input bit clk,
            input logic [2:0] sel,
            input bit [7:0] values[2],
            output logic [7:0] combinational,
            output logic [7:0] registered
        );
            assign combinational = values[sel];
            always_ff @(posedge clk) registered <= values[sel];
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("dynamic_array_invalid_two_state_read.sv"))],
        "Top",
    )
    .four_state(true)
    .build_cranelift()
    .unwrap();
    let sel = sim.signal("sel");
    let values = sim.signal("values");
    let combinational = sim.signal("combinational");
    let registered = sim.signal("registered");
    sim.modify(|io| {
        io.set(values, 0x2211u16);
        io.set(sel, 5u8);
    })
    .unwrap();
    let zero = (BigUint::default(), BigUint::default());
    assert_eq!(sim.get_four_state(combinational), zero);
    sim.tick(sim.event("clk")).unwrap();
    assert_eq!(sim.get_four_state(registered), zero);
    sim.modify(|io| io.set_four_state(sel, BigUint::default(), BigUint::from(0b111u8)))
        .unwrap();
    assert_eq!(sim.get_four_state(combinational), zero);
    sim.tick(sim.event("clk")).unwrap();
    assert_eq!(sim.get_four_state(registered), zero);
}

#[test]
fn returns_unknown_for_invalid_inner_runtime_array_indices() {
    let source = r#"
        module Top(
            input logic [1:0] sel,
            input logic [7:0] values[2][3],
            output logic [7:0] selected
        );
            assign selected = values[0][sel];
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("dynamic_multidimensional_array_read.sv"))],
        "Top",
    )
    .four_state(true)
    .build_cranelift()
    .unwrap();
    let sel = sim.signal("sel");
    let values = sim.signal("values");
    let selected = sim.signal("selected");
    sim.modify(|io| {
        io.set(sel, 3u8);
        io.set_wide(values, BigUint::from(0x6655_4433_2211u64));
    })
    .unwrap();
    assert_eq!(
        sim.get_four_state(selected),
        (BigUint::from(0xffu16), BigUint::from(0xffu16))
    );
}

#[test]
fn reads_packed_subselects_of_runtime_unpacked_array_elements() {
    let source = r#"
        module Top(
            input logic [1:0] sel,
            input logic [7:0] values[4],
            output logic selected_bit,
            output logic [3:0] selected_part
        );
            assign selected_bit = values[sel][4];
            assign selected_part = values[sel][7:4];
        endmodule
        "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("dynamic_array_packed_subselect.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let sel = sim.signal("sel");
    let values = sim.signal("values");
    let selected_bit = sim.signal("selected_bit");
    let selected_part = sim.signal("selected_part");
    sim.modify(|io| {
        io.set(sel, 2u8);
        io.set(values, 0x44332211u32);
    })
    .unwrap();
    assert_eq!(sim.get(selected_bit), 1u8.into());
    assert_eq!(sim.get(selected_part), 3u8.into());
}

#[test]
fn writes_unpacked_array_elements_at_runtime_indices() {
    let source = r#"
        module Top(
            input logic [1:0] sel,
            input logic [7:0] data,
            output logic [7:0] value
        );
            logic [7:0] values[4];
            always_comb values[sel] = data;
            assign value = values[sel];
        endmodule
        "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("dynamic_array_write.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    let sel = sim.signal("sel");
    let data = sim.signal("data");
    let value = sim.signal("value");
    sim.modify(|io| {
        io.set(sel, 2u8);
        io.set(data, 0x5au8);
    })
    .unwrap();
    assert_eq!(sim.get(value), 0x5au8.into());
}

#[test]
fn composes_dynamic_array_writes_after_whole_array_assignments() {
    let source = r#"
        module Top(
            input logic [1:0] sel,
            input logic [7:0] data,
            output logic [7:0] selected,
            output logic [7:0] cleared
        );
            logic [7:0] values[4];
            always_comb begin
                values = '0;
                values[sel] = data;
            end
            assign selected = values[sel];
            assign cleared = values[0];
        endmodule
        "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("dynamic_array_write_after_default.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let sel = sim.signal("sel");
    let data = sim.signal("data");
    let selected = sim.signal("selected");
    let cleared = sim.signal("cleared");
    sim.modify(|io| {
        io.set(sel, 0u8);
        io.set(data, 0xa5u8);
    })
    .unwrap();
    assert_eq!(sim.get(selected), 0xa5u8.into());
    sim.modify(|io| {
        io.set(sel, 2u8);
        io.set(data, 0x5au8);
    })
    .unwrap();
    assert_eq!(sim.get(selected), 0x5au8.into());
    assert_eq!(sim.get(cleared), 0u8.into());
}

#[test]
fn preserves_dynamic_selected_writes_before_conditional_whole_writes() {
    let source = r#"
        module Top(
            input logic [1:0] index,
            input logic data,
            input logic replace,
            output logic [3:0] value
        );
            always_comb begin
                value = '0;
                value[index] = data;
                if (replace) value = '1;
            end
        endmodule
        "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(
            source,
            Path::new("dynamic_select_before_conditional_whole_write.sv"),
        )],
        "Top",
    )
    .four_state(true)
    .build_cranelift()
    .unwrap();
    let index = sim.signal("index");
    let data = sim.signal("data");
    let replace = sim.signal("replace");
    let value = sim.signal("value");

    sim.modify(|io| {
        io.set(index, 2u8);
        io.set(data, true);
        io.set(replace, false);
    })
    .unwrap();
    assert_eq!(sim.get(value), 0b0100u8.into());

    sim.modify(|io| io.set(replace, true)).unwrap();
    assert_eq!(sim.get(value), 0b1111u8.into());

    sim.modify(|io| {
        io.set_four_state(index, BigUint::default(), BigUint::from(0b11u8));
        io.set(replace, false);
    })
    .unwrap();
    assert_eq!(
        sim.get_four_state(value),
        (BigUint::default(), BigUint::default())
    );
}

#[test]
fn preserves_dynamic_array_writes_before_conditional_whole_writes() {
    let source = r#"
        module Top(
            input logic [1:0] index,
            input logic [7:0] data,
            input logic replace,
            output logic [7:0] value
        );
            logic [7:0] values[4];
            always_comb begin
                values = '0;
                values[index] = data;
                if (replace) values = '1;
            end
            assign value = values[2];
        endmodule
        "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(
            source,
            Path::new("dynamic_array_before_conditional_whole_write.sv"),
        )],
        "Top",
    )
    .four_state(true)
    .build_cranelift()
    .unwrap();
    let index = sim.signal("index");
    let data = sim.signal("data");
    let replace = sim.signal("replace");
    let value = sim.signal("value");

    sim.modify(|io| {
        io.set(index, 2u8);
        io.set(data, 0xa5u8);
        io.set(replace, false);
    })
    .unwrap();
    assert_eq!(sim.get(value), 0xa5u8.into());

    sim.modify(|io| io.set(replace, true)).unwrap();
    assert_eq!(sim.get(value), 0xffu8.into());

    sim.modify(|io| {
        io.set_four_state(index, BigUint::default(), BigUint::from(0b11u8));
        io.set(replace, false);
    })
    .unwrap();
    assert_eq!(
        sim.get_four_state(value),
        (BigUint::default(), BigUint::default())
    );
}

#[test]
fn ignores_unknown_dynamic_array_write_indices() {
    let source = r#"
        module Top(
            input logic [1:0] sel,
            input logic [7:0] data,
            output logic [7:0] value
        );
            logic [7:0] values[4];
            always_comb begin
                values = '0;
                values[sel] = data;
            end
            assign value = values[0];
        endmodule
        "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("dynamic_array_write_unknown_index.sv"))],
        "Top",
    )
    .four_state(true)
    .build_cranelift()
    .unwrap();
    let sel = sim.signal("sel");
    let data = sim.signal("data");
    let value = sim.signal("value");
    sim.modify(|io| {
        io.set_four_state(sel, BigUint::default(), BigUint::from(0b11u8));
        io.set(data, 0xffu8);
    })
    .unwrap();
    assert_eq!(
        sim.get_four_state(value),
        (BigUint::default(), BigUint::default())
    );
}

#[test]
fn applies_dynamic_array_writes_after_partial_array_assignments() {
    let source = r#"
        module Top(
            input logic [1:0] sel,
            input logic [7:0] data,
            output logic [7:0] value,
            output logic [7:0] first
        );
            logic [7:0] values[4];
            always_comb begin
                values[0] = 8'h00;
                values[sel] = data;
            end
            assign value = values[sel];
            assign first = values[0];
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("partial_then_dynamic.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    let sel = sim.signal("sel");
    let data = sim.signal("data");
    sim.modify(|io| {
        io.set(sel, 2u8);
        io.set(data, 0x5au8);
    })
    .unwrap();
    assert_eq!(sim.get(sim.signal("value")), 0x5au8.into());
    assert_eq!(sim.get(sim.signal("first")), 0u8.into());
    sim.modify(|io| io.set(sel, 0u8)).unwrap();
    assert_eq!(sim.get(sim.signal("value")), 0x5au8.into());
    assert_eq!(sim.get(sim.signal("first")), 0x5au8.into());
}

#[test]
fn rejects_dynamic_array_writes_in_continuous_assignments() {
    let error = cranelift_build_error(
        r#"
        module Top(
            input logic [1:0] sel,
            input logic [7:0] data,
            output logic [7:0] value
        );
            wire [7:0] values[4];
            assign values[sel] = data;
            assign value = values[sel];
        endmodule
        "#,
    );
    assert!(
        error.contains("combinational assignment target `values`"),
        "unexpected error: {error}"
    );
}

#[test]
fn writes_unpacked_array_elements_at_runtime_indices_in_always_ff() {
    let source = r#"
        module Top(
            input logic clk,
            input logic [1:0] sel,
            input logic [7:0] data,
            output logic [7:0] value
        );
            logic [7:0] values[4];
            always_ff @(posedge clk) values[sel] <= data;
            assign value = values[sel];
        endmodule
        "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("dynamic_array_ff_write.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let sel = sim.signal("sel");
    let data = sim.signal("data");
    let value = sim.signal("value");
    sim.modify(|io| {
        io.set(sel, 2u8);
        io.set(data, 0x5au8);
    })
    .unwrap();
    sim.tick(sim.event("clk")).unwrap();
    assert_eq!(sim.get(value), 0x5au8.into());
}

#[test]
fn writes_packed_subselects_of_runtime_array_elements_in_always_ff() {
    let source = r#"
        module Top(
            input logic clk,
            input logic [1:0] sel,
            input logic [3:0] data,
            output logic [7:0] value
        );
            logic [7:0] values[2];
            always_ff @(posedge clk) begin
                values[sel][7:4] <= 4'ha;
                values[sel][3:0] <= data;
            end
            assign value = values[1];
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("dynamic_array_ff_packed_write.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let sel = sim.signal("sel");
    let data = sim.signal("data");
    let value = sim.signal("value");
    sim.modify(|io| {
        io.set(sel, 1u8);
        io.set(data, 0x5u8);
    })
    .unwrap();
    sim.tick(sim.event("clk")).unwrap();
    assert_eq!(sim.get(value), 0xa5u8.into());
}

#[test]
fn writes_packed_subselects_of_runtime_array_elements_in_always_comb() {
    let source = r#"
        module Top(
            input logic [1:0] sel,
            input logic [3:0] data,
            output logic [7:0] value
        );
            logic [7:0] values[2];
            always_comb begin
                values = '0;
                values[sel][7:4] = 4'ha;
                values[sel][3:0] = data;
            end
            assign value = values[1];
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("dynamic_array_comb_packed_write.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let sel = sim.signal("sel");
    let data = sim.signal("data");
    let value = sim.signal("value");
    sim.modify(|io| {
        io.set(sel, 1u8);
        io.set(data, 0x5u8);
    })
    .unwrap();
    assert_eq!(sim.get(value), 0xa5u8.into());
}

#[test]
fn ignores_invalid_dynamic_array_indices_in_always_ff() {
    let source = r#"
        module Top(
            input bit clk,
            input logic [2:0] sel,
            input logic [7:0] data,
            output logic [7:0] value0
        );
            logic [7:0] values[2];
            always_ff @(posedge clk) values[sel] <= data;
            assign value0 = values[0];
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("dynamic_array_ff_invalid_write.sv"))],
        "Top",
    )
    .four_state(true)
    .build_cranelift()
    .unwrap();
    let sel = sim.signal("sel");
    let data = sim.signal("data");
    let value0 = sim.signal("value0");
    sim.modify(|io| {
        io.set(sel, 0u8);
        io.set(data, 0x11u8);
    })
    .unwrap();
    sim.tick(sim.event("clk")).unwrap();
    assert_eq!(sim.get(value0), 0x11u8.into());
    sim.modify(|io| {
        io.set(sel, 5u8);
        io.set(data, 0x22u8);
    })
    .unwrap();
    sim.tick(sim.event("clk")).unwrap();
    assert_eq!(sim.get(value0), 0x11u8.into());
    sim.modify(|io| {
        io.set_four_state(sel, BigUint::default(), BigUint::from(0b111u8));
        io.set(data, 0x33u8);
    })
    .unwrap();
    sim.tick(sim.event("clk")).unwrap();
    assert_eq!(sim.get(value0), 0x11u8.into());
}

#[test]
fn uses_the_first_always_ff_event_as_a_negedge_clock() {
    let source = r#"
        module Top(input logic clk, input logic rst, input logic d, output logic q);
            always_ff @(negedge clk or posedge rst) begin
                if (rst) q <= 1'b0;
                else q <= d;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("negedge_ff.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let d = sim.signal("d");
    let q = sim.signal("q");
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(d, 1u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 1u8.into());
}

#[test]
fn rejects_ambiguous_multi_event_always_ff_roles() {
    for source in [
        r#"
        module Top(input logic clk, input logic rst, input logic d, output logic q);
            always_ff @(posedge rst or posedge clk)
                if (rst) q <= 1'b0; else q <= d;
        endmodule
        "#,
        r#"
        module Top(input logic clk, input logic rst_n, input logic d, output logic q);
            always_ff @(posedge clk or negedge rst_n)
                if (clk) q <= d; else q <= 1'b0;
        endmodule
        "#,
    ] {
        let error = cranelift_build_error(source);
        assert!(
            error.contains("always_ff event control"),
            "unexpected error: {error}"
        );
    }
}

#[test]
fn preserves_comb_function_return_width() {
    let source = r#"
        module Top(input logic [7:0] x, output logic [15:0] y);
            function automatic logic [7:0] increment(input logic [7:0] value);
                return value + 1'b1;
            endfunction
            assign y = increment(x);
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("function_width.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let x = sim.signal("x");
    let y = sim.signal("y");
    sim.modify(|io| io.set(x, 0xffu8)).unwrap();
    assert_eq!(sim.get(y), 0u16.into());
}

#[test]
fn preserves_declared_parameter_width_in_comb_expressions() {
    let source = r#"
        module Top #(
            parameter logic [63:0] VALUE = 64'h0000_0001_0000_0000
        ) (output logic [63:0] y);
            assign y = VALUE;
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("parameter_width.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0x1_0000_0000u64.into());
}

#[test]
fn specializes_negative_parameter_overrides() {
    let source = r#"
        module Child #(parameter P = 0) (output logic y);
            if (P == -1) assign y = 1'b1;
            else assign y = 1'b0;
        endmodule
        module Top(output logic y);
            Child #(.P(-1)) child(.y(y));
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("negative_parameter_override.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn sign_extends_wide_negative_parameters() {
    let source = r#"
        module Top(output logic [255:0] y);
            parameter logic signed [255:0] P = -1;
            assign y = P;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("wide_negative_parameter.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let expected: BigUint = (BigUint::from(1u8) << 256usize) - BigUint::from(1u8);
    assert_eq!(sim.get(sim.signal("y")), expected);
}

#[test]
fn zero_extends_unsigned_function_return_expressions() {
    let source = r#"
        module Top(output logic [15:0] y);
            function automatic logic signed [15:0] value();
                return 8'h80;
            endfunction
            assign y = value();
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("unsigned_function_return.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0x0080u16.into());
}

#[test]
fn rejects_child_outputs_connected_to_parameters() {
    let error = cranelift_build_error(
        r#"
        module Child(output logic y); assign y = 1'b1; endmodule
        module Top(output logic out);
            parameter P = 0;
            Child child(.y(P));
            assign out = P;
        endmodule
        "#,
    );
    assert!(
        error.contains("cannot drive parameter `P`"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_multiple_child_outputs_driving_an_implicit_net() {
    let error = cranelift_build_error(
        r#"
        module Source(output wire y); assign y = 1'b1; endmodule
        module Sink(input logic a, output logic y); assign y = a; endmodule
        module Top(output logic out);
            Source first(.y(w));
            Source second(.y(w));
            Sink sink(.a(w), .y(out));
        endmodule
        "#,
    );
    assert!(
        error.contains("multiple child outputs drive implicit net `w`"),
        "unexpected error: {error}"
    );
}

#[test]
fn preserves_signed_bitwise_complement_width_in_constants() {
    let source = r#"
        module Top(output logic y);
            parameter FLAG = (~4'sh0 == 4'hf);
            assign y = FLAG;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("signed_constant_complement.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn applies_unsigned_coercion_to_mixed_signed_constant_division() {
    let source = r#"
        module Top(output logic [7:0] quotient, output logic [7:0] remainder);
            localparam QUOTIENT = 8'shfe / 8'h02;
            localparam REMAINDER = 8'shfe % 8'h02;
            assign quotient = QUOTIENT;
            assign remainder = REMAINDER;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("mixed_constant_division.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("quotient")), 127u8.into());
    assert_eq!(sim.get(sim.signal("remainder")), 0u8.into());
}

#[test]
fn converts_unknown_bits_when_assigning_to_bit() {
    let source = r#"
        module Top(input logic x, output bit y);
            assign y = x;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("two_state.sv"))], "Top")
        .four_state(true)
        .build_cranelift()
        .unwrap();
    let x = sim.signal("x");
    let y = sim.signal("y");
    sim.modify(|io| io.set_four_state(x, BigUint::from(1u8), BigUint::from(1u8)))
        .unwrap();
    assert_eq!(
        sim.get_four_state(y),
        (BigUint::from(0u8), BigUint::from(0u8))
    );
}

#[test]
fn coerces_assignments_to_declared_function_local_types() {
    let source = r#"
        module Top(input logic [7:0] a, input logic x,
                   output logic [7:0] truncated, output logic two_state);
            function automatic logic [7:0] truncate(input logic [7:0] value);
                logic [3:0] tmp;
                tmp = value;
                return tmp;
            endfunction
            function automatic logic convert(input logic value);
                bit tmp;
                tmp = value;
                return tmp;
            endfunction
            assign truncated = truncate(a);
            assign two_state = convert(x);
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("function_local_types.sv"))], "Top")
            .four_state(true)
            .build_cranelift()
            .unwrap();
    let a = sim.signal("a");
    let x = sim.signal("x");
    sim.modify(|io| {
        io.set(a, 0xabu8);
        io.set_four_state(x, BigUint::from(1u8), BigUint::from(1u8));
    })
    .unwrap();
    assert_eq!(sim.get(sim.signal("truncated")), 0x0bu8.into());
    assert_eq!(
        sim.get_four_state(sim.signal("two_state")),
        (BigUint::default(), BigUint::default())
    );
}

#[test]
fn initializes_undriven_ansi_net_outputs_to_high_impedance() {
    let source = "module Top(output wire [7:0] y); endmodule";
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("undriven_port.sv"))], "Top")
        .four_state(true)
        .build_cranelift()
        .unwrap();
    assert_eq!(
        sim.get_four_state(sim.signal("y")),
        (BigUint::default(), BigUint::from(0xffu8))
    );
}

#[test]
fn converts_unknown_hierarchical_inputs_to_bit() {
    let source = r#"
        module Child(input bit a, output logic y);
            assign y = a;
        endmodule
        module Top(input logic x, output logic y);
            Child child(.a(x), .y(y));
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("child_bit.sv"))], "Top")
        .four_state(true)
        .build_cranelift()
        .unwrap();
    let x = sim.signal("x");
    let y = sim.signal("y");
    sim.modify(|io| io.set_four_state(x, BigUint::from(1u8), BigUint::from(1u8)))
        .unwrap();
    assert_eq!(
        sim.get_four_state(y),
        (BigUint::from(0u8), BigUint::from(0u8))
    );
}

#[test]
fn converts_unknown_child_outputs_to_parent_bit() {
    let source = r#"
        module Child(output logic y); assign y = 1'bx; endmodule
        module Top(output bit y); Child child(.y(y)); endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("parent_bit.sv"))], "Top")
        .four_state(true)
        .build_cranelift()
        .unwrap();
    assert_eq!(
        sim.get_four_state(sim.signal("y")),
        (BigUint::from(0u8), BigUint::from(0u8))
    );
}

#[test]
fn converts_unknown_mixed_sv_outputs_to_veryl_bit() {
    let veryl = r#"
        module Top (y: output bit) {
            inst child: $sv::Child (y);
        }
    "#;
    let sv = "module Child(output logic y); assign y = 1'bx; endmodule";
    let mut sim = Simulator::from_mixed_sources(
        vec![(veryl, Path::new("top.veryl"))],
        vec![(sv, Path::new("child.sv"))],
        "Top",
    )
    .four_state(true)
    .build_cranelift()
    .unwrap();
    assert_eq!(
        sim.get_four_state(sim.signal("y")),
        (BigUint::from(0u8), BigUint::from(0u8))
    );
}

#[test]
fn preserves_nested_loop_generate_assignments() {
    let source = r#"
        module Top #(parameter ENABLE = 1) (
            input logic [1:0] a,
            output logic [1:0] y
        );
            if (ENABLE) begin
                for (genvar i = 0; i < 2; i++) begin
                    assign y[i] = a[i];
                end
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("nested_loop.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let a = sim.signal("a");
    let y = sim.signal("y");
    sim.modify(|io| io.set(a, 2u8)).unwrap();
    assert_eq!(sim.get(y), 2u8.into());
}

#[test]
fn preserves_decimal_unknown_literals() {
    let source = r#"
        module Top(output logic [3:0] x, output logic [7:0] z);
            assign x = 4'dx;
            assign z = 8'dz;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("decimal_xz.sv"))], "Top")
        .four_state(true)
        .build_cranelift()
        .unwrap();
    assert_eq!(
        sim.get_four_state(sim.signal("x")),
        (BigUint::from(0x0fu8), BigUint::from(0x0fu8))
    );
    assert_eq!(
        sim.get_four_state(sim.signal("z")),
        (BigUint::from(0u8), BigUint::from(0xffu8))
    );
}

#[test]
fn preserves_arithmetic_shift_operators() {
    let source = r#"
        module Top(
            input logic signed [7:0] a,
            input logic [7:0] u,
            output logic signed [7:0] left,
            output logic signed [7:0] right,
            output logic [7:0] unsigned_right
        );
            assign left = a <<< 1;
            assign right = a >>> 1;
            assign unsigned_right = u >>> 1;
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("arithmetic_shift.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    let a = sim.signal("a");
    let u = sim.signal("u");
    sim.modify(|io| {
        io.set(a, 0xfcu8);
        io.set(u, 0xfcu8);
    })
    .unwrap();
    assert_eq!(sim.get(sim.signal("left")), 0xf8u8.into());
    assert_eq!(sim.get(sim.signal("right")), 0xfeu8.into());
    assert_eq!(sim.get(sim.signal("unsigned_right")), 0x7eu8.into());
}

#[test]
fn does_not_treat_typedef_variables_as_instances() {
    let source = r#"
        module Top(input logic [7:0] a, output logic [7:0] y);
            typedef logic [7:0] word_t;
            word_t value;
            assign value = a;
            assign y = value;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("typedef_signal.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let a = sim.signal("a");
    let y = sim.signal("y");
    sim.modify(|io| io.set(a, 0xa5u8)).unwrap();
    assert_eq!(sim.get(y), 0xa5u8.into());
}

#[test]
fn preserves_ternary_parameter_values() {
    let source = r#"
        module Top #(
            parameter ENABLE = 1,
            parameter W = ENABLE ? 8 : 4
        ) (
            input logic [W-1:0] a,
            output logic [W-1:0] y
        );
            assign y = a;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("ternary_param.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let a = sim.signal("a");
    let y = sim.signal("y");
    sim.modify(|io| io.set(a, 0xa5u8)).unwrap();
    assert_eq!(sim.get(y), 0xa5u8.into());
}

#[test]
fn sign_extends_signed_literals_in_constant_expressions() {
    let source = r#"
        module Top #(
            parameter FLAG = (8'shff < 0)
        ) (
            output logic y
        );
            assign y = FLAG;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("signed_param.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn produces_unknown_for_four_state_division_by_zero() {
    let source = r#"
        module Top(output logic [7:0] div, output logic [7:0] rem);
            assign div = 8'd5 / 8'd0;
            assign rem = 8'd5 % 8'd0;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("zero_div.sv"))], "Top")
        .four_state(true)
        .build_cranelift()
        .unwrap();
    let div = sim.signal("div");
    let rem = sim.signal("rem");
    assert_eq!(
        sim.get_four_state(div),
        (BigUint::from(0xffu8), BigUint::from(0xffu8))
    );
    assert_eq!(
        sim.get_four_state(rem),
        (BigUint::from(0xffu8), BigUint::from(0xffu8))
    );
}

#[test]
fn rejects_incomplete_cases_for_potential_four_state_division_by_zero() {
    // In four-state simulation, a division by zero is X and matches no item.
    let error = four_state_cranelift_build_error(
        r#"
        module Top(input bit a, b, output logic y);
            always_comb begin
                case (a / b)
                    1'b0: y = 1'b0;
                    1'b1: y = 1'b1;
                endcase
            end
        endmodule
        "#,
    );
    assert!(
        error.contains("latch inference inside always_comb"),
        "unexpected error: {error}"
    );
}

#[test]
fn recognizes_complete_cases_for_nonzero_two_state_divisors() {
    let source = r#"
        module Top(input bit a, output logic y);
            always_comb begin
                case (a / 1'b1)
                    1'b0: y = 1'b0;
                    1'b1: y = 1'b1;
                endcase
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("nonzero_two_state_case_divisor.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let a = sim.signal("a");
    let y = sim.signal("y");
    sim.modify(|io| io.set(a, false)).unwrap();
    assert_eq!(sim.get(y), false.into());
    sim.modify(|io| io.set(a, true)).unwrap();
    assert_eq!(sim.get(y), true.into());
}

#[test]
fn preserves_typedef_function_return_width_in_ff_case() {
    let source = r#"
        module Top(input logic clk, output logic [7:0] q);
            typedef logic [1:0] word_t;
            function automatic word_t decode(input logic ignored);
                return 4;
            endfunction
            always_ff @(posedge clk) begin
                case (decode(1'b0))
                    2'b00: q <= 8'hfa;
                    default: q <= 0;
                endcase
            end
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("typedef_function.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    sim.tick(sim.event("clk")).unwrap();
    assert_eq!(sim.get(sim.signal("q")), 0xfau8.into());
}

#[test]
fn coerces_hierarchy_widths_and_leaves_omitted_ports_unconnected() {
    let source = r#"
        module Child(input logic [7:0] i, input logic omitted, output logic o);
            assign o = i[0] | omitted;
        endmodule
        module Top(input logic a, input logic omitted, output logic [7:0] y);
            Child child(.i(a), .o(y));
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("widths.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let a = sim.signal("a");
    let omitted = sim.signal("omitted");
    let y = sim.signal("y");
    sim.modify(|io| {
        io.set(a, 1u8);
        io.set(omitted, 1u8);
    })
    .unwrap();
    assert_eq!(sim.get(y), 1u8.into());
}

#[test]
fn forwards_top_parameter_overrides_for_pure_sv() {
    let source = r#"
        module Top #(parameter VALUE = 1) (output logic [3:0] y);
            assign y = VALUE;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("param.sv"))], "Top")
        .param("VALUE", 9)
        .build_cranelift()
        .unwrap();
    let y = sim.signal("y");
    assert_eq!(sim.get(y), 9u8.into());
}

#[test]
fn preserves_signed_operations_and_assignment_extension() {
    let source = r#"
        module Top(input logic signed [7:0] a,
                   output logic signed [15:0] extended,
                   output logic signed [7:0] divided,
                   output logic less);
            assign extended = a;
            assign divided = a / 8'sd2;
            assign less = a < 8'sd1;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("signed.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let a = sim.signal("a");
    let extended = sim.signal("extended");
    let divided = sim.signal("divided");
    let less = sim.signal("less");
    sim.modify(|io| io.set(a, 0xfcu8)).unwrap();
    assert_eq!(sim.get(extended), 0xfffcu16.into());
    assert_eq!(sim.get(divided), 0xfeu8.into());
    assert_eq!(sim.get(less), 1u8.into());
}

#[test]
fn propagates_assignment_width_into_combinational_expressions() {
    let source = r#"
        module Top(input logic [7:0] a, output logic [15:0] y);
            assign y = a << 8;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("context_width.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let a = sim.signal("a");
    let y = sim.signal("y");
    sim.modify(|io| io.set(a, 1u8)).unwrap();
    assert_eq!(sim.get(y), 0x100u16.into());
}

#[test]
fn preserves_last_write_order_inside_always_comb() {
    let source = r#"
        module Top(input logic a, b, output logic y);
            always_comb begin
                y = a;
                y = b;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("comb_order.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");
    sim.modify(|io| {
        io.set(a, 1u8);
        io.set(b, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(y), 0u8.into());
}

#[test]
fn merges_overlapping_writes_inside_always_comb() {
    let source = r#"
        module Top(input logic a, output logic [7:0] y);
            always_comb begin
                y = 8'h00;
                y[0] = a;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("comb_overlap.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let a = sim.signal("a");
    let y = sim.signal("y");
    sim.modify(|io| io.set(a, 1u8)).unwrap();
    assert_eq!(sim.get(y), 1u8.into());
    sim.modify(|io| io.set(a, 0u8)).unwrap();
    assert_eq!(sim.get(y), 0u8.into());
}

#[test]
fn preserves_prior_conditional_writes_across_overlapping_comb_slices() {
    let source = r#"
        module Top(input logic c, d, output logic [3:0] x);
            always_comb begin
                x = '0;
                if (c) x[3:1] = 3'b111;
                if (d) x[2:0] = 3'b000;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("comb_overlapping_slice_fallback.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let c = sim.signal("c");
    let d = sim.signal("d");
    let x = sim.signal("x");
    sim.modify(|io| {
        io.set(c, 1u8);
        io.set(d, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(x), 0b1110u8.into());
}

#[test]
fn later_exhaustive_comb_chain_overrides_an_earlier_chain() {
    let source = r#"
        module Top(input logic c, d, a, b, e, f, output logic x);
            always_comb begin
                if (c) x = a;
                else x = b;
                if (d) x = e;
                else x = f;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("comb_consecutive_exhaustive_chains.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let c = sim.signal("c");
    let d = sim.signal("d");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let e = sim.signal("e");
    let f = sim.signal("f");
    let x = sim.signal("x");
    sim.modify(|io| {
        io.set(c, 1u8);
        io.set(d, 0u8);
        io.set(a, 1u8);
        io.set(b, 1u8);
        io.set(e, 1u8);
        io.set(f, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(x), 0u8.into());
}

#[test]
fn preserves_selected_values_before_conditional_whole_comb_writes() {
    let source = r#"
        module Top(input logic c, a, output logic [7:0] x);
            always_comb begin
                x = '0;
                x[0] = a;
                if (c) x = 8'hff;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("comb_selected_then_whole.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let c = sim.signal("c");
    let a = sim.signal("a");
    let x = sim.signal("x");
    sim.modify(|io| {
        io.set(c, 0u8);
        io.set(a, 1u8);
    })
    .unwrap();
    assert_eq!(sim.get(x), 1u8.into());
}

#[test]
fn reads_values_assigned_earlier_on_each_comb_branch() {
    let source = r#"
        module Top(input logic c, output logic x, y);
            always_comb begin
                if (c) begin
                    x = 1'b1;
                    y = x;
                end else begin
                    x = 1'b0;
                    y = x;
                end
            end
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("comb_path_local_read.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    let c = sim.signal("c");
    let x = sim.signal("x");
    let y = sim.signal("y");
    sim.modify(|io| io.set(c, 1u8)).unwrap();
    assert_eq!(sim.get(x), 1u8.into());
    assert_eq!(sim.get(y), 1u8.into());
    sim.modify(|io| io.set(c, 0u8)).unwrap();
    assert_eq!(sim.get(x), 0u8.into());
    assert_eq!(sim.get(y), 0u8.into());
}

#[test]
fn freezes_comb_branch_guards_before_predicate_writes() {
    let source = r#"
        module Top(input logic en, output logic t, y);
            always_comb begin
                t = en;
                y = 1'b0;
                if (t) begin
                    t = 1'b0;
                    y = 1'b1;
                end
            end
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("comb_frozen_predicate.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    let en = sim.signal("en");
    let t = sim.signal("t");
    let y = sim.signal("y");
    sim.modify(|io| io.set(en, 1u8)).unwrap();
    assert_eq!(sim.get(t), 0u8.into());
    assert_eq!(sim.get(y), 1u8.into());
}

#[test]
fn freezes_all_sibling_comb_guards_before_branch_writes() {
    let source = r#"
        module Top(input logic en, output logic s, y);
            always_comb begin
                s = en;
                y = 1'b0;
                if (s)
                    s = 1'b0;
                else if (!s)
                    y = 1'b1;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("comb_sibling_frozen_predicate.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let en = sim.signal("en");
    let s = sim.signal("s");
    let y = sim.signal("y");
    sim.modify(|io| io.set(en, 1u8)).unwrap();
    assert_eq!(sim.get(s), 0u8.into());
    assert_eq!(sim.get(y), 0u8.into());
    sim.modify(|io| io.set(en, 0u8)).unwrap();
    assert_eq!(sim.get(y), 1u8.into());
}

#[test]
fn propagates_nested_comb_writes_to_the_enclosing_path() {
    let source = r#"
        module Top(input logic c, d, output logic [1:0] x, y);
            always_comb begin
                if (c) begin
                    x = 2'd1;
                    if (d) x = 2'd2;
                    y = x;
                end else begin
                    x = 2'd0;
                    y = x;
                end
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("comb_nested_path_value.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let c = sim.signal("c");
    let d = sim.signal("d");
    let x = sim.signal("x");
    let y = sim.signal("y");
    sim.modify(|io| {
        io.set(c, 1u8);
        io.set(d, 1u8);
    })
    .unwrap();
    assert_eq!(sim.get(x), 2u8.into());
    assert_eq!(sim.get(y), 2u8.into());
}

#[test]
fn substitutes_whole_vector_reads_after_selected_comb_writes() {
    let source = r#"
        module Top(input logic c, a, b, output logic [7:0] x, y);
            always_comb begin
                x = '0;
                x[0] = a;
                y = x;
                if (c) x[0] = b;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("comb_selected_then_whole_read.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let c = sim.signal("c");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let x = sim.signal("x");
    let y = sim.signal("y");
    sim.modify(|io| {
        io.set(c, 1u8);
        io.set(a, 1u8);
        io.set(b, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(x), 0u8.into());
    assert_eq!(sim.get(y), 1u8.into());
}

#[test]
fn reevaluates_constant_casts_after_parameter_overrides() {
    let source = r#"
        module Top #(parameter A = 3) (output logic [7:0] y);
            typedef logic [7:0] byte_t;
            localparam B = byte_t'(A);
            assign y = B;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("parameter_dependent_cast.sv"))],
        "Top",
    )
    .param("A", 4)
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 4u8.into());
}

#[test]
fn resolves_typedefs_in_size_function_cast_targets() {
    let source = r#"
        module Top(output logic [7:0] y);
            typedef logic [7:0] byte_t;
            localparam P = $bits(byte_t)'(16'h1ff);
            assign y = P;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("typedef_size_function_cast.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0xffu8.into());
}

#[test]
fn resolves_parameterized_direct_types_in_size_function_cast_targets() {
    let source = r#"
        module Top(output logic [7:0] y);
            parameter W = 8;
            localparam Q = $bits(logic [W'(7):0])'(8'hff);
            assign y = Q;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("direct_type_size_function_cast.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0xffu8.into());
}

#[test]
fn infers_parameter_expression_widths_in_size_function_cast_targets() {
    let source = r#"
        module Top(output logic [7:0] y);
            parameter logic [7:0] P = 0;
            localparam Q = $bits(P)'(4'hf);
            assign y = Q;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("expression_size_function_cast.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0x0fu8.into());
}

#[test]
fn infers_variable_widths_in_size_function_cast_targets() {
    let source = r#"
        module Top(
            input logic [7:0] a,
            output logic [7:0] port_bits,
            output logic [7:0] port_size,
            output logic [31:0] signal_bits,
            output logic [7:0] signal_size
        );
            logic [7:0] internal [0:3];
            localparam PB = $bits(a)'(12'h1ff);
            localparam PS = $size(a)'(12'h1ff);
            localparam SB = $bits(internal)'(40'h1fffffffff);
            localparam SS = $size(internal)'(8'h1f);
            assign port_bits = PB;
            assign port_size = PS;
            assign signal_bits = SB;
            assign signal_size = SS;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("variable_size_function_cast.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("port_bits")), 0xffu8.into());
    assert_eq!(sim.get(sim.signal("port_size")), 0xffu8.into());
    assert_eq!(sim.get(sim.signal("signal_bits")), 0xffff_ffffu32.into());
    assert_eq!(sim.get(sim.signal("signal_size")), 0x0fu8.into());
}

#[test]
fn recognizes_exhaustive_comb_coverage_across_selected_writes() {
    let source = r#"
        module Top(input logic c, a, b, output logic [1:0] x);
            always_comb begin
                if (c)
                    x = 2'b00;
                else begin
                    x[1] = a;
                    x[0] = b;
                end
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("comb_overlapping_branch_coverage.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let c = sim.signal("c");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let x = sim.signal("x");
    sim.modify(|io| {
        io.set(c, 0u8);
        io.set(a, 1u8);
        io.set(b, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(x), 2u8.into());
    sim.modify(|io| io.set(c, 1u8)).unwrap();
    assert_eq!(sim.get(x), 0u8.into());
}

#[test]
fn reevaluates_constant_casts_with_enum_operands() {
    let source = r#"
        module Top(output logic [7:0] y);
            typedef logic [7:0] byte_t;
            typedef enum logic [1:0] { N = 2 } E;
            localparam B = byte_t'(N);
            assign y = B;
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("enum_dependent_cast.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 2u8.into());
}

#[test]
fn coerces_each_guarded_rhs_before_building_a_mux() {
    let source = r#"
        module Top(input logic c, output logic [7:0] x);
            always_comb begin
                if (c)
                    x = 1'sb1;
                else
                    x = 8'b0;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("comb_guarded_assignment_coercion.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let c = sim.signal("c");
    let x = sim.signal("x");
    sim.modify(|io| io.set(c, 1u8)).unwrap();
    assert_eq!(sim.get(x), 0xffu8.into());
    sim.modify(|io| io.set(c, 0u8)).unwrap();
    assert_eq!(sim.get(x), 0u8.into());
}

#[test]
fn coerces_unconditional_fallbacks_before_building_a_mux() {
    let source = r#"
        module Top(input logic c, output logic [7:0] x);
            always_comb begin
                x = 1'sb1;
                if (c)
                    x = 8'b0;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(
            source,
            Path::new("comb_unconditional_assignment_coercion.sv"),
        )],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let c = sim.signal("c");
    let x = sim.signal("x");
    sim.modify(|io| io.set(c, 0u8)).unwrap();
    assert_eq!(sim.get(x), 0xffu8.into());
    sim.modify(|io| io.set(c, 1u8)).unwrap();
    assert_eq!(sim.get(x), 0u8.into());
}

#[test]
fn sizes_unpacked_array_type_function_cast_targets() {
    let source = r#"
        module Top(output logic [31:0] bits_y, output logic [7:0] size_y);
            typedef logic [7:0] bytes_t [0:3];
            localparam B = $bits(bytes_t)'(40'h1fffffffff);
            localparam S = $size(bytes_t)'(8'h1f);
            assign bits_y = B;
            assign size_y = S;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("unpacked_typedef_size_function_cast.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("bits_y")), 0xffff_ffffu32.into());
    assert_eq!(sim.get(sim.signal("size_y")), 0x0fu8.into());
}

#[test]
fn resolves_parameter_ranges_with_enum_constants() {
    let source = r#"
        module Top(output logic [7:0] y);
            typedef enum logic [1:0] { W = 2 } E;
            localparam logic signed [W-1:0] P = 2'b11;
            assign y = P;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("enum_dependent_parameter_range.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0xffu8.into());
}

#[test]
fn resolves_named_casts_in_function_return_ranges() {
    let source = r#"
        module Top #(parameter W = 8) (output logic [7:0] y);
            function automatic logic [W'(7):0] f();
                return 8'hff;
            endfunction
            always_comb y = f();
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("named_cast_function_return_range.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0xffu8.into());
}

#[test]
fn context_sizes_wide_unbased_fills_in_conditional_writes() {
    let source = r#"
        module Top(input logic c, output logic [63:0] y);
            always_comb begin
                if (c) y = '1;
                else y = '0;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("wide_conditional_unbased_fill.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    sim.set(sim.signal("c"), 1u8);
    assert_eq!(sim.get(sim.signal("y")), u64::MAX.into());
    sim.set(sim.signal("c"), 0u8);
    assert_eq!(sim.get(sim.signal("y")), 0u64.into());
}

#[test]
fn preserves_enum_dependent_parameters_in_instance_overrides() {
    let source = r#"
        module Child #(parameter Q = 0) (output logic [1:0] y);
            assign y = Q;
        endmodule
        module Top(output logic [1:0] y);
            typedef enum logic [1:0] { A = 2 } E;
            localparam P = A;
            Child #(.Q(P)) child(.y(y));
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("enum_dependent_instance_parameter.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 2u8.into());
}

#[test]
fn translates_declared_packed_indices_when_composing_whole_writes() {
    let source = r#"
        module Top(
            input logic c,
            input logic b,
            input logic [3:0] descending_value,
            input logic [3:0] ascending_value,
            output logic [4:1] descending_x,
            output logic [1:4] ascending_x
        );
            always_comb begin
                descending_x = descending_value;
                if (c)
                    descending_x[2] = b;
            end
            always_comb begin
                ascending_x = ascending_value;
                if (c)
                    ascending_x[1] = b;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("declared_packed_comb_coordinates.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let c = sim.signal("c");
    let b = sim.signal("b");
    let descending_value = sim.signal("descending_value");
    let ascending_value = sim.signal("ascending_value");
    let descending_x = sim.signal("descending_x");
    let ascending_x = sim.signal("ascending_x");
    sim.modify(|io| {
        io.set(c, 0u8);
        io.set(b, 0u8);
        io.set(descending_value, 0b0010u8);
        io.set(ascending_value, 0b1000u8);
    })
    .unwrap();
    assert_eq!(sim.get(descending_x), 0b0010u8.into());
    assert_eq!(sim.get(ascending_x), 0b1000u8.into());
    sim.modify(|io| io.set(c, 1u8)).unwrap();
    assert_eq!(sim.get(descending_x), 0u8.into());
    assert_eq!(sim.get(ascending_x), 0u8.into());
}

#[test]
fn types_earlier_enum_members_in_later_initializers() {
    let source = r#"
        module Top(output logic y);
            typedef enum logic signed [1:0] {
                A = 2'b10,
                B = (A < 0)
            } E;
            assign y = B;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("typed_enum_member_initializer.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn converts_two_state_blocking_writes_before_intervening_reads() {
    let source = r#"
        module Top(input logic c, output logic y);
            bit x;
            always_comb begin
                x = 1'bx;
                y = x;
                if (c)
                    x = 1'b1;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("two_state_intervening_comb_read.sv"))],
        "Top",
    )
    .four_state(true)
    .build_cranelift()
    .unwrap();
    let c = sim.signal("c");
    let y = sim.signal("y");
    sim.modify(|io| io.set(c, 0u8)).unwrap();
    assert_eq!(
        sim.get_four_state(y),
        (BigUint::from(0u8), BigUint::from(0u8))
    );
    sim.modify(|io| io.set(c, 1u8)).unwrap();
    assert_eq!(
        sim.get_four_state(y),
        (BigUint::from(0u8), BigUint::from(0u8))
    );
}

#[test]
fn accepts_constant_true_always_comb_guards() {
    let source = r#"
        module Top(input logic a, output logic x);
            always_comb
                if (1'b1)
                    x = a;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("constant_true_comb_guard.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let a = sim.signal("a");
    let x = sim.signal("x");
    sim.modify(|io| io.set(a, 0u8)).unwrap();
    assert_eq!(sim.get(x), 0u8.into());
    sim.modify(|io| io.set(a, 1u8)).unwrap();
    assert_eq!(sim.get(x), 1u8.into());
}

#[test]
fn recognizes_constant_true_nested_comb_guards_as_definite() {
    let source = r#"
        module Top(input logic c, a, b, output logic y);
            always_comb begin
                if (c) begin
                    if (1'b1)
                        y = a;
                end else begin
                    y = b;
                end
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("constant_true_nested_comb_guard.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let c = sim.signal("c");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");
    sim.modify(|io| {
        io.set(c, false);
        io.set(a, false);
        io.set(b, true);
    })
    .unwrap();
    assert_eq!(sim.get(y), true.into());
    sim.modify(|io| {
        io.set(c, true);
        io.set(a, false);
    })
    .unwrap();
    assert_eq!(sim.get(y), false.into());
}

#[test]
fn treats_constant_unknown_nested_comb_guards_as_false() {
    let source = r#"
        module Top(input logic c, a, b, output logic y);
            always_comb begin
                if (c) begin
                    if (1'bx)
                        ;
                    else
                        y = a;
                end else begin
                    y = b;
                end
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("constant_unknown_nested_comb_guard.sv"))],
        "Top",
    )
    .four_state(true)
    .build_cranelift()
    .unwrap();
    let c = sim.signal("c");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");
    sim.modify(|io| {
        io.set(c, false);
        io.set(a, false);
        io.set(b, true);
    })
    .unwrap();
    assert_eq!(sim.get(y), true.into());
    sim.modify(|io| {
        io.set(c, true);
        io.set(a, false);
    })
    .unwrap();
    assert_eq!(sim.get(y), false.into());
}

#[test]
fn recognizes_complementary_guarded_writes_as_definite() {
    let source = r#"
        module Top(
            input logic c, a, b, e,
            input bit d,
            output logic x
        );
            always_comb begin
                if (c)
                    x = a;
                else begin
                    if (d)
                        x = b;
                    if (!d)
                        x = e;
                end
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("complementary_guarded_comb_writes.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let c = sim.signal("c");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let d = sim.signal("d");
    let e = sim.signal("e");
    let x = sim.signal("x");

    sim.modify(|io| {
        io.set(c, true);
        io.set(a, true);
        io.set(b, false);
        io.set(d, false);
        io.set(e, false);
    })
    .unwrap();
    assert_eq!(sim.get(x), true.into());

    sim.modify(|io| {
        io.set(c, false);
        io.set(d, true);
    })
    .unwrap();
    assert_eq!(sim.get(x), false.into());

    sim.modify(|io| {
        io.set(d, false);
        io.set(e, true);
    })
    .unwrap();
    assert_eq!(sim.get(x), true.into());
}

#[test]
fn does_not_combine_guards_across_condition_writes() {
    let error = cranelift_build_error(
        r#"
        module Top(input bit s, input logic a, b, output logic x);
            bit d;
            always_comb begin
                d = s;
                if (d)
                    x = a;
                d = !d;
                if (!d)
                    x = b;
            end
        endmodule
        "#,
    );
    assert!(
        error.contains("latch inference inside always_comb"),
        "unexpected error: {error}"
    );
}

#[test]
fn registers_enum_types_with_aliased_bases() {
    let source = r#"
        module Top(output logic [1:0] y);
            typedef logic [1:0] base_t;
            typedef enum base_t { A = 2'd2 } E;
            E state;
            always_comb state = A;
            assign y = state;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("aliased_enum_base_type.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 2u8.into());
}

#[test]
fn collects_enum_constants_from_parameterized_aliased_bases() {
    let source = r#"
        module Top #(parameter W = 2) (output logic [1:0] y);
            typedef logic [W'(1):0] B;
            typedef enum B { A = 2'b10 } E;
            assign y = A;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("parameterized_aliased_enum_base.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 2u8.into());
}

#[test]
fn collects_enum_constants_from_parameterized_direct_bases() {
    let source = r#"
        module Top #(parameter W = 2) (output logic [3:0] y);
            typedef enum logic [W'(2'd3):0] { A = 4'b1010 } E;
            assign y = A;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("parameterized_direct_enum_base.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0xau8.into());
}

#[test]
fn preserves_masks_in_compound_constant_cast_operands() {
    let source = r#"
        module Top(output logic [3:0] y);
            localparam logic [3:0] Q = 4'(2'bx1 | 2'b00);
            assign y = Q;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("compound_masked_constant_cast.sv"))],
        "Top",
    )
    .four_state(true)
    .build_cranelift()
    .unwrap();
    assert_eq!(
        sim.get_four_state(sim.signal("y")),
        (BigUint::from(0b0011u8), BigUint::from(0b0010u8))
    );
}

#[test]
fn preserves_body_parameter_overrides_during_enum_collection() {
    let source = r#"
        module Child(output logic y);
            parameter P = 0;
            typedef enum logic { A = P } E;
            assign y = A;
        endmodule
        module Top(output logic y);
            Child #(.P(1)) child(.y(y));
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("body_parameter_override_enum.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), true.into());
}

#[test]
fn coerces_selected_writes_before_whole_vector_normalization() {
    let source = r#"
        module Top(input logic c, output logic [7:0] x);
            always_comb begin
                x = '0;
                x[3:1] = 1'sb1;
                if (c)
                    x = 8'b0;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(
            source,
            Path::new("selected_write_before_whole_normalization.sv"),
        )],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let c = sim.signal("c");
    let x = sim.signal("x");
    sim.modify(|io| io.set(c, 0u8)).unwrap();
    assert_eq!(sim.get(x), 0x0eu8.into());
    sim.modify(|io| io.set(c, 1u8)).unwrap();
    assert_eq!(sim.get(x), 0u8.into());
}

#[test]
fn preserves_four_state_masks_through_constant_casts() {
    let source = r#"
        module Top(output logic y);
            typedef logic [1:0] two_t;
            localparam logic [1:0] PX = two_t'(1'bx);
            localparam logic [1:0] PZ = two_t'(1'bz);
            assign y = (PX === 2'b0x) && (PZ === 2'b0z);
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("four_state_constant_cast.sv"))],
        "Top",
    )
    .four_state(true)
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn expands_unbased_fill_literals_to_constant_cast_widths() {
    let source = r#"
        module Top(output logic y);
            typedef logic [63:0] wide_t;
            localparam logic [63:0] P1 = wide_t'('1);
            localparam logic [63:0] PX = wide_t'('x);
            localparam logic [63:0] PZ = wide_t'('z);
            assign y = (P1 === 64'hffffffffffffffff)
                    && (PX === 64'hxxxxxxxxxxxxxxxx)
                    && (PZ === 64'hzzzzzzzzzzzzzzzz);
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("unbased_fill_constant_cast.sv"))],
        "Top",
    )
    .four_state(true)
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn accepts_selected_writes_killed_by_later_whole_writes() {
    let source = r#"
        module Top(input logic c, a, output logic [1:0] x);
            always_comb begin
                if (c)
                    x[0] = a;
                x = '0;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("killed_selected_comb_write.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let c = sim.signal("c");
    let a = sim.signal("a");
    let x = sim.signal("x");
    sim.modify(|io| {
        io.set(c, 0u8);
        io.set(a, 1u8);
    })
    .unwrap();
    assert_eq!(sim.get(x), 0u8.into());
    sim.modify(|io| io.set(c, 1u8)).unwrap();
    assert_eq!(sim.get(x), 0u8.into());
}

#[test]
fn recognizes_complete_cases_over_two_state_selectors() {
    let source = r#"
        module Top(input bit s, output logic y);
            always_comb begin
                case (s)
                    1'b0: y = 1'b0;
                    1'b1: y = 1'b1;
                endcase
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("complete_two_state_case.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let s = sim.signal("s");
    let y = sim.signal("y");
    sim.modify(|io| io.set(s, 0u8)).unwrap();
    assert_eq!(sim.get(y), 0u8.into());
    sim.modify(|io| io.set(s, 1u8)).unwrap();
    assert_eq!(sim.get(y), 1u8.into());
}

#[test]
fn recognizes_complete_cases_over_constant_selectors() {
    let source = r#"
        module Top(input logic a, output logic y);
            localparam logic P = 1'b0;
            always_comb begin
                case (P)
                    1'b0: y = a;
                endcase
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("complete_constant_case.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let a = sim.signal("a");
    let y = sim.signal("y");
    sim.modify(|io| io.set(a, true)).unwrap();
    assert_eq!(sim.get(y), true.into());
    sim.modify(|io| io.set(a, false)).unwrap();
    assert_eq!(sim.get(y), false.into());
}

#[test]
fn recognizes_identical_two_state_conditional_case_arms() {
    let source = r#"
        module Top(input logic c, a, output logic y);
            always_comb begin
                case (c ? 1'b0 : 1'b0)
                    1'b0: y = a;
                endcase
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("identical_conditional_case_arms.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let c = sim.signal("c");
    let a = sim.signal("a");
    let y = sim.signal("y");
    sim.modify(|io| {
        io.set(c, true);
        io.set(a, true);
    })
    .unwrap();
    assert_eq!(sim.get(y), true.into());
}

#[test]
fn recognizes_complete_cases_over_two_state_function_results() {
    let source = r#"
        module Top(input bit s, input logic a, b, output logic y);
            function automatic bit select();
                return s;
            endfunction
            always_comb begin
                case (select())
                    1'b0: y = a;
                    1'b1: y = b;
                endcase
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("complete_two_state_function_case.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let s = sim.signal("s");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");
    sim.modify(|io| {
        io.set(s, false);
        io.set(a, true);
        io.set(b, false);
    })
    .unwrap();
    assert_eq!(sim.get(y), true.into());
    sim.modify(|io| io.set(s, true)).unwrap();
    assert_eq!(sim.get(y), false.into());
}

#[test]
fn skips_unreachable_duplicate_items_in_complete_two_state_cases() {
    let source = r#"
        module Top(input bit s, input logic a, b, output logic y);
            always_comb begin
                case (s)
                    1'b0: y = a;
                    1'b1: y = b;
                    1'b0: ;
                endcase
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("unreachable_duplicate_case_item.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let s = sim.signal("s");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");
    sim.modify(|io| {
        io.set(s, false);
        io.set(a, true);
        io.set(b, false);
    })
    .unwrap();
    assert_eq!(sim.get(y), true.into());
    sim.modify(|io| io.set(s, true)).unwrap();
    assert_eq!(sim.get(y), false.into());
}

#[test]
fn regroups_comb_targets_after_dynamic_index_substitution() {
    let error = cranelift_build_error(
        r#"
        module Top(
            input logic c, a, b,
            output logic [1:0] x
        );
            logic i;
            always_comb begin
                if (c) begin
                    i = 1'b0;
                    x[i] = a;
                end else begin
                    i = 1'b1;
                    x[i] = b;
                end
            end
        endmodule
        "#,
    );
    assert!(
        error.contains("latch inference inside always_comb"),
        "unexpected error: {error}"
    );
}

#[test]
fn recognizes_complete_cases_over_two_state_expressions() {
    let source = r#"
        module Top(input bit a, b, output logic y);
            always_comb begin
                case ({a, b})
                    2'b00: y = 1'b0;
                    2'b01: y = 1'b1;
                    2'b10: y = 1'b1;
                    2'b11: y = 1'b0;
                endcase
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("complete_two_state_expression_case.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");
    sim.modify(|io| {
        io.set(a, 0u8);
        io.set(b, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(y), 0u8.into());
    sim.modify(|io| io.set(b, 1u8)).unwrap();
    assert_eq!(sim.get(y), 1u8.into());
    sim.modify(|io| io.set(a, 1u8)).unwrap();
    assert_eq!(sim.get(y), 0u8.into());
}

#[test]
fn respects_signed_case_item_sizing_when_checking_coverage() {
    let error = cranelift_build_error(
        r#"
        module Top(input bit signed [1:0] s, output logic y);
            always_comb begin
                case (s)
                    0: y = 1'b0;
                    1: y = 1'b1;
                    2: y = 1'b0;
                    3: y = 1'b1;
                endcase
            end
        endmodule
        "#,
    );
    assert!(
        error.contains("latch inference inside always_comb"),
        "unexpected error: {error}"
    );
}

#[test]
fn recognizes_complete_signed_cases_with_sized_labels() {
    let source = r#"
        module Top(input bit signed [1:0] s, output logic y);
            always_comb begin
                case (s)
                    2'sb00: y = 1'b0;
                    2'sb01: y = 1'b1;
                    2'sb10: y = 1'b0;
                    2'sb11: y = 1'b1;
                endcase
            end
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("complete_signed_case.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    let s = sim.signal("s");
    let y = sim.signal("y");
    sim.modify(|io| io.set(s, 2u8)).unwrap();
    assert_eq!(sim.get(y), 0u8.into());
    sim.modify(|io| io.set(s, 3u8)).unwrap();
    assert_eq!(sim.get(y), 1u8.into());
}

#[test]
fn recognizes_nested_complete_cases_as_definite_assignments() {
    let source = r#"
        module Top(input logic c, input bit s, output logic x);
            always_comb begin
                if (c)
                    case (s)
                        1'b0: x = 1'b0;
                        1'b1: x = 1'b1;
                    endcase
                else
                    x = 1'b0;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("nested_complete_two_state_case.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let c = sim.signal("c");
    let s = sim.signal("s");
    let x = sim.signal("x");
    sim.modify(|io| {
        io.set(c, 0u8);
        io.set(s, 1u8);
    })
    .unwrap();
    assert_eq!(sim.get(x), 0u8.into());
    sim.modify(|io| io.set(c, 1u8)).unwrap();
    assert_eq!(sim.get(x), 1u8.into());
}

#[test]
fn excludes_unreachable_case_arms_from_definite_assignments() {
    let source = r#"
        module Top(
            input logic c,
            input bit s,
            input logic a, b, d,
            output logic y
        );
            always_comb begin
                if (c)
                    case (s)
                        1'b0: y = a;
                        1'b1: y = b;
                        2'b10: ;
                        default: ;
                    endcase
                else
                    y = d;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(
            source,
            Path::new("nested_complete_case_unreachable_arms.sv"),
        )],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let c = sim.signal("c");
    let s = sim.signal("s");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let d = sim.signal("d");
    let y = sim.signal("y");
    sim.modify(|io| {
        io.set(c, true);
        io.set(s, false);
        io.set(a, true);
        io.set(b, false);
        io.set(d, false);
    })
    .unwrap();
    assert_eq!(sim.get(y), true.into());
    sim.modify(|io| io.set(s, true)).unwrap();
    assert_eq!(sim.get(y), false.into());
    sim.modify(|io| {
        io.set(c, false);
        io.set(d, true);
    })
    .unwrap();
    assert_eq!(sim.get(y), true.into());
}

#[test]
fn registers_enum_types_with_default_bases() {
    let source = r#"
        module Top(output logic [31:0] y);
            typedef enum { Idle = 0, Run = 1 } State;
            State state;
            always_comb state = Run;
            assign y = state;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("default_enum_base_type.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn substitutes_enum_members_after_expanding_instance_connection_functions() {
    let source = r#"
        module Child(input logic [1:0] x, output logic [1:0] y);
            assign y = x;
        endmodule

        module Top(output logic [1:0] y);
            typedef enum logic [1:0] { A = 2'b10 } E;
            function automatic logic [1:0] f();
                return A;
            endfunction
            Child child(.x(f()), .y(y));
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("enum_function_instance_connection.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 2u8.into());
}

#[test]
fn rejects_ranged_enum_members_instead_of_registering_the_base_name() {
    let error = cranelift_build_error(
        r#"
        module Top(output logic [31:0] y);
            typedef enum int { S[2] = 4 } E;
            assign y = S0;
        endmodule
        "#,
    );
    assert!(
        error.contains("ranged enum member"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_enum_initializers_that_do_not_fit_the_base_type() {
    let error = cranelift_build_error(
        r#"
        module Top(output logic [1:0] y);
            typedef enum logic [1:0] { A = 3'd4 } E;
            assign y = A;
        endmodule
        "#,
    );
    assert!(
        error.contains("enum member `A` value does not fit its base type"),
        "unexpected error: {error}"
    );
}

#[test]
fn preserves_masked_parameter_guards_before_latch_detection() {
    let source = r#"
        module Top(input logic a, output logic y);
            localparam logic [1:0] S = 2'bx1;
            always_comb begin
                case (S)
                    2'bx1: y = a;
                endcase
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("masked_parameter_case_guard.sv"))],
        "Top",
    )
    .four_state(true)
    .build_cranelift()
    .unwrap();
    let a = sim.signal("a");
    let y = sim.signal("y");
    sim.modify(|io| io.set(a, 0u8)).unwrap();
    assert_eq!(sim.get(y), 0u8.into());
    sim.modify(|io| io.set(a, 1u8)).unwrap();
    assert_eq!(sim.get(y), 1u8.into());
}

#[test]
fn coerces_whole_unpacked_array_writes_to_the_flattened_width() {
    // The element types are equivalent, as an unpacked array assignment
    // requires (IEEE 1800-2023 6.22.2, 7.6); signed elements must not
    // extend into the neighboring element.
    let source = r#"
        module Top(
            input logic c,
            input logic signed [7:0] a[2], b[2],
            output logic signed [7:0] x[2]
        );
            always_comb begin
                if (c)
                    x = a;
                else
                    x = b;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("whole_unpacked_array_comb_write.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let c = sim.signal("c");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let x = sim.signal("x");
    sim.modify(|io| {
        io.set(c, 0u8);
        io.set(a, 0x2211u16);
        io.set(b, 0x4433u16);
    })
    .unwrap();
    assert_eq!(sim.get(x), 0x4433u16.into());
    sim.modify(|io| io.set(c, 1u8)).unwrap();
    assert_eq!(sim.get(x), 0x2211u16.into());
}

#[test]
fn normalizes_element_writes_before_conditional_whole_array_writes() {
    let source = r#"
        module Top(
            input logic c,
            input logic [7:0] a, b,
            input logic [7:0] d[2],
            output logic [7:0] x[2]
        );
            always_comb begin
                x[0] = a;
                x[1] = b;
                if (c)
                    x = d;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("mixed_unpacked_array_comb_writes.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let c = sim.signal("c");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let d = sim.signal("d");
    let x = sim.signal("x");
    sim.modify(|io| {
        io.set(c, 0u8);
        io.set(a, 0x11u8);
        io.set(b, 0x22u8);
        io.set(d, 0x4433u16);
    })
    .unwrap();
    assert_eq!(sim.get(x), 0x2211u16.into());
    sim.modify(|io| io.set(c, 1u8)).unwrap();
    assert_eq!(sim.get(x), 0x4433u16.into());
}

#[test]
fn merges_exhaustive_writes_across_different_slice_partitions() {
    let source = r#"
        module Top(
            input logic c,
            input logic [1:0] a,
            input logic b, d,
            output logic [1:0] x
        );
            always_comb begin
                if (c)
                    x[1:0] = a;
                else begin
                    x[1] = b;
                    x[0] = d;
                end
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("exhaustive_slice_partitions.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let c = sim.signal("c");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let d = sim.signal("d");
    let x = sim.signal("x");
    sim.modify(|io| {
        io.set(c, 0u8);
        io.set(a, 0u8);
        io.set(b, 1u8);
        io.set(d, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(x), 2u8.into());
    sim.modify(|io| {
        io.set(c, 1u8);
        io.set(a, 1u8);
    })
    .unwrap();
    assert_eq!(sim.get(x), 1u8.into());
}

#[test]
fn merges_exhaustive_slice_partitions_within_a_wider_vector() {
    let source = r#"
        module Top(
            input logic c,
            input logic [1:0] a,
            input logic b, d,
            output logic [3:0] x
        );
            always_comb begin
                if (c)
                    x[1:0] = a;
                else begin
                    x[1] = b;
                    x[0] = d;
                end
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("partial_exhaustive_slice_partitions.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let c = sim.signal("c");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let d = sim.signal("d");
    let x = sim.signal("x");
    sim.modify(|io| {
        io.set(c, false);
        io.set(a, 0u8);
        io.set(b, true);
        io.set(d, false);
    })
    .unwrap();
    assert_eq!(sim.get(x), 2u8.into());
    sim.modify(|io| {
        io.set(c, true);
        io.set(a, 1u8);
    })
    .unwrap();
    assert_eq!(sim.get(x), 1u8.into());
}

#[test]
fn rejects_incomplete_slice_partitions_within_a_wider_vector() {
    let error = cranelift_build_error(
        r#"
        module Top(input logic c, input logic [1:0] a, input logic b, output logic [3:0] x);
            always_comb begin
                if (c)
                    x[1:0] = a;
                else
                    x[0] = b;
            end
        endmodule
        "#,
    );
    assert!(
        error.contains("latch inference inside always_comb"),
        "unexpected error: {error}"
    );
}

#[test]
fn does_not_treat_wildcard_equality_as_inherently_two_state() {
    let error = four_state_cranelift_build_error(
        r#"
        module Top(input logic a, output logic y);
            always_comb begin
                case (a ==? 1'b0)
                    1'b0: y = 1'b0;
                    1'b1: y = 1'b1;
                endcase
            end
        endmodule
        "#,
    );
    assert!(
        error.contains("latch inference inside always_comb"),
        "unexpected error: {error}"
    );
}

#[test]
fn reads_previous_values_before_a_later_definite_fallback_write() {
    let source = r#"
        module Top(
            input logic c, d, a, b, e,
            output logic x, y
        );
            always_comb begin
                y = 1'b0;
                if (c)
                    x = a;
                else begin
                    if (d)
                        x = b;
                    y = x;
                    x = e;
                end
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("fallback_read.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let [c, d, a, b, e, x, y] = ["c", "d", "a", "b", "e", "x", "y"].map(|name| sim.signal(name));
    sim.modify(|io| {
        io.set(c, 1u8);
        io.set(a, 1u8);
    })
    .unwrap();
    assert_eq!(sim.get(x), 1u8.into());
    assert_eq!(sim.get(y), 0u8.into());
    // `y = x` reads the value `x` held before this evaluation.
    sim.modify(|io| {
        io.set(c, 0u8);
        io.set(d, 0u8);
        io.set(e, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(x), 0u8.into());
    assert_eq!(sim.get(y), 1u8.into());
    sim.modify(|io| {
        io.set(d, 1u8);
        io.set(b, 1u8);
    })
    .unwrap();
    assert_eq!(sim.get(y), 1u8.into());
    assert_eq!(sim.get(x), 0u8.into());
}

#[test]
fn interprets_signed_literals_before_constant_unary_operations() {
    let source = r#"
        module Top #(
            parameter NEGATED = -8'shff,
            parameter FLAG = (NEGATED == 1)
        ) (output logic y);
            assign y = FLAG;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("signed_unary.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn resolves_parent_parameters_in_input_port_connections() {
    let source = r#"
        module Child(input logic [15:0] value, output logic [15:0] y);
            assign y = value;
        endmodule
        module Top #(parameter WIDTH_VALUE = 9) (output logic [15:0] y);
            Child child(.value(WIDTH_VALUE), .y(y));
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("parent_constant.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 9u16.into());
}

#[test]
fn preserves_inferred_parameter_types_in_hierarchy_connections() {
    let source = r#"
        module Child(input logic signed [63:0] value, output logic [63:0] y);
            assign y = value;
        endmodule
        module Top(output logic [63:0] y);
            parameter P = 8'shff;
            Child child(.value(P), .y(y));
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("inferred_parent_constant.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), u64::MAX.into());
}

#[test]
fn applies_parent_parameter_types_to_child_overrides() {
    let source = r#"
        module Child #(parameter SELECT = 1) (output logic y);
            if (SELECT) assign y = 1'b1;
            else assign y = 1'b0;
        endmodule
        module Top #(
            parameter logic signed [7:0] BASE = -1
        ) (output logic y);
            Child #(.SELECT(BASE < 8'h01)) child(.y(y));
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("typed_parameter_override.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0u8.into());
}

#[test]
fn preserves_parameter_types_in_regular_rhs_lowering() {
    let source = r#"
        module Top(
            input logic clk,
            output logic [15:0] comb_y,
            output logic [15:0] ff_y
        );
            parameter logic signed [7:0] P = -1;
            assign comb_y = P;
            always_ff @(posedge clk) ff_y <= P;
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("typed_parameter_rhs.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    assert_eq!(sim.get(sim.signal("comb_y")), u16::MAX.into());
    sim.tick(sim.event("clk")).unwrap();
    assert_eq!(sim.get(sim.signal("ff_y")), u16::MAX.into());
}

#[test]
fn accepts_reachable_parameter_specialized_net_drivers() {
    let source = r#"
        module Driver(output wire y); assign y = 1'b1; endmodule
        module Child #(parameter ENABLE = 0) (output logic y);
            wire w;
            if (ENABLE) Driver driver(.y(w));
            assign y = w;
        endmodule
        module Top(output logic y);
            Child #(.ENABLE(1)) child(.y(y));
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("specialized_net_driver.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn counts_only_active_conditional_generate_instance_drivers() {
    let source = r#"
        module DriveZero(output wire y); assign y = 1'b0; endmodule
        module DriveOne(output wire y); assign y = 1'b1; endmodule
        module Top #(parameter SELECT = 1) (output wire y);
            if (SELECT) DriveOne selected(.y(y));
            else DriveZero unselected(.y(y));
        endmodule
    "#;
    let mut selected = Simulator::from_sv_sources(
        vec![(source, Path::new("conditional_instance_drivers.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(selected.get(selected.signal("y")), 1u8.into());

    let mut unselected = Simulator::from_sv_sources(
        vec![(source, Path::new("conditional_instance_drivers.sv"))],
        "Top",
    )
    .param("SELECT", 0)
    .build_cranelift()
    .unwrap();
    assert_eq!(unselected.get(unselected.signal("y")), 0u8.into());
}

#[test]
fn handles_fill_literals_ascending_ranges_atom_types_and_unary_constants() {
    let source = r#"
        module Top(input logic [0:7] ascending, input int a, b,
                   output logic first, output logic last,
                   output logic [63:0] fill,
                   output logic [31:0] sum,
                   output logic folded);
            assign first = ascending[0];
            assign last = ascending[7];
            assign fill = '1;
            assign sum = a + b;
            if ((~8'h00) == 8'hff && (&8'hff)) begin
                assign folded = 1'b1;
            end else begin
                assign folded = 1'b0;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("types.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let ascending = sim.signal("ascending");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let first = sim.signal("first");
    let last = sim.signal("last");
    let fill = sim.signal("fill");
    let sum = sim.signal("sum");
    let folded = sim.signal("folded");
    sim.modify(|io| {
        io.set(ascending, 0x80u8);
        io.set(a, 40u32);
        io.set(b, 2u32);
    })
    .unwrap();
    assert_eq!(sim.get(first), 1u8.into());
    assert_eq!(sim.get(last), 0u8.into());
    assert_eq!(sim.get(fill), u64::MAX.into());
    assert_eq!(sim.get(sum), 42u32.into());
    assert_eq!(sim.get(folded), 1u8.into());
}

#[test]
fn treats_unknown_procedural_conditions_as_false() {
    let source = r#"
        module Top(input bit clk, input logic clear, input logic en, output logic q);
            always_ff @(posedge clk) begin
                if (clear) q <= 1'b0;
                else if (en) q <= 1'b1;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("condition.sv"))], "Top")
        .four_state(true)
        .build_cranelift()
        .unwrap();
    let clk = sim.event("clk");
    let clear = sim.signal("clear");
    let en = sim.signal("en");
    let q = sim.signal("q");
    sim.modify(|io| io.set(clear, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| {
        io.set(clear, 0u8);
        io.set_four_state(en, BigUint::from(1u8), BigUint::from(1u8));
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.get_four_state(q),
        (BigUint::from(0u8), BigUint::from(0u8))
    );
}

#[test]
fn takes_always_comb_else_branch_for_unknown_predicates() {
    let source = r#"
        module Top(input logic sel, output logic y);
            always_comb begin
                if (sel) y = 1'b1;
                else y = 1'b0;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("unknown_always_comb_else.sv"))],
        "Top",
    )
    .four_state(true)
    .build_cranelift()
    .unwrap();
    let sel = sim.signal("sel");
    let y = sim.signal("y");
    sim.modify(|io| io.set_four_state(sel, BigUint::from(1u8), BigUint::from(1u8)))
        .unwrap();
    assert_eq!(
        sim.get_four_state(y),
        (BigUint::from(0u8), BigUint::from(0u8))
    );
}

#[test]
fn takes_always_ff_else_branch_for_unknown_predicates() {
    let source = r#"
        module Top(input bit clk, input logic sel, output logic q);
            always_ff @(posedge clk) begin
                if (sel) q <= 1'b1;
                else q <= 1'b0;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("unknown_else.sv"))], "Top")
        .four_state(true)
        .build_cranelift()
        .unwrap();
    let clk = sim.event("clk");
    let sel = sim.signal("sel");
    let q = sim.signal("q");
    sim.modify(|io| io.set(sel, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 1u8.into());
    sim.modify(|io| io.set_four_state(sel, BigUint::from(1u8), BigUint::from(1u8)))
        .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.get_four_state(q),
        (BigUint::from(0u8), BigUint::from(0u8))
    );
}

#[test]
fn discovers_implicit_output_nets_before_instance_glue() {
    let source = r#"
        module Sink(input logic a, output logic y); assign y = a; endmodule
        module Source(output logic y); assign y = 1'b1; endmodule
        module Top(output logic out);
            Sink sink(.a(w), .y(out));
            Source source(.y(w));
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("implicit_order.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    assert_eq!(sim.get(sim.signal("out")), 1u8.into());
}

#[test]
fn lowers_local_comb_logic_after_discovering_implicit_child_outputs() {
    let source = r#"
        module Source(output logic y); assign y = 1'b1; endmodule
        module Top(output logic out);
            Source source(.y(w));
            assign out = w;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("implicit_read.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    assert_eq!(sim.get(sim.signal("out")), 1u8.into());
}

#[test]
fn lowers_mux_expressions_in_child_input_connections() {
    let source = r#"
        module Child(input logic a, output logic y); assign y = a; endmodule
        module Top(input logic sel, a, b, output logic y);
            Child child(.a(sel ? a : b), .y(y));
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("glue_mux.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let sel = sim.signal("sel");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");
    sim.modify(|io| {
        io.set(sel, 1u8);
        io.set(a, 1u8);
        io.set(b, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(y), 1u8.into());
    sim.modify(|io| io.set(sel, 0u8)).unwrap();
    assert_eq!(sim.get(y), 0u8.into());
}

#[test]
fn expands_function_calls_in_child_input_connections() {
    let source = r#"
        module Child(input logic a, output logic y); assign y = a; endmodule
        module Top(input logic x, output logic y);
            function automatic logic invert(input logic value);
                return ~value;
            endfunction
            Child child(.a(invert(x)), .y(y));
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("glue_call.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let x = sim.signal("x");
    let y = sim.signal("y");
    sim.modify(|io| io.set(x, 1u8)).unwrap();
    assert_eq!(sim.get(y), 0u8.into());
    sim.modify(|io| io.set(x, 0u8)).unwrap();
    assert_eq!(sim.get(y), 1u8.into());
}

#[test]
fn rejects_local_drivers_of_implicit_child_output_nets() {
    let source = r#"
        module Source(output logic y); assign y = 1'b1; endmodule
        module Top(input logic a, output logic out);
            Source source(.y(w));
            assign w = a;
            assign out = w;
        endmodule
    "#;
    let error = cranelift_build_error(source);
    assert!(error.contains("multiple net drivers for `w`"), "{error}");
}

#[test]
fn preserves_typed_parameter_override_literals_during_specialization() {
    let source = r#"
        module Child #(parameter P = 0) (output logic y); assign y = &P; endmodule
        module Top(output logic y); Child #(.P(4'hf)) child(.y(y)); endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("typed_override.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn preserves_parent_parameter_types_in_child_overrides() {
    let source = r#"
        module Child #(parameter P = 0) (output logic y);
            assign y = (P == 4'hf);
        endmodule
        module Top #(
            parameter logic signed [3:0] P = -1
        ) (output logic y);
            Child #(.P(P)) child(.y(y));
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("typed_parent_override.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn ignores_unreachable_non_ansi_modules() {
    let source = r#"
        module Top(output logic y); assign y = 1'b1; endmodule
        module Legacy(y); output y; assign y = 1'b0; endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("unreachable_nonansi.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn propagates_function_assignments_out_of_nested_blocks() {
    let source = r#"
        module Top(input logic a, output logic y);
            function automatic logic invert(input logic value);
                begin value = ~value; end
                return value;
            endfunction
            assign y = invert(a);
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("nested_function.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    let a = sim.signal("a");
    let y = sim.signal("y");
    sim.modify(|io| io.set(a, 1u8)).unwrap();
    assert_eq!(sim.get(y), 0u8.into());
    sim.modify(|io| io.set(a, 0u8)).unwrap();
    assert_eq!(sim.get(y), 1u8.into());
}

#[test]
fn sizes_unbased_literals_as_one_bit_inside_concatenations() {
    let source = r#"
        module Top(output logic [1:0] y); assign y = {1'b0, '1}; endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("concat_fill.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn lowers_complemented_reductions_in_always_ff() {
    let source = r#"
        module Top(input logic clk, input logic [1:0] d, output logic q);
            always_ff @(posedge clk) q <= ~&d;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("reduction_ff.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let q = sim.signal("q");
    sim.modify(|io| io.set(d, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 1u8.into());
    sim.modify(|io| io.set(d, 3u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0u8.into());
}

#[test]
fn converts_unknown_ff_values_when_storing_to_bit() {
    let source = r#"
        module Top(input bit clk, input logic d, output bit q);
            always_ff @(posedge clk) q <= d;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("ff_bit.sv"))], "Top")
        .four_state(true)
        .build_cranelift()
        .unwrap();
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let q = sim.signal("q");
    sim.modify(|io| io.set_four_state(d, BigUint::from(1u8), BigUint::from(1u8)))
        .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0u8.into());
}

#[test]
fn context_sizes_unbased_fill_literals_in_glue_comparisons() {
    let source = r#"
        module Child(input logic value, output logic y); assign y = value; endmodule
        module Top(input logic [3:0] a, output logic y);
            Child child(.value(a == '1), .y(y));
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("glue_fill_comparison.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    let a = sim.signal("a");
    sim.modify(|io| io.set(a, 0x0fu8)).unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
    sim.modify(|io| io.set(a, 0x07u8)).unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0u8.into());
}

#[test]
fn preserves_multidimensional_packed_offsets_on_lvalue_part_selects() {
    let source = r#"
        module Top(input logic [7:0] a, output logic [1:0][7:0] y);
            always_comb begin
                y = '0;
                y[1][7:0] = a;
            end
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("packed_lvalue_prefix.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    let a = sim.signal("a");
    sim.modify(|io| io.set(a, 0xabu8)).unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0xab00u16.into());
}

#[test]
fn applies_generate_localparams_inside_always_ff() {
    let source = r#"
        module Top #(
            parameter P = 0
        ) (input logic clk, output logic q);
            if (1) begin : active
                localparam P = 1;
                always_ff @(posedge clk) q <= P;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("ff_generate_localparam.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    sim.tick(sim.event("clk")).unwrap();
    assert_eq!(sim.get(sim.signal("q")), 1u8.into());
}

#[test]
fn rejects_nonpositive_generate_localparam_array_sizes() {
    let error = cranelift_build_error(
        r#"
        module Top();
            if (1) begin : active
                localparam N = 0;
                logic [7:0] values[N];
            end
        endmodule
        "#,
    );
    assert!(
        error.contains("nonpositive unpacked array dimension"),
        "unexpected error: {error}"
    );
}

#[test]
fn preserves_bit_selects_on_computed_expressions() {
    let source = r#"
        module Top(
            input logic [7:0] a,
            input logic [7:0] b,
            output logic y
        );
            assign y = {a + b}[0];
        endmodule
        "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("computed_expression_bit_select.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");
    sim.modify(|io| {
        io.set(a, 1u8);
        io.set(b, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(y), 1u8.into());
}

#[test]
fn reads_partial_unpacked_array_selections() {
    let source = r#"
        module Top(output logic [23:0] row);
            logic [7:0] values [0:1][0:2];
            assign row = values[1];
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("partial_unpacked_selection.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let values = sim.signal("values");
    sim.modify(|io| io.set_wide(values, BigUint::from(0x060504030201u64)))
        .unwrap();
    assert_eq!(sim.get(sim.signal("row")), 0x060504u32.into());
}

#[test]
fn writes_partial_unpacked_array_selections() {
    let source = r#"
        module Top(
            input logic [23:0] row,
            output logic [7:0] selected[3]
        );
            wire [7:0] values[2][3];
            assign values[0] = row;
            assign selected = values[0];
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("partial_unpacked_lvalue_selection.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let row = sim.signal("row");
    let selected = sim.signal("selected");
    sim.modify(|io| io.set_wide(row, BigUint::from(0x060504u32)))
        .unwrap();
    assert_eq!(sim.get(selected), 0x060504u32.into());
}

#[test]
fn reads_runtime_partial_unpacked_array_selections() {
    let source = r#"
        module Top(
            input logic sel,
            input logic [7:0] values[2][3],
            output logic [23:0] row
        );
            assign row = values[sel];
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("dynamic_partial_unpacked_selection.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let sel = sim.signal("sel");
    let values = sim.signal("values");
    let row = sim.signal("row");
    sim.modify(|io| {
        io.set(sel, 1u8);
        io.set_wide(values, BigUint::from(0x060504030201u64));
    })
    .unwrap();
    assert_eq!(sim.get(row), 0x060504u32.into());
}

#[test]
fn writes_runtime_partial_unpacked_array_selections_in_always_ff() {
    let source = r#"
        module Top(
            input logic clk,
            input logic sel,
            input logic [23:0] row,
            output logic [23:0] selected
        );
            logic [7:0] values[2][3];
            always_ff @(posedge clk) values[sel] <= row;
            assign selected = values[sel];
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("dynamic_partial_unpacked_lvalue.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let sel = sim.signal("sel");
    let row = sim.signal("row");
    let selected = sim.signal("selected");
    sim.modify(|io| {
        io.set(sel, 1u8);
        io.set_wide(row, BigUint::from(0x060504u32));
    })
    .unwrap();
    sim.tick(sim.event("clk")).unwrap();
    assert_eq!(sim.get(selected), 0x060504u32.into());
}

#[test]
fn writes_runtime_partial_unpacked_array_selections_in_always_comb() {
    let source = r#"
        module Top(
            input logic sel,
            input logic [23:0] row,
            output logic [23:0] selected
        );
            logic [7:0] values[2][3];
            always_comb values[sel] = row;
            assign selected = values[sel];
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("dynamic_partial_unpacked_comb_lvalue.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let sel = sim.signal("sel");
    let row = sim.signal("row");
    let selected = sim.signal("selected");
    sim.modify(|io| {
        io.set(sel, 1u8);
        io.set_wide(row, BigUint::from(0x060504u32));
    })
    .unwrap();
    assert_eq!(sim.get(selected), 0x060504u32.into());
}

#[test]
fn reads_unpacked_array_ranges() {
    let source = r#"
        module Top(
            input logic [7:0] source[4],
            output logic [7:0] selected[2]
        );
            assign selected = source[1:0];
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("unpacked_array_range_selection.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let source = sim.signal("source");
    let selected = sim.signal("selected");
    sim.modify(|io| io.set_wide(source, BigUint::from(0x04030201u64)))
        .unwrap();
    assert_eq!(sim.get(selected), 0x0102u16.into());
}

#[test]
fn writes_reversed_unpacked_array_lvalue_ranges() {
    let source = r#"
        module Top(
            input logic [7:0] source[4],
            output logic [7:0] target[2]
        );
            assign target[1:0] = source[0:1];
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("reversed_unpacked_array_lvalue.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let source = sim.signal("source");
    let target = sim.signal("target");
    sim.modify(|io| io.set_wide(source, BigUint::from(0x04030201u64)))
        .unwrap();
    assert_eq!(sim.get(target), 0x0102u16.into());
}

#[test]
fn reads_bit_selects_of_scalar_array_elements() {
    let source = r#"
        module Top(
            input logic values[2],
            output logic selected
        );
            assign selected = values[0][0];
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("scalar_array_bit_selection.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let values = sim.signal("values");
    let selected = sim.signal("selected");
    sim.modify(|io| io.set(values, 1u8)).unwrap();
    assert_eq!(sim.get(selected), 1u8.into());
}

#[test]
fn rejects_constructs_that_are_not_yet_lowered() {
    let cases = [
        (
            "module instance array",
            r#"
            module Child(input logic [3:0] a); endmodule
            module Top(input logic [6:0] a); Child child[1:0](.a(a)); endmodule
        "#,
        ),
        (
            "with a negative index",
            r#"
            module Child(input logic a); endmodule
            module Top(input logic [1:0] a); Child child[0:-1](.a(a)); endmodule
        "#,
        ),
        (
            "index 2 of `values` outside its declared range",
            r#"
            module Top(output logic y);
                logic [7:0] values [0:1][0:2];
                assign y = values[0][3];
            endmodule
        "#,
        ),
        (
            "index 2 of `values` outside its declared range",
            r#"
            module Top(output logic y);
                localparam J = 3;
                logic [7:0] values [0:1][0:2];
                assign y = values[0][J];
            endmodule
        "#,
        ),
        (
            "combinational expression",
            r#"
            module Top(input logic a, output logic y); assign y = unknown(a); endmodule
        "#,
        ),
        (
            "streaming concatenation",
            r#"
            module Child(input logic a); endmodule
            module Top(input logic [3:0] x);
                Child child(.a({<<{x}}));
            endmodule
        "#,
        ),
        (
            "multiple net drivers for `y`",
            r#"
            module Top(output wire y);
                assign y = 1'b0;
                assign y = 1'b1;
            endmodule
        "#,
        ),
        (
            "module-level net alias",
            r#"
            module Top(input wire a, output wire y);
                alias y = a;
            endmodule
        "#,
        ),
        (
            "streaming concatenation",
            r#"
            module Top(input logic clk, input logic [3:0] a, b, d, e, output logic [3:0] q);
                always_ff @(posedge clk) begin
                    if ({<<{a}}) q <= d;
                    else q <= e;
                end
            endmodule
        "#,
        ),
        (
            "streaming concatenation",
            r#"
            module Top(input logic clk, input logic [3:0] a, b, d, output logic [3:0] q);
                always_ff @(posedge clk) begin
                    case ({<<{a}})
                        0: q <= d;
                        default: q <= '0;
                    endcase
                end
            endmodule
        "#,
        ),
        (
            "streaming concatenation",
            r#"
            module Top(input logic clk, input logic [3:0] a, b, output logic [3:0] q);
                always_ff @(posedge clk) q <= {<<{a}};
            endmodule
        "#,
        ),
        (
            "concurrent assertion",
            r#"
            module Top(input logic clk, valid);
                assert property (@(posedge clk) valid);
            endmodule
        "#,
        ),
        (
            "final construct",
            r#"
            module Top(input logic done);
                final assert (done);
            endmodule
        "#,
        ),
        (
            "non-ANSI module port declarations",
            r#"
            module Top(y); output y; assign y = 1'b1; endmodule
        "#,
        ),
        (
            "ref port direction",
            r#"
            module Top(ref logic value); endmodule
        "#,
        ),
        (
            "always and always_latch processes",
            r#"
            module Top(input logic a, output logic y); always_latch y = a; endmodule
        "#,
        ),
        (
            "always_ff event control",
            r#"
            module Top(input logic a, b, d, output logic q);
                always_ff @(posedge a or posedge b) q <= d;
            endmodule
        "#,
        ),
        (
            "always_ff event control",
            r#"
            module Top(input logic clk, sample, output logic q);
                always_ff @(posedge clk or posedge sample) q <= clk;
            endmodule
        "#,
        ),
        (
            "procedural initialization of `value`, which a continuous assignment",
            r#"
            module Top(input logic a, output logic y);
                logic value = 1'b1;
                assign value = a;
                assign y = value;
            endmodule
        "#,
        ),
        (
            "procedural initialization of `value`, which a continuous assignment",
            r#"
            module Child(output logic o); assign o = 1'b0; endmodule
            module Top(output logic y);
                logic value;
                initial value = 1'b1;
                Child child(.o(value));
                assign y = value;
            endmodule
        "#,
        ),
        (
            "initializer of member `value` in the interface array `lanes`",
            r#"
            interface Lane; logic value = 1'b1; endinterface
            module Top(output logic y); Lane lanes [2] (); assign y = lanes[0].value; endmodule
        "#,
        ),
        (
            "non-zero-based multidimensional packed range",
            r#"
            module Top(input logic [2:1][7:0] a, output logic [7:0] y);
                assign y = a[1];
            endmodule
        "#,
        ),
        (
            "unpacked struct, union, or unsupported packed struct member",
            r#"
            module Top(output logic [7:0] y);
                union packed { logic [7:0] a; logic [7:0] b; } value;
                assign y = value;
            endmodule
        "#,
        ),
        (
            "wildcard port connection",
            r#"
            module Child(input logic a, output logic y); assign y = a; endmodule
            module Top(input logic a, output logic y); Child child (.*); endmodule
        "#,
        ),
        (
            "delayed continuous assignment",
            r#"
            module Top(input logic a, output wire y); assign #5 y = a; endmodule
        "#,
        ),
        (
            "mixed clock-edge polarities for one signal",
            r#"
            module Top(input logic clk, a, b, output logic qa, qb);
                always_ff @(posedge clk) qa <= a;
                always_ff @(negedge clk) qb <= b;
            endmodule
        "#,
        ),
        (
            "streaming concatenation",
            r#"
            module Top(input logic [7:0] a, output logic [7:0] y);
                assign y = {<<{a}};
            endmodule
        "#,
        ),
        (
            "streaming concatenation",
            r#"
            module Top(input logic [3:0] a, output logic y);
                function automatic logic choose(input logic [3:0] value);
                    if ({<<{value}}) return 1'b1;
                    else return 1'b0;
                endfunction
                assign y = choose(a);
            endmodule
        "#,
        ),
        (
            "streaming concatenation",
            r#"
            module Top(input logic [3:0] a, output logic [3:0] y);
                function automatic logic [3:0] square(input logic [3:0] value);
                    logic [3:0] tmp;
                    tmp = {<<{value}};
                    return tmp;
                endfunction
                assign y = square(a);
            endmodule
        "#,
        ),
        (
            "streaming concatenation",
            r#"
            module Top(input logic [3:0] a, output logic y);
                function automatic logic choose(input logic [3:0] value);
                    case ({<<{value}})
                        1: return 1'b1;
                        default: return 1'b0;
                    endcase
                endfunction
                assign y = choose(a);
            endmodule
        "#,
        ),
        (
            "streaming concatenation",
            r#"
            module Top(input logic [3:0] a, b, output logic y);
                function automatic logic choose(
                    input logic [3:0] value,
                    input logic [3:0] item
                );
                    case (value)
                        {<<{item}}: return 1'b1;
                        default: return 1'b0;
                    endcase
                endfunction
                assign y = choose(a, b);
            endmodule
        "#,
        ),
        (
            "duplicate parameter override `P`",
            r#"
            module Child #(parameter P = 0) (output logic y); assign y = P; endmodule
            module Top(output logic y); Child #(.P(0), .P(1)) child(.y(y)); endmodule
        "#,
        ),
        (
            "duplicate function declaration `f`",
            r#"
            module Top(output logic y);
                function logic f(); return 1'b0; endfunction
                function logic f(); return 1'b1; endfunction
                assign y = f();
            endmodule
        "#,
        ),
        (
            "duplicate internal signal `w`",
            r#"
            module Top(output logic y);
                logic [7:0] w;
                logic w;
                assign y = w;
            endmodule
        "#,
        ),
        (
            "localparam override `P`",
            r#"
            module Child #(localparam P = 0) (output logic y); assign y = P; endmodule
            module Top(output logic y); Child #(.P(1)) child(.y(y)); endmodule
        "#,
        ),
        (
            "combinational expression assigned to `y`",
            r#"
            module Top(output logic y);
                if (0) begin : inactive
                    localparam P = 1;
                end
                assign y = P;
            endmodule
        "#,
        ),
        (
            "loop-generate unroll limit exceeded",
            r#"
            module Top(input logic [10000:0] a, output logic [10000:0] y);
                for (genvar i = 0; i < 10001; i++) assign y[i] = a[i];
            endmodule
        "#,
        ),
        (
            "nonblocking assignment inside always_comb",
            r#"
            module Top(input logic a, output logic y); always_comb y <= a; endmodule
        "#,
        ),
        (
            "iff-qualified always_ff event",
            r#"
            module Top(input logic clk, enable, d, output logic q);
                always_ff @(posedge clk iff enable) q <= d;
            endmodule
        "#,
        ),
        (
            "reduction operator in parameter expression",
            r#"
            module Top #(parameter logic [3:0] P = 4'hf, parameter FLAG = &P)
                       (output logic y);
                assign y = FLAG;
            endmodule
        "#,
        ),
        (
            "genvar update operator",
            r#"
            module Top(input logic [7:0] a, output logic [7:0] y);
                for (genvar i = 7; i > 0; i &= i - 1) assign y[i] = a[i];
            endmodule
        "#,
        ),
        (
            "gate primitive instantiation",
            r#"
            module Top(input logic a, b, output logic y); and (y, a, b); endmodule
        "#,
        ),
        (
            "undriven net declaration `w`",
            r#"
            module Top(output logic y); wire w; assign y = (w === 1'bz); endmodule
        "#,
        ),
        (
            "non-integer module parameter override `P`",
            r#"
            module Child #(parameter logic P = 1'b0) (output logic y); assign y = P; endmodule
            module Top(output logic y); Child #(.P(1'bx)) child(.y(y)); endmodule
        "#,
        ),
        (
            "unknown or duplicate systemverilog child port connection",
            r#"
            module Child(input logic a, output logic y); assign y = a; endmodule
            module Top(input logic a, output logic y); Child child(.aa(a), .y(y)); endmodule
        "#,
        ),
        (
            "always_ff event expression",
            r#"
            module Top(input logic clk, enable, d, output logic q);
                always_ff @(posedge (clk & enable)) q <= d;
            endmodule
        "#,
        ),
        (
            "streaming concatenation",
            r#"
            module Top(input logic clk, a, b, output logic q);
                always_ff @(posedge clk)
                    case (a)
                        ({<<{b}}): q <= 1'b1;
                        default: q <= 1'b0;
                    endcase
            endmodule
        "#,
        ),
        (
            "signal width overflow",
            r#"
            module Top(output logic y);
                logic [7:0] values [0:9223372036854775807][0:1];
                assign y = values[0][0];
            endmodule
        "#,
        ),
        (
            "index 2 of `values` outside its declared range",
            r#"
            module Top(output logic y);
                logic [7:0] values [0:1][0:2];
                assign y = values[1][3];
            endmodule
        "#,
        ),
        (
            "systemverilog inout port",
            r#"
            module Top(inout wire io); endmodule
        "#,
        ),
        (
            "width-dependent complement in parameter expression",
            r#"
            module Top #(
                parameter logic [7:0] P = 8'hff,
                parameter FLAG = (~P == 0)
            ) (output logic y);
                assign y = FLAG;
            endmodule
        "#,
        ),
        (
            "streaming concatenation",
            r#"
            module Top(input logic [3:0] a, b, output logic [7:0] y);
                always_comb y = {<<{a}};
            endmodule
        "#,
        ),
        (
            "mixed reset-edge polarities for one signal",
            r#"
            module Top(input logic clk1, clk2, rst, d, output logic q1, q2);
                always_ff @(posedge clk1 or posedge rst)
                    if (rst) q1 <= 1'b0; else q1 <= d;
                always_ff @(posedge clk2 or negedge rst)
                    if (!rst) q2 <= 1'b0; else q2 <= d;
            endmodule
        "#,
        ),
        (
            "mixed clock/reset-edge polarities for one signal",
            r#"
            module Top(input logic sig, clk2, rst2, d, output logic q1, q2);
                always_ff @(posedge sig or posedge rst2)
                    if (rst2) q1 <= 1'b0; else q1 <= d;
                always_ff @(posedge clk2 or negedge sig)
                    if (!sig) q2 <= 1'b0; else q2 <= d;
            endmodule
        "#,
        ),
        (
            "mixed clock/reset-edge polarities for one signal",
            r#"
            module Top(input logic sig, clk2, rst2, d, output logic q1, q2);
                always_ff @(posedge sig or posedge rst2)
                    if (rst2) q1 <= 1'b0; else q1 <= d;
                always_ff @(posedge clk2 or posedge sig)
                    if (sig) q2 <= 1'b0; else q2 <= d;
            endmodule
        "#,
        ),
        (
            "unsupported function formal data type",
            r#"
            module Top(input logic [1:0] a, output logic [1:0] y);
                function automatic logic [1:0] shift(input real value);
                    return value << 1;
                endfunction
                assign y = shift(a);
            endmodule
        "#,
        ),
        (
            "void function `f` used as a value",
            r#"
            module Top(output logic y);
                function automatic real f();
                    return 1'b1;
                endfunction
                assign y = f();
            endmodule
        "#,
        ),
        (
            "defparam assignment",
            r#"
            module Child #(parameter W = 1) (output logic [W-1:0] y);
                assign y = '0;
            endmodule
            module Top(output logic y);
                Child child(.y(y));
                defparam child.W = 8;
            endmodule
        "#,
        ),
        (
            "unsupported parameter data type",
            r#"
            module Top #(parameter real P = 1) (output logic y);
                assign y = P;
            endmodule
        "#,
        ),
        (
            "empty parameter override `NO_SUCH`",
            r#"
            module Child #(parameter P = 1) (output logic y);
                assign y = P;
            endmodule
            module Top(output logic y);
                Child #(.NO_SUCH()) child(.y(y));
            endmodule
        "#,
        ),
        (
            "unsupported net data type",
            r#"
            module Top(output logic y);
                wire enum { A, B, C } value;
                assign value = C;
                assign y = value;
            endmodule
        "#,
        ),
        (
            "duplicate declaration of `a` in one scope",
            r#"
            module Top(input logic a, output logic y);
                function automatic logic f(input logic a);
                    logic a;
                    return a;
                endfunction
                assign y = f(a);
            endmodule
        "#,
        ),
        (
            "static variable `saved` of subroutine `saved_value` that keeps its value between calls",
            r#"
            module Top(input logic set, value, output logic y);
                function logic saved_value(input logic do_set, input logic new_value);
                    logic saved;
                    if (do_set) saved = new_value;
                    return saved;
                endfunction
                assign y = saved_value(set, value);
            endmodule
        "#,
        ),
        (
            "implicit net `missing` disabled",
            r#"
            `default_nettype none
            module Child(output logic y); assign y = 1'b1; endmodule
            module Top(output logic y);
                Child child(.y(missing));
                assign y = missing;
            endmodule
        "#,
        ),
        (
            "pull or supply net type",
            r#"
            module Top(output tri1 y); endmodule
        "#,
        ),
        (
            "ANSI port default value",
            r#"
            module Top(input wire a = 1'b1, output wire y); assign y = a; endmodule
        "#,
        ),
        (
            "interconnect net declaration",
            r#"
            module Top(output wire y);
                interconnect link;
                assign y = 1'b0;
            endmodule
        "#,
        ),
    ];

    for (expected, source) in cases {
        let error = cranelift_build_error(source);
        assert!(error.contains(expected), "{expected}: {error}");
    }
}

#[test]
fn rejects_unknown_top_parameter_overrides() {
    let source = r#"
        module Top #(parameter WIDTH = 4) (output logic [WIDTH-1:0] y);
            assign y = '0;
        endmodule
    "#;
    let error = match Simulator::from_sv_sources(vec![(source, Path::new("bad_param.sv"))], "Top")
        .param("WIDHT", 8)
        .build_cranelift()
    {
        Ok(_) => panic!("unknown parameter override unexpectedly compiled"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("unknown top-level parameter override `WIDHT`"));
}

#[test]
fn rejects_cycles_across_the_module_graph() {
    let source = r#"
        module A(); B child(); endmodule
        module B(); A child(); endmodule
    "#;
    let error = match Simulator::from_sv_sources(vec![(source, Path::new("cycle.sv"))], "A")
        .build_cranelift()
    {
        Ok(_) => panic!("recursive hierarchy unexpectedly compiled"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("recursive systemverilog module instantiation"),
        "{error}"
    );
}

#[test]
fn bounds_parameter_specialized_recursive_elaboration() {
    let source = r#"
        module Top #(parameter P = 0) ();
            Top #(.P(P + 1)) child();
        endmodule
    "#;
    let error = cranelift_build_error(source);
    assert!(
        error.contains("systemverilog module specialization limit exceeded"),
        "{error}"
    );
}

#[test]
fn ignores_unreachable_invalid_systemverilog_hierarchy_in_mixed_designs() {
    let veryl = r#"
        module Top (y: output logic) {
            inst good: $sv::Good (y);
        }
    "#;
    let sv = r#"
        module Good(output logic y); assign y = 1'b1; endmodule
        module Unused(output logic y); Missing child(.y(y)); endmodule
    "#;
    let mut sim = Simulator::from_mixed_sources(
        vec![(veryl, Path::new("top.veryl"))],
        vec![(sv, Path::new("external.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn ignores_unreachable_unsupported_systemverilog_modules_in_mixed_designs() {
    let veryl = r#"
        module Top (y: output logic) {
            inst good: $sv::Good (y);
        }
    "#;
    let sv = r#"
        module Good(output logic y); assign y = 1'b1; endmodule
        module Unused(input logic a, b, output logic y); assign y = a ** b; endmodule
    "#;
    let mut sim = Simulator::from_mixed_sources(
        vec![(veryl, Path::new("top.veryl"))],
        vec![(sv, Path::new("external.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn ignores_missing_sv_components_referenced_only_by_unreachable_veryl_modules() {
    let veryl = r#"
        module Helper (y: output logic) {
            inst missing: $sv::Missing (y);
        }
        module Top (y: output logic) {
            inst good: $sv::Good (y);
        }
    "#;
    let sv = "module Good(output logic y); assign y = 1'b1; endmodule";
    let mut sim = Simulator::from_mixed_sources(
        vec![(veryl, Path::new("top.veryl"))],
        vec![(sv, Path::new("good.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn preserves_assignment_context_width_for_selected_shift_operands() {
    let source = r#"
        module Top(input logic a, output logic [15:0] y);
            assign y = a[0] << 8;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("selected_shift_context.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let a = sim.signal("a");
    sim.modify(|io| io.set(a, 1u8)).unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0x100u16.into());
}

#[test]
fn preserves_assignment_context_width_through_resized_function_results() {
    let source = r#"
        module Top(input logic a, output logic [15:0] y);
            function automatic logic [7:0] widen(input logic value);
                return {7'b0, value};
            endfunction
            assign y = widen(a) << 8;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("function_resize_context.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let a = sim.signal("a");
    sim.modify(|io| io.set(a, 1u8)).unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0x100u16.into());
}

#[test]
fn preserves_ff_context_width_through_resized_function_results() {
    let source = r#"
        module Top(input logic clk, input logic a, output logic [15:0] y);
            function automatic logic [7:0] widen(input logic value);
                return {7'b0, value};
            endfunction
            always_ff @(posedge clk) y <= widen(a) << 8;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("ff_function_resize_context.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let clk = sim.event("clk");
    let a = sim.signal("a");
    sim.modify(|io| io.set(a, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0x100u16.into());
}

#[test]
fn totalizes_division_by_zero_in_two_state_mode() {
    let source = r#"
        module Top(output logic [7:0] y);
            assign y = 8'd5 / 8'd0;
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("two_state_div_zero.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0u8.into());
}

#[test]
fn collapses_unknown_assignment_results_in_two_state_mode() {
    let source = r#"
        module Child(input logic value, output logic y);
            assign y = value;
        endmodule
        module Top(
            input logic clk,
            output logic comb_y,
            output logic ff_y,
            output logic child_y
        );
            assign comb_y = 1'bx;
            always_ff @(posedge clk) ff_y <= 1'bz;
            Child child(.value(1'bx), .y(child_y));
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("two_state_unknown_assignment.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("comb_y")), 0u8.into());
    assert_eq!(sim.get(sim.signal("child_y")), 0u8.into());
    let clk = sim.event("clk");
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(sim.signal("ff_y")), 0u8.into());
}

#[test]
fn preserves_ff_context_width_for_selected_shift_operands() {
    let source = r#"
        module Top(input logic clk, input logic a, output logic [15:0] y);
            always_ff @(posedge clk) y <= a[0] << 8;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("ff_selected_shift_context.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let clk = sim.event("clk");
    let a = sim.signal("a");
    sim.modify(|io| io.set(a, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0x100u16.into());
}

#[test]
fn sizes_ff_left_fill_literals_from_the_shift_context() {
    let source = r#"
        module Top(input logic clk, input logic [5:0] sh, output logic [63:0] y);
            always_ff @(posedge clk) y <= '1 << sh;
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("ff_fill_shift_context.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    let clk = sim.event("clk");
    let sh = sim.signal("sh");
    sim.modify(|io| io.set(sh, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0xffff_ffff_ffff_fffeu64.into());
}

#[test]
fn sizes_unbased_shift_amounts_as_self_determined() {
    let source = r#"
        module Top(
            input logic clk,
            input logic [7:0] a,
            input logic [2:0] sh,
            output logic [7:0] comb_y,
            output logic [7:0] comb_left_fill_y,
            output logic [7:0] ff_y
        );
            assign comb_y = a << '1;
            assign comb_left_fill_y = '1 << sh;
            always_ff @(posedge clk) ff_y <= a << '1;
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("unbased_shift_amount.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    let clk = sim.event("clk");
    let a = sim.signal("a");
    let sh = sim.signal("sh");
    sim.modify(|io| {
        io.set(a, 1u8);
        io.set(sh, 1u8);
    })
    .unwrap();
    assert_eq!(sim.get(sim.signal("comb_y")), 2u8.into());
    assert_eq!(sim.get(sim.signal("comb_left_fill_y")), 0xfeu8.into());
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(sim.signal("ff_y")), 2u8.into());
}

#[test]
fn rejects_unrepresentable_dynamic_selects_instead_of_dropping_them() {
    let error = cranelift_build_error(
        r#"
        module Top(input logic [1:0] a, input logic sel, output logic y);
            assign y = a[sel ? 1 : 0];
        endmodule
        "#,
    );
    assert!(
        error.contains("select index `sel ? 1 : 0`"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_duplicate_external_systemverilog_output_targets() {
    let veryl = r#"
        module Top (y: output logic) {
            inst child: $sv::TwoOut (y, y);
        }
    "#;
    let sv = r#"
        module TwoOut(output logic a, output logic b);
            assign a = 1'b0;
            assign b = 1'b1;
        endmodule
    "#;
    let error = match Simulator::from_mixed_sources(
        vec![(veryl, Path::new("top.veryl"))],
        vec![(sv, Path::new("two_out.sv"))],
        "Top",
    )
    .build_cranelift()
    {
        Ok(_) => panic!("duplicate external output targets unexpectedly compiled"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("multiple output ports drive overlapping target"),
        "{error}"
    );
}

#[test]
fn rejects_external_systemverilog_outputs_connected_to_veryl_inputs() {
    let veryl = r#"
        module Top (a: input logic) {
            inst child: $sv::OneOut (a);
        }
    "#;
    let sv = "module OneOut(output logic y); assign y = 1'b1; endmodule";
    let error = match Simulator::from_mixed_sources(
        vec![(veryl, Path::new("top.veryl"))],
        vec![(sv, Path::new("one_out.sv"))],
        "Top",
    )
    .build_cranelift()
    {
        Ok(_) => panic!("external child output unexpectedly drove a Veryl input"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("child output cannot drive input port"),
        "{error}"
    );
}

#[test]
fn rejects_duplicate_external_outputs_across_instances() {
    let veryl = r#"
        module Top (y: output logic) {
            inst first: $sv::Source (y);
            inst second: $sv::Source (y);
        }
    "#;
    let sv = "module Source(output logic y); assign y = 1'b1; endmodule";
    let error = match Simulator::from_mixed_sources(
        vec![(veryl, Path::new("top.veryl"))],
        vec![(sv, Path::new("source.sv"))],
        "Top",
    )
    .build_cranelift()
    {
        Ok(_) => panic!("duplicate external outputs unexpectedly compiled"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("multiple output ports drive overlapping target"),
        "{error}"
    );
}

#[test]
fn rejects_external_outputs_that_overlap_veryl_local_drivers() {
    let veryl = r#"
        module Top (a: input logic, y: output logic) {
            assign y = a;
            inst child: $sv::Source (y);
        }
    "#;
    let sv = "module Source(output logic y); assign y = 1'b1; endmodule";
    let error = match Simulator::from_mixed_sources(
        vec![(veryl, Path::new("top.veryl"))],
        vec![(sv, Path::new("source.sv"))],
        "Top",
    )
    .build_cranelift()
    {
        Ok(_) => panic!("external output and local driver unexpectedly compiled"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("external output overlaps local driver"),
        "{error}"
    );
}

#[test]
fn rejects_external_outputs_that_overlap_veryl_child_outputs() {
    let veryl = r#"
        module VerylSource (y: output logic) {
            assign y = 1'b0;
        }
        module Top (y: output logic) {
            inst veryl_child: VerylSource (y);
            inst sv_child: $sv::SvSource (y);
        }
    "#;
    let sv = "module SvSource(output logic y); assign y = 1'b1; endmodule";
    let error = match Simulator::from_mixed_sources(
        vec![(veryl, Path::new("top.veryl"))],
        vec![(sv, Path::new("source.sv"))],
        "Top",
    )
    .build_cranelift()
    {
        Ok(_) => panic!("external and Veryl child outputs unexpectedly shared a target"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("external output overlaps child output driver"),
        "{error}"
    );
}

#[test]
fn rejects_invalid_systemverilog_hierarchy_when_mixed_design_reaches_it() {
    let veryl = r#"
        module Top (y: output logic) {
            inst broken: $sv::Broken (y);
        }
    "#;
    let sv = r#"
        module Broken(output logic y); Missing child(.y(y)); endmodule
    "#;
    let error = match Simulator::from_mixed_sources(
        vec![(veryl, Path::new("top.veryl"))],
        vec![(sv, Path::new("external.sv"))],
        "Top",
    )
    .build_cranelift()
    {
        Ok(_) => panic!("reachable invalid hierarchy unexpectedly compiled"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("`Missing`"), "{error}");
}

#[test]
fn rejects_duplicate_modules_across_source_files() {
    let first = "module Top(output logic y); assign y = 1'b0; endmodule";
    let second = "module Top(output logic y); assign y = 1'b1; endmodule";
    let error = match Simulator::from_sv_sources(
        vec![
            (first, Path::new("first.sv")),
            (second, Path::new("second.sv")),
        ],
        "Top",
    )
    .build_cranelift()
    {
        Ok(_) => panic!("duplicate module unexpectedly compiled"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("Duplicate module declaration: Top"),
        "{error}"
    );
}

#[test]
fn substitutes_generate_localparams_in_child_port_actuals() {
    let source = r#"
        module Child(input logic a, output logic y); assign y = a; endmodule
        module Top(output logic y);
            if (1) begin : enabled
                localparam P = 1'b1;
                Child child(.a(P), .y(y));
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("generate_localparam_actual.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn preserves_multidimensional_packed_selects_in_child_port_actuals() {
    let source = r#"
        module Child(input logic [7:0] a, output logic [7:0] y); assign y = a; endmodule
        module Top(input logic [1:0][7:0] a, output logic [7:0] y);
            Child child(.a(a[1]), .y(y));
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("packed_actual.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let a = sim.signal("a");
    sim.modify(|io| io.set(a, 0xabcdu16)).unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0xabu8.into());
}

#[test]
fn evaluates_packed_ranges_with_typed_parameters() {
    let source = r#"
        module Top #(
            parameter logic [3:0] P = 0
        ) (output logic [15:0] y);
            logic [~P:0] value;
            assign value = '1;
            assign y = value;
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("typed_parameter_range.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    assert_eq!(sim.get(sim.signal("y")), u16::MAX.into());
}

#[test]
fn preserves_ascending_inner_packed_dimension_widths() {
    let source = r#"
        module Top(
            input logic [1:0][0:7] a,
            output logic [7:0] y,
            output logic first,
            output logic last
        );
            assign y = a[1];
            assign first = a[1][0];
            assign last = a[1][7];
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("ascending_inner_dimension.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let a = sim.signal("a");
    sim.modify(|io| io.set(a, 0x80cdu16)).unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0x80u8.into());
    assert_eq!(sim.get(sim.signal("first")), 1u8.into());
    assert_eq!(sim.get(sim.signal("last")), 0u8.into());
}

#[test]
fn rejects_unsupported_internal_data_types() {
    let error = cranelift_build_error(
        r#"
        module Top(output logic y);
            real value;
            assign y = 1'b0;
        endmodule
        "#,
    );
    assert!(
        error.contains("unsupported internal data type"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_duplicate_named_external_port_associations() {
    let veryl = r#"
        module Top (
            xa: input logic,
            xb: input logic,
            y: output logic,
        ) {
            inst child: $sv::NamedPorts (
                a: xa,
                a: xb,
                y,
            );
        }
    "#;
    let sv = r#"
        module NamedPorts(input logic a, input logic b, output logic y);
            assign y = a & b;
        endmodule
    "#;
    let error = match Simulator::from_mixed_sources(
        vec![(veryl, Path::new("duplicate_named.veryl"))],
        vec![(sv, Path::new("named_ports.sv"))],
        "Top",
    )
    .build_cranelift()
    {
        Ok(_) => panic!("duplicate named external port associations unexpectedly compiled"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("duplicate named SystemVerilog port association `a`"),
        "{error}"
    );
}

#[test]
fn skips_validation_in_disabled_generate_branches() {
    let source = r#"
        module Top #(parameter USE_INITIAL = 0) (output logic y);
            if (USE_INITIAL) begin : disabled
                initial y = 1'b0;
            end else begin : enabled
                assign y = 1'b1;
            end
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("disabled_generate.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn honors_functions_from_selected_generate_branches() {
    let source = r#"
        module Top(output logic y);
            if (1) begin : selected
                function automatic logic value();
                    return 1'b1;
                endfunction
                assign y = value();
            end
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("generate_function.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn collapses_nested_unknown_literals_in_two_state_mode() {
    let source = r#"
        module Top(output logic [1:0] y);
            assign y = {1'bx, 1'b0};
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("nested_unknown_literal.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0u8.into());
}

#[test]
fn rejects_user_constant_function_calls() {
    let source = r#"
        module Top(input logic F, output logic y);
            parameter P = F(0);
            assign y = P;
        endmodule
    "#;
    let error = cranelift_build_error(source);
    assert!(error.contains("user constant function call"), "{error}");
}

#[test]
fn tracks_scoped_constants_when_skipping_inactive_generate_branches() {
    let source = r#"
        module Top(output logic localparam_y, genvar_y);
            if (1) begin : outer
                localparam ENABLE = 0;
                if (ENABLE) initial localparam_y = 1'b0;
                else assign localparam_y = 1'b1;
            end
            for (genvar i = 0; i < 1; i++) begin : loop_block
                if (i == 1) initial genvar_y = 1'b0;
                else assign genvar_y = 1'b1;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("scoped_generate_constants.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("localparam_y")), 1u8.into());
    assert_eq!(sim.get(sim.signal("genvar_y")), 1u8.into());
}

#[test]
fn propagates_comparison_width_into_nested_operands() {
    let source = r#"
        module Child(input logic value, output logic y);
            assign y = value;
        endmodule
        module Top(
            input logic clk,
            input logic [3:0] a, b,
            input logic [7:0] c,
            output logic comb_eq, ff_eq, glue_eq
        );
            assign comb_eq = (a + b) == c;
            always_ff @(posedge clk) ff_eq <= ((a + b) == c);
            Child child(.value((a + b) == c), .y(glue_eq));
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("comparison_operand_width.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let c = sim.signal("c");
    sim.modify(|io| {
        io.set(a, 15u8);
        io.set(b, 1u8);
        io.set(c, 16u8);
    })
    .unwrap();
    assert_eq!(sim.get(sim.signal("comb_eq")), 1u8.into());
    assert_eq!(sim.get(sim.signal("glue_eq")), 1u8.into());
    sim.tick(sim.event("clk")).unwrap();
    assert_eq!(sim.get(sim.signal("ff_eq")), 1u8.into());
}

#[test]
fn evaluates_repeat_counts_with_typed_parameter_widths() {
    let source = r#"
        module Child(input logic [14:0] value, output logic [14:0] y);
            assign y = value;
        endmodule
        module Top(
            input logic clk,
            output logic [14:0] comb, ff_value, glue
        );
            parameter logic [3:0] P = 0;
            assign comb = {(~P){1'b1}};
            always_ff @(posedge clk) ff_value <= {(~P){1'b1}};
            Child child(.value({(~P){1'b1}}), .y(glue));
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("typed_repeat_count.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    assert_eq!(sim.get(sim.signal("comb")), 0x7fffu16.into());
    assert_eq!(sim.get(sim.signal("glue")), 0x7fffu16.into());
    sim.tick(sim.event("clk")).unwrap();
    assert_eq!(sim.get(sim.signal("ff_value")), 0x7fffu16.into());
}

#[test]
fn evaluates_part_select_bounds_with_typed_parameter_widths() {
    let source = r#"
        module Child(input logic [15:0] value, output logic [15:0] y);
            assign y = value;
        endmodule
        module Top(
            input logic clk,
            input logic [15:0] a,
            output logic [15:0] comb, ff_value, glue
        );
            parameter logic [3:0] P = 0;
            assign comb = a[~P:0];
            always_ff @(posedge clk) ff_value <= a[~P:0];
            Child child(.value(a[~P:0]), .y(glue));
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("typed_part_select_bounds.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let a = sim.signal("a");
    sim.modify(|io| io.set(a, 0xabcdu16)).unwrap();
    assert_eq!(sim.get(sim.signal("comb")), 0xabcdu16.into());
    assert_eq!(sim.get(sim.signal("glue")), 0xabcdu16.into());
    sim.tick(sim.event("clk")).unwrap();
    assert_eq!(sim.get(sim.signal("ff_value")), 0xabcdu16.into());
}

#[test]
fn remaps_trailing_part_select_bounds_in_ascending_packed_dimensions() {
    let source = r#"
        module Top(input logic [1:0][0:7] a, output logic [3:0] y);
            assign y = a[1][0:3];
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("ascending_packed_part_select.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let a = sim.signal("a");
    sim.modify(|io| io.set(a, 0xabcdu16)).unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0xau8.into());
}

#[test]
fn preserves_declared_coordinates_for_non_array_packed_part_selects() {
    let source = r#"
        module Top(input logic [15:8] a, output logic [3:0] y);
            assign y = a[15:12];
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("non_array_packed_part_select.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let a = sim.signal("a");
    sim.modify(|io| io.set(a, 0xabu8)).unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0xau8.into());
}

#[test]
fn preserves_part_selects_on_concatenation_expressions() {
    let source = r#"
        module Top(input logic [7:0] value, output logic [3:0] y);
            function automatic logic [7:0] get_word(input logic [7:0] value);
                return value;
            endfunction
            assign y = {4'h0, get_word(value)}[3:0];
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("concatenation_part_select.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let value = sim.signal("value");
    sim.modify(|io| io.set(value, 0xabu8)).unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0xbu8.into());
}

#[test]
fn appends_use_site_packed_dimensions_to_typedefs() {
    let source = r#"
        module Top(input logic [7:0] value, output logic [7:0] y);
            typedef logic [3:0] nibble_t;
            nibble_t [1:0] a;
            assign a = value;
            assign y = a;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("typedef_use_site_dimension.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let value = sim.signal("value");
    sim.modify(|io| io.set(value, 0xabu8)).unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0xabu8.into());
}

#[test]
fn prepends_declarator_dimensions_to_typedef_dimensions() {
    let source = r#"
        module Top(
            input logic [7:0] value,
            output logic [47:0] all
        );
            typedef logic [7:0] row_t [0:2];
            row_t matrix [0:1];

            always_comb begin
                matrix[0][0] = value;
                matrix[0][1] = value + 1;
                matrix[0][2] = value + 2;
                matrix[1][0] = value + 3;
                matrix[1][1] = value + 4;
                matrix[1][2] = value + 5;
            end
            assign all = matrix;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("typedef_dimension_order.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let value = sim.signal("value");
    sim.modify(|io| io.set(value, 0x10u8)).unwrap();
    assert_eq!(sim.get(sim.signal("all")), 0x151413121110u64.into());
}

#[test]
fn uses_left_operand_signedness_when_context_sizing_shifts() {
    let source = r#"
        module Child(input logic [7:0] value, output logic [7:0] y);
            assign y = value;
        endmodule
        module Top(
            input logic clk,
            input logic signed [3:0] a,
            input logic [1:0] sh,
            output logic [7:0] comb_y,
            output logic [7:0] ff_y,
            output logic [7:0] glue_y
        );
            assign comb_y = a << sh;
            always_ff @(posedge clk) ff_y <= a << sh;
            Child child(.value(a << sh), .y(glue_y));
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("shift_left_operand_signedness.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let a = sim.signal("a");
    let sh = sim.signal("sh");
    sim.modify(|io| {
        io.set(a, 0x0fu8);
        io.set(sh, 1u8);
    })
    .unwrap();
    assert_eq!(sim.get(sim.signal("comb_y")), 0xfeu8.into());
    assert_eq!(sim.get(sim.signal("glue_y")), 0xfeu8.into());
    sim.tick(sim.event("clk")).unwrap();
    assert_eq!(sim.get(sim.signal("ff_y")), 0xfeu8.into());
}

#[test]
fn preserves_constant_system_function_result_types() {
    let source = r#"
        module Top(output logic y);
            localparam P = $onehot(1'b1);
            localparam P0 = $onehot0(1'b0);
            assign y = (~P == 1'b0) && (~P0 == 1'b0);
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("constant_system_function_types.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn rejects_trireg_charge_storage() {
    let error = cranelift_build_error(
        r#"
        module Top(input logic en, output logic y);
            trireg q;
            assign q = en ? 1'b1 : 1'bz;
            assign y = q;
        endmodule
        "#,
    );
    assert!(
        error.contains("trireg charge storage"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_elaboration_system_tasks() {
    let error = cranelift_build_error(
        r#"
        module Top(output logic y);
            $fatal(1, "invalid configuration");
            assign y = 1'b1;
        endmodule
        "#,
    );
    assert!(
        error.contains("elaboration system task"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_bind_directives() {
    let error = cranelift_build_error(
        r#"
        module Driver(output logic y);
            assign y = 1'b1;
        endmodule
        module Top(output wire y);
            bind Top Driver driver(.y(y));
        endmodule
        "#,
    );
    assert!(
        error.contains("bind directive"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_specify_blocks() {
    let error = cranelift_build_error(
        r#"
        module Top(input logic a, output logic y);
            assign y = a;
            specify
                (a => y) = 5;
            endspecify
        endmodule
        "#,
    );
    assert!(error.contains("specify block"), "unexpected error: {error}");
}

#[test]
fn context_sizes_unbased_fill_literals_in_constant_binary_expressions() {
    let source = r#"
        module Top(output logic y);
            localparam MATCH = 8'hff == '1;
            assign y = MATCH;
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("constant_unbased_fill.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn rejects_expressionless_function_returns() {
    let error = cranelift_build_error(
        r#"
        module Top(input logic a, output logic y);
            function automatic logic f(input logic value);
                return;
            endfunction
            assign y = f(a);
        endmodule
        "#,
    );
    assert!(
        error.contains("expressionless function return"),
        "unexpected error: {error}"
    );
}

#[test]
fn collapses_unknowns_for_two_state_parameters() {
    let source = r#"
        module Top(output logic y);
            localparam bit P = 1'bx;
            assign y = P;
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("two_state_parameter.sv"))], "Top")
            .four_state(true)
            .build_cranelift()
            .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0u8.into());
}

#[test]
fn evaluates_signed_bit_patterns_in_constant_onehot_functions() {
    let source = r#"
        module Top(output logic y);
            localparam O = $onehot(4'sb1000);
            localparam O0 = $onehot0(4'sb1000);
            assign y = O && O0;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("signed_constant_onehot.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn folds_dominant_four_state_constant_operators() {
    let source = r#"
        module Top(output logic y);
            localparam BIT_AND = 1'b0 & 1'bx;
            localparam BIT_OR = 1'b1 | 1'bz;
            localparam LOGIC_AND = 1'b0 && 1'bx;
            localparam LOGIC_OR = 1'b1 || 1'bz;
            if (BIT_AND || LOGIC_AND)
                assign y = 1'b0;
            else if (BIT_OR && LOGIC_OR)
                assign y = 1'b1;
            else
                assign y = 1'b0;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("dominant_four_state_constants.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn merges_constant_ternary_arms_for_unknown_conditions() {
    let source = r#"
        module Top(output logic y);
            localparam P = 1'bx ? 1'b1 : 1'b1;
            if (P)
                assign y = 1'b1;
            else
                assign y = 1'b0;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("unknown_constant_ternary.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn tracks_packed_ranges_for_function_scoped_values() {
    let source = r#"
        module Top(
            input logic [3:0] value,
            output logic formal_y,
            output logic local_y,
            output logic [1:0] formal_slice,
            output logic [1:0] local_slice
        );
            function automatic logic formal_msb(input logic [7:4] v);
                return v[7];
            endfunction
            function automatic logic local_first(input logic [3:0] v);
                logic [0:3] temp;
                temp = v;
                return temp[0];
            endfunction
            function automatic logic [1:0] formal_top2(input logic [7:4] v);
                return v[7:6];
            endfunction
            function automatic logic [1:0] local_top2(input logic [3:0] v);
                logic [0:3] temp;
                temp = v;
                return temp[0:1];
            endfunction
            assign formal_y = formal_msb(value);
            assign local_y = local_first(value);
            assign formal_slice = formal_top2(value);
            assign local_slice = local_top2(value);
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("function_packed_ranges.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let value = sim.signal("value");
    sim.modify(|io| io.set(value, 8u8)).unwrap();
    assert_eq!(sim.get(sim.signal("formal_y")), 1u8.into());
    assert_eq!(sim.get(sim.signal("local_y")), 1u8.into());
    assert_eq!(sim.get(sim.signal("formal_slice")), 2u8.into());
    assert_eq!(sim.get(sim.signal("local_slice")), 2u8.into());
}

#[test]
fn declares_implicit_child_output_nets_as_scalar_unsigned_wires() {
    let source = r#"
        module Child(output logic signed [7:0] y); assign y = 8'h81; endmodule
        module Top(output logic [7:0] y);
            Child child(.y(w));
            assign y = w;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("implicit_output_net_type.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn simulates_four_state_always_ff_event_signals_in_four_state_mode() {
    let source = r#"
        module Top(input logic clk, input logic rst_n, input logic [7:0] d,
                   output logic [7:0] q);
            always_ff @(posedge clk or negedge rst_n)
                if (!rst_n) q <= 8'd0; else q <= d;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("review.sv"))], "Top")
        .four_state(true)
        .build_cranelift()
        .unwrap();
    let clk = sim.event("clk");
    let rst_n = sim.signal("rst_n");
    let d = sim.signal("d");
    let q = sim.signal("q");
    sim.modify(|io| {
        io.set(rst_n, 1u8);
        io.set(d, 0x5au8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0x5au8.into());
    sim.modify(|io| io.set(rst_n, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0u8.into());
}

#[test]
fn preserves_parenthesized_binary_grouping() {
    let source = r#"
        module Child #(parameter P = 0) (output logic [7:0] y);
            assign y = P;
        endmodule
        module Top(
            output logic [7:0] runtime_mul,
            output logic [7:0] runtime_sub,
            output logic [7:0] constant_mul,
            output logic [7:0] constant_sub,
            output logic [7:0] override_mul
        );
            localparam MUL = 2 * (3 + 4);
            localparam SUB = 10 - (7 - 2);
            assign runtime_mul = 2 * (3 + 4);
            assign runtime_sub = 10 - (7 - 2);
            assign constant_mul = MUL;
            assign constant_sub = SUB;
            Child #(.P(2 * (3 + 4))) child(.y(override_mul));
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("parenthesized_binary_grouping.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("runtime_mul")), 14u8.into());
    assert_eq!(sim.get(sim.signal("runtime_sub")), 5u8.into());
    assert_eq!(sim.get(sim.signal("constant_mul")), 14u8.into());
    assert_eq!(sim.get(sim.signal("constant_sub")), 5u8.into());
    assert_eq!(sim.get(sim.signal("override_mul")), 14u8.into());
}

#[test]
fn rejects_mintypmax_expressions() {
    for source in [
        r#"
        module Top(output logic y);
            assign y = (1'b0 : 1'b1 : 1'b0);
        endmodule
        "#,
        r#"
        module Top(output logic y);
            localparam P = 1'b0 : 1'b1 : 1'b0;
            assign y = P;
        endmodule
        "#,
    ] {
        let error = cranelift_build_error(source);
        assert!(
            error.contains("mintypmax expression"),
            "unexpected error: {error}"
        );
    }
}

#[test]
fn rejects_default_nettype_changes_inside_modules() {
    for source in [
        r#"
        `default_nettype wire
        module Child(output logic y); assign y = 1'b1; endmodule
        module Top(output logic y);
            `default_nettype none
            Child child(.y(undeclared));
            assign y = undeclared;
        endmodule
        "#,
        r#"
        module Child(output logic y); assign y = 1'b1; endmodule
        `default_nettype none
        module Top(output logic y);
            `default_nettype wire
            Child child(.y(undeclared));
            assign y = undeclared;
        endmodule
        "#,
    ] {
        let error = cranelift_build_error(source);
        assert!(
            error.contains("`default_nettype change inside module `Top`"),
            "unexpected error: {error}"
        );
    }
}

#[test]
fn converts_negative_and_unknown_constant_selects_to_x() {
    let source = r#"
        module Top(output logic negative_is_x, output logic x_is_x, output logic z_is_x);
            localparam logic [7:0] VALUE = 8'hff;
            localparam NEGATIVE = VALUE[-1];
            localparam X_INDEX = VALUE[1'bx];
            localparam Z_INDEX = VALUE[1'bz];
            assign negative_is_x = (NEGATIVE === 1'bx);
            assign x_is_x = (X_INDEX === 1'bx);
            assign z_is_x = (Z_INDEX === 1'bx);
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("unknown_constant_selects.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("negative_is_x")), 1u8.into());
    assert_eq!(sim.get(sim.signal("x_is_x")), 1u8.into());
    assert_eq!(sim.get(sim.signal("z_is_x")), 1u8.into());
}

#[test]
fn infers_standalone_unbased_parameter_literals_as_one_bit() {
    let source = r#"
        module Top(output logic [31:0] one, output logic [31:0] zero);
            localparam ONE = '1;
            localparam ZERO = '0;
            assign one = ONE;
            assign zero = ZERO;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("unbased_parameter_type.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("one")), 1u8.into());
    assert_eq!(sim.get(sim.signal("zero")), 0u8.into());
}

#[test]
fn rejects_unsupported_default_net_types() {
    for net_type in ["tri0", "wand", "wor"] {
        let source = format!(
            "`default_nettype {net_type}\nmodule Top(output logic y); assign y = 1'b0; endmodule"
        );
        let error = cranelift_build_error(&source);
        assert!(
            error.contains(&format!("`default_nettype {net_type}`")),
            "unexpected error: {error}"
        );
    }
}

#[test]
fn preserves_wide_unsigned_parameters_in_generate_conditions() {
    let source = r#"
        module Top(output logic y);
            parameter logic [127:0] P = 128'h80000000000000000000000000000000;
            if (P == 128'h80000000000000000000000000000000)
                assign y = 1'b1;
            else
                assign y = 1'b0;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("wide_unsigned_parameter.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
}

#[test]
fn rejects_multi_bit_always_ff_event_signals() {
    let error = cranelift_build_error(
        r#"
        module Top(input logic [1:0] clk, input logic d, output logic q);
            always_ff @(posedge clk) q <= d;
        endmodule
        "#,
    );
    assert!(
        error.contains("multi-bit always_ff event signal"),
        "unexpected error: {error}"
    );
}

#[test]
fn shares_an_asynchronous_reset_between_clock_domains() {
    let source = r#"
        module Top(
            input logic clk_a,
            input logic clk_b,
            input logic rst_n,
            input logic d,
            output logic q_a,
            output logic q_b
        );
            always_ff @(posedge clk_a or negedge rst_n)
                if (!rst_n) q_a <= 1'b0; else q_a <= d;
            always_ff @(posedge clk_b or negedge rst_n)
                if (!rst_n) q_b <= 1'b0; else q_b <= d;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("shared_reset.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let (rst_n, d) = (sim.signal("rst_n"), sim.signal("d"));
    let (q_a, q_b) = (sim.signal("q_a"), sim.signal("q_b"));
    sim.modify(|io| {
        io.set(rst_n, 1u8);
        io.set(d, 1u8);
    })
    .unwrap();
    sim.tick(sim.event("clk_a")).unwrap();
    assert_eq!(sim.get(q_a), 1u8.into());
    assert_eq!(sim.get(q_b), 0u8.into());
    sim.tick(sim.event("clk_b")).unwrap();
    assert_eq!(sim.get(q_b), 1u8.into());
    sim.modify(|io| io.set(rst_n, 0u8)).unwrap();
    sim.tick(sim.event("clk_a")).unwrap();
    assert_eq!(sim.get(q_a), 0u8.into());
    assert_eq!(sim.get(q_b), 1u8.into());
    sim.tick(sim.event("clk_b")).unwrap();
    assert_eq!(sim.get(q_b), 0u8.into());
}

#[test]
fn applies_function_writes_outside_the_inlined_scope() {
    let source = r#"
        module Top(input logic a, output logic y, side);
            function automatic logic f(input logic value);
                side = ~value;
                return value;
            endfunction
            assign y = f(a);
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("function_side_effect.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    let a = sim.signal("a");
    for value in [0u8, 1, 0] {
        sim.modify(|io| io.set(a, value)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), value.into());
        assert_eq!(sim.get(sim.signal("side")), (value ^ 1).into());
    }
}

#[test]
fn handles_wide_two_state_cases_without_enumerating_the_selector_domain() {
    let source = r#"
        module Top(input bit [31:0] s, input logic a, b, output logic y);
            always_comb begin
                case (s)
                    32'd0: y = a;
                    default: y = b;
                endcase
            end
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("wide_two_state_case.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    let s = sim.signal("s");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");
    sim.modify(|io| {
        io.set(s, 0u32);
        io.set(a, true);
        io.set(b, false);
    })
    .unwrap();
    assert_eq!(sim.get(y), true.into());
    sim.modify(|io| io.set(s, 1u32)).unwrap();
    assert_eq!(sim.get(y), false.into());
}

#[test]
fn treats_nonempty_static_for_loops_as_definite_assignments() {
    let source = r#"
        module Top(input logic c, a, b, output logic y);
            always_comb begin
                if (c)
                    y = a;
                else
                    for (int i = 0; i < 1; i++)
                        y = b;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("definite_static_for_loop.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let c = sim.signal("c");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");
    sim.modify(|io| {
        io.set(c, true);
        io.set(a, true);
        io.set(b, false);
    })
    .unwrap();
    assert_eq!(sim.get(y), true.into());
    sim.modify(|io| io.set(c, false)).unwrap();
    assert_eq!(sim.get(y), false.into());
}

#[test]
fn rejects_unmatched_constant_cases_that_would_infer_latches() {
    let error = cranelift_build_error(
        r#"
        module Top(input logic a, output logic y);
            always_comb begin
                case (1'b0)
                    1'b1: y = a;
                endcase
            end
        endmodule
        "#,
    );
    assert!(
        error.contains("latch inference inside always_comb"),
        "unexpected error: {error}"
    );
}

#[test]
fn sign_extends_function_calls_in_conditional_assignments() {
    let source = r#"
        module Top(input logic c, output logic [7:0] y);
            function automatic logic signed [3:0] f();
                return -1;
            endfunction
            always_comb begin
                if (c)
                    y = f();
                else
                    y = 8'h00;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(
            source,
            Path::new("signed_function_conditional_assignment.sv"),
        )],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let c = sim.signal("c");
    let y = sim.signal("y");
    sim.modify(|io| io.set(c, true)).unwrap();
    assert_eq!(sim.get(y), 0xffu8.into());
    sim.modify(|io| io.set(c, false)).unwrap();
    assert_eq!(sim.get(y), 0u8.into());
}

#[test]
fn coerces_function_returns_in_procedural_lvalue_indices() {
    let source = r#"
        module Top(
            input bit [1:0] index,
            input logic data,
            input logic replace,
            output logic [1:0] x
        );
            function automatic bit idx();
                return index;
            endfunction
            always_comb begin
                x = '0;
                x[idx()] = data;
                if (replace)
                    x = '1;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("function_typed_lvalue_index.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let index = sim.signal("index");
    let data = sim.signal("data");
    let replace = sim.signal("replace");
    let x = sim.signal("x");
    sim.modify(|io| {
        io.set(index, 2u8);
        io.set(data, true);
        io.set(replace, false);
    })
    .unwrap();
    assert_eq!(sim.get(x), 1u8.into());
    sim.modify(|io| io.set(index, 1u8)).unwrap();
    assert_eq!(sim.get(x), 2u8.into());
}

sv_backends! {
    fn preserves_use_site_dimensions_in_parameter_alias_types(sim) {
        @case "review_regressions::preserves_use_site_dimensions_in_parameter_alias_types";
    }

    fn body_parameters_are_local_with_a_parameter_port_list(sim) {
        @case "review_regressions::body_parameters_are_local_with_a_parameter_port_list";
    }

    fn preserves_four_state_arithmetic_case_constants(sim) {
        @case "review_regressions::preserves_four_state_arithmetic_case_constants";
    }

    fn preserves_four_state_shift_and_bit_select_constants(sim) {
        @case "review_regressions::preserves_four_state_shift_and_bit_select_constants";
    }

    fn preserves_unsigned_128_bit_enum_arithmetic(sim) {
        @case "review_regressions::preserves_unsigned_128_bit_enum_arithmetic";
    }

    fn preserves_four_state_reduction_case_constants(sim) {
        @case "review_regressions::preserves_four_state_reduction_case_constants";
    }

    fn preserves_use_site_dimensions_in_function_alias_types(sim) {
        @case "review_regressions::preserves_use_site_dimensions_in_function_alias_types";
    }

    fn preserves_four_state_relational_case_selectors(sim) {
        @case "review_regressions::preserves_four_state_relational_case_selectors";
    }

    fn folds_compound_four_state_case_labels(sim) {
        @case "review_regressions::folds_compound_four_state_case_labels";
    }

    fn preserves_four_state_equality_case_selectors(sim) {
        @case "review_regressions::preserves_four_state_equality_case_selectors";
    }

    fn preserves_constant_case_selector_context(sim) {
        @case "review_regressions::preserves_constant_case_selector_context";
    }

    fn preserves_size_cast_dimensions_in_declarations_and_selections(sim) {
        @case "review_regressions::preserves_size_cast_dimensions_in_declarations_and_selections";
    }

    fn resolves_generate_local_alias_casts_for_all_processes(sim) {
        @case "review_regressions::resolves_generate_local_alias_casts_for_all_processes";
    }

    fn covers_single_bit_bitwise_complementary_guards(sim) {
        @case "review_regressions::covers_single_bit_bitwise_complementary_guards";
    }

    fn normalizes_function_parameter_cast_dimensions(sim) {
        @case "review_regressions::normalizes_function_parameter_cast_dimensions";
    }

    fn lowers_parameter_casts_in_conditional_generate(sim) {
        @case "review_regressions::lowers_parameter_casts_in_conditional_generate";
    }

    fn lowers_parameter_casts_in_loop_generate(sim) {
        @case "review_regressions::lowers_parameter_casts_in_loop_generate";
    }

    fn default_pattern_fills_each_element_of_unpacked_subarrays(sim) {
        @case "review_regressions::default_pattern_fills_each_element_of_unpacked_subarrays";
    }

    fn int_parameters_are_signed_in_comparisons_and_loop_bounds(sim) {
        @case "review_regressions::int_parameters_are_signed_in_comparisons_and_loop_bounds";
    }

    fn run_time_for_condition_uses_the_relational_operand_context(sim) {
        @case "review_regressions::run_time_for_condition_uses_the_relational_operand_context";
    }

    fn size_of_a_type_uses_the_dimension_argument(sim) {
        @case "review_regressions::size_of_a_type_uses_the_dimension_argument";
    }
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn collapses_unknown_initializers_in_two_state_native_images() {
    use celox::{NativeProgramInstance, SimBackend};

    let source = "module Top(output logic y); endmodule";
    let sim = Simulator::from_sv_sources(
        vec![(source, Path::new("two_state_native_image.sv"))],
        "Top",
    )
    .build_native()
    .unwrap();
    let image = sim.shared_code().program_image().clone();
    drop(sim);

    // Safety: the image was produced in-process by the compiler above.
    let runtime = unsafe { NativeProgramInstance::from_image(image) }.unwrap();
    let y = runtime.signal_ref("Top.y").unwrap();
    assert_eq!(runtime.backend().get_as::<u8>(y), 0);
}

#[test]
fn generate_function_uses_lexical_constants() {
    let source = r#"
        module Top(output logic [7:0] y);
            localparam W = 7;
            if (1) begin : g
                localparam W = 2;
                if (1) begin : nested
                    localparam V = W;
                    function automatic logic [W-1:0] f();
                        return V + 7;
                    endfunction
                    assign y = f();
                end
            end
        endmodule
    "#;
    let mut simulator =
        Simulator::from_sv_sources(vec![(source, Path::new("generate_function.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    assert_eq!(simulator.get(simulator.signal("y")), 1u8.into());
}

#[test]
fn function_partial_returns_preserve_continuations() {
    let source = r#"
        module Top(input logic c, d, output logic [3:0] y);
            function automatic logic [3:0] f(input logic c, d);
                logic [3:0] x;
                x = 4'd3;
                if (c) begin
                    if (d) return 4'd9;
                    else x = 4'd4;
                    x = x + 1;
                end else if (d) return 4'd7;
                else x = 4'd1;
                x = x + 1;
                return x;
            endfunction
            assign y = f(c, d);
        endmodule
    "#;
    let mut simulator =
        Simulator::from_sv_sources(vec![(source, Path::new("partial_return.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    let c = simulator.signal("c");
    let d = simulator.signal("d");
    let y = simulator.signal("y");
    for (cv, dv, expected) in [(0u8, 0u8, 2u8), (0, 1, 7), (1, 0, 6), (1, 1, 9)] {
        simulator
            .modify(|io| {
                io.set(c, cv);
                io.set(d, dv);
            })
            .unwrap();
        assert_eq!(simulator.get(y), expected.into());
    }
}

#[test]
fn dynamic_case_label_matches_two_state_selector() {
    let source = r#"
        module Top(input bit selector, dynamic_label, input logic a, b, output logic y);
            always_comb case (selector)
                dynamic_label: y = a;
                default: y = b;
            endcase
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("dynamic_case_label.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    let selector = sim.signal("selector");
    let label = sim.signal("dynamic_label");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");
    for selector_value in 0..=1u8 {
        for label_value in 0..=1u8 {
            for a_value in 0..=1u8 {
                sim.modify(|io| {
                    io.set(selector, selector_value);
                    io.set(label, label_value);
                    io.set(a, a_value);
                    io.set(b, 1 - a_value);
                })
                .unwrap();
                let expected = if selector_value == label_value {
                    a_value
                } else {
                    1 - a_value
                };
                assert_eq!(sim.get(y), expected.into());
            }
        }
    }
}

#[test]
fn constant_case_coverage_preserves_declared_coordinates_and_equality() {
    for (declaration, selector, label) in [
        ("localparam logic [0:3] P = 4'bxx00;", "P[0:1]", "2'bxx"),
        ("localparam logic [4:7] P = 4'bxx00;", "P[6:7]", "2'b00"),
        ("", "(1'bx | 1'b0) === 1'bx", "1'b1"),
        ("", "(1'bx | 1'b0) !== 1'bx", "1'b0"),
        ("", "(2'bx0 | 2'b00) ==? 2'bx0", "1'b1"),
        ("", "(2'bx0 | 2'b00) !=? 2'b00", "1'bx"),
    ] {
        let source = format!(
            "module Top(input logic a, output logic y);
             {declaration}
             always_comb case ({selector}) {label}: y = a; endcase endmodule"
        );
        let mut sim = Simulator::from_sv_sources(
            vec![(&source, Path::new("constant_case_coverage.sv"))],
            "Top",
        )
        .four_state(true)
        .build_cranelift()
        .unwrap();
        let a = sim.signal("a");
        let y = sim.signal("y");
        for value in [0u8, 1, 0] {
            sim.modify(|io| io.set(a, value)).unwrap();
            assert_eq!(sim.get(y), value.into(), "{selector}");
        }
    }
}

#[test]
fn function_size_cast_uses_formal_and_local_dimensions() {
    let source = r#"
        module Top(output logic [7:0] y);
            logic [15:0] x;
            function automatic logic [7:0] f(input logic [3:0] x);
                logic [1:0][3:0] local_value;
                return {~$bits(x)'(0), ~$size(local_value)'(0), 2'b00};
            endfunction
            assign y = f(0);
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("function_size_cast.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0xfcu8.into());
}

#[test]
fn function_case_partial_returns_preserve_continuations() {
    for (body, expected) in [
        (
            "case (v) 0: return 1; default: x = 0; endcase return x;",
            [1u8, 0, 0, 0],
        ),
        (
            "x = 3; case (v) 0: return 9; 1, 2: x = 4; endcase
             x = x + 1; return x;",
            [9, 5, 5, 4],
        ),
        (
            "x = 3; case (v)
             default: x = 1;
             0, 1: begin
                 case (v) 0: return 9; default: x = 4; endcase
                 x = x + 1;
             end
             1: return 12;
             2: begin if (v == 2) return 7; else x = 8; end
             endcase x = x + 1; return x;",
            [9, 6, 7, 2],
        ),
        (
            "x = v; case (x)
             0: begin x = 2; return 9; end
             1: x = 3;
             2: return 7;
             default: ;
             endcase x = x + 1; return x;",
            [9, 4, 7, 4],
        ),
    ] {
        let source = format!(
            "module Top(input logic [1:0] v, output logic [3:0] y);
             function automatic logic [3:0] f(input logic [1:0] v);
                 logic [3:0] x;
                 {body}
             endfunction
             assign y = f(v);
             endmodule"
        );
        let mut sim =
            Simulator::from_sv_sources(vec![(&source, Path::new("case_partial_return.sv"))], "Top")
                .build_cranelift()
                .unwrap();
        let v = sim.signal("v");
        let y = sim.signal("y");
        for (input, expected) in expected.into_iter().enumerate() {
            sim.modify(|io| io.set(v, input as u8)).unwrap();
            assert_eq!(sim.get(y), expected.into(), "v={input}, {body}");
        }
    }
}

#[test]
fn function_case_partial_returns_match_four_state_labels() {
    use num_bigint::BigUint;

    let source = r#"
        module Top(input logic v, output logic [3:0] y);
            function automatic logic [3:0] f(input logic v);
                logic [3:0] x;
                case (v)
                    1'bx: return 9;
                    1'bz: x = 4;
                    default: x = 1;
                endcase
                x = x + 1;
                return x;
            endfunction
            assign y = f(v);
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("four_state_case_return.sv"))],
        "Top",
    )
    .four_state(true)
    .build_cranelift()
    .unwrap();
    let v = sim.signal("v");
    let y = sim.signal("y");
    for (value, mask, expected) in [(0u8, 0u8, 2u8), (1, 0, 2), (1, 1, 9), (0, 1, 5)] {
        sim.modify(|io| io.set_four_state(v, BigUint::from(value), BigUint::from(mask)))
            .unwrap();
        assert_eq!(sim.get(y), expected.into());
    }
}

#[test]
fn masked_parameter_case_labels_are_exhaustive() {
    for (value, selector, label) in [
        ("1'bx", "1'bx", "P"),
        ("1'bz", "1'bz", "P"),
        ("2'bxz", "2'bxz", "P"),
        ("1'bx", "1'bx", "(P | 1'b0)"),
    ] {
        let source = format!(
            "module Top(input logic a, output logic y);
             localparam P = {value};
             always_comb case ({selector}) {label}: y = a; endcase endmodule"
        );
        let mut sim = Simulator::from_sv_sources(
            vec![(&source, Path::new("masked_parameter_label.sv"))],
            "Top",
        )
        .four_state(true)
        .build_cranelift()
        .unwrap();
        let a = sim.signal("a");
        let y = sim.signal("y");
        for value in [0u8, 1, 0] {
            sim.modify(|io| io.set(a, value)).unwrap();
            assert_eq!(sim.get(y), value.into());
        }
        let mismatch = source.replace(&format!("case ({selector})"), "case (3'b111)");
        assert!(four_state_cranelift_build_error(&mismatch).contains("latch inference"));
    }
}

#[test]
fn function_early_returns_without_else_preserve_continuations() {
    for (body, expected) in [
        ("if (c) return 1; return 0;", [0u8, 0, 1, 1]),
        (
            "x = 2; if (c) begin if (d) return 9; x = 4; end
          x = x + 1; return x;",
            [3, 3, 5, 9],
        ),
        (
            "case (c) 1: begin if (d) return 9; x = 4; end
          default: x = 2; endcase return x;",
            [2, 2, 4, 9],
        ),
    ] {
        let source = format!(
            "module Top(input logic c, d, output logic [3:0] y);
             function automatic logic [3:0] f(input logic c, d);
             logic [3:0] x; {body} endfunction
             assign y = f(c, d); endmodule"
        );
        let mut sim =
            Simulator::from_sv_sources(vec![(&source, Path::new("early_return.sv"))], "Top")
                .build_cranelift()
                .unwrap();
        let c = sim.signal("c");
        let d = sim.signal("d");
        let y = sim.signal("y");
        for (input, expected) in expected.into_iter().enumerate() {
            sim.modify(|io| {
                io.set(c, (input >> 1) as u8);
                io.set(d, (input & 1) as u8);
            })
            .unwrap();
            assert_eq!(sim.get(y), expected.into(), "{body}, {input}");
        }
    }
}

#[test]
fn enum_dependent_aliases_preserve_parameter_widths() {
    // An unsized signed 3 becomes -1 after a two-bit size cast, giving
    // [-1:0]. An unsigned operand instead gives [3:0].
    for (bound, signing, expected) in [
        ("3", "", 0x02u8),
        ("3", "signed", 0xfe),
        ("32'd3", "", 0x0a),
        ("32'd3", "signed", 0xfa),
    ] {
        let source = format!(
            "module Top(output logic [7:0] y);
             typedef enum logic [1:0] {{ W = 2 }} E;
             typedef logic {signing} [W'({bound}):0] word_t;
             localparam word_t P = 4'b1010;
             assign y = P;
             endmodule"
        );
        let mut sim = Simulator::from_sv_sources(
            vec![(&source, Path::new("enum_alias_parameter.sv"))],
            "Top",
        )
        .build_cranelift()
        .unwrap();
        let y = sim.signal("y");
        assert_eq!(sim.get(y), expected.into());
    }
}

#[test]
fn complementary_else_if_chains_are_exhaustive() {
    for body in [
        "if (s) y = a; else if (!s) y = b;",
        "if (s == 1'b1) y = a; else if (s != 1'b1) y = b;",
        "if (s) y = a; else if (~s) y = b;",
        "if (outer) begin if (s) y = a; else if (!s) y = b; end else y = b;",
    ] {
        let source = format!(
            "module Top(input bit s, input logic outer, a, b, output logic y);
             always_comb begin {body} end endmodule"
        );
        let mut sim = Simulator::from_sv_sources(
            vec![(&source, Path::new("complementary_else_if.sv"))],
            "Top",
        )
        .build_cranelift()
        .unwrap();
        let s = sim.signal("s");
        let outer = sim.signal("outer");
        let a = sim.signal("a");
        let b = sim.signal("b");
        let y = sim.signal("y");
        for input in 0..16u8 {
            let sv = input & 1;
            let av = (input >> 1) & 1;
            let bv = (input >> 2) & 1;
            let ov = (input >> 3) & 1;
            sim.modify(|io| {
                io.set(s, sv);
                io.set(a, av);
                io.set(b, bv);
                io.set(outer, ov);
            })
            .unwrap();
            let expected = if sv != 0 && (!body.contains("outer") || ov != 0) {
                av
            } else {
                bv
            };
            assert_eq!(sim.get(y), expected.into(), "{body}, {input}");
        }
    }
    for (kind, body) in [
        ("logic", "if (s) y = a; else if (!s) y = b;"),
        ("bit", "if (s) y = a; else if (s) y = b;"),
        ("bit", "if (s) y = a; else if (!s) begin end"),
        (
            "bit",
            "if (outer) begin if (s) y = a; else if (!s) begin end end else y = b;",
        ),
    ] {
        let source = format!(
            "module Top(input {kind} s, input logic outer, a, b, output logic y);
             always_comb begin {body} end endmodule"
        );
        assert!(
            four_state_cranelift_build_error(&source).contains("latch inference"),
            "{body}"
        );
    }
}

#[test]
fn generate_local_literals_shadow_module_constants_in_comb() {
    for declaration in [
        "parameter logic [3:0] P = 0;",
        "typedef enum logic [3:0] { P = 0 } E;",
    ] {
        let source = format!(
            "module Top(output logic [7:0] local_y, nested_y, sibling_y, module_y);
             {declaration}
             if (1) begin : g
                 localparam logic signed [3:0] P = 4'hf;
                 always_comb local_y = P;
                 if (1) begin : nested
                     always_comb nested_y = P;
                 end
             end
             if (1) begin : sibling
                 always_comb sibling_y = P;
             end
             always_comb module_y = P;
             endmodule"
        );
        let mut sim = Simulator::from_sv_sources(
            vec![(&source, Path::new("generate_local_literals.sv"))],
            "Top",
        )
        .build_cranelift()
        .unwrap();
        for (name, expected) in [
            ("local_y", 255u8),
            ("nested_y", 255),
            ("sibling_y", 0),
            ("module_y", 0),
        ] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), expected.into(), "{declaration}, {name}");
        }
    }
}

#[test]
fn masked_generate_locals_shadow_numeric_outer_constants() {
    use num_bigint::BigUint;
    for literal in ["1'bx", "1'bz"] {
        let source = format!(
            "module Top(output logic y);
             parameter logic P = 0;
             if (1) begin : g
                 localparam logic P = {literal};
                 always_comb y = P;
             end endmodule"
        );
        let mut sim =
            Simulator::from_sv_sources(vec![(&source, Path::new("masked_generate.sv"))], "Top")
                .four_state(true)
                .build_cranelift()
                .unwrap();
        let y = sim.signal("y");
        let (value, mask) = sim.get_four_state(y);
        assert_eq!(mask, BigUint::from(1u8));
        assert_eq!(value, BigUint::from(u8::from(literal == "1'bx")));
    }
}

#[test]
fn function_predicates_prove_complementary_else_if() {
    let source = r#"
        module Top(input bit s, input logic a, b, output logic y);
            function automatic bit inv(input bit v); return !v; endfunction
            always_comb if (s) y = a; else if (inv(s)) y = b;
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("function_complement.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    let s = sim.signal("s");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");
    for input in 0..8u8 {
        sim.modify(|io| {
            io.set(s, input & 1);
            io.set(a, (input >> 1) & 1);
            io.set(b, (input >> 2) & 1);
        })
        .unwrap();
        let expected = if input & 1 != 0 {
            (input >> 1) & 1
        } else {
            (input >> 2) & 1
        };
        assert_eq!(sim.get(y), expected.into());
    }
}

#[test]
fn out_of_range_bits_of_a_two_state_packed_select_read_zero() {
    let source = r#"
        module Top (
            input logic [2:0] b,
            input bit [7:0] v,
            output logic [3:0] y
        );
            assign y = v[b +: 4];
        endmodule
    "#;
    let mut sim =
        Simulator::from_sv_sources(vec![(source, Path::new("two_state_select.sv"))], "Top")
            .four_state(true)
            .build_cranelift()
            .unwrap();
    let b = sim.signal("b");
    let v = sim.signal("v");
    let y = sim.signal("y");
    sim.modify(|io| {
        io.set(b, 6u8);
        io.set(v, 0xffu8);
    })
    .unwrap();
    // Bits 6 and 7 exist; bits 8 and 9 of a two-state vector read 0.
    assert_eq!(
        sim.get_four_state(y),
        (BigUint::from(0b0011u8), BigUint::default())
    );
}

#[test]
fn run_time_bound_loop_return_reports_unsupported_on_a_small_stack() {
    // Each symbolically unrolled iteration deepens the loop's `break` and
    // `return` conditions. Constant evaluation of those conditions must not
    // recurse once per level: the diagnostic is reported on a 2 MiB thread,
    // libtest's default, in a debug build.
    let source = r#"
        module Top (
            input  logic [7:0] a,
            input  logic       stop,
            input  logic [1:0] count,
            output logic [7:0] out
        );
            function automatic void notify(input logic [7:0] x);
                $display("notify=%0d", x);
            endfunction

            function automatic logic [7:0] pass(
                input logic [7:0] x,
                input logic       stop_early,
                input logic [1:0] count
            );
                for (int i = 0; i < count; i++) begin
                    if (stop_early && i == 1) begin
                        return x + i;
                    end
                    notify(x + i);
                end
                notify(x);
                return x;
            endfunction

            always_comb begin
                out = pass(a, stop, count);
            end
        endmodule
    "#;
    let error = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || cranelift_build_error(source))
        .unwrap()
        .join()
        .unwrap();
    assert!(
        error.contains("loop condition that depends on run-time values"),
        "{error}"
    );
}

#[test]
fn reports_incompatible_unpacked_arrays_in_each_assignment_like_context() {
    // An unpacked array needs an equivalent element type and equal element
    // counts wherever it is assigned (IEEE 1800-2023 7.6, 10.8); the
    // diagnostic names the context and both array types.
    let cases = [
        (
            r#"
            module Top(output logic signed [7:0] q);
                function automatic logic signed [7:0] pick(input logic signed [7:0] x [2]);
                    return x[0];
                endfunction
                logic signed [3:0] narrow [2];
                always_comb begin
                    narrow[0] = 4'sh8;
                    narrow[1] = 4'sh1;
                    q = pick(narrow);
                end
            endmodule
            "#,
            "argument 1 of `pick`: an unpacked array of type `logic signed [3:0] [2]` is not \
             assignment compatible with `logic signed [7:0] [2]`",
        ),
        (
            r#"
            module Top(output logic [3:0] q);
                task automatic fill(output logic [7:0] x [2]);
                    x[0] = 8'h12;
                    x[1] = 8'h34;
                endtask
                logic [3:0] narrow [2];
                always_comb begin
                    fill(narrow);
                    q = narrow[0];
                end
            endmodule
            "#,
            "argument 1 of `fill`: an unpacked array of type `logic [3:0] [2]` is not \
             assignment compatible with `logic [7:0] [2]`",
        ),
        (
            r#"
            module Top(input logic a, output logic q);
                function automatic void first(input logic x [2], output logic y);
                    y = x[0];
                endfunction
                bit two [2];
                always_comb begin
                    two[0] = a;
                    two[1] = a;
                    first(.y(q), .x(two));
                end
            endmodule
            "#,
            "argument 1 of `first`: an unpacked array of type `bit [2]` is not assignment \
             compatible with `logic [2]`",
        ),
        (
            r#"
            module Top(input logic [7:0] a, output logic [7:0] q);
                logic [7:0] grid [2][3];
                logic [7:0] rows [2][2];
                always_comb begin
                    grid = '{default: a};
                    rows = grid;
                    q = rows[0][0];
                end
            endmodule
            "#,
            "assignment: an unpacked array of type `logic [7:0] [2][3]` is not assignment \
             compatible with `logic [7:0] [2][2]`",
        ),
        (
            r#"
            module Top(input logic [7:0] a, output logic [7:0] q);
                logic [7:0] row [3];
                logic [7:0] grid [2][2];
                assign row[0] = a;
                assign row[1] = a;
                assign row[2] = a;
                assign grid[1] = row;
                assign grid[0] = '{a, a};
                assign q = grid[1][0];
            endmodule
            "#,
            "continuous assignment: an unpacked array of type `logic [7:0] [3]` is not \
             assignment compatible with `logic [7:0] [2]`",
        ),
        (
            r#"
            module Child(output logic signed [7:0] y [2]);
                assign y[0] = 8'sd1;
                assign y[1] = -8'sd1;
            endmodule
            module Top(output logic [7:0] q);
                logic [7:0] wide [2];
                Child u(.y(wide));
                assign q = wide[1];
            endmodule
            "#,
            "connection of port `y`: an unpacked array of type `logic [7:0] [2]` is not \
             assignment compatible with `logic signed [7:0] [2]`",
        ),
    ];
    for (source, expected) in cases {
        let error = match Simulator::from_sv_sources(vec![(source, Path::new("review.sv"))], "Top")
            .build_cranelift()
        {
            Ok(_) => panic!("incompatible unpacked arrays unexpectedly compiled:\n{source}"),
            Err(error) => error,
        };
        match error.kind() {
            celox::SimulatorErrorKind::SIRParser(celox::ParserError::IllegalContext {
                detail,
                ..
            }) => assert_eq!(detail, expected),
            _ => panic!("expected an illegal-context error, got {error:?}"),
        }
    }
}

#[test]
fn runs_both_ff_select_index_arms_for_an_unknown_condition() {
    // An ambiguous condition evaluates both arms (IEEE 1800-2023 11.4.11).
    let source = r#"
        module Top(input bit clk, input logic c, output logic [3:0] count);
            function automatic logic [1:0] tick(input logic [3:0] prior,
                                                output logic [3:0] after);
                after = prior + 4'd1;
                return 2'd1;
            endfunction
            always_ff @(posedge clk) begin
                logic [3:0] calls;
                logic [3:0] bits;
                calls = 4'd0;
                bits = 4'd0;
                bits[c ? tick(calls, calls) : tick(calls, calls)] = 1'b1;
                count <= calls;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("ff_mux_x.sv"))], "Top")
        .four_state(true)
        .build_cranelift()
        .unwrap();
    let c = sim.signal("c");
    let count = sim.signal("count");
    let clk = sim.event("clk");
    for (payload, mask, calls) in [(1u8, 0u8, 1u8), (0, 0, 1), (0, 1, 2)] {
        sim.modify(|io| io.set_four_state(c, BigUint::from(payload), BigUint::from(mask)))
            .unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(
            sim.get_four_state(count),
            (BigUint::from(calls), BigUint::default()),
            "c = {payload}/{mask}"
        );
    }
}

#[test]
fn runs_one_select_index_arm_for_a_true_condition_with_unknown_bits() {
    // 2'b1x is true: only the first arm runs; 2'b0x is ambiguous: both run
    // (IEEE 1800-2023 11.4.11).
    let source = r#"
        module Top(input logic [1:0] c, output logic [3:0] first, output logic [3:0] second);
            function automatic logic [1:0] tick(input logic [3:0] prior,
                                                output logic [3:0] after);
                after = prior + 4'd1;
                return 2'd1;
            endfunction
            always_comb begin
                logic [3:0] a;
                logic [3:0] b;
                logic [3:0] bits;
                a = 4'd0;
                b = 4'd0;
                bits = 4'd0;
                bits[c ? tick(a, a) : tick(b, b)] = 1'b1;
                first = a;
                second = b;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("comb_mux_x.sv"))], "Top")
        .four_state(true)
        .build_cranelift()
        .unwrap();
    let c = sim.signal("c");
    let first = sim.signal("first");
    let second = sim.signal("second");
    for (payload, mask, a, b) in [
        (0b10u8, 0b01u8, 1u8, 0u8),
        (0b00, 0b01, 1, 1),
        (0b00, 0b00, 0, 1),
    ] {
        sim.modify(|io| io.set_four_state(c, BigUint::from(payload), BigUint::from(mask)))
            .unwrap();
        let known = |value: u8| (BigUint::from(value), BigUint::default());
        assert_eq!(
            sim.get_four_state(first),
            known(a),
            "c = {payload:b}/{mask:b}"
        );
        assert_eq!(
            sim.get_four_state(second),
            known(b),
            "c = {payload:b}/{mask:b}"
        );
    }
}

#[test]
fn reemits_run_time_loop_events_when_only_the_bound_changes() {
    let source = r#"
        module Top(input logic [2:0] count, input logic a, output logic y);
            always_comb begin
                for (int i = 0; i < count; i++) $display("tick %0d", i);
                y = a;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("loop_bound.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let count = sim.signal("count");
    let ticks = |n: usize| {
        (0..n)
            .map(|i| celox::RuntimeEvent::Display {
                message: format!("tick {i}"),
            })
            .collect::<Vec<_>>()
    };
    sim.drain_runtime_events();
    sim.modify(|io| io.set(count, 2u8)).unwrap();
    assert_eq!(sim.drain_runtime_events(), ticks(2));
    sim.modify(|io| io.set(count, 3u8)).unwrap();
    assert_eq!(sim.drain_runtime_events(), ticks(3));
}

#[test]
fn commits_ff_output_actuals_whose_index_calls_a_function() {
    // `outer` writes `state[inner(k)]` from an always_ff select index; the
    // write is committed with the process.
    let source = r#"
        module Top(input bit clk, input logic [1:0] k, output logic [3:0] st,
                   output logic [3:0] bits);
            function automatic logic [1:0] inner(input logic [1:0] x);
                return x;
            endfunction
            function automatic logic [1:0] outer(output logic y);
                y = 1'b1;
                return 2'd2;
            endfunction
            logic [3:0] state;
            always_ff @(posedge clk) begin
                state <= 4'd0;
                bits[outer(state[inner(k)])] <= 1'b1;
            end
            assign st = state;
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("ff_nested.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    let k = sim.signal("k");
    let st = sim.signal("st");
    let bits = sim.signal("bits");
    let clk = sim.event("clk");
    for index in [1u8, 3, 0] {
        sim.modify(|io| io.set(k, index)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get_as::<u8>(st), 1 << index, "k = {index}");
        assert_eq!(sim.get_as::<u8>(bits), 4);
    }
}
