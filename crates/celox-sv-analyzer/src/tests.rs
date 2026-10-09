use super::*;

fn eval_test_const_expr(expr: &ir::ConstExpr) -> Option<i128> {
    match expr {
        ir::ConstExpr::Literal(value) => value.parse().ok(),
        ir::ConstExpr::Binary { left, op, right } => {
            let left = eval_test_const_expr(left)?;
            let right = eval_test_const_expr(right)?;
            Some(match op {
                ir::BinaryOp::Add => left + right,
                ir::BinaryOp::Sub => left - right,
                ir::BinaryOp::Mul => left * right,
                ir::BinaryOp::Ge => (left >= right) as i128,
                ir::BinaryOp::Le => (left <= right) as i128,
                ir::BinaryOp::LogicAnd => ((left != 0) && (right != 0)) as i128,
                ir::BinaryOp::LogicOr => ((left != 0) || (right != 0)) as i128,
                _ => return None,
            })
        }
        ir::ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            if eval_test_const_expr(condition)? != 0 {
                eval_test_const_expr(then_expr)
            } else {
                eval_test_const_expr(else_expr)
            }
        }
        _ => None,
    }
}

#[test]
fn caps_aggregate_nested_generate_expansion() {
    for (outer, inner, accepted) in [(2, 3, true), (100, 100, false), (10_000, 10_000, false)] {
        let source = format!(
            r#"
            module Top(output wire y);
                for (genvar i = 0; i < {outer}; i++) begin : outer_loop
                    for (genvar j = 0; j < {inner}; j++) begin : inner_loop
                        assign y = 1'b1;
                    end
                end
            endmodule
        "#
        );
        let syntax = syntax::parse_source(&source, Path::new("nested_generate.sv")).unwrap();
        let result = ast::Source::from_syntax(&syntax);
        if accepted {
            result.expect("small nested loops should expand");
        } else {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("loop-generate unroll limit exceeded")
            );
        }
    }
}

#[test]
fn recognizes_complementary_equality_guards_as_exhaustive() {
    analyze_source(
        r#"
            module Top(input logic outer, input bit s, input logic a, b, c, output logic y);
                always_comb begin
                    if (outer) begin
                        if (s == 0) y = a;
                        if (s != 0) y = b;
                    end else begin
                        y = c;
                    end
                end
            endmodule
        "#,
        Path::new("complementary_equality_guards.sv"),
    )
    .expect("complementary two-state equality guards should define y exhaustively");
}

#[test]
fn recognizes_block_wrapped_complementary_guards_as_exhaustive() {
    analyze_source(
        r#"
            module Top(input logic outer, input bit s, input logic a, b, c, output logic y);
                always_comb begin
                    if (outer) begin
                        if (s) begin y = a; end
                        if (!s) begin y = b; end
                    end else begin
                        y = c;
                    end
                end
            endmodule
        "#,
        Path::new("block_wrapped_complementary_guards.sv"),
    )
    .expect("block-wrapped complementary guards should define y exhaustively");
}

#[test]
fn preserves_complementary_guards_across_harmless_blocks() {
    analyze_source(
        r#"
            module Top(input logic outer, input bit s, input logic a, b, output logic y, z);
                always_comb begin
                    if (outer) begin
                        if (s) y = a;
                        begin z = 1'b0; end
                        if (!s) y = b;
                    end else begin
                        y = a;
                        z = 1'b1;
                    end
                end
            endmodule
        "#,
        Path::new("harmless_block_between_complementary_guards.sv"),
    )
    .expect("a block that cannot change the guard should preserve its proof");
}

#[test]
fn uses_selected_writes_before_conditional_whole_vector_writes() {
    analyze_source(
        r#"
            module Top(input logic c, a, output logic [7:0] x);
                always_comb begin
                    x = '0;
                    x[0] = a;
                    if (c) x = 8'hff;
                end
            endmodule
        "#,
        Path::new("selected_then_conditional_whole.sv"),
    )
    .expect("the preceding selected write should initialize the whole-write fallback");
}

#[test]
fn permits_reads_after_assignments_on_the_same_comb_path() {
    analyze_source(
        r#"
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
        "#,
        Path::new("path_local_comb_read.sv"),
    )
    .expect("each guarded read is preceded by a write on the same path");
}

#[test]
fn sign_extends_negative_literals_in_widening_constant_casts() {
    let ir = analyze_source(
        r#"
            module Top #(parameter V = $bits(logic signed [7:0])'(4'shf)) ();
            endmodule
        "#,
        Path::new("sign_extend_cast.sv"),
    )
    .expect("SV analysis should succeed");
    assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(-1));
}

#[test]
fn sizes_first_dimension_for_size_cast_targets() {
    let ir = analyze_source(
        r#"
            module Top #(
                parameter W = $size(logic [1:0][3:0])'(3'd7),
                parameter B = $bits(logic [1:0][3:0])'(4'd7)
            ) ();
            endmodule
        "#,
        Path::new("size_cast.sv"),
    )
    .expect("SV analysis should succeed");
    // A 2-bit $size target truncates 7 to 3; an 8-bit $bits target
    // keeps it.
    assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(3));
    assert_eq!(ir.modules()[0].parameters()[1].resolved_value(), Some(7));
}

#[test]
fn infers_size_cast_targets_from_selected_expressions() {
    let ir = analyze_source(
        r#"
            module Top(input logic [7:0] a);
                localparam Q = $bits(a[3:0])'(4'hf);
                localparam S = $size(a[3:0])'(4'hf);
            endmodule
        "#,
        Path::new("selected_expression_size_cast.sv"),
    )
    .expect("selected expression types should determine size cast widths");
    assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(15));
    assert_eq!(ir.modules()[0].parameters()[1].resolved_value(), Some(15));
}

#[test]
fn treats_scalar_size_cast_targets_as_one_bit() {
    let ir = analyze_source(
        r#"
            module Top #(parameter W = $size(logic)'(2'd3)) ();
            endmodule
        "#,
        Path::new("scalar_size_cast.sv"),
    )
    .expect("a scalar $size cast target should resolve to one bit");
    assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(1));
}

#[test]
fn resolves_constant_cast_targets_from_module_environments() {
    let alias_ir = analyze_source(
        r#"
            module Top;
                typedef logic [7:0] byte_t;
                localparam P = byte_t'(4'd3);
            endmodule
        "#,
        Path::new("constant_alias_cast_env.sv"),
    )
    .expect("typedef cast target should resolve");
    assert_eq!(
        alias_ir.modules()[0].parameters()[0].resolved_value(),
        Some(3)
    );

    let width_ir = analyze_source(
        r#"
            module Top;
                localparam W = 8;
                localparam Q = W'(4'd3);
            endmodule
        "#,
        Path::new("constant_width_cast_env.sv"),
    )
    .expect("parameter-sized cast target should resolve");
    assert_eq!(
        width_ir.modules()[0].parameters()[1].resolved_value(),
        Some(3)
    );
}

#[test]
fn evaluates_constant_cast_operand_expressions() {
    let ir = analyze_source(
        r#"
            module Top;
                typedef logic [7:0] byte_t;
                localparam A = 3;
                localparam B = byte_t'(A);
                localparam C = byte_t'(1 + 2);
            endmodule
        "#,
        Path::new("constant_cast_operands.sv"),
    )
    .expect("constant cast operands should be evaluated in the module environment");
    assert_eq!(ir.modules()[0].parameters()[1].resolved_value(), Some(3));
    assert_eq!(ir.modules()[0].parameters()[2].resolved_value(), Some(3));
}

