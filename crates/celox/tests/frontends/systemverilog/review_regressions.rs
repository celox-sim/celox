use super::*;

fn cranelift_build_error(source: &str) -> String {
    match Simulator::from_sv_sources(vec![(source, Path::new("review.sv"))], "Top")
        .build_cranelift()
    {
        Ok(_) => panic!("unsupported SystemVerilog unexpectedly compiled:\n{source}"),
        Err(error) => error.to_string(),
    }
}

#[test]
fn rejects_cross_lhs_read_before_write_in_always_comb() {
    let error = cranelift_build_error(
        r#"
        module Top(input logic b, output logic a, output logic c);
            always_comb begin
                c = a;
                a = b;
            end
        endmodule
        "#,
    );
    assert!(
        error.contains("read-before-write dependency inside always_comb"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_same_vector_slice_read_before_write_in_always_comb() {
    let error = cranelift_build_error(
        r#"
        module Top(input logic a, output logic [1:0] y);
            always_comb begin
                y[0] = y[1];
                y[1] = a;
            end
        endmodule
        "#,
    );
    assert!(
        error.contains("read-before-write dependency inside always_comb"),
        "unexpected error: {error}"
    );
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
fn rejects_generate_locals_that_shadow_parameters() {
    let error = cranelift_build_error(
        r#"
        module Top #(parameter P = 0) (output wire y);
            if (1) begin : g
                logic P;
                assign P = 1'b1;
                assign y = P;
            end
        endmodule
        "#,
    );
    assert!(
        error.contains("local data declaration inside conditional-generate"),
        "unexpected error: {error}"
    );
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
        error.contains("local data declaration inside conditional-generate"),
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
        error.contains("type declaration inside conditional-generate"),
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
                always_ff @(posedge clk)
                    assert (d) q <= 1'b1; else q <= 1'b0;
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
    assert!(
        error.contains("unsupported statement inside always_ff"),
        "{error}"
    );
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
fn infers_the_clock_when_reset_precedes_it_in_the_event_list() {
    let source = r#"
        module Top(input logic clk, input logic rst, input logic d, output logic q);
            always_ff @(posedge rst or posedge clk) begin
                if (rst) q <= 1'b0;
                else q <= d;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("reset_first.sv"))], "Top")
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
        module Top(input logic clk, input logic clear, input logic en, output logic q);
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
fn takes_always_ff_else_branch_for_unknown_predicates() {
    let source = r#"
        module Top(input logic clk, input logic sel, output logic q);
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
        module Top(input logic clk, input logic d, output bit q);
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
fn rejects_constructs_that_are_not_yet_lowered() {
    let cases = [
        (
            "ordered port connection",
            r#"
            module Child(input logic a); endmodule
            module Top(input logic a); Child child(a); endmodule
        "#,
        ),
        (
            "ordered parameter assignment",
            r#"
            module Child #(parameter W = 1) (); endmodule
            module Top(); Child #(8) child(); endmodule
        "#,
        ),
        (
            "parameter override expression",
            r#"
            module Child #(parameter W = 1) (output logic [W-1:0] y); assign y = '0; endmodule
            module Top(output logic [7:0] y); Child #(.W(2 ** 3)) child(.y(y)); endmodule
        "#,
        ),
        (
            "module instantiation inside loop-generate",
            r#"
            module Child(); endmodule
            module Top(); for (genvar i = 0; i < 2; i++) Child child(); endmodule
        "#,
        ),
        (
            "control flow inside always_comb",
            r#"
            module Top(input logic s, a, b, output logic y);
                always_comb if (s) y = a; else y = b;
            endmodule
        "#,
        ),
        (
            "unsupported statement inside always_comb",
            r#"
            module Top(input logic a, output logic y);
                always_comb $display("%0d", a);
            endmodule
        "#,
        ),
        (
            "net declaration assignment",
            r#"
            module Top(input logic a, output logic y); wire n = a; assign y = n; endmodule
        "#,
        ),
        (
            "blocking assignment inside always_ff",
            r#"
            module Top(input logic clk, d, output logic q);
                always_ff @(posedge clk) q = d;
            endmodule
        "#,
        ),
        (
            "output port lvalue connection",
            r#"
            module Child(output logic [7:0] y); assign y = 8'hff; endmodule
            module Top(output logic [15:0] y); Child child(.y(y[7:0])); endmodule
        "#,
        ),
        (
            "combinational expression",
            r#"
            module Top(input logic a, output logic y); assign y = unknown(a); endmodule
        "#,
        ),
        (
            "always_ff assignment lowering",
            r#"
            module Top(input logic clk, input logic i, d, output logic [1:0] q);
                always_ff @(posedge clk) q[i] <= d;
            endmodule
        "#,
        ),
        (
            "unsupported statement inside always_ff",
            r#"
            module Top(input logic clk, d, output logic q);
                always_ff @(posedge clk)
                    assert (d) q <= 1'b1; else q <= 1'b0;
            endmodule
        "#,
        ),
        (
            "named port connection expression",
            r#"
            module Child(input logic a); endmodule
            module Top(input logic [3:0] x);
                Child child(.a(x ** 2));
            endmodule
        "#,
        ),
        (
            "loop-generate initializer",
            r#"
            module Top(output wire y);
                for (genvar i = 2 ** 0; i < 2; i++) assign y = 1'b1;
            endmodule
        "#,
        ),
        (
            "loop-generate condition",
            r#"
            module Top(output wire y);
                for (genvar i = 0; i < 2 ** 1; i++) assign y = 1'b1;
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
            "always_ff predicate lowering",
            r#"
            module Top(input logic clk, input logic [3:0] a, b, d, e, output logic [3:0] q);
                always_ff @(posedge clk) begin
                    if (a ** b) q <= d;
                    else q <= e;
                end
            endmodule
        "#,
        ),
        (
            "always_ff case selector lowering",
            r#"
            module Top(input logic clk, input logic [3:0] a, b, d, output logic [3:0] q);
                always_ff @(posedge clk) begin
                    case (a ** b)
                        0: q <= d;
                        default: q <= '0;
                    endcase
                end
            endmodule
        "#,
        ),
        (
            "always_ff assignment lowering",
            r#"
            module Top(input logic clk, input logic [3:0] a, b, output logic [3:0] q);
                always_ff @(posedge clk) q <= a ** b;
            endmodule
        "#,
        ),
        (
            "unpacked array dimension",
            r#"
            module Top(input logic [7:0] mem [0:1], output logic [7:0] y);
                assign y = mem[0];
            endmodule
        "#,
        ),
        (
            "local data declaration inside loop-generate",
            r#"
            module Top(input logic [1:0] a, output logic [1:0] y);
                for (genvar i = 0; i < 2; i++) begin
                    logic tmp;
                    assign tmp = a[i];
                    assign y[i] = tmp;
                end
            endmodule
        "#,
        ),
        (
            "initial construct",
            r#"
            module Top(output logic y); initial y = 1'b1; endmodule
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
            "dependent repeated assignment inside always_comb",
            r#"
            module Top(input logic a, output logic y);
                always_comb begin y = a; y = y + 1'b1; end
            endmodule
        "#,
        ),
        (
            "always and always_latch processes",
            r#"
            module Top(input logic a, output logic y); always @* y = a; endmodule
        "#,
        ),
        (
            "always and always_latch processes",
            r#"
            module Top(input logic a, output logic y); always_latch y = a; endmodule
        "#,
        ),
        (
            "case-generate construct",
            r#"
            module Top #(parameter MODE = 0) (output logic y);
                case (MODE)
                    0: assign y = 1'b0;
                    default: assign y = 1'b1;
                endcase
            endmodule
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
            "always_ff inside loop-generate",
            r#"
            module Top(input logic clk, input logic [1:0] d, output logic [1:0] q);
                for (genvar i = 0; i < 2; i++) always_ff @(posedge clk) q[i] <= d[i];
            endmodule
        "#,
        ),
        (
            "variable declaration initializer",
            r#"
            module Top(output logic y); logic value = 1'b1; assign y = value; endmodule
        "#,
        ),
        (
            "indexed part-select",
            r#"
            module Top(input logic [15:0] a, input logic [3:0] index,
                       output logic [7:0] y);
                assign y = a[index +: 8];
            endmodule
        "#,
        ),
        (
            "non-zero-based or ascending multidimensional packed range",
            r#"
            module Top(input logic [2:1][7:0] a, output logic [7:0] y);
                assign y = a[1];
            endmodule
        "#,
        ),
        (
            "casez, casex, or pattern case inside always_ff",
            r#"
            module Top(input logic clk, input logic [1:0] a, output logic y);
                always_ff @(posedge clk) casez (a) 2'b1?: y <= 1'b1; default: y <= 0; endcase
            endmodule
        "#,
        ),
        (
            "local data declaration inside conditional-generate",
            r#"
            module Top #(parameter ENABLE = 1) (input logic a, output logic y);
                if (ENABLE) begin
                    logic tmp; assign tmp = a; assign y = tmp;
                end else begin
                    logic [1:0] tmp; assign tmp = {a, a}; assign y = tmp[0];
                end
            endmodule
        "#,
        ),
        (
            "packed struct or union type",
            r#"
            module Top(output logic [7:0] y);
                struct packed { logic [3:0] a; logic [3:0] b; } value;
                assign y = value;
            endmodule
        "#,
        ),
        (
            "cast expression",
            r#"
            module Top(input logic a, b, output logic [1:0] y);
                assign y = {logic'(a), b};
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
            "procedural loop inside always_ff",
            r#"
            module Top(input logic clk, d, output logic q);
                always_ff @(posedge clk) repeat (1) q <= d;
            endmodule
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
            "procedural local data declaration",
            r#"
            module Top(input logic a, output logic y);
                always_comb begin logic tmp; tmp = a; y = tmp; end
            endmodule
        "#,
        ),
        (
            "continuous assignment expression",
            r#"
            module Top(input logic [7:0] a, output logic [7:0] y);
                assign y = {<<{a}};
            endmodule
        "#,
        ),
        (
            "conditional function return without else",
            r#"
            module Top(input logic a, output logic y);
                function automatic logic choose(input logic x);
                    if (x) return 1'b1;
                    return 1'b0;
                endfunction
                assign y = choose(a);
            endmodule
        "#,
        ),
        (
            "unsupported function conditional predicate",
            r#"
            module Top(input logic [3:0] a, output logic y);
                function automatic logic choose(input logic [3:0] value);
                    if (value ** 2) return 1'b1;
                    else return 1'b0;
                endfunction
                assign y = choose(a);
            endmodule
        "#,
        ),
        (
            "unsupported function assignment expression",
            r#"
            module Top(input logic [3:0] a, output logic [3:0] y);
                function automatic logic [3:0] square(input logic [3:0] value);
                    logic [3:0] tmp;
                    tmp = value ** 2;
                    return tmp;
                endfunction
                assign y = square(a);
            endmodule
        "#,
        ),
        (
            "unsupported function case selector",
            r#"
            module Top(input logic [3:0] a, output logic y);
                function automatic logic choose(input logic [3:0] value);
                    case (value ** 2)
                        1: return 1'b1;
                        default: return 1'b0;
                    endcase
                endfunction
                assign y = choose(a);
            endmodule
        "#,
        ),
        (
            "unsupported function case item expression",
            r#"
            module Top(input logic [3:0] a, b, output logic y);
                function automatic logic choose(
                    input logic [3:0] value,
                    input logic [3:0] item
                );
                    case (value)
                        item ** 2: return 1'b1;
                        default: return 1'b0;
                    endcase
                endfunction
                assign y = choose(a, b);
            endmodule
        "#,
        ),
        (
            "conditional-generate condition",
            r#"
            module Top #(
                parameter logic [3:0] P = 4'hf
            ) (input logic clk, d, output logic q);
                if (P ** 2) always_ff @(posedge clk) q <= d;
            endmodule
        "#,
        ),
        (
            "unknown conditional-generate condition",
            r#"
            module Top(output logic y);
                for (genvar i = -1; i < 0; i++) begin : outer
                    if (&i) assign y = 1'b1;
                end
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
            "concatenated always_ff assignment target",
            r#"
            module Top(input logic clk, input logic [1:0] d, output logic q1, q0);
                always_ff @(posedge clk) {q1, q0} <= d;
            endmodule
        "#,
        ),
        (
            "output or inout function argument",
            r#"
            module Top(input logic a, output logic y, side);
                function automatic logic f(output logic out, input logic value);
                    out = value;
                    return value;
                endfunction
                assign y = f(side, a);
            endmodule
        "#,
        ),
        (
            "dependent repeated assignment inside always_comb",
            r#"
            module Top(input logic b, d, output logic a, c);
                always_comb begin a = b; c = a; a = d; end
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
            "casez or casex inside function",
            r#"
            module Top(input logic [1:0] a, output logic y);
                function automatic logic f(input logic [1:0] x);
                    casez (x) 2'b1?: return 1'b1; default: return 1'b0; endcase
                endfunction
                assign y = f(a);
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
            "procedural loop inside always_comb",
            r#"
            module Top(input logic a, output logic y); always_comb repeat (1) y = a; endmodule
        "#,
        ),
        (
            "local data declaration inside conditional-generate",
            r#"
            module Top #(parameter ENABLE = 0) (output logic y);
                logic [3:0] local_value;
                if (ENABLE) begin : enabled
                    logic local_value;
                    assign y = local_value;
                end else assign y = local_value[0];
            endmodule
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
            "package-dependent systemverilog module",
            r#"
            package p; parameter W = 8; endpackage
            module Top(output logic [7:0] y); import p::*; logic [W-1:0] value; assign y = value; endmodule
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
            "unknown conditional-generate condition",
            r#"
            module Top #(parameter logic P = 1'bx) (output logic y);
                if (P) assign y = 1'b1;
            endmodule
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
            "always_ff case item expression lowering",
            r#"
            module Top(input logic clk, a, b, output logic q);
                always_ff @(posedge clk)
                    case (a)
                        (b ** 2): q <= 1'b1;
                        default: q <= 1'b0;
                    endcase
            endmodule
        "#,
        ),
        (
            "unsupported statement inside function",
            r#"
            module Top(output logic y);
                function automatic logic f();
                    logic x;
                    x = 1'b0;
                    repeat (1) x = 1'b1;
                    return x;
                endfunction
                assign y = f();
            endmodule
        "#,
        ),
        (
            "conditional-generate condition lowering",
            r#"
            module Top(output logic y);
                if (2 ** 3) assign y = 1'b1;
                else assign y = 1'b0;
            endmodule
        "#,
        ),
        (
            "local data declaration inside conditional-generate",
            r#"
            module Top #(parameter SELECT = 1) (input logic clk, output logic q);
                if (SELECT) begin : selected
                    localparam VALUE = 1'b1;
                    always_ff @(posedge clk) q <= VALUE;
                end else begin : unselected
                    localparam VALUE = 1'b0;
                    always_ff @(posedge clk) q <= VALUE;
                end
            endmodule
        "#,
        ),
        (
            "local data declaration inside conditional-generate",
            r#"
            module Top(output logic a_value, output logic b_value);
                if (1) begin : a
                    logic tmp;
                    assign tmp = 1'b0;
                    assign a_value = tmp;
                end
                if (1) begin : b
                    logic tmp;
                    assign tmp = 1'b1;
                    assign b_value = tmp;
                end
            endmodule
        "#,
        ),
        (
            "unresolved explicit packed width",
            r#"
            module Top #(parameter W = 2 ** 3)
                      (input logic [W-1:0] a, output logic [W-1:0] y);
                assign y = a;
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
            "selected or composite assignment inside function",
            r#"
            module Top(input logic [1:0] a, output logic [1:0] y);
                function automatic logic [1:0] set_bit(input logic [1:0] value);
                    value[0] = 1'b1;
                    return value;
                endfunction
                assign y = set_bit(a);
            endmodule
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
            "always_comb assignment expression",
            r#"
            module Top(input logic [3:0] a, b, output logic [7:0] y);
                always_comb y = a ** b;
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
fn rejects_unrepresentable_dynamic_selects_instead_of_dropping_them() {
    let error = cranelift_build_error(
        r#"
        module Top(input logic [1:0] a, input logic sel, output logic y);
            assign y = a[sel ? 1 : 0];
        endmodule
        "#,
    );
    assert!(
        error.contains("continuous assignment expression"),
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
    assert!(error.contains("module \"Missing\""), "{error}");
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