#[test]
fn context_sizes_unbased_enum_member_initializers() {
    let ir = analyze_source(
        r#"
            module Top;
                typedef enum logic [3:0] { A = '1 } E;
                logic [A:0] data;
            endmodule
        "#,
        Path::new("unbased_enum_initializer.sv"),
    )
    .expect("an unbased fill should be sized to the enum base type");
    let width = ir.modules()[0]
        .signals()
        .iter()
        .find(|signal| signal.name() == "data")
        .and_then(|signal| signal.r#type().resolved_width());
    assert_eq!(width, Some(16));
}

#[test]
fn preserves_enum_base_types_during_constant_substitution() {
    let ir = analyze_source(
        r#"
            module Top(output logic [31:0] y);
                typedef enum logic [1:0] { A = 2'd0 } E;
                assign y = ~A;
            endmodule
        "#,
        Path::new("typed_enum_constant.sv"),
    )
    .expect("SV analysis should succeed");
    let rhs = ir.modules()[0].assignments()[0].rhs();
    let ir::Expr::Unary { expr, .. } = rhs else {
        panic!("expected enum complement: {rhs:?}");
    };
    assert_eq!(&**expr, &ir::Expr::Literal("2'd0".to_string()));
}

#[test]
fn accepts_casez_nested_under_comb_conditionals() {
    analyze_source(
        r#"
            module Top(input logic en, sel, output logic y);
                always_comb begin
                    y = 1'b0;
                    if (en) casez (sel)
                        1'b?: y = 1'b1;
                    endcase
                end
            endmodule
        "#,
        Path::new("nested_casez.sv"),
    )
    .expect("casez nested under a conditional must analyze");
}

#[test]
fn uses_enum_members_as_module_constants() {
    let ir = analyze_source(
        r#"
            module Top #(
                parameter logic [1:0] BASE = 2'd1
            ) (input logic a, output logic y);
                typedef enum logic [1:0] { N = BASE + 2'd1 } E;
                logic [N-1:0] data;
                always_comb begin
                    if (N != 0) y = a;
                    else y = 1'b0;
                end
            endmodule
        "#,
        Path::new("enum_const_env.sv"),
    )
    .expect("SV analysis should succeed");
    let width = ir.modules()[0]
        .signals()
        .iter()
        .find(|signal| signal.name() == "data")
        .map(|signal| signal.r#type().resolved_width())
        .unwrap();
    assert_eq!(width, Some(2));
}

#[test]
fn resolves_parameters_that_reference_enum_members() {
    let ir = analyze_source(
        r#"
            module Top;
                typedef enum logic [1:0] { N = 2 } E;
                localparam W = N;
                logic [W-1:0] data;
            endmodule
        "#,
        Path::new("enum_parameter_dependency.sv"),
    )
    .expect("parameters should be re-resolved after enum collection");
    let width = ir.modules()[0]
        .signals()
        .iter()
        .find(|signal| signal.name() == "data")
        .and_then(|signal| signal.r#type().resolved_width());
    assert_eq!(width, Some(2));
}

#[test]
fn re_resolves_parameters_between_enum_declarations() {
    let ir = analyze_source(
        r#"
            module Top;
                typedef enum logic [1:0] { A = 2 } E0;
                localparam W = A + 1;
                typedef enum logic [3:0] { B = W } E1;
                logic [B-1:0] data;
            endmodule
        "#,
        Path::new("enum_parameter_enum_dependency.sv"),
    )
    .expect("parameters between enum declarations should be available to later enums");
    let width = ir.modules()[0]
        .signals()
        .iter()
        .find(|signal| signal.name() == "data")
        .and_then(|signal| signal.r#type().resolved_width());
    assert_eq!(width, Some(3));
}

#[test]
fn rebuilds_typedefs_after_preceding_enum_members() {
    let ir = analyze_source(
        r#"
            module Top;
                typedef enum int { W = 3 } E;
                typedef logic [W'(2):0] B;
                typedef enum B { A = 3'b101 } F;
                logic [A-1:0] data;
            endmodule
        "#,
        Path::new("enum_dependent_typedef.sv"),
    )
    .expect("later enum bases should use typedefs rebuilt from preceding members");
    let width = ir.modules()[0]
        .signals()
        .iter()
        .find(|signal| signal.name() == "data")
        .and_then(|signal| signal.r#type().resolved_width());
    assert_eq!(width, Some(5));
}

#[test]
fn treats_constant_equivalent_mux_case_selectors_as_two_state() {
    analyze_source(
        r#"
            module Top(input logic c, a, output logic y);
                localparam logic P = 0;
                localparam logic Q = 0;
                always_comb begin
                    case (c ? P : Q)
                        1'b0: y = a;
                    endcase
                end
            endmodule
        "#,
        Path::new("constant_equivalent_mux_case.sv"),
    )
    .expect("equal constant mux arms should make the case selector exhaustive");
}

#[test]
fn skips_unreachable_sparse_case_items_with_default() {
    analyze_source(
        r#"
            module Top(input bit [1:0] s, input logic a, b, output logic y);
                always_comb begin
                    case (s)
                        2'd0: y = a;
                        2'd0: ;
                        default: y = b;
                    endcase
                end
            endmodule
        "#,
        Path::new("sparse_case_duplicate_with_default.sv"),
    )
    .expect("unreachable duplicate items should not prevent definite assignment");
}

#[test]
fn skips_duplicate_case_items_for_four_state_selectors() {
    analyze_source(
        r#"
            module Top(input logic s, input logic a, b, output logic y);
                always_comb begin
                    case (s)
                        1'b0: y = a;
                        1'b0: ;
                        default: y = b;
                    endcase
                end
            endmodule
        "#,
        Path::new("four_state_duplicate_case_item.sv"),
    )
    .expect("an unreachable duplicate case item should not infer a latch");
}

#[test]
fn folds_compound_four_state_constant_case_selectors_for_coverage() {
    analyze_source(
        r#"
            module Top(input logic c, a, b, output logic y);
                always_comb begin
                    case (1'bx | 1'b0)
                        1'bx: if (c) y = a; else y = b;
                    endcase
                end
            endmodule
        "#,
        Path::new("compound_four_state_constant_case_selector.sv"),
    )
    .expect("a compound constant X selector should preserve its mask for coverage");
}

#[test]
fn expands_constant_function_predicates_for_definite_assignments() {
    analyze_source(
        r#"
            module Top(input logic outer, a, b, output logic y);
                function automatic bit one();
                    return 1'b1;
                endfunction
                always_comb begin
                    if (outer) begin
                        if (one()) y = a;
                    end else begin
                        y = b;
                    end
                end
            endmodule
        "#,
        Path::new("constant_function_definite_assignment.sv"),
    )
    .expect("a constant-true function predicate should make the inner write definite");
}

#[test]
fn folds_concatenated_four_state_constant_case_selectors_for_coverage() {
    analyze_source(
        r#"
            module Top(input logic a, output logic y);
                always_comb begin
                    case ({1'bx})
                        1'bx: y = a;
                    endcase
                end
            endmodule
        "#,
        Path::new("concatenated_four_state_constant_case_selector.sv"),
    )
    .expect("a constant concatenation should retain its X mask for case coverage");
}

#[test]
fn recognizes_mixed_boolean_and_zero_equality_complements() {
    analyze_source(
        r#"
            module Top(input logic outer, input bit s, input logic a, b, c, output logic y);
                always_comb begin
                    if (outer) begin
                        if (s) y = a;
                        if (s == 0) y = b;
                    end else begin
                        y = c;
                    end
                end
            endmodule
        "#,
        Path::new("mixed_boolean_equality_complements.sv"),
    )
    .expect("a two-state predicate and its zero equality should be complementary");
}

#[test]
fn resolves_function_types_in_size_cast_targets() {
    let ir = analyze_source(
        r#"
            module Top(output logic [7:0] y);
                function automatic logic [7:0] f();
                    return 8'h00;
                endfunction
                localparam P = $bits(f())'(16'hffff);
                always_comb y = P;
            endmodule
        "#,
        Path::new("function_size_cast_target.sv"),
    )
    .expect("a size-function cast target should use the function return type");
    assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(0xff));

    let ir = analyze_source(
        r#"
            module Top(output logic [7:0] y);
                function automatic logic [$bits(g())'(7):0] f();
                    return 8'h00;
                endfunction
                function automatic logic [7:0] g();
                    return 8'h00;
                endfunction
                localparam P = $bits(f())'(16'hffff);
                always_comb y = P;
            endmodule
        "#,
        Path::new("dependent_function_size_cast_target.sv"),
    )
    .expect("function return metadata discovery should resolve dependencies without recursion");
    assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(0xff));
}

#[test]
fn preserves_function_return_dimensions_in_size_cast_targets() {
    let ir = analyze_source(
        r#"
            module Top(output logic [1:0] y);
                function automatic logic [1:0][3:0] f();
                    return '0;
                endfunction
                localparam P = $size(f())'(8'hff);
                always_comb y = P;
            endmodule
        "#,
        Path::new("function_dimension_size_cast_target.sv"),
    )
    .expect("$size should use the first packed function return dimension");
    assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(3));

    let ir = analyze_source(
        r#"
            module Top(output logic [1:0] y);
                function automatic logic [$bits(g())'(1):0][3:0] f();
                    return '0;
                endfunction
                function automatic logic [7:0] g();
                    return '0;
                endfunction
                localparam P = $size(f())'(8'hff);
                always_comb y = P;
            endmodule
        "#,
        Path::new("dependent_function_dimension_size_cast_target.sv"),
    )
    .expect("dependent function return dimensions should remain available to $size");
    assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(3));
}

#[test]
fn substitutes_loop_indices_when_tracking_comb_writes() {
    analyze_source(
        r#"
            module Top(input logic outer, q, a, b, c, output logic y);
                logic [1:0] s;
                always_comb begin
                    s = {q, q};
                    if (outer) begin
                        if (s[0] === 1'b1) y = a;
                        for (int i = 1; i < 2; i++) s[i] = b;
                        if (s[0] !== 1'b1) y = c;
                    end else begin
                        y = a;
                    end
                end
            endmodule
        "#,
        Path::new("indexed_loop_comb_writes.sv"),
    )
    .expect("a concrete nonoverlapping loop write should preserve the guard proof");
}

#[test]
fn analyzes_comb_processes_with_generate_local_constants() {
    analyze_source(
        r#"
            module Top(input logic a, output logic y);
                if (1) begin : selected
                    localparam bit S = 1'b0;
                    always_comb begin
                        case (S)
                            1'b0: y = a;
                        endcase
                    end
                end
            endmodule
        "#,
        Path::new("generate_local_comb_constant.sv"),
    )
    .expect("generate-local constants should participate in always_comb analysis");
}

#[test]
fn folds_four_state_conditional_case_selectors_for_coverage() {
    analyze_source(
        r#"
            module Top(input logic a, output logic y);
                always_comb begin
                    case (1'bx ? 1'b0 : 1'b1)
                        1'bx: y = a;
                    endcase
                end
            endmodule
        "#,
        Path::new("conditional_four_state_case_selector.sv"),
    )
    .expect("a constant conditional selector should retain its merged X mask");
}

#[test]
fn types_unpacked_array_elements_in_size_cast_targets() {
    let ir = analyze_source(
        r#"
            module Top(output logic [7:0] y);
                logic [7:0] a[2];
                localparam P = $bits(a[0])'(16'hffff);
                always_comb y = P;
            endmodule
        "#,
        Path::new("array_element_size_cast_target.sv"),
    )
    .expect("size-function expression typing should include module variable dimensions");
    assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(0xff));
}

#[test]
fn preserves_logical_constant_case_selector_masks() {
    for selector in [
        "1'bx && 1'b1",
        "1'b0 || 1'bz",
        "(1'bx && 1'b1) || 1'b0",
        "!(1'bx || 1'b0)",
    ] {
        let source = format!(
            "module Top(input logic a, output logic y); \
             always_comb case ({selector}) 1'bx: y = a; endcase endmodule"
        );
        analyze_source(&source, Path::new("logical_constant_case.sv"))
            .unwrap_or_else(|error| panic!("{selector}: {error}"));
    }
}

#[test]
fn resolves_function_scope_size_casts() {
    for function in [
        "function automatic logic [7:0] f(input logic [3:0] x);
         return $bits(x)'(0); endfunction",
        "function automatic logic [7:0] f;
         input logic [3:0] x; return $bits(x)'(0); endfunction",
        "function automatic logic [7:0] f(input logic [3:0] x);
         logic [3:0] local_value; return $size(local_value)'(0); endfunction",
        "function automatic logic [7:0] f(input logic [1:0][3:0] x);
         return $bits(x[0])'(0); endfunction",
    ] {
        let source = format!(
            "module Top(input logic a, output logic y);
             logic [15:0] x;
             {function}
             always_comb case (f(0)) 8'h00: y = a; endcase endmodule"
        );
        analyze_source(&source, Path::new("function_scope_size_cast.sv"))
            .unwrap_or_else(|error| panic!("{function}: {error}"));
    }
}

#[test]
fn preserves_use_site_dimensions_in_parameter_alias_types() {
    let source = r#"
        module Top #(parameter W = 4, N = 2) ();
            typedef logic [3:0] nibble_t;
            typedef logic signed [3:0] signed_nibble_t;
            parameter nibble_t [1:0] P = 8'hab;
            localparam nibble_t [W'(2):W'(1)] L = P;
            localparam signed_nibble_t [1:0] S = 8'hab;
            localparam nibble_t [1:0] F = '1;
            localparam logic [nibble_t'(7):nibble_t'(0)] B = 8'hab;
            localparam BITS = $bits(P);
            parameter nibble_t [N-1:0] R = 16'hcdef;
        endmodule
    "#;
    for (overrides, p_value, r_width, r_value) in [
        (HashMap::default(), 0xab, 8, 0xef),
        (
            [("N".to_string(), 4)].into_iter().collect(),
            0xab,
            16,
            0xcdef,
        ),
    ] {
        let ir = analyze_source_with_module_parameter_overrides(
            source,
            Path::new("parameter_alias_use_site_dimensions.sv"),
            "Top",
            &overrides,
        )
        .expect("parameter aliases should retain use-site packed dimensions");
        let parameters = ir.modules()[0].parameters();
        for (name, width, signed, value) in [
            ("P", Some(8), Some(false), p_value),
            ("L", Some(8), Some(false), p_value),
            ("S", Some(8), Some(false), 0xab),
            ("F", Some(8), Some(false), 255),
            ("B", Some(8), Some(false), 0xab),
            ("BITS", None, None, 8),
            ("R", Some(r_width), Some(false), r_value),
        ] {
            let parameter = parameters
                .iter()
                .find(|parameter| parameter.name() == name)
                .unwrap();
            assert_eq!(parameter.declared_width(), width, "{name}");
            assert_eq!(parameter.declared_signed(), signed, "{name}");
            assert_eq!(parameter.resolved_value(), Some(value), "{name}");
        }
    }
}

#[test]
fn body_parameters_are_local_with_a_parameter_port_list() {
    // IEEE 1800-2023 6.20.1: a parameter port list, even an empty one, turns
    // a `parameter` in the module body into a localparam.
    for header in ["#(parameter N = 2)", "#()"] {
        let source = format!("module Top {header} (); parameter P = 1; endmodule");
        let overrides = [("P".to_string(), 2)].into_iter().collect();
        let error = analyze_source_with_module_parameter_overrides(
            &source,
            Path::new("body_parameter_override.sv"),
            "Top",
            &overrides,
        )
        .expect_err("a body parameter must not be overridable");
        assert!(error.to_string().contains("localparam override"), "{error}");
    }
    let overrides = [("P".to_string(), 2)].into_iter().collect();
    let ir = analyze_source_with_module_parameter_overrides(
        "module Top (); parameter P = 1; endmodule",
        Path::new("body_parameter_override.sv"),
        "Top",
        &overrides,
    )
    .expect("without a parameter port list a body parameter is overridable");
    let parameter = &ir.modules()[0].parameters()[0];
    assert_eq!(parameter.resolved_value(), Some(2));
}

#[test]
fn preserves_unsigned_128_bit_enum_expression_results() {
    let ir = analyze_source(
        "module Top(output logic [127:0] y);
         typedef enum logic [127:0] {
             A = 128'h7fff_ffff_ffff_ffff_ffff_ffff_ffff_ffff + 128'h1
         } E;
         localparam logic [127:0] P = A;
         assign y = P;
         endmodule",
        Path::new("unsigned_128_bit_enum.sv"),
    )
    .expect("unsigned enum arithmetic must retain all 128 bits");
    assert_eq!(
        ir.modules()[0].parameters()[0].resolved_value(),
        Some(i128::MIN)
    );
}

#[test]
fn rejects_nonblocking_comb_assignments_before_coverage() {
    // These are intentionally unsupported: treating NBA writes as blocking
    // writes would change reads of the destination within the same process.
    for body in [
        "if (s) y <= a; else y <= b;",
        "case (s) 1'b0: y <= a; default: y <= b; endcase",
        "if (s) begin if (t) y <= a; else y <= b; end else y <= b;",
        "if (s) y <= a;",
    ] {
        let source = format!(
            "module Top(input logic s, t, a, b, output logic y); \
             always_comb begin {body} end endmodule"
        );
        let error = analyze_source(&source, Path::new("nonblocking_comb.sv"))
            .expect_err("nonblocking assignments must fail before latch analysis");
        assert!(
            error
                .to_string()
                .contains("nonblocking assignment inside always_comb"),
            "{body}: {error}"
        );
    }
}

#[test]
fn preserves_use_site_dimensions_in_function_alias_types() {
    let ir = analyze_source(
        r#"
            module Top #(parameter W = 4)(output logic [7:0] y);
                typedef logic [3:0] nibble_t;
                function automatic nibble_t [W'(2):W'(1)] f();
                    return 8'hab;
                endfunction
                localparam BITS = $bits(f())'(16'hffff);
                localparam SIZE = $size(f())'(8'hff);
                always_comb y = f();
            endmodule
        "#,
        Path::new("function_alias_use_site_dimensions.sv"),
    )
    .expect("function aliases should retain their use-site packed dimensions");
    let parameters = ir.modules()[0].parameters();
    assert_eq!(parameters[1].resolved_value(), Some(255));
    assert_eq!(parameters[2].resolved_value(), Some(3));
}

#[test]
fn preserves_selected_size_argument_dimensions() {
    for (argument, expected) in [
        ("a", 5),
        ("a[0]", 3),
        ("a[0][0]", 2),
        ("a[0][0][0]", 4),
        ("a[0][0][0][0]", 1),
        ("(a[0][0])", 2),
        ("a[0][0][1:0]", 2),
        ("a[0][0][0][2:1]", 2),
    ] {
        let source = format!(
            "module Top(output logic [31:0] y); \
             logic [1:0][3:0] a[5][3]; \
             localparam P = $size({argument})'(32'hffff_ffff); \
             always_comb y = P; endmodule"
        );
        let ir = analyze_source(&source, Path::new("selected_size_dimensions.sv"))
            .unwrap_or_else(|error| panic!("{argument}: {error}"));
        assert_eq!(
            ir.modules()[0].parameters()[0].resolved_value(),
            Some((1 << expected) - 1),
            "$size({argument})"
        );
    }
}

#[test]
fn resolves_alias_casts_in_generate_local_parameters() {
    for declaration in ["localparam S = t'(4);", "localparam t S = 4;"] {
        let source = format!(
            r#"
                module Top(output logic y);
                    typedef logic [1:0] t;
                    if (1) begin : selected
                        {declaration}
                        if (S) begin : disabled
                            function automatic logic invalid(input real x);
                                return x;
                            endfunction
                        end else begin : enabled
                            assign y = 1'b1;
                        end
                    end
                endmodule
            "#
        );
        let ir = analyze_source(&source, Path::new("generate_local_alias_cast.sv"))
            .expect("generate-local aliases must select the reachable branch");
        assert_eq!(ir.modules()[0].comb_processes().len(), 1);
    }
}

#[test]
fn preserves_known_conditional_case_selector_types() {
    // An unsigned label makes the whole comparison unsigned, so a signed arm
    // of the selector is zero-extended (IEEE 1800-2023 11.8.2, 12.5).
    for (selector, label) in [
        ("1'b1 ? 1'sb1 : 2'sb00", "2'b01"),
        ("1'b0 ? 2'sb00 : 1'sb1", "2'b01"),
        ("1'b1 ? 1'sb1 : 2'b00", "2'b01"),
        ("1'b1 ? 1'sbx : 2'sb00", "2'b0x"),
        ("1'b1 ? 1'sbz : 2'b00", "2'b0z"),
        ("1'b1 ? '1 : 2'b00", "2'b11"),
        ("1'b1 ? 1'sb1 : 2'sb00", "2'sb11"),
    ] {
        let source = format!(
            "module Top(input logic a, output logic y); \
             always_comb case ({selector}) {label}: y = a; endcase endmodule"
        );
        analyze_source(&source, Path::new("known_conditional_case.sv"))
            .unwrap_or_else(|error| panic!("{selector}: {error}"));
    }
}

#[test]
fn resolves_size_casts_in_declaration_ranges_without_recursion() {
    for declaration in [
        "output logic [$bits(f())'(7):0] y",
        "output logic [$size(f())'(7):0] y",
    ] {
        let source = format!(
            "module Top({declaration}); \
             function logic [7:0] f(); return '0; endfunction \
             logic [$bits(f())'(7):0] a; \
             always_comb begin a = '1; y = a; end endmodule"
        );
        let ir = analyze_source(&source, Path::new("declaration_size_cast.sv"))
            .expect("size casts must not rebuild the declaration recursively");
        assert_eq!(
            ir.modules()[0].ports()[0].r#type().resolved_width(),
            Some(8)
        );
        assert_eq!(
            ir.modules()[0].signals()[0].r#type().resolved_width(),
            Some(8)
        );
    }
}

#[test]
fn expands_function_calls_in_case_labels_for_coverage() {
    analyze_source(
        r#"
            module Top(input logic outer, a, b, output logic y);
                function automatic bit zero();
                    return 1'b0;
                endfunction
                always_comb begin
                    if (outer) begin
                        case (1'b0)
                            zero(): y = a;
                        endcase
                    end else begin
                        y = b;
                    end
                end
            endmodule
        "#,
        Path::new("function_case_label_coverage.sv"),
    )
    .expect("a folded function case label should make the branch exhaustive");
}

#[test]
fn skips_inactive_generate_blocks_with_parameter_casts() {
    for condition in ["W'(4)", "select_t'(4)"] {
        let source = format!(
            r#"
                module Top #(parameter W = 2)(output logic y);
                    typedef logic [W-1:0] select_t;
                    if ({condition}) begin : disabled
                        function automatic logic invalid(input real x);
                            return x;
                        endfunction
                    end else begin : enabled
                        assign y = 1'b1;
                    end
                endmodule
            "#
        );
        let ir = analyze_source(&source, Path::new("inactive_generate_cast.sv"))
            .expect("cast truncation should select the supported generate branch");
        assert_eq!(ir.modules()[0].comb_processes().len(), 1);
    }
}

#[test]
fn skips_inactive_loop_generate_blocks_with_parameter_casts() {
    let source = r#"
        module Top #(parameter W = 2)(output logic y);
            typedef logic [W-1:0] select_t;
            for (genvar i = W'(0); i < select_t'(4); i += W'(1)) begin : disabled
                function automatic logic invalid(input real x);
                    return x;
                endfunction
            end
            assign y = 1'b1;
        endmodule
    "#;
    let ir = analyze_source(source, Path::new("inactive_loop_generate_cast.sv"))
        .expect("a zero-iteration loop must skip unsupported declarations");
    assert_eq!(ir.modules()[0].comb_processes().len(), 1);
}

#[test]
fn unrolled_bit_writes_do_not_grow_tracked_value_exponentially() {
    // Each bit write used to reference the previously tracked value twice
    // (upper and lower slice), so N writes built an O(2^N) expression tree.
    let start = std::time::Instant::now();
    analyze_source(
        r#"
            module Top(input logic [15:0] a, output logic [15:0] y);
                always_comb begin
                    y = 16'd0;
                    for (int i = 0; i < 16; i++)
                        y[i] = a[i];
                end
            endmodule
        "#,
        Path::new("unrolled_bit_writes.sv"),
    )
    .expect("a loop of constant-index bit writes must analyze");
    assert!(
        start.elapsed() < std::time::Duration::from_secs(2),
        "analysis took {:?}",
        start.elapsed()
    );
}

#[test]
fn guarded_partial_writes_keep_tracked_value_linear() {
    // Each guarded lane write used to reference the tracked value three times.
    let mut source = String::from(
        "module Top(input logic en, input logic [4:0] sel, input logic [5:0] v, \
         input logic [95:0] base, output logic [95:0] lanes);\n\
         always_comb begin lanes = base; if (en) begin\n",
    );
    for lane in 0..16 {
        source.push_str(&format!(
            "if (sel == 5'd{lane}) lanes[{} +: 6] = v;\n",
            lane * 6
        ));
    }
    source.push_str("end end endmodule\n");
    let start = std::time::Instant::now();
    analyze_source(&source, Path::new("guarded_lane_writes.sv"))
        .expect("guarded lane writes must analyze");
    assert!(
        start.elapsed() < std::time::Duration::from_secs(10),
        "analysis took {:?}",
        start.elapsed()
    );
}

#[test]
fn accepts_runtime_casts_and_signedness_system_functions() {
    for (name, body) in [
        ("size cast", "assign o = 16'(a);"),
        ("narrowing size cast", "assign o = 16'(4'(a));"),
        ("signing cast", "assign o = signed'(a);"),
        ("int cast", "assign o = int'(a);"),
        ("$signed", "assign o = $signed(a);"),
        ("$unsigned in always_comb", "always_comb o = $unsigned(a);"),
    ] {
        let source =
            format!("module Top(input logic [7:0] a, output logic [31:0] o); {body} endmodule");
        analyze_source(&source, Path::new("runtime_cast.sv"))
            .unwrap_or_else(|error| panic!("{name} must analyze: {error}"));
    }
}

#[test]
fn restricts_enum_alias_types_to_the_declared_base() {
    let ir = analyze_source(
        r#"
            module Top(output E y);
                typedef enum logic { A = int'(1) } E;
                always_comb y = A;
            endmodule
        "#,
        Path::new("enum_member_cast_type_is_not_base.sv"),
    )
    .expect("types in enum member initializers must not replace the enum base");
    assert_eq!(
        ir.modules()[0].ports()[0].r#type().resolved_width(),
        Some(1)
    );
    assert!(!ir.modules()[0].ports()[0].r#type().is_signed());
}

#[test]
fn recognizes_complete_four_state_cases() {
    analyze_source(
        r#"
            module Top(input logic s, input logic a, output logic y);
                always_comb begin
                    case (s)
                        1'b0: y = a;
                        1'b1: y = a;
                        1'bx: y = a;
                        1'bz: y = a;
                    endcase
                end
            endmodule
        "#,
        Path::new("complete_four_state_case.sv"),
    )
    .expect("all four states should exhaust a one-bit logic selector");
}

#[test]
fn recognizes_exhaustive_constant_four_state_cases() {
    analyze_source(
        r#"
            module Top(input logic c, a, b, output logic y);
                always_comb begin
                    case (1'bx)
                        1'bx: if (c) y = a; else y = b;
                    endcase
                end
            endmodule
        "#,
        Path::new("constant_four_state_case_selector.sv"),
    )
    .expect("a matching X-valued constant case item should be exhaustive");
}

#[test]
fn expands_constant_function_case_selectors_for_coverage() {
    analyze_source(
        r#"
            module Top(input logic a, output logic y);
                function automatic bit f();
                    return 1'b0;
                endfunction
                always_comb begin
                    case (f())
                        1'b0: y = a;
                    endcase
                end
            endmodule
        "#,
        Path::new("constant_function_case_selector.sv"),
    )
    .expect("a constant function selector should make its matching item exhaustive");

    analyze_source(
        r#"
            module Top(input logic a, output logic y);
                function automatic logic f();
                    return 1'bx;
                endfunction
                always_comb begin
                    case (f())
                        1'bx: y = a;
                    endcase
                end
            endmodule
        "#,
        Path::new("constant_unknown_function_case_selector.sv"),
    )
    .expect("an X-valued constant function selector should preserve its mask");
}

#[test]
fn expands_function_calls_in_procedural_lvalue_indices() {
    analyze_source(
        r#"
            module Top(input bit index, input logic data, output logic [1:0] x);
                function automatic bit idx();
                    return index;
                endfunction
                always_comb begin
                    x = '0;
                    x[idx()] = data;
                end
            endmodule
        "#,
        Path::new("function_lvalue_index.sv"),
    )
    .expect("a supported function call in an lvalue index should be expanded");
}

#[test]
fn resolves_value_dependent_type_parameter_defaults() {
    let ir = analyze_source(
        r#"
            module Top #(
                parameter W = 8,
                parameter type T = logic [W'(7):0]
            ) (output T y);
                always_comb y = 8'hff;
            endmodule
        "#,
        Path::new("value_dependent_type_parameter.sv"),
    )
    .expect("a type parameter default should use preceding value parameters");
    assert_eq!(
        ir.modules()[0].ports()[0].r#type().resolved_width(),
        Some(8)
    );
}

#[test]
fn materializes_static_loop_indices_in_definite_write_targets() {
    analyze_source(
        r#"
            module Top(input logic c, a, b, output logic [1:0] x);
                always_comb begin
                    if (c) begin
                        x[0] = a;
                        x[1] = a;
                    end else begin
                        for (int i = 0; i < 2; i++) x[i] = b;
                    end
                end
            endmodule
        "#,
        Path::new("materialized_definite_loop_targets.sv"),
    )
    .expect("each static loop iteration should contribute its concrete target");
}

#[test]
fn preserves_named_constant_casts_in_packed_ranges() {
    let ir = analyze_source(
        r#"
            module Top #(
                parameter W = 8
            ) (
                output logic [W'(15):0] y
            );
            endmodule
        "#,
        Path::new("named_cast_packed_range.sv"),
    )
    .expect("named constant casts should lower with the module environment");
    assert_eq!(
        ir.modules()[0].ports()[0].r#type().resolved_width(),
        Some(16)
    );
}

#[test]
fn resolves_named_casts_while_collecting_parameter_ranges() {
    let ir = analyze_source(
        r#"
            module Top #(
                parameter W = 3,
                parameter logic [W'(2):0] P = 3'b100
            ) ();
            endmodule
        "#,
        Path::new("named_cast_parameter_range.sv"),
    )
    .expect("parameter ranges should use preceding parameters in named casts");
    let parameter = &ir.modules()[0].parameters()[1];
    assert_eq!(parameter.declared_width(), Some(3));
    assert_eq!(parameter.resolved_value(), Some(4));
}

#[test]
fn preserves_named_constant_casts_in_unpacked_ranges() {
    let ir = analyze_source(
        r#"
            module Top #(
                parameter W = 2
            ) (
                output logic y [W'(1):0]
            );
            endmodule
        "#,
        Path::new("named_cast_unpacked_range.sv"),
    )
    .expect("named constant casts in unpacked dimensions should use the module environment");
    assert_eq!(
        ir.modules()[0].ports()[0].r#type().resolved_width(),
        Some(2)
    );
}

#[test]
fn preserves_operand_signedness_for_named_numeric_size_casts() {
    let ir = analyze_source(
        r#"
            module Top;
                localparam W = 8;
                localparam P = W'(4'shf);
                localparam Q = 16'(P);
            endmodule
        "#,
        Path::new("named_numeric_size_cast.sv"),
    )
    .expect("named numeric size casts should preserve operand signedness");
    let parameters = ir.modules()[0].parameters();
    assert_eq!(parameters[1].resolved_value(), Some(-1));
    assert_eq!(parameters[1].resolved_signed(), Some(true));
    assert_eq!(parameters[2].resolved_value(), Some(-1));
}

#[test]
fn applies_typedef_signedness_to_constant_primary_cast_targets() {
    let ir = analyze_source(
        r#"
            module Top;
                typedef logic signed [7:0] S;
                localparam P = S'(8'hff);
            endmodule
        "#,
        Path::new("constant_primary_typedef_cast.sv"),
    )
    .expect("a typedef cast should use the typedef signedness");
    let parameter = &ir.modules()[0].parameters()[0];
    assert_eq!(parameter.resolved_value(), Some(-1));
    assert_eq!(parameter.resolved_signed(), Some(true));
}

#[test]
fn preserves_casted_ranges_while_collecting_typedefs() {
    let ir = analyze_source(
        r#"
            module Top #(parameter W = 8);
                typedef logic [W'(15):0] T;
                T x;
            endmodule
        "#,
        Path::new("casted_typedef_range.sv"),
    )
    .expect("typedef ranges should use the populated module environment");
    assert_eq!(
        ir.modules()[0].signals()[0].r#type().resolved_width(),
        Some(16)
    );
}

#[test]
fn preserves_named_casts_in_constant_select_indices() {
    let ir = analyze_source(
        r#"
            module Top;
                localparam W = 1;
                localparam logic [1:0] A = 2'b10;
                localparam B = A[W'(0)];
            endmodule
        "#,
        Path::new("casted_constant_select.sv"),
    )
    .expect("constant select indices should use the populated module environment");
    assert_eq!(ir.modules()[0].parameters()[2].resolved_value(), Some(0));
}

#[test]
fn converts_unknowns_when_constant_casting_to_two_state_types() {
    let ir = analyze_source(
        r#"
            module Top;
                typedef bit [1:0] two_t;
                localparam logic [1:0] P = two_t'(2'bx1);
            endmodule
        "#,
        Path::new("two_state_constant_cast.sv"),
    )
    .expect("two-state constant casts should convert unknown bits to zero");
    assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(1));
}

#[test]
fn resolves_typedef_declared_parameter_types() {
    let ir = analyze_source(
        r#"
            module Top;
                typedef enum logic signed [1:0] { Z = 0 } E;
                localparam E P = '0;
                localparam B = $bits(P);
            endmodule
        "#,
        Path::new("typedef_declared_parameter.sv"),
    )
    .expect("typedef-declared parameters should retain their declared type");
    let parameters = ir.modules()[0].parameters();
    assert_eq!(parameters[0].declared_width(), Some(2));
    assert_eq!(parameters[0].declared_signed(), Some(true));
    assert_eq!(parameters[1].resolved_value(), Some(2));
}

#[test]
fn preserves_enum_types_in_instance_parameter_overrides() {
    let ir = analyze_source(
        r#"
            module Child #(parameter P = 0) ();
            endmodule

            module Top;
                typedef enum logic signed [1:0] { A = 2'b10 } E;
                Child #(.P(A)) child();
            endmodule
        "#,
        Path::new("typed_enum_parameter_override.sv"),
    )
    .expect("enum parameter overrides should retain their declared type");
    let top = ir
        .modules()
        .iter()
        .find(|module| module.name() == "Top")
        .expect("Top module should exist");
    assert_eq!(
        top.instances()[0].parameter_overrides()[0].value(),
        Some(&ir::ConstExpr::Literal("2'sd2".to_string()))
    );
}

#[test]
fn rejects_conditional_predicate_conjunction_terms() {
    let error = analyze_source(
        r#"
            module Top(input logic a, b, output logic y);
                always_comb begin
                    if (a &&& b) y = 1'b1;
                    else y = 1'b0;
                end
            endmodule
        "#,
        Path::new("predicate_conjunction.sv"),
    )
    .expect_err("unsupported predicate conjunctions must not be partially lowered")
    .to_string();
    assert!(
        error.contains("`&&&` in a condition"),
        "unexpected error: {error}"
    );
}

#[test]
fn analyzes_basic_sv_module_name() {
    let ir = analyze_source(
        r#"
            module Top(input logic [7:0] a, output logic y);
                assign y = a;
            endmodule
        "#,
        Path::new("Top.sv"),
    )
    .expect("SV analysis should succeed");

    assert_eq!(ir.modules()[0].name(), "Top");
    assert_eq!(ir.modules()[0].ports().len(), 2);
    assert_eq!(ir.modules()[0].ports()[0].name(), "a");
    assert_eq!(
        ir.modules()[0].ports()[0].direction(),
        ir::PortDirection::Input
    );
    assert_eq!(
        ir.modules()[0].ports()[0].r#type().kind(),
        ir::TypeKind::Logic
    );
    assert_eq!(ir.modules()[0].ports()[0].r#type().packed_ranges().len(), 1);
    assert_eq!(
        ir.modules()[0].ports()[0].r#type().packed_ranges()[0].left(),
        &ir::ConstExpr::Literal("7".to_string())
    );
    assert_eq!(
        ir.modules()[0].ports()[0].r#type().packed_ranges()[0].right(),
        &ir::ConstExpr::Literal("0".to_string())
    );
    assert_eq!(
        ir.modules()[0].ports()[0].r#type().resolved_width(),
        Some(8)
    );
    assert_eq!(ir.modules()[0].ports()[1].name(), "y");
    assert_eq!(
        ir.modules()[0].ports()[1].direction(),
        ir::PortDirection::Output
    );
    assert_eq!(
        ir.modules()[0].ports()[1].r#type().kind(),
        ir::TypeKind::Logic
    );
    assert_eq!(
        ir.modules()[0].ports()[1].r#type().resolved_width(),
        Some(1)
    );
}

#[test]
fn preserves_operand_signedness_for_size_casts() {
    let ir = analyze_source(
        r#"
            module Top(output logic y);
                assign y = (8'(0) < -1);
            endmodule
        "#,
        Path::new("size_cast_signedness.sv"),
    )
    .expect("size casts should preserve operand signedness");
    let expression = ir.modules()[0].comb_processes()[0].assignments()[0].rhs();
    let ir::Expr::Binary {
        op: ir::BinaryOp::Lt,
        left,
        ..
    } = expression
    else {
        panic!("expected a signed less-than comparison, got {expression:?}");
    };
    assert!(matches!(
        left.as_ref(),
        ir::Expr::Resize {
            width: 8,
            signed: true,
            ..
        }
    ));
}

#[test]
fn tracks_default_nettype_state_for_each_module() {
    let source = r#"
        `default_nettype none
        module First(); endmodule
        `default_nettype wire
        module Second(); endmodule
        `default_nettype none
        `resetall
        module Third(); endmodule
        `default_nettype none
        module \Top.core (); endmodule
    "#;
    assert_eq!(
        source_module_implicit_net_permissions(source, Path::new("nettype.sv")).unwrap(),
        vec![
            ("First".to_string(), false),
            ("Second".to_string(), true),
            ("Third".to_string(), true),
            ("\\Top.core".to_string(), false),
        ]
    );
}

#[test]
fn rejects_default_nettype_state_changes_inside_modules() {
    for source in [
        r#"
            `default_nettype wire
            module Top();
                `default_nettype none
            endmodule
        "#,
        r#"
            `default_nettype none
            module Top();
                `default_nettype wire
            endmodule
        "#,
        r#"
            `default_nettype none
            module Top();
                `resetall
            endmodule
        "#,
    ] {
        let error = source_module_implicit_net_permissions(source, Path::new("nettype.sv"))
            .expect_err("in-module default nettype change should be rejected");
        assert!(
            error
                .to_string()
                .contains("`default_nettype change inside module `Top`"),
            "unexpected error: {error}"
        );
    }
}

#[test]
fn permits_redundant_default_nettype_directives_inside_modules() {
    let source = r#"
        `default_nettype none
        module Top();
            `default_nettype none
        endmodule
    "#;
    assert_eq!(
        source_module_implicit_net_permissions(source, Path::new("nettype.sv")).unwrap(),
        vec![("Top".to_string(), false)]
    );
}

#[test]
fn rejects_unsupported_default_net_types() {
    for net_type in ["tri0", "wand", "wor"] {
        let source = format!("`default_nettype {net_type}\nmodule Top(); endmodule");
        let error = source_module_implicit_net_permissions(&source, Path::new("nettype.sv"))
            .expect_err("unsupported default net type should be rejected");
        assert!(
            error
                .to_string()
                .contains(&format!("`default_nettype {net_type}`")),
            "unexpected error: {error}"
        );
    }
}

#[test]
fn records_always_ff_case_branches() {
    let ir = analyze_source(
        r#"
            module Top(
                input logic clk,
                input logic [1:0] mode,
                input logic [7:0] d0,
                input logic [7:0] d1,
                input logic [7:0] d2,
                output logic [7:0] q
            );
                always_ff @(posedge clk) begin
                    case (mode)
                        2'b00: q <= d0;
                        2'b01, 2'b10: q <= d1;
                        default: q <= d2;
                    endcase
                end
            endmodule
        "#,
        Path::new("ff_case.sv"),
    )
    .expect("SV analysis should succeed");

    let process = &ir.modules()[0].ff_processes()[0];
    assert_eq!(process.events().len(), 1);
    assert_eq!(assignment_count(process.body()), 3);
    let [crate::ir::Stmt::Case { items, default, .. }] = process.body() else {
        panic!("expected a case statement: {:?}", process.body());
    };
    assert_eq!(items.len(), 2);
    assert_eq!(items[1].labels.len(), 2);
    assert!(default.is_some());
}

/// The assignments a statement body contains, at any depth.
fn assignment_count(body: &[crate::ir::Stmt]) -> usize {
    let mut count = 0;
    for stmt in body {
        stmt.walk(&mut |stmt| {
            count += usize::from(matches!(stmt, crate::ir::Stmt::Assign { .. }));
        });
    }
    count
}

#[test]
fn accepts_unknown_labels_in_always_ff_case() {
    let ir = analyze_source(
        r#"
            module Top(
                input logic clk,
                input logic [1:0] mode,
                output logic q
            );
                always_ff @(posedge clk) begin
                    case (mode)
                        2'b1x: q <= 1'b1;
                        default: q <= 1'b0;
                    endcase
                end
            endmodule
        "#,
        Path::new("ff_case_unknown_label.sv"),
    )
    .expect("X/Z case labels should use exact four-state case equality");

    assert_eq!(
        assignment_count(ir.modules()[0].ff_processes()[0].body()),
        2
    );
}

#[test]
fn accepts_dynamic_labels_in_always_ff_case() {
    let ir = analyze_source(
        r#"
            module Top(
                input logic clk,
                input logic [1:0] selector,
                input logic [1:0] dynamic_label,
                output logic q
            );
                always_ff @(posedge clk) begin
                    case (selector)
                        dynamic_label: q <= 1'b1;
                        default: q <= 1'b0;
                    endcase
                end
            endmodule
        "#,
        Path::new("ff_case_dynamic_label.sv"),
    )
    .expect("dynamic labels should use exact four-state case equality");

    assert_eq!(
        assignment_count(ir.modules()[0].ff_processes()[0].body()),
        2
    );
}

#[test]
fn rejects_duplicate_module_names() {
    let err = analyze_source(
        r#"
            module Top; endmodule
            module Top; endmodule
        "#,
        Path::new("duplicate.sv"),
    )
    .expect_err("duplicate modules should be rejected");

    assert!(matches!(err, AnalyzerError::DuplicateModule { name } if name == "Top"));
}

#[test]
fn rejects_duplicate_port_names() {
    let err = analyze_source(
        r#"
            module Top(input logic a, output logic a);
            endmodule
        "#,
        Path::new("duplicate_port.sv"),
    )
    .expect_err("duplicate ports should be rejected");

    assert!(matches!(
        err,
        AnalyzerError::DuplicatePort { module, name } if module == "Top" && name == "a"
    ));
}

#[test]
fn rejects_repeated_ansi_port_names() {
    let err = analyze_source(
        r#"
            module Top(output logic y, output logic y);
                assign y = 1'b1;
            endmodule
        "#,
        Path::new("repeated_ansi_port.sv"),
    )
    .expect_err("repeated ANSI ports should be rejected");

    assert!(matches!(
        err,
        AnalyzerError::DuplicatePort { module, name } if module == "Top" && name == "y"
    ));
}

#[test]
fn folds_constant_range_expressions() {
    let ir = analyze_source(
        r#"
            module Top(input logic [(4 * 2) - 1:0] data);
            endmodule
        "#,
        Path::new("constant_expr.sv"),
    )
    .expect("SV analysis should succeed");

    assert_eq!(
        ir.modules()[0].ports()[0].r#type().resolved_width(),
        Some(8)
    );
}

#[test]
fn folds_parameter_range_expressions() {
    let ir = analyze_source(
        r#"
            module Top #(
                parameter WIDTH = (4 * 2)
            ) (
                input logic [WIDTH - 1:0] data
            );
            endmodule
        "#,
        Path::new("parameter_expr.sv"),
    )
    .expect("SV analysis should succeed");

    assert_eq!(ir.modules()[0].parameters()[0].name(), "WIDTH");
    assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(8));
    assert_eq!(ir.modules()[0].ports()[0].r#type().packed_ranges().len(), 1);
    assert_eq!(
        ir.modules()[0].ports()[0].r#type().resolved_width(),
        Some(8)
    );
}

#[test]
fn folds_based_number_literals() {
    let ir = analyze_source(
        r#"
            module Top(input logic [4'hf:8'd8] data);
            endmodule
        "#,
        Path::new("based_number.sv"),
    )
    .expect("SV analysis should succeed");

    assert_eq!(
        ir.modules()[0].ports()[0].r#type().resolved_width(),
        Some(8)
    );
}

#[test]
fn folds_localparam_range_expressions() {
    let ir = analyze_source(
        r#"
            module Top(input logic [WIDTH - 1:0] data);
                localparam WIDTH = 8;
            endmodule
        "#,
        Path::new("localparam_expr.sv"),
    )
    .expect("SV analysis should succeed");

    assert_eq!(ir.modules()[0].parameters()[0].name(), "WIDTH");
    assert_eq!(
        ir.modules()[0].ports()[0].r#type().resolved_width(),
        Some(8)
    );
}

#[test]
fn analyzes_fixed_unpacked_array_dimensions() {
    let ir = analyze_source(
        r#"
            module Top #(parameter N = 2) (
                input logic [7:0] data,
                output logic [7:0] out
            );
                logic [7:0] values [N];
                assign values[0] = data;
                assign values[1] = data;
                assign out = values[1];
            endmodule
        "#,
        Path::new("fixed_array.sv"),
    )
    .expect("fixed unpacked arrays should be analyzed");

    let top = &ir.modules()[0];
    let values = top
        .signals()
        .iter()
        .find(|signal| signal.name() == "values")
        .expect("array signal should be present");
    assert_eq!(values.r#type().unpacked_ranges().len(), 1);
    assert_eq!(values.r#type().resolved_width(), Some(16));
    assert_eq!(top.ports()[1].r#type().resolved_width(), Some(8));
}

#[test]
fn rejects_nonpositive_implicit_unpacked_array_dimensions() {
    for size in ["0", "-1", "SIZE"] {
        let source = format!(
            r#"
                module Top #(parameter SIZE = 0) ();
                    logic [7:0] values[{size}];
                endmodule
            "#
        );
        let error = analyze_source(&source, Path::new("invalid_array_size.sv"))
            .expect_err("nonpositive implicit array dimensions should be rejected");
        assert!(
            matches!(error, AnalyzerError::Unsupported(message) if message == "nonpositive unpacked array dimension")
        );
    }
}

#[test]
fn rejects_unpacked_arrays_of_nonequivalent_element_types() {
    // IEEE 1800-2023 6.22.2 and 7.6: an unpacked array assignment needs
    // equivalent element types (same width, state count and signedness) and
    // equal element counts.
    let source = r#"
        module Top(output logic [7:0] q);
            function automatic logic [7:0] first(input logic [7:0] x [2][2]);
                return x[0][0];
            endfunction
            logic signed [7:0] row [2];
            always_comb begin
                row[0] = 8'sd1;
                row[1] = 8'sd2;
                q = first('{row, '{8'd3, 8'd4}});
            end
        endmodule
    "#;
    let error = analyze_source(source, Path::new("nonequivalent_unpacked.sv"))
        .expect_err("a signed row is not equivalent to an unsigned one");
    let row = |signed| typecheck::UnpackedArrayType {
        dims: vec![2],
        element_width: 8,
        signed,
        four_state: true,
    };
    assert_eq!(
        error,
        AnalyzerError::IncompatibleUnpackedArray {
            context: "assignment pattern item".to_string(),
            actual: row(true),
            target: row(false),
        }
    );
}

#[test]
fn accepts_unpacked_arrays_of_equivalent_element_types() {
    // Bounds, the packed range direction and a packed structure of the same
    // width, state count and signedness do not matter (IEEE 1800-2023
    // 6.22.2, 7.6).
    let source = r#"
        module Top(output logic [7:0] q, output logic [7:0] r);
            typedef struct packed { logic [3:0] hi; logic [3:0] lo; } pair_t;
            function automatic logic [7:0] first(input logic [7:0] x [1:0][2]);
                return x[1][0];
            endfunction
            logic [0:7] row [5:6];
            pair_t pairs [2];
            logic [7:0] grid [2][2];
            always_comb begin
                row[5] = 8'd1;
                row[6] = 8'd2;
                pairs = row;
                grid = '{row, '{8'd0, 8'd0}};
                q = first('{pairs, row});
                r = grid[0][0];
            end
        endmodule
    "#;
    analyze_source(source, Path::new("equivalent_unpacked.sv"))
        .expect("equivalent unpacked array types are assignment compatible");
}

#[test]
fn flattens_partial_unpacked_array_selections() {
    let ir = analyze_source(
        r#"
            module Top(
                input logic [7:0] values[2][3],
                output logic [7:0] row[3]
            );
                assign row = values[0];
            endmodule
        "#,
        Path::new("partial_unpacked_selection.sv"),
    )
    .expect("partial unpacked selections should be analyzed");
    let expression = ir.modules()[0].comb_processes()[0].assignments()[0].rhs();
    let ir::Expr::Select { msb, lsb, .. } = expression else {
        panic!("expected a flattened partial selection, got {expression:?}");
    };
    assert_eq!(eval_test_const_expr(msb), Some(23));
    assert_eq!(eval_test_const_expr(lsb), Some(0));
}

#[test]
fn flattens_partial_unpacked_array_lvalue_selections() {
    let ir = analyze_source(
        r#"
            module Top(
                input logic [7:0] row[3],
                output logic [7:0] values[2][3]
            );
                assign values[0] = row;
            endmodule
        "#,
        Path::new("partial_unpacked_lvalue_selection.sv"),
    )
    .expect("partial unpacked lvalues should be analyzed");
    let assignment = &ir.modules()[0].comb_processes()[0].assignments()[0];
    let ir::LValue::Select { name, msb, lsb, .. } = assignment.lhs_value() else {
        panic!(
            "expected a flattened partial lvalue selection, got {:?}",
            assignment.lhs_value()
        );
    };
    assert_eq!(name, "values");
    assert_eq!(eval_test_const_expr(msb), Some(23));
    assert_eq!(eval_test_const_expr(lsb), Some(0));
}

#[test]
fn flattens_unpacked_array_range_selections() {
    let ir = analyze_source(
        r#"
            module Top(
                input logic [7:0] source[4],
                output logic [7:0] target[4]
            );
                assign target[1:0] = source[1:0];
            endmodule
        "#,
        Path::new("unpacked_array_range_selection.sv"),
    )
    .expect("unpacked array ranges should be analyzed");
    let assignment = &ir.modules()[0].comb_processes()[0].assignments()[0];
    let ir::LValue::Select {
        msb,
        lsb,
        array_slice_width,
        array_slice_reversed,
        ..
    } = assignment.lhs_value()
    else {
        panic!("expected a flattened unpacked range lvalue");
    };
    assert_eq!(eval_test_const_expr(msb), Some(15));
    assert_eq!(eval_test_const_expr(lsb), Some(0));
    assert_eq!(
        array_slice_width.as_ref().map(eval_test_const_expr),
        Some(Some(8))
    );
    assert!(*array_slice_reversed);
    let ir::Expr::Concat(parts) = assignment.rhs() else {
        panic!("expected an element-ordered unpacked range expression");
    };
    assert_eq!(parts.len(), 2);
    let ir::Expr::Select { msb, lsb, .. } = &parts[0] else {
        panic!("expected the first unpacked range element selection");
    };
    assert_eq!(eval_test_const_expr(msb), Some(7));
    assert_eq!(eval_test_const_expr(lsb), Some(0));
    let ir::Expr::Select { msb, lsb, .. } = &parts[1] else {
        panic!("expected the second unpacked range element selection");
    };
    assert_eq!(eval_test_const_expr(msb), Some(15));
    assert_eq!(eval_test_const_expr(lsb), Some(8));
}

#[test]
fn preserves_implicit_packed_bit_selects_on_scalar_arrays() {
    let ir = analyze_source(
        r#"
            module Top(input logic values[2], output logic y);
                assign y = values[0][0];
            endmodule
        "#,
        Path::new("scalar_array_bit_selection.sv"),
    )
    .expect("scalar array bit-selects should be analyzed");
    let expression = ir.modules()[0].comb_processes()[0].assignments()[0].rhs();
    let ir::Expr::Select { msb, lsb, .. } = expression else {
        panic!("expected a scalar array bit-select, got {expression:?}");
    };
    assert_eq!(eval_test_const_expr(msb), Some(0));
    assert_eq!(eval_test_const_expr(lsb), Some(0));
}

#[test]
fn rejects_duplicate_parameters() {
    let err = analyze_source(
        r#"
            module Top #(parameter WIDTH = 8, parameter WIDTH = 4) ();
            endmodule
        "#,
        Path::new("duplicate_parameter.sv"),
    )
    .expect_err("duplicate parameters should be rejected");

    assert!(matches!(
        err,
        AnalyzerError::DuplicateParameter { module, name } if module == "Top" && name == "WIDTH"
    ));
}

#[test]
fn rejects_duplicate_module_scope_parameters() {
    let err = analyze_source(
        r#"
            module Top(output logic y);
                parameter P = 0;
                parameter P = 1;
                assign y = P;
            endmodule
        "#,
        Path::new("duplicate_module_parameter.sv"),
    )
    .expect_err("duplicate module-scope parameters should be rejected");

    assert!(matches!(
        err,
        AnalyzerError::DuplicateParameter { module, name } if module == "Top" && name == "P"
    ));
}

#[test]
fn rejects_duplicate_instance_names() {
    let err = analyze_source(
        r#"
            module Child(output logic y); assign y = 1'b1; endmodule
            module Top(output logic a, output logic b);
                Child u(.y(a));
                Child u(.y(b));
            endmodule
        "#,
        Path::new("duplicate_instance.sv"),
    )
    .expect_err("duplicate instances should be rejected");

    assert!(matches!(
        err,
        AnalyzerError::DuplicateInstance { module, name } if module == "Top" && name == "u"
    ));
}

#[test]
fn preserves_signed_port_types() {
    let ir = analyze_source(
        r#"
            module Top(
                input logic signed [7:0] a,
                output signed [3:0] b,
                input logic unsigned [1:0] c
            );
            endmodule
        "#,
        Path::new("signed_ports.sv"),
    )
    .expect("SV analysis should succeed");

    let ports = ir.modules()[0].ports();
    assert!(ports[0].r#type().is_signed());
    assert_eq!(ports[0].r#type().kind(), ir::TypeKind::Logic);
    assert_eq!(ports[0].r#type().resolved_width(), Some(8));
    assert!(ports[1].r#type().is_signed());
    assert_eq!(ports[1].r#type().kind(), ir::TypeKind::Implicit);
    assert_eq!(ports[1].r#type().resolved_width(), Some(4));
    assert!(!ports[2].r#type().is_signed());
    assert_eq!(ports[2].r#type().resolved_width(), Some(2));
}

#[test]
fn records_module_instantiations() {
    let ir = analyze_source(
        r#"
            module Child(input logic a, output logic y);
            endmodule

            module Top(input logic a, output logic y);
                Child #(.WIDTH(8)) u_child (.a(a), .y(y));
            endmodule
        "#,
        Path::new("instantiation.sv"),
    )
    .expect("SV analysis should succeed");

    let top = ir
        .modules()
        .iter()
        .find(|module| module.name() == "Top")
        .expect("Top module should exist");
    assert_eq!(top.instances().len(), 1);
    assert_eq!(top.instances()[0].module_name(), "Child");
    assert_eq!(top.instances()[0].name(), "u_child");
    assert_eq!(top.instances()[0].parameter_names(), &["WIDTH"]);
    assert_eq!(top.instances()[0].parameter_overrides().len(), 1);
    assert_eq!(top.instances()[0].parameter_overrides()[0].name(), "WIDTH");
    assert_eq!(
        top.instances()[0].parameter_overrides()[0].value(),
        Some(&ir::ConstExpr::Literal("8".to_string()))
    );
    assert_eq!(top.instances()[0].port_names(), &["a", "y"]);
}

#[test]
fn records_continuous_assignments() {
    let ir = analyze_source(
        r#"
            module Top(input logic a, input logic b, output logic y, output logic z);
                assign y = a & b;
                assign z = a | b;
            endmodule
        "#,
        Path::new("continuous_assign.sv"),
    )
    .expect("SV analysis should succeed");

    let top = &ir.modules()[0];
    assert_eq!(top.assignments().len(), 2);
    assert_eq!(top.assignments()[0].lhs(), "y");
    assert_eq!(top.assignments()[1].lhs(), "z");
    assert_eq!(top.comb_processes().len(), 2);
    assert_eq!(
        top.comb_processes()[0].kind(),
        ir::CombProcessKind::ContinuousAssign
    );
    assert_eq!(top.comb_processes()[0].assignments()[0].lhs(), "y");
}

#[test]
fn folds_countbits_with_four_state_parameter_operands_and_controls() {
    let ir = analyze_source(
        r#"
            module Top #(
                parameter logic [7:0] MIXED = 8'b10xz_11xz,
                parameter ALIAS = MIXED,
                parameter logic [7:0] CTRL = 8'bxxxx_xxxz,
                parameter logic signed [3:0] SIGNED_X = 4'bx001,
                parameter logic signed [7:0] EXTENDED_X = SIGNED_X,
                parameter X = $countbits(ALIAS, 'x),
                parameter Z = $countbits(MIXED, CTRL, CTRL),
                parameter ALL = $countbits(MIXED, '0, '1, 'x, 'z),
                parameter EXTENDED = $countbits(EXTENDED_X, 'x),
                parameter logic [255:0] WIDE = 'x,
                parameter W = $countbits(WIDE, 'x)
            )(output logic [X-1:0] y);
                assign y = '0;
            endmodule
        "#,
        Path::new("countbits_parameters.sv"),
    )
    .expect("four-state countbits parameters should be evaluated");
    let module = &ir.modules()[0];
    for (index, expected) in [(5, 2), (6, 2), (7, 8), (8, 5), (10, 256)] {
        assert_eq!(module.parameters()[index].resolved_value(), Some(expected));
        assert_eq!(module.parameters()[index].resolved_width(), Some(32));
        assert_eq!(module.parameters()[index].resolved_signed(), Some(true));
    }
    assert_eq!(module.parameters()[1].resolved_width(), Some(8));
    assert_eq!(module.ports()[0].r#type().resolved_width(), Some(2));
}

#[test]
fn folds_countbits_with_self_determined_argument_and_control_types() {
    let ir = analyze_source(
        r#"
            module Top #(
                parameter logic signed [7:0] NEG = -1,
                parameter C = $countbits(NEG, '1, '1),
                parameter X = $countbits(8'b10xz_11xz, 'x),
                parameter Z = $countbits(8'b10xz_11xz, 'z),
                parameter ALL = $countbits(8'b10xz_11xz, '0, '1, 'x, 'z),
                parameter F = $countbits('1, '1),
                parameter N = $countbits(-1, '1),
                parameter W = $countbits(256'hx, 'x),
                parameter S = ($countbits(NEG, '1) - 32'sd9) < 0,
                parameter B = $bits($countbits(NEG, '1)),
                parameter D = $size($countbits(NEG, '1)),
                parameter LSB = $countbits(8'hfe, 8'hfe),
                parameter logic [7:0] MASK = 8'hfe,
                parameter SELECTED = $countbits(MASK[0], '0)
            )(output logic [C-1:0] y);
                assign y = '0;
            endmodule
        "#,
        Path::new("countbits.sv"),
    )
    .expect("countbits constants should be evaluated");
    let module = &ir.modules()[0];
    for (parameter, expected) in module
        .parameters()
        .iter()
        .zip([-1, 8, 2, 2, 8, 1, 32, 256, 1, 32, 32, 1, 254, 1])
    {
        assert_eq!(
            parameter.resolved_value(),
            Some(expected),
            "{}",
            parameter.name()
        );
    }
    assert_eq!(module.parameters()[1].resolved_width(), Some(32));
    assert_eq!(module.parameters()[1].resolved_signed(), Some(true));
    assert_eq!(module.ports()[0].r#type().resolved_width(), Some(8));
}

#[test]
fn folds_countones_with_self_determined_argument_types() {
    let ir = analyze_source(
        r#"
            module Top #(
                parameter logic signed [7:0] NEG = -1,
                parameter C = $countones(NEG),
                parameter X = $countones(8'b10xz_11xz),
                parameter F = $countones('1),
                parameter N = $countones(-1),
                parameter W = $countones(256'hffff_ffff_ffff_ffff_ffff_ffff_ffff_ffff_ffff),
                parameter S = ($countones(NEG) - 32'sd9) < 0,
                parameter B = $bits($countones(NEG)),
                parameter D = $size($countones(NEG)),
                parameter logic [7:0] MASK = 8'hfe,
                parameter SELECTED = $countones(MASK[0])
            )(output logic [C-1:0] y);
                assign y = '0;
            endmodule
        "#,
        Path::new("countones.sv"),
    )
    .expect("countones constants should be evaluated");
    let module = &ir.modules()[0];
    for (parameter, expected) in module
        .parameters()
        .iter()
        .zip([-1, 8, 3, 1, 32, 144, 1, 32, 32, 254, 0])
    {
        assert_eq!(
            parameter.resolved_value(),
            Some(expected),
            "{}",
            parameter.name()
        );
    }
    assert_eq!(module.parameters()[1].resolved_width(), Some(32));
    assert_eq!(module.parameters()[1].resolved_signed(), Some(true));
    assert_eq!(module.ports()[0].r#type().resolved_width(), Some(8));
}

#[test]
fn folds_bit_vector_predicates_with_self_determined_argument_types() {
    let ir = analyze_source(
        r#"
            module Top #(
                parameter H = $onehot(8'b0x0z_0001),
                parameter H0 = $onehot('x),
                parameter Z = $onehot0('z),
                parameter U = $isunknown(256'hx),
                parameter K = $isunknown(-1),
                parameter B = $bits($onehot(8'd1)),
                parameter D = $size($isunknown('z)),
                parameter logic [7:0] MASK = 8'hfe,
                parameter SELECTED = $onehot(MASK[0]),
                parameter SELECTED_ZERO = $onehot0(MASK[0]),
                parameter UNKNOWN_Z = $isunknown(1'bz),
                parameter SUM = $onehot(8'hff + 8'd1),
                parameter MIXED = $isunknown(1'bx ? 4'b1010 : 4'b1000)
            ) (output logic y);
                assign y = H;
            endmodule
        "#,
        Path::new("bit_vector_predicate_constants.sv"),
    )
    .expect("bit vector predicates should be evaluated with their declared return types");
    let module = &ir.modules()[0];
    assert_eq!(module.parameters().len(), 13);
    for (parameter, expected) in module
        .parameters()
        .iter()
        .zip([1, 0, 1, 1, 0, 1, 1, 254, 0, 1, 1, 0, 1])
    {
        assert_eq!(
            parameter.resolved_value(),
            Some(expected),
            "{}",
            parameter.name()
        );
    }
    for index in [0, 1, 2, 3, 4, 8, 9, 10, 11, 12] {
        assert_eq!(module.parameters()[index].resolved_width(), Some(1));
        assert_eq!(module.parameters()[index].resolved_signed(), Some(false));
    }
}

#[test]
fn rejects_malformed_constant_bit_vector_function_calls() {
    for name in ["$countones", "$onehot", "$onehot0", "$isunknown"] {
        for args in ["", "1, 1", ", 1", "1,"] {
            let source =
                format!("module Top(output logic y); assign y = {name}({args}); endmodule");
            assert!(
                analyze_source(&source, Path::new("invalid_system_function.sv")).is_err(),
                "{name}({args})"
            );
        }
    }
}

#[test]
fn folds_supported_system_functions() {
    let ir = analyze_source(
        r#"
            module Top #(
                parameter W = $clog2(9),
                parameter O = $onehot(8),
                parameter Z = $onehot0(0)
            ) (
                input logic [W-1:0] data
            );
            endmodule
        "#,
        Path::new("system_functions.sv"),
    )
    .expect("SV analysis should succeed");

    let top = &ir.modules()[0];
    assert_eq!(top.parameters()[0].resolved_value(), Some(4));
    assert_eq!(top.parameters()[1].resolved_value(), Some(1));
    assert_eq!(top.parameters()[2].resolved_value(), Some(1));
    assert_eq!(top.ports()[0].r#type().resolved_width(), Some(4));
}

#[test]
fn analyzes_veryl_emitted_benchmark_sv() {
    let cases = [
        (
            "Countones.sv",
            include_str!("../testdata/verilator/Countones.sv"),
        ),
        (
            "StdCounter.sv",
            include_str!("../testdata/verilator/StdCounter.sv"),
        ),
        (
            "GrayCounter.sv",
            include_str!("../testdata/verilator/GrayCounter.sv"),
        ),
        (
            "GrayCodec.sv",
            include_str!("../testdata/verilator/GrayCodec.sv"),
        ),
        (
            "EdgeDetector.sv",
            include_str!("../testdata/verilator/EdgeDetector.sv"),
        ),
        ("Onehot.sv", include_str!("../testdata/verilator/Onehot.sv")),
        ("Lfsr.sv", include_str!("../testdata/verilator/Lfsr.sv")),
    ];

    for (name, code) in cases {
        let ir = analyze_source(code, Path::new(name))
            .unwrap_or_else(|err| panic!("failed to analyze Veryl-emitted {name}: {err}"));
        assert!(
            !ir.modules().is_empty(),
            "Veryl-emitted {name} should contain modules"
        );
    }
}

#[test]
fn rejects_unlowered_constructs() {
    let error = analyze_source(
        "module Top(input logic a, output logic y); always_comb begin fork y = a; join end endmodule",
        Path::new("fork.sv"),
    )
    .expect_err("unlowered constructs must not be silently ignored");
    assert!(matches!(error, AnalyzerError::Unsupported(_)), "{error:?}");
}

#[test]
fn analyzes_veryl_emitted_fifo_with_runtime_casts() {
    analyze_source(
        include_str!("../testdata/verilator/Fifo.sv"),
        Path::new("Fifo.sv"),
    )
    .expect("Veryl-emitted FIFO uses only supported constructs");
}

#[test]
fn analyzes_generate_constructs_in_veryl_emitted_sources() {
    for (name, source) in [
        ("Top.sv", include_str!("../testdata/verilator/Top.sv")),
        (
            "LinearSec.sv",
            include_str!("../testdata/verilator/LinearSec.sv"),
        ),
    ] {
        analyze_source(source, Path::new(name)).unwrap();
    }
}

#[test]
fn records_veryl_emitted_module_instantiations() {
    let ir = analyze_source(
        include_str!("../testdata/verilator/StdCounter.sv"),
        Path::new("StdCounter.sv"),
    )
    .expect("SV analysis should succeed");
    let top = ir
        .modules()
        .iter()
        .find(|module| module.name() == "Top")
        .expect("Top module should exist");

    assert_eq!(top.instances().len(), 1);
    assert_eq!(top.instances()[0].module_name(), "counter");
    assert_eq!(top.instances()[0].name(), "u");
    assert_eq!(top.instances()[0].parameter_names(), &["WIDTH"]);
    assert_eq!(
        top.instances()[0].port_names(),
        &[
            "i_clk",
            "i_rst",
            "i_clear",
            "i_set",
            "i_set_value",
            "i_up",
            "i_down",
            "o_count",
            "o_count_next",
            "o_wrap_around",
        ]
    );
}

#[test]
fn unsupported_constructs_map_to_their_tracking_issues() {
    let issue =
        |construct: &str| AnalyzerError::Unsupported(construct.to_string()).tracking_issue();
    assert_eq!(issue("gate primitive instantiation"), 457);
    assert_eq!(issue("duplicate internal signal `t`"), 445);
    assert_eq!(issue("undriven net declaration `n`"), 460);
    assert_eq!(
        issue("combinational expression assigned to `y`"),
        SV_FRONTEND_TRACKING_ISSUE
    );
    assert_eq!(
        AnalyzerError::Parse("bad".to_string()).tracking_issue(),
        SV_FRONTEND_TRACKING_ISSUE
    );
}

#[test]
fn rejects_package_variables_and_nets() {
    // A package variable is one object shared by every module, but inlining
    // would give each module its own copy.
    for (code, construct) in [
        (
            "package p; logic [7:0] shared; endpackage",
            "package variable or net `p::shared`",
        ),
        (
            "package p; var int count = 0; endpackage",
            "package variable or net `p::count`",
        ),
        (
            "package p; wire w; endpackage",
            "package variable or net `p::w`",
        ),
    ] {
        let error = source_packages(code, Path::new("state.sv")).unwrap_err();
        assert_eq!(error, AnalyzerError::Unsupported(construct.to_string()));
        assert_eq!(error.tracking_issue(), 1146);
    }
    // Constants, and the variables of package functions, are not shared state.
    let code = "package p; const int K = 3; localparam int W = 4;\n\
                function automatic int f(int x); int t; t = x + K; return t; endfunction\n\
                endpackage";
    assert_eq!(
        source_packages(code, Path::new("state.sv")).unwrap().len(),
        1
    );
}

#[test]
fn package_inlining_keeps_source_text_after_a_dpi_import() {
    // The preprocessor widens the space after `"DPI-C"`, so syntax tree
    // offsets after it no longer match the source text.
    let code = "package p; import \"DPI-C\" function int twice(input int x); endpackage\n\
                module Top(input int a, output int y); import p::*; assign y = a; endmodule\n";
    let path = Path::new("dpi.sv");
    let packages = source_packages(code, path)
        .unwrap()
        .into_iter()
        .map(|package| (package.name.clone(), package))
        .collect::<HashMap<_, _>>();
    let inlined = inline_module_packages(code, path, "Top", &packages)
        .unwrap()
        .unwrap();
    assert!(
        inlined.contains("\nimport \"DPI-C\" function int twice(input int x); \nendmodule"),
        "{inlined}"
    );
}

fn elaborate(source: &str) -> Result<Option<String>, AnalyzerError> {
    elaborate_interfaces(&[(source, Path::new("interfaces.sv"))])
        .map(|sources| sources.map(|mut sources| sources.remove(0)))
}

#[test]
fn leaves_sources_without_interfaces_unchanged() {
    let source = "module Top(input logic a, output logic y); assign y = a; endmodule";
    assert_eq!(elaborate(source), Ok(None));
}

#[test]
fn expands_interface_instances_ports_and_generic_ports() {
    let elaborated = elaborate(
        r#"
        interface Bus #(parameter int W = 4);
            logic [W-1:0] data;
            modport w(output data);
        endinterface
        module Writer(Bus.w bus, input logic [7:0] v);
            assign bus.data = v;
        endmodule
        module Pass(interface bus, input logic [7:0] v);
            Writer u(.bus(bus), .v(v));
        endmodule
        module Top(input logic [7:0] v, output logic [7:0] y);
            Bus #(.W(6)) b();
            Pass p(.bus(b), .v(v));
            assign y = b.data;
        endmodule
        "#,
    )
    .unwrap()
    .unwrap();
    for expected in [
        // The instance becomes a localparam and a signal per member.
        "localparam int b$W = 6;",
        "var logic [b$W-1:0] b$data;",
        "assign y = b$data;",
        // The port becomes a member port and a parameter.
        "parameter int bus$W = 4",
        "output var logic [bus$W-1:0] bus$data",
        "assign bus$data = v;",
        // The generic port is bound in a copy of the module.
        "Pass$Bus #(.bus$W(b$W)) p (.bus$data(b$data), .v(v));",
        "module Pass$Bus #(",
        "Writer #(.bus$W(bus$W)) u (.bus$data(bus$data), .v(v));",
    ] {
        assert!(
            elaborated.contains(expected),
            "missing `{expected}` in:\n{elaborated}"
        );
    }
    // The copy stays beside the original, under the same directives.
    assert!(
        elaborated.find("module Pass$Bus") < elaborated.find("module Top"),
        "{elaborated}"
    );
}

#[test]
fn expands_interface_parameters_functions_and_grouped_ports() {
    let elaborated = elaborate(
        r#"
        interface Counter;
            parameter W = 4;
            logic [W-1:0] state;
            function automatic void load(input logic [W-1:0] v);
                state = v;
            endfunction
            modport loader(import load);
        endinterface
        interface Util #(parameter int K = 2);
            function automatic logic [7:0] scaled(input logic [7:0] v);
                return v * K;
            endfunction
            modport user(import scaled);
        endinterface
        module Loader(Counter.loader c, input logic [7:0] v);
            always_comb c.load(v);
        endmodule
        module Pair(Counter a, b, output logic [7:0] o);
            assign o = a.state + b.state;
        endmodule
        module Scaler(Util.user u, input logic [7:0] v, output logic [7:0] o);
            assign o = u.scaled(v);
        endmodule
        module Top(input logic [7:0] v, output logic [7:0] o, output logic [7:0] s);
            Counter #(.W(8)) named();
            Counter #(6) ordered();
            Util #(.K(3)) util();
            Loader l(.c(named), .v(v));
            Pair p(.a(named), .b(ordered), .o(o));
            Scaler u(.u(util), .v(v), .o(s));
        endmodule
        "#,
    )
    .unwrap()
    .unwrap();
    for expected in [
        // A body `parameter` without a parameter port list can be overridden.
        "localparam named$W = 8;",
        "localparam ordered$W = 6;",
        "parameter c$W = 4",
        // A member an imported function assigns is driven through the port.
        "output var logic [c$W-1:0] c$state",
        "named$state = v;",
        // A port without a header repeats the interface of the previous one.
        "input var logic [b$W-1:0] b$state",
        "assign o = a$state + b$state;",
        // A port that carries only a parameter and a function disappears.
        "Scaler #(.u$K(util$K)) u (.v(v), .o(s));",
        "assign o = u$scaled(v);",
    ] {
        assert!(
            elaborated.contains(expected),
            "missing `{expected}` in:\n{elaborated}"
        );
    }
}

#[test]
fn expands_nested_references_functions_and_written_arguments() {
    let elaborated = elaborate(
        r#"
        interface Cfg #(parameter int N = 2);
        endinterface
        interface Lane;
            function automatic int bits();
                return 4;
            endfunction
            function automatic logic [3:0] inc(input logic [3:0] v);
                return v + 1;
            endfunction
            logic [bits()-1:0] i0;
            logic [3:0] g0;
            assign g0 = inc(i0);
            modport r(input g0);
        endinterface
        module Reader(Lane.r l, output logic [3:0] o);
            assign o = l.g0;
        endmodule
        module Driver(Lane l, input logic [3:0] v);
            function automatic void drive(output logic [3:0] d, input logic [3:0] s);
                d = s;
            endfunction
            always_comb drive(l.i0, v);
        endmodule
        module Top(input logic [3:0] v, output logic [3:0] o);
            Cfg #(.N(3)) cfg();
            Lane rows [cfg.N] ();
            Driver d(.l(rows[cfg.N - 1]), .v(v));
            Reader r(.l(rows[cfg.N - 1]), .o(o));
            assign rows[cfg.N - 2].i0 = v;
        endmodule
        "#,
    )
    .unwrap()
    .unwrap();
    for expected in [
        // References inside selects and instance dimensions are rewritten.
        "var logic [rows$bits()-1:0] rows$i0[cfg$N];",
        "assign rows$i0[cfg$N - 2] = v;",
        ".l$i0(rows$i0[cfg$N - 1])",
        // Loop helper names cannot collide with the items `i0` and `g0`.
        "for (genvar rows$$i0 = 0; rows$$i0 <= (cfg$N) - 1; rows$$i0++) begin : rows$$g0",
        "assign rows$g0[rows$$i0] = rows$inc(rows$i0[rows$$i0]);",
        // A function a declaration calls is declared with the port.
        "function automatic int l$bits();",
        // An actual of an `output` subroutine argument is written.
        "output var logic [l$bits()-1:0] l$i0",
    ] {
        assert!(
            elaborated.contains(expected),
            "missing `{expected}` in:\n{elaborated}"
        );
    }
}

#[test]
fn infers_writes_through_child_ports_and_scoped_subroutines() {
    let elaborated = elaborate(
        r#"
        interface Bus;
            logic [7:0] x;
            logic [7:0] y;
            logic [7:0] z;
        endinterface
        interface Gen;
            logic a;
            logic b;
            generate
                if (1) begin : copy
                    assign b = a;
                end
            endgenerate
        endinterface
        package pk;
            function automatic void touch(output logic [7:0] a);
                a = 0;
            endfunction
        endpackage
        module Out(output logic [7:0] o);
            assign o = 8'd1;
        endmodule
        module M(Bus p, output logic [7:0] o);
            function automatic logic [7:0] touch(input logic [7:0] a);
                return a;
            endfunction
            Out named(.o(p.x));
            Out ordered(p.y);
            assign o = touch(p.z);
        endmodule
        module Top(input logic v, output logic o);
            Gen g [2] ();
            assign g[0].a = v;
            assign g[1].a = v;
            assign o = g[1].b;
        endmodule
        "#,
    )
    .unwrap()
    .unwrap();
    for expected in [
        // Actuals of child outputs are written; the module's own `touch`
        // reads its argument, whatever a package declares.
        "output var logic [7:0] p$x",
        "output var logic [7:0] p$y",
        "input var logic [7:0] p$z",
        // The items of a generate region are placed in the array loop.
        "for (genvar g$$i0 = 0; g$$i0 <= (2) - 1; g$$i0++) begin : g$$g0",
    ] {
        assert!(
            elaborated.contains(expected),
            "missing `{expected}` in:\n{elaborated}"
        );
    }
    let top = &elaborated[elaborated.find("module Top").unwrap()..];
    assert!(!top.contains("generate"), "{elaborated}");
    assert_eq!(
        elaborate("interface I; endinterface interface I; endinterface"),
        Err(AnalyzerError::DuplicateModule {
            name: "I".to_string()
        })
    );
}

#[test]
fn interface_functions_write_members_through_subroutine_arguments() {
    let elaborated = elaborate(
        r#"
        interface S;
            logic [7:0] state;
            function automatic void put(output logic [7:0] d, input logic [7:0] v);
                d = v;
            endfunction
            function automatic void load(input logic [7:0] v);
                put(state, v);
            endfunction
            modport m(import load);
        endinterface
        module L(S.m s, input logic [7:0] v);
            always_comb s.load(v);
        endmodule
        "#,
    )
    .unwrap()
    .unwrap();
    assert!(
        elaborated.contains("output var logic [7:0] s$state"),
        "{elaborated}"
    );
}

#[test]
fn rejects_invalid_and_colliding_interface_declarations() {
    assert_eq!(
        elaborate("interface I; logic x; modport m(input x); modport m(output x); endinterface"),
        Err(AnalyzerError::DuplicateModport {
            interface: "I".to_string(),
            name: "m".to_string()
        })
    );
    assert_eq!(
        elaborate(
            "interface I; logic a;
                 function automatic logic helper(); return a; endfunction
                 function automatic logic f(input logic helper); return helper; endfunction
             endinterface"
        ),
        Err(AnalyzerError::Unsupported(
            "declaration of `helper` in a nested scope of interface `I`, which shadows an interface item"
                .to_string()
        ))
    );
    assert_eq!(
        elaborate(
            r#"
            package pa; typedef logic [7:0] word_t; endpackage
            package pb; typedef logic [3:0] word_t; endpackage
            interface A; import pa::*; word_t d; modport r(input d); endinterface
            interface B; import pb::*; word_t d; modport r(input d); endinterface
            module M(A.r a, B.r b, output logic [7:0] o, output logic [3:0] q);
                assign o = a.d;
                assign q = b.d;
            endmodule
            "#
        ),
        Err(AnalyzerError::Unsupported(
            "package items `pa::word_t` and `pb::word_t` in module `M`, one of them imported through an interface"
                .to_string()
        ))
    );
    assert_eq!(
        elaborate(
            r#"
            package pa; typedef logic [7:0] word_t; endpackage
            interface A; import pa::*; word_t d; modport r(input d); endinterface
            module M(A.r a, output logic [7:0] o);
                typedef logic [3:0] word_t;
                assign o = a.d;
            endmodule
            "#
        ),
        Err(AnalyzerError::Unsupported(
            "declaration of `word_t` in module `M`, which hides the package item `pa::word_t` of an interface it uses"
                .to_string()
        ))
    );
}

#[test]
fn resolves_interface_handles_by_scope_and_carries_unit_imports() {
    let elaborated = elaborate(
        r#"
        interface Bus;
            logic [7:0] x;
            logic [7:0] y;
        endinterface
        interface A;
            logic [7:0] x;
        endinterface
        interface B;
            logic [3:0] x;
        endinterface
        interface T;
            function automatic logic [7:0] touch(input logic [7:0] v);
                return v;
            endfunction
        endinterface
        interface U;
            logic [7:0] s;
            function automatic void touch(output logic [7:0] v);
                v = s;
            endfunction
        endinterface
        module M(Bus p, output logic [7:0] o);
            T a();
            assign o = a.touch(p.y);
        endmodule
        module Top(input logic [7:0] v, output logic [7:0] o);
            if (1) begin : g1
                A h();
                assign h.x = v;
                assign o = h.x;
            end else begin : g2
                B h();
                assign h.x = v[3:0];
            end
        endmodule
        "#,
    )
    .unwrap()
    .unwrap();
    for expected in [
        // `a.touch` is the input-only function of `T`.
        "input var logic [7:0] p$y",
        // Each generate block has its own `h`.
        "var logic [7:0] h$x;",
        "var logic [3:0] h$x;",
    ] {
        assert!(
            elaborated.contains(expected),
            "missing `{expected}` in:\n{elaborated}"
        );
    }

    let sources = elaborate_interfaces(&[
        (
            "package pk; typedef logic [5:0] word_t; endpackage
             import pk::*;
             interface I; word_t x; modport r(input x); endinterface",
            Path::new("interface.sv"),
        ),
        (
            "module M(I.r p, output logic [5:0] o); assign o = p.x; endmodule",
            Path::new("module.sv"),
        ),
    ])
    .unwrap()
    .unwrap();
    assert!(
        sources[1].contains("module M import pk::*;"),
        "{}",
        sources[1]
    );
    assert!(
        sources[1].contains("input var word_t p$x"),
        "{}",
        sources[1]
    );
}

#[test]
fn keeps_generate_branches_imports_and_uncalled_writers_in_scope() {
    let elaborated = elaborate(
        r#"
        package pa;
            function automatic void touch(input logic [7:0] v);
            endfunction
        endpackage
        package pb;
            function automatic void touch(output logic [7:0] v);
                v = 0;
            endfunction
        endpackage
        interface I;
            logic [7:0] x;
            logic [7:0] y;
            assign y = x + 1;
            function automatic void set(input logic [7:0] v);
                x = v;
            endfunction
            modport m(input x, import set);
        endinterface
        module R(I.m p, output logic [7:0] o);
            assign o = p.x;
        endmodule
        module S(I p);
            import pa::*;
            always_comb touch(p.x);
        endmodule
        module Top(input logic [7:0] v, output logic [7:0] o);
            if (1) I h(); else I h();
            for (genvar i = 0; i < 1; i++) I k();
            I b();
            assign b.x = v;
            R r(.p(b), .o(o));
            S s(.p(b));
        endmodule
        "#,
    )
    .unwrap()
    .unwrap();
    for expected in [
        // The items of a bare generate item stay in its branch.
        "if (1) begin",
        "end else begin",
        "i++) begin",
    ] {
        assert!(
            elaborated.contains(expected),
            "missing `{expected}` in:\n{elaborated}"
        );
    }
    // `R` never calls `set`, and `pb::touch` is not imported into `S`.
    assert!(
        !elaborated.contains("output var logic [7:0] p$x"),
        "{elaborated}"
    );

    // A compilation-unit import after a module is copied into it.
    let elaborated = elaborate(
        r#"
        package pk; typedef logic [5:0] word_t; endpackage
        module M(I p, output logic [5:0] o); assign o = p.x; endmodule
        import pk::*;
        interface I; word_t x; endinterface
        "#,
    )
    .unwrap()
    .unwrap();
    assert!(
        elaborated.contains("module M import pk::*;"),
        "{elaborated}"
    );
}

#[test]
fn resolves_declaration_dependencies_scopes_and_implicit_connections() {
    let elaborated = elaborate(
        r#"
        package pk; typedef logic [3:0] word_t; endpackage
        interface I;
            import pk::*;
            localparam int W = 8;
            typedef logic [W-1:0] data_t;
            logic [7:0] shape;
            data_t data;
            word_t w;
            modport m(input data);
        endinterface
        module C(I.m p, input logic clk, output logic [7:0] o);
            function automatic logic [7:0] f(input logic [7:0] word_t);
                return word_t;
            endfunction
            always_ff @(posedge clk) o <= f(p.data);
        endmodule
        module M(I p, input logic clk, output logic [7:0] o);
            if (1) begin : g
                I p();
                assign p.data = 1;
            end
            C c(.p(p), .clk, .o);
        endmodule
        "#,
    )
    .unwrap()
    .unwrap();
    for expected in [
        // Imports precede the parameter port list the expansion adds.
        "module C import pk::*; #(",
        // The write is to the instance in `g`, not to port `p` of `M`.
        "input var p$data_t p$data",
        ".clk, .o)",
    ] {
        assert!(
            elaborated.contains(expected),
            "missing `{expected}` in:\n{elaborated}"
        );
    }
    // A port declares the types in its header, before the member ports.
    assert_eq!(
        elaborate(
            "interface I; logic [7:0] shape; typedef logic [$bits(shape)-1:0] data_t; data_t data;
             modport m(input data); endinterface
             module C(I.m p, output logic [7:0] o); assign o = p.data; endmodule"
        ),
        Err(AnalyzerError::Unsupported(
            "reference to member `shape` in a declaration of interface `I`, which port `p` of module `C` carries".to_string()
        ))
    );
    // An imported function's member passes through but stays inaccessible.
    assert_eq!(
        elaborate(
            "interface I; logic [7:0] x; logic [7:0] y;
             function automatic logic [7:0] get(); return x; endfunction
             modport m(input y, import get); endinterface
             module C(I.m p, output logic [7:0] o); assign o = p.get() + p.x; endmodule"
        ),
        Err(AnalyzerError::Unsupported(
            "access of `p.x`, which the modport of port `p` does not list".to_string()
        ))
    );
    for (modport, name, expected) in [
        ("modport m(input y);", "y", "a member"),
        ("modport m(input x, import typo);", "typo", "a function"),
    ] {
        assert_eq!(
            elaborate(&format!(
                "interface I; logic x; {modport} endinterface module Top; I h(); endmodule"
            )),
            Err(AnalyzerError::UnknownModportItem {
                interface: "I".to_string(),
                modport: "m".to_string(),
                name: name.to_string(),
                expected,
            })
        );
    }
}

#[test]
fn infers_writes_of_memory_loads_concatenations_and_scoped_imports() {
    let elaborated = elaborate(
        r#"
        package pa;
            function automatic void touch(input logic [7:0] v);
            endfunction
        endpackage
        package pb;
            function automatic void touch(output logic [7:0] v);
                v = 0;
            endfunction
        endpackage
        interface I;
            logic [7:0] mem [4];
            logic [7:0] hi;
            logic [7:0] lo;
            logic [1:0] idx;
            logic [7:0] x;
        endinterface
        module Out(output logic [15:0] o);
            assign o = 16'h1234;
        endmodule
        module L(I p);
            initial $readmemh("data.hex", p.mem);
        endmodule
        module C(I p);
            Out u(.o({p.hi, p.lo}));
        endmodule
        module S(I p);
            import pa::*;
            function automatic void g();
                import pb::*;
            endfunction
            always_comb touch(p.x);
        endmodule
        "#,
    )
    .unwrap()
    .unwrap();
    let module = |name: &str| {
        let start = elaborated.find(&format!("module {name}(")).unwrap();
        let end = start + elaborated[start..].find("endmodule").unwrap();
        elaborated[start..end].to_string()
    };
    let (l, c, s) = (module("L"), module("C"), module("S"));
    // An imported function that loads a member drives it.
    let loaded = elaborate(
        r#"
        interface I;
            logic [7:0] mem [4];
            function automatic void load();
                $readmemh("data.hex", mem);
            endfunction
            modport m(import load);
        endinterface
        module L(I.m p);
            initial p.load();
        endmodule
        "#,
    )
    .unwrap()
    .unwrap();
    assert!(
        loaded.contains("output var logic [7:0] p$mem[4]"),
        "{loaded}"
    );
    assert_eq!(
        elaborate("interface I; logic x; modport m(input x, output x); endinterface"),
        Err(AnalyzerError::DuplicateModportItem {
            interface: "I".to_string(),
            modport: "m".to_string(),
            name: "x".to_string(),
        })
    );
    for (text, expected) in [
        (&l, "output var logic [7:0] p$mem[4]"),
        (&c, "output var logic [7:0] p$hi"),
        (&c, "output var logic [7:0] p$lo"),
        (&c, "input var logic [1:0] p$idx"),
        // `pb::touch` is imported only inside `g`.
        (&s, "input var logic [7:0] p$x"),
    ] {
        assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
    }
}

#[test]
fn resolves_scoped_handles_imports_and_grouped_instances() {
    let elaborated = elaborate(
        r#"
        package pa; typedef logic [7:0] word_t; endpackage
        package pb; typedef logic [3:0] word_t; endpackage
        interface I;
            import pa::*;
            word_t x;
        endinterface
        interface A;
            function automatic void touch(input logic [7:0] v);
            endfunction
        endinterface
        interface B;
            function automatic void touch(output logic [7:0] v);
                v = 0;
            endfunction
        endinterface
        interface F;
            logic [7:0] y;
            function automatic logic [7:0] get();
                return y;
            endfunction
        endinterface
        module M(I p);
            function automatic void f();
                import pb::*;
            endfunction
            if (1) begin : g1
                A c();
                always_comb c.touch(p.x);
            end else begin : g2
                B c();
            end
            F a(), b();
        endmodule
        "#,
    )
    .unwrap()
    .unwrap();
    // `c` in `g1` is an `A`, whose `touch` only reads `p.x`.
    assert!(elaborated.contains("input var word_t p$x"), "{elaborated}");
    assert!(!elaborated.contains("endfunctionvar"), "{elaborated}");
    assert_eq!(
        elaborate("interface I; logic x; logic x; endinterface"),
        Err(AnalyzerError::DuplicateInterfaceItem {
            interface: "I".to_string(),
            name: "x".to_string(),
        })
    );
    assert_eq!(
        elaborate(
            "interface I #(parameter int W = 1); endinterface
             module Top; I #(.TYPO(3)) h(); endmodule"
        ),
        Err(AnalyzerError::UnknownInterfaceParameter {
            interface: "I".to_string(),
            name: "TYPO".to_string(),
        })
    );
}

#[test]
fn separates_type_names_unit_subroutines_and_macros_by_file() {
    // A module type may share the name of an interface port.
    let elaborated = elaborate(
        "module bus(input logic a); endmodule
         interface I; logic x; endinterface
         module M(I bus); bus u(.a(bus.x)); endmodule",
    )
    .unwrap()
    .unwrap();
    assert!(elaborated.contains("bus u(.a(bus$x))"), "{elaborated}");

    // Compilation-unit functions of another file are not visible.
    let sources = elaborate_interfaces(&[
        (
            "function automatic void touch(output logic [7:0] v); v = 0; endfunction
             module N(); endmodule",
            Path::new("other.sv"),
        ),
        (
            "function automatic void touch(input logic [7:0] v); endfunction
             interface I; logic [7:0] x; endinterface
             module M(I p); always_comb touch(p.x); endmodule",
            Path::new("module.sv"),
        ),
    ])
    .unwrap()
    .unwrap();
    assert!(
        sources[1].contains("input var logic [7:0] p$x"),
        "{}",
        sources[1]
    );

    // The expansion of a macro would keep the names it emits.
    assert_eq!(
        elaborate(
            "`define DRIVE assign x = 1;
             interface I; logic x; `DRIVE endinterface
             module M(); I h(); endmodule"
        ),
        Err(AnalyzerError::Unsupported(
            "macro or compiler directive in interface `I`".to_string()
        ))
    );

    // Compilation-unit items are visible only in their own file, after
    // their declaration.
    let interface = "function automatic logic source(); return 1; endfunction
         interface I; logic x; assign x = source(); endinterface";
    assert!(
        elaborate(&format!("{interface}\nmodule M(); I h(); endmodule"))
            .unwrap()
            .is_some()
    );
    let message = "compilation-unit item in interface `I`, which a module of another source file or before it uses";
    assert_eq!(
        elaborate_interfaces(&[
            (interface, Path::new("interface.sv")),
            ("module M(); I h(); endmodule", Path::new("module.sv")),
        ]),
        Err(AnalyzerError::Unsupported(message.to_string()))
    );
    assert_eq!(
        elaborate(&format!("module M(); I h(); endmodule\n{interface}")),
        Err(AnalyzerError::Unsupported(message.to_string()))
    );
}

#[test]
fn resolves_sibling_generic_ports_and_struct_fields() {
    let elaborated = elaborate(
        r#"
        interface A;
            function automatic void touch(output logic [7:0] v);
                v = 0;
            endfunction
        endinterface
        interface B;
            function automatic void touch(input logic [7:0] v);
            endfunction
        endinterface
        interface P;
            logic [7:0] x;
        endinterface
        module G(interface api, P p);
            always_comb api.touch(p.x);
        endmodule
        module S(P bus, output logic [7:0] o);
            typedef struct packed { logic [7:0] bus; } T;
            T t;
            assign t.bus = bus.x;
            assign o = t.bus;
        endmodule
        module Top(output logic [7:0] o);
            B b();
            P q();
            G g(.api(b), .p(q));
            S s(.bus(q), .o(o));
        endmodule
        "#,
    )
    .unwrap()
    .unwrap();
    // `api` is bound to `B`, whose `touch` only reads `p.x`.
    assert!(
        elaborated.contains("input var logic [7:0] p$x"),
        "{elaborated}"
    );
    // A struct field `h` is not a generate-qualified instance.
    assert!(
        elaborate(
            "interface I; logic x; endinterface
             module M(output logic y);
                 typedef struct packed { struct packed { logic x; } h; } T;
                 T record;
                 assign record = 0;
                 if (1) begin : g I h(); end
                 assign y = record.h.x;
             endmodule"
        )
        .unwrap()
        .is_some()
    );
    assert_eq!(
        elaborate(
            "interface I #(parameter int P = 0); endinterface
             module M(); I #(.P(1), .P(2)) h(); endmodule"
        ),
        Err(AnalyzerError::DuplicateInterfaceParameterOverride {
            interface: "I".to_string(),
            name: "P".to_string(),
        })
    );
    for (source, message) in [
        (
            "interface I; logic x; endinterface interface J; endinterface
             module C(I p, J q); endmodule
             module M(); I a(); C c(.p(a)); endmodule",
            "unconnected interface port `q` of instance of `C`",
        ),
        (
            "interface I; if (1) begin logic x; end logic y; assign y = genblk1.x; endinterface",
            "reference to the implicit generate block name `genblk1` in interface `I`",
        ),
        (
            "interface I; logic x; endinterface
             module M(output logic y); if (1) begin : g I h(); end assign y = g.h.x; endmodule",
            "hierarchical reference to interface instance `h` through a generate block",
        ),
        (
            "interface I; endinterface module Top; I h(,); endmodule",
            "ports of interface `I`",
        ),
    ] {
        assert_eq!(
            elaborate(source),
            Err(AnalyzerError::Unsupported(message.to_string())),
            "{source}"
        );
    }
}

#[test]
fn keeps_qualified_names_comments_and_preceding_imports() {
    // A qualified package function, a backtick in a comment and a struct
    // field chain through a generate block name are no unit items, macros or
    // generate-qualified instances.
    let sources = elaborate_interfaces(&[
        (
            "package pk;
                 typedef logic [3:0] word_t;
                 function automatic logic source(); return 1; endfunction
             endpackage
             function automatic logic source(); return 0; endfunction
             interface I;
                 // don't use `FOO here
                 logic x;
                 assign x = pk::source();
             endinterface",
            Path::new("interface.sv"),
        ),
        (
            "module M(output logic y);
                 typedef struct packed {
                     struct packed { struct packed { logic x; } h; } g;
                 } T;
                 T record;
                 assign record = 0;
                 I a();
                 if (1) begin : g I h(); end
                 assign y = record.g.h.x;
             endmodule",
            Path::new("module.sv"),
        ),
    ])
    .unwrap()
    .unwrap();
    assert!(sources[1].contains("pk::source()"), "{}", sources[1]);

    // A compilation-unit import before the interface and the module is
    // copied, as packages are inlined only for imports in a module.
    let elaborated = elaborate(
        "package pk; typedef logic [3:0] word_t; endpackage
         import pk::*;
         interface I; word_t x; endinterface
         module M(I p, output logic [3:0] o); assign o = p.x; endmodule",
    )
    .unwrap()
    .unwrap();
    assert!(
        elaborated.contains("module M import pk::*;"),
        "{elaborated}"
    );
}

#[test]
fn rejects_unsupported_interface_uses() {
    const BUS: &str = r#"
        interface Bus;
            logic [7:0] x;
            logic [7:0] y;
            function automatic logic [7:0] peek();
                return x;
            endfunction
            function automatic logic [7:0] get(input logic [7:0] k);
                return peek() + k;
            endfunction
            modport r(input x, import get);
            modport w(output x);
        endinterface
    "#;
    for (body, message) in [
        (
            "module Top(output logic [7:0] o); Bus b(); assign o = b; endmodule",
            "use of interface `b` other than as a port connection or through a member",
        ),
        (
            "module M(Bus.r p, output logic [7:0] o); assign o = p.y; endmodule",
            "access of `p.y`, which the modport of port `p` does not list",
        ),
        (
            "module W(Bus.w p); assign p.x = 0; endmodule
             module M(Bus.r p); W u(.p(p)); endmodule",
            "port `p` of `W` drives member `x`, an input of port `p` of `M`",
        ),
        (
            "module Top(output logic [7:0] o); Bus b [2] (); assign o = b[0].get(1); endmodule",
            "call of a function of the interface array `b`",
        ),
        (
            "module M(Bus p, output logic [7:0] o); assign o = p.x; endmodule
             module Top(output logic [7:0] o); Bus b(); M u(.p(b.r), .o(o)); endmodule",
            "modport `r` selected in the connection of port `p` of `M`, which does not declare it",
        ),
        (
            "module C(Bus p, input logic [7:0] q); endmodule
             module Top(); Bus i(); C c(.p(i), .q(i)); endmodule",
            "use of interface `i` other than as a port connection or through a member",
        ),
        (
            "module Top(output logic [7:0] o); Bus h(); logic [7:0] h$x; assign o = h.x; endmodule",
            "identifier `h$x` containing `$` in a design with interfaces, which interface elaboration reserves for generated names",
        ),
        (
            "module M(Bus.r p, output logic [7:0] o);
                 function automatic logic [7:0] f(input logic [7:0] p); return p; endfunction
                 assign o = f(p.x);
             endmodule",
            "declaration of `p` in module `M`, which shadows an interface",
        ),
        (
            "module Top(output logic [7:0] o); Bus \\b.0 (); assign o = \\b.0 .x; endmodule",
            "escaped identifier `\\b.0` that is not a simple identifier in a design with interfaces",
        ),
        (
            "module M(Bus.r p, output logic [7:0] o); assign o = p.peek(); endmodule",
            "call of `p.peek`, which the modport of port `p` does not import",
        ),
        (
            "module F(Bus p); function automatic void h(); p.x = 1; endfunction endmodule",
            "write of `p.x` inside a function or task of module `F`, whose port `p` has no modport",
        ),
        (
            "module W(Bus p); if (0) begin : g assign p.x = 0; end endmodule",
            "write of `p.x` inside a generate construct of module `W`, whose port `p` has no modport",
        ),
        (
            "module W(Bus.w p); assign p.x = 0; endmodule
             module M(Bus p); if (1) begin : g W u(.p(p)); end endmodule",
            "write of `p.x` inside a generate construct of module `M`, whose port `p` has no modport",
        ),
    ] {
        let source = format!("{BUS}{body}");
        assert_eq!(
            elaborate(&source),
            Err(AnalyzerError::Unsupported(message.to_string())),
            "{body}"
        );
    }
    for (interface, message) in [
        (
            "interface I; logic x; logic y; function automatic logic f(); return x; endfunction
             assign y = f(); endinterface
             module Top(); I a [2] (); endmodule",
            "call of function `f` of interface `I`, which accesses members, in the logic of the instance array `a`",
        ),
        (
            "interface I(input logic clk); endinterface",
            "ports of interface `I`",
        ),
        (
            "interface I; enum {Idle, Busy} state; endinterface",
            "enum type in interface `I`",
        ),
        (
            "interface I; assign x = 1'b1; endinterface",
            "implicit net `x` in interface `I`",
        ),
        (
            "interface I; typedef enum {Idle, Busy} state_t; state_t state; endinterface",
            "enum type in interface `I`",
        ),
        (
            "interface I; logic k; function automatic logic f(input logic k); return k; endfunction endinterface",
            "declaration of `k` in a nested scope of interface `I`, which shadows an interface item",
        ),
        (
            "interface I; logic a; modport m(inout a); endinterface",
            "inout or ref modport port in interface `I`",
        ),
    ] {
        assert_eq!(
            elaborate(interface),
            Err(AnalyzerError::Unsupported(message.to_string())),
            "{interface}"
        );
    }
}

#[test]
fn analyzing_a_source_again_gives_the_same_call_sites() {
    let code = r#"
        module Top(input logic [1:0] i, output logic [3:0] y);
            function automatic logic [1:0] pick(input logic [1:0] x);
                return x;
            endfunction
            always_comb begin
                y = 4'd0;
                y[pick(i)] = 1'b1;
            end
        endmodule
    "#;
    let path = Path::new("sites.sv");
    let first = analyze_source(code, path).unwrap();
    let second = analyze_source(code, path).unwrap();
    assert_eq!(first, second);
}

#[test]
fn countbits_alias_parameter_keeps_unknown_bits_in_assignments() {
    let ir = analyze_source(
        r#"module Child #(parameter logic [7:0] MASK = 8'b10xz_11xz,
                         parameter int C = $countbits(MASK, 'x, 'z))
                        (output logic [C:0] y, output int count);
            localparam ALIAS = MASK;
            assign y = '1;
            assign count = $countbits(ALIAS, 'x, 'z);
        endmodule"#,
        Path::new("countbits_alias.sv"),
    )
    .unwrap();
    let module = &ir.modules()[0];
    assert_eq!(module.parameters()[2].resolved_value(), None);
    let ir::Expr::Literal(literal) = module.assignments()[1].rhs() else {
        panic!(
            "a constant count should be folded: {:?}",
            module.assignments()[1].rhs()
        );
    };
    let literal = typecheck::parse_integral_literal(literal).unwrap();
    assert_eq!(literal.width, 32);
    assert!(literal.signed);
    assert_eq!(literal.value, num_bigint::BigUint::from(4u8));
    assert_eq!(literal.mask, num_bigint::BigUint::default());
}
