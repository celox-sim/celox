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
        let syntax = source_tree::parse_source(&source, Path::new("nested_generate.sv")).unwrap();
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
fn selects_inferred_constant_parameter_dimensions() {
    for (initializer, selection, expected) in [
        ("18", "W[3:0]", 2),
        ("18", "W[0 +: 4]", 2),
        ("64'habcdef", "W[11:8]", 13),
        ("-1", "W[31:28]", 15),
    ] {
        let source = format!(
            "module Top; parameter W = {initializer}; \
            localparam N = {selection}; localparam BITS = $bits(N); endmodule"
        );
        let ir = analyze_source(&source, Path::new("inferred_parameter_select.sv")).unwrap();
        let parameters = ir.modules()[0].parameters();
        assert_eq!(parameters[1].resolved_value(), Some(expected));
        assert_eq!(parameters[1].resolved_width(), Some(4));
        assert_eq!(parameters[2].resolved_value(), Some(4));
    }
}

#[test]
fn permits_short_circuited_constant_parameter_operands() {
    for (expression, expected) in [
        ("0 && 65537'd0", 0),
        ("1 || 65537'bx", 1),
        ("0 && 1000000000'd0", 0),
        ("1 || 1000000000'bx", 1),
        ("0 && 1000000000'd0 || 1", 1),
        ("1 || 1000000000'd0 && 0", 1),
        ("(0 && 1000000000'd0) || 1", 1),
        ("0 && 8'(1000000000'd0)", 0),
        ("1'bx && 0 && 1000000000'd0", 0),
    ] {
        let source = format!("module Top; localparam N = {expression}; endmodule");
        let ir = analyze_source(&source, Path::new("short_circuit_parameter.sv")).unwrap();
        assert_eq!(
            ir.modules()[0].parameters()[0].resolved_value(),
            Some(expected)
        );
        assert_eq!(ir.modules()[0].parameters()[0].resolved_width(), Some(1));
    }
    for expression in [
        "1 && 1000000000'd0",
        "0 || 1000000000'd0",
        "0 && 1000000000'd0 || 1000000000'd0",
        "1'bx && 1000000000'd0",
        "1000000000'd0 === 1000000000'd0",
        "0 && $bits(data[0 +: 1000000000'd1])",
    ] {
        let source =
            format!("module Top(input logic [7:0] data); localparam N = {expression}; endmodule");
        let error =
            analyze_source(&source, Path::new("evaluated_parameter_operand.sv")).unwrap_err();
        assert!(
            error.to_string().contains("constant materialization limit"),
            "{expression}: {error}"
        );
    }
    assert!(
        analyze_source(
            "module Top; localparam N = 0 && missing_type'(65537'd0); endmodule",
            Path::new("skipped_invalid_cast.sv"),
        )
        .is_err()
    );
}

#[test]
fn validates_shapes_in_short_circuited_parameter_operands() {
    for selection in [
        "W[0 +: 0]",
        "W[0 -: -1]",
        "W[0][0 +: 1]",
        "{W}[0 +: 0]",
        "{W}[missing +: 1]",
        "{W}[missing]",
        "{W}[missing:0]",
        "{W}[0:missing]",
        "W[missing]",
        "W[missing:0]",
        "$bits(W[0][0 +: 1])",
    ] {
        let source = format!(
            "module Top; localparam logic [7:0] W = 0; localparam N = 0 && ({selection} || 1000000000'd0); endmodule"
        );
        assert!(
            analyze_source(&source, Path::new("skipped_parameter_shape.sv")).is_err(),
            "{selection}"
        );
    }
    for selection in [
        "W[0 +: 1]",
        "{W}[0 +: 1]",
        "{W}['x +: 1]",
        "{W}[0]",
        "{W}['x]",
        "{W}[3:0]",
    ] {
        let source = format!(
            "module Top; localparam logic [7:0] W = 0; localparam N = 0 && ({selection} || 1000000000'd0); endmodule"
        );
        let ir = analyze_source(&source, Path::new("skipped_valid_parameter_shape.sv")).unwrap();
        assert_eq!(ir.modules()[0].parameters()[1].resolved_value(), Some(0));
    }
}

#[test]
fn validates_user_calls_in_short_circuited_parameter_operands() {
    for call in ["f(.missing(1))", "f(.x(1), .x(2))", "f(1, 2)", "f()", "f"] {
        let source = format!(
            "module Top; function automatic int f(input int x); return x; endfunction localparam N = 0 && ({call} || 1000000000'd0); endmodule"
        );
        assert!(
            analyze_source(&source, Path::new("skipped_parameter_user_call.sv")).is_err(),
            "{call}"
        );
    }
    for call in ["f(.x(1000000000'd0))", "f()", "f"] {
        let source = format!(
            "module Top; function automatic int f(input int x = 1); return x; endfunction localparam N = 0 && ({call} || 1000000000'd0); endmodule"
        );
        let ir = analyze_source(&source, Path::new("skipped_valid_user_call.sv")).unwrap();
        assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(0));
    }
}

#[test]
fn validates_names_in_short_circuited_parameter_operands() {
    for operand in [
        "missing",
        "missing[0]",
        "f(missing)",
        "f(.x(missing))",
        "$clog2(missing)",
        "{missing, 1'b0}",
        "(1 ? 0 : missing)",
        "data",
        "f(data)",
        "$bits(missing)",
        "$bits(f(missing))",
        "$size(data, missing)",
    ] {
        let source = format!(
            "module Top(input logic [7:0] data); function automatic int f(input int x); return x; endfunction localparam N = 0 && ({operand} || 1000000000'd0); endmodule"
        );
        assert!(
            analyze_source(&source, Path::new("skipped_parameter_name.sv")).is_err(),
            "accepted invalid operand: {operand}"
        );
    }
    for operand in [
        "W",
        "W[0]",
        "f(W)",
        "f(.x(W))",
        "p::W",
        "E",
        "X",
        "$bits(data)",
        "$bits(word_t)",
        "$bits(f(data))",
        "$size(data)",
        "$bits(1000000000'd0)",
        "f(1000000000'd0)",
    ] {
        let source = format!(
            "package p; localparam W = 1; endpackage module Top(input logic [7:0] data); localparam W = 1; localparam X = 1'bx; typedef logic [7:0] word_t; typedef enum logic {{ E = 0 }} enum_t; function automatic int f(input int x); return x; endfunction localparam N = 0 && ({operand} || 1000000000'd0); endmodule"
        );
        let ir = analyze_source(&source, Path::new("skipped_valid_parameter_name.sv"))
            .unwrap_or_else(|error| panic!("{operand}: {error}"));
        assert_eq!(
            ir.modules()[0]
                .parameters()
                .last()
                .unwrap()
                .resolved_value(),
            Some(0)
        );
    }
}

#[test]
fn folded_parameter_names_require_a_declaration_prefix() {
    for body in [
        "localparam A = 0 && A && 1000000000'd0;",
        "localparam A = 0 && B && 1000000000'd0; localparam B = 1;",
        "localparam A = 0 && f(A) && 1000000000'd0;",
        "localparam A = 0 && $bits(A) && 1000000000'd0;",
    ] {
        let source = format!(
            "module Top; function automatic int f(input int x); return x; endfunction {body} endmodule"
        );
        assert!(
            analyze_source(&source, Path::new("folded_prefix.sv")).is_err(),
            "{body}"
        );
    }
    let source = "module Top; localparam A = 1; localparam B = 0 && A && 1000000000'd0; endmodule";
    let ir = analyze_source(source, Path::new("folded_prefix_valid.sv")).unwrap();
    assert_eq!(ir.modules()[0].parameters()[1].resolved_value(), Some(0));
}

#[test]
fn logical_query_types_resolve_both_operands() {
    for expression in [
        "missing && 0",
        "0 && missing",
        "missing || 1",
        "1 || missing",
    ] {
        let source = format!("module Top; localparam N = $bits({expression}); endmodule");
        assert!(
            analyze_source(&source, Path::new("logical_query_name.sv")).is_err(),
            "{expression}"
        );
    }
    for expression in ["data && 0", "0 && data", "data || 1", "1 || data"] {
        let source = format!(
            "module Top(input logic [7:0] data); localparam N = $bits({expression}); endmodule"
        );
        let ir = analyze_source(&source, Path::new("logical_query_valid.sv")).unwrap();
        assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(1));
    }
}

#[test]
fn folded_parameter_queries_validate_member_paths() {
    for member in ["missing", "a.missing"] {
        let source = format!(
            "module Top; struct packed {{ logic a; }} value; localparam N = 0 && ($bits(value.{member}) || 1000000000'd0); endmodule"
        );
        assert!(
            analyze_source(&source, Path::new("folded_member.sv")).is_err(),
            "{member}"
        );
    }
    let source = "module Top; struct packed { logic a; } value; localparam N = 0 && ($bits(value.a) || 1000000000'd0); endmodule";
    let ir = analyze_source(source, Path::new("folded_member_valid.sv")).unwrap();
    assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(0));
}

#[test]
fn folded_parameter_replication_counts_are_validated() {
    for count in ["-1", "1'bx", "1'bz", "missing"] {
        for operand in [
            format!("{{{count}{{1'b0}}}}"),
            format!("f({{{count}{{1'b0}}}})"),
            format!("word_t'('{{{count}{{1'b0}}}})"),
        ] {
            let source = format!(
                "module Top; typedef logic [1:0] word_t; function automatic int f(input int x); return x; endfunction localparam N = 0 && ({operand} || 1000000000'd0); endmodule"
            );
            assert!(
                analyze_source(&source, Path::new("folded_repeat.sv")).is_err(),
                "{operand}"
            );
        }
    }
    for operand in [
        "{2{1'b0}}",
        "{1000000000{1'b0}}",
        "{2{1000000000'd0}}",
        "{1'b0, {0{1'b0}}}",
        "word_t'('{2{1'b0}})",
    ] {
        let source = format!(
            "module Top; typedef logic [1:0] word_t; localparam N = 0 && ({operand} || 1000000000'd0); endmodule"
        );
        let ir = analyze_source(&source, Path::new("folded_repeat_valid.sv"))
            .unwrap_or_else(|error| panic!("{operand}: {error}"));
        assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(0));
    }
}

#[test]
fn folded_parameter_streaming_slice_sizes_are_validated() {
    // IEEE 1800-2023 11.4.14.2 requires a positive constant slice size.
    for size in [
        "0", "-1", "1'bx", "1'bz", "missing", "ZERO", "UNKNOWN", "(2 - 2)",
    ] {
        let source = format!(
            "module Top; function automatic int f(input int x); return x; endfunction localparam ZERO = 0; localparam UNKNOWN = 1'bx; localparam N = 0 && (f({{<< {size} {{1'b0}}}}) || 1000000000'd0); endmodule"
        );
        assert!(
            analyze_source(&source, Path::new("folded_stream_size.sv")).is_err(),
            "{size}"
        );
    }
    for size in ["", "1", "(2 + 2)", "STEP", "byte", "word_t"] {
        let source = format!(
            "module Top; function automatic int f(input int x); return x; endfunction typedef logic [3:0] word_t; localparam STEP = 4; localparam N = 0 && (f({{<< {size} {{1000000000'd0}}}}) || 1000000000'd0); endmodule"
        );
        let ir = analyze_source(&source, Path::new("folded_stream_size_valid.sv"))
            .unwrap_or_else(|error| panic!("{size}: {error}"));
        assert_eq!(
            ir.modules()[0]
                .parameters()
                .last()
                .unwrap()
                .resolved_value(),
            Some(0)
        );
    }
}

#[test]
fn folded_parameter_streaming_with_ranges_are_validated() {
    for range in [
        "0 +: 0",
        "0 -: -1",
        "0 +: 1'bx",
        "0 -: 1'bz",
        "missing",
        "0 : missing",
    ] {
        let source = format!(
            "module Top; function automatic int f(input int x); return x; endfunction localparam logic [7:0] W [0:1] = '{{0, 0}}; localparam N = 0 && (f({{<< 1 {{W with [{range}]}}}}) || 1000000000'd0); endmodule"
        );
        assert!(
            analyze_source(&source, Path::new("folded_stream_range.sv")).is_err(),
            "{range}"
        );
    }
    for range in ["0", "0:1", "0 +: 2", "1 -: 2"] {
        let source = format!(
            "module Top; function automatic int f(input int x); return x; endfunction localparam logic [7:0] W [0:1] = '{{0, 0}}; localparam N = 0 && (f({{<< 1 {{W with [{range}]}}}}) || 1000000000'd0); endmodule"
        );
        let ir = analyze_source(&source, Path::new("folded_stream_range_valid.sv"))
            .unwrap_or_else(|error| panic!("{range}: {error}"));
        assert_eq!(
            ir.modules()[0]
                .parameters()
                .last()
                .unwrap()
                .resolved_value(),
            Some(0)
        );
    }

    for declaration in [
        "localparam W = 8'b0;",
        "localparam logic [7:0] W [0:1][0:1] = '{'{0, 0}, '{0, 0}};",
    ] {
        let source = format!(
            "module Top; function automatic int f(input int x); return x; endfunction {declaration} localparam N = 0 && (f({{<< 1 {{W with [0 +: 1]}}}}) || 1000000000'd0); endmodule"
        );
        let error = analyze_source(&source, Path::new("folded_stream_array_shape.sv")).unwrap_err();
        assert!(
            error.to_string().contains("one-dimensional unpacked array"),
            "{error}"
        );
    }
}

#[test]
fn folded_array_queries_allow_out_of_range_dimensions() {
    // IEEE 1800-2023 20.7 returns X for an out-of-range dimension;
    // the call remains legal and may be skipped by a logical operator.
    for name in ["$size", "$left", "$right", "$low", "$high", "$increment"] {
        for dimension in ["0", "-1", "2", "1'bx", "1'bz"] {
            let source = format!(
                "module Top(input logic [7:0] data); localparam N = 0 && ({name}(data, {dimension}) || 1000000000'd0); endmodule"
            );
            let ir = analyze_source(&source, Path::new("folded_query_dimension.sv"))
                .unwrap_or_else(|error| panic!("{name}/{dimension}: {error}"));
            assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(0));
        }
    }
}

#[test]
fn bounded_parameter_proofs_preserve_left_associative_power() {
    // IEEE 1800-2023 Table 11-2: ** is left-associative.
    for expression in [
        "((2 ** 3 ** 2) == 512) && 1000000000'bx",
        "((2 ** 3 ** 2) == 64) || 1000000000'bx",
    ] {
        let source = format!("module Top; localparam N = {expression}; endmodule");
        let ir = analyze_source(&source, Path::new("bounded_power.sv")).unwrap();
        assert_eq!(
            ir.modules()[0].parameters()[0].resolved_value(),
            Some(i128::from(expression.contains("||")))
        );
    }
    for expression in [
        "((2 ** 3 ** 2) == 64) && 1000000000'bx",
        "((2 ** (3 ** 2)) == 512) && 1000000000'bx",
    ] {
        let source = format!("module Top; localparam N = {expression}; endmodule");
        assert!(
            analyze_source(&source, Path::new("evaluated_power.sv")).is_err(),
            "{expression}"
        );
    }
}

#[test]
fn bounded_parameter_proofs_skip_computed_repetitions() {
    for (expression, expected) in [
        ("0 && {1000000000{1'b0}}", 0),
        ("1 || {1000000000{1'bx}}", 1),
        ("0 && {2{65536'b0}}", 0),
        ("0 && {2{{1000000000{1'b0}}}}", 0),
    ] {
        let source = format!("module Top; localparam N = {expression}; endmodule");
        let ir = analyze_source(&source, Path::new("bounded_repetition.sv")).unwrap();
        assert_eq!(
            ir.modules()[0].parameters()[0].resolved_value(),
            Some(expected),
            "{expression}"
        );
    }
}

#[test]
fn bounded_parameter_proofs_use_typed_constant_selections() {
    for (declaration, selected, expected) in [
        ("localparam P = 2'b00;", "P[0]", 0),
        ("localparam P = 2'b10;", "P[1]", 1),
        ("localparam logic [4:3] P = 2'b10;", "P[3]", 0),
        ("localparam logic [3:4] P = 2'b10;", "P[3]", 1),
        ("localparam logic [1:0][1:0] P = 4'b0011;", "P[1]", 0),
        ("localparam P = 4'b1000;", "P[2:1]", 0),
        ("localparam P = 4'b1000;", "P[0 +: 2]", 0),
    ] {
        let operator = if expected == 0 { "&&" } else { "||" };
        let source = format!(
            "module Top; {declaration} localparam N = {selected} {operator} 1000000000'd0; endmodule"
        );
        let ir = analyze_source(&source, Path::new("bounded_parameter_select.sv"))
            .unwrap_or_else(|error| panic!("{selected}: {error}"));
        assert_eq!(
            ir.modules()[0]
                .parameters()
                .last()
                .unwrap()
                .resolved_value(),
            Some(expected)
        );
    }
    for selected in ["P[0]", "P[2]", "P['x]"] {
        let source = format!(
            "module Top; localparam P = 2'b1x; localparam N = {selected} && 1000000000'd0; endmodule"
        );
        assert!(
            analyze_source(&source, Path::new("bounded_unknown_select.sv")).is_err(),
            "{selected}"
        );
    }
}

#[test]
fn folded_parameter_patterns_validate_target_shapes() {
    for pattern in [
        "pair_t'('{1'b0})",
        "pair_t'('{1'b0, 1'b1, 1'b0})",
        "pair_t'('{missing: 1'b0, default: 1'b0})",
        "pair_t'('{a: 1'b0})",
        "pair_t'{a: 1'b0}",
        "nested_t'{p: '{a: 1'b0}}",
        "f('{a: 1'b0})",
        "word_t'('{2: 1'b0, default: 1'b0})",
        "word_t'('{0: 1'b0})",
        "word_t'('{1000000000{1'b0}})",
    ] {
        let source = format!(
            "module Top; typedef struct packed {{ logic a; logic b; }} pair_t; typedef logic [1:0] word_t; typedef struct packed {{ pair_t p; }} nested_t; typedef logic [999999999:0] giant_t; function automatic int f(input pair_t p); return p.a; endfunction localparam N = 0 && ({pattern} || 1000000000'd0); endmodule"
        );
        assert!(
            analyze_source(&source, Path::new("folded_pattern_shape.sv")).is_err(),
            "{pattern}"
        );
    }
    for pattern in [
        "pair_t'('{1'b0, 1'b1})",
        "pair_t'('{a: 1'b0, default: 1'b1})",
        "pair_t'{default: 1000000000'd0}",
        "word_t'('{2{1000000000'd0}})",
        "word_t'('{0: 1'b0, default: 1'b0})",
        "nested_t'{p: '{default: 1'b0}}",
        "f('{default: 1000000000'd0})",
        "giant_t'('{default: 1'b0})",
        "giant_t'('{1000000000{1'b0}})",
    ] {
        let source = format!(
            "module Top; typedef struct packed {{ logic a; logic b; }} pair_t; typedef logic [1:0] word_t; typedef struct packed {{ pair_t p; }} nested_t; typedef logic [999999999:0] giant_t; function automatic int f(input pair_t p); return p.a; endfunction localparam N = 0 && ({pattern} || 1000000000'd0); endmodule"
        );
        let ir = analyze_source(&source, Path::new("folded_pattern_shape_valid.sv"))
            .unwrap_or_else(|error| panic!("{pattern}: {error}"));
        assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(0));
    }
}

#[test]
fn local_values_shadow_imported_bare_function_calls() {
    for declaration in ["localparam f = 0;", "typedef enum logic { f = 0 } E;"] {
        let source = format!(
            "package p; function automatic int f(input int x); return x; endfunction endpackage module Top; import p::*; {declaration} localparam N = 0 && (f || 1000000000'd0); endmodule"
        );
        let ir = analyze_source(&source, Path::new("shadowed_bare_function.sv")).unwrap();
        let n = ir.modules()[0]
            .parameters()
            .iter()
            .find(|parameter| parameter.name() == "N")
            .unwrap();
        assert_eq!(n.resolved_value(), Some(0));
        assert_eq!(n.resolved_width(), Some(1));
    }
}

#[test]
fn current_parameter_shadows_imports_in_its_initializer() {
    for flag in ["1'b0", "1'bx"] {
        let source = format!(
            "package p; localparam FLAG = {flag}; endpackage module Top; import p::*; localparam FLAG = FLAG && 1000000000'd0; endmodule"
        );
        assert!(
            analyze_source(&source, Path::new("self_referencing_parameter.sv")).is_err(),
            "{flag}"
        );
    }
    let ir = analyze_source(
        "package p; localparam FLAG = 0; endpackage module Top; import p::*; localparam FLAG = p::FLAG && 1000000000'd0; endmodule",
        Path::new("qualified_parameter_import.sv"),
    ).unwrap();
    assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(0));
}

#[test]
fn validates_calls_in_short_circuited_constant_parameter_operands() {
    for call in ["$random()", "$unknown()", "$clog2()", "$bits()"] {
        let source =
            format!("module Top; localparam N = 0 && ({call} || 1000000000'd0); endmodule");
        let error =
            analyze_source(&source, Path::new("skipped_parameter_call.sv")).expect_err(call);
        assert!(
            error.to_string().contains(call.split('(').next().unwrap()),
            "{error}"
        );
    }
    let ir = analyze_source(
        "module Top; localparam N = 0 && $clog2(1000000000'd0); endmodule",
        Path::new("skipped_valid_parameter_call.sv"),
    )
    .unwrap();
    assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(0));
}

#[test]
fn short_circuits_four_state_parameter_bindings() {
    for flag in ["1'bx", "1'bz", "4'bx001"] {
        for (expression, expected) in [
            ("FLAG && 0 && 1000000000'd0", 0),
            ("FLAG || 1 || 1000000000'd0", 1),
        ] {
            for declaration in [
                format!("module Top; localparam FLAG = {flag};"),
                format!("package p; localparam FLAG = {flag}; endpackage module Top; import p::*;"),
                format!(
                    "package p; localparam FLAG = 1; endpackage module Top; import p::*; localparam FLAG = {flag};"
                ),
            ] {
                let source = format!("{declaration} localparam N = {expression}; endmodule");
                let ir = analyze_source(&source, Path::new("four_state_parameter_proof.sv"))
                    .unwrap_or_else(|error| panic!("{source}: {error}"));
                let n = ir.modules()[0]
                    .parameters()
                    .iter()
                    .find(|p| p.name() == "N")
                    .unwrap();
                assert_eq!(n.resolved_value(), Some(expected), "{source}");
                assert_eq!(n.resolved_width(), Some(1), "{source}");
            }
        }
        let source = format!(
            "module Top; localparam FLAG = {flag}; localparam N = FLAG && 1000000000'd0; endmodule"
        );
        let error =
            analyze_source(&source, Path::new("evaluated_four_state_parameter.sv")).unwrap_err();
        assert!(
            error.to_string().contains("constant materialization limit"),
            "{error}"
        );
    }
}

#[test]
fn resolves_constant_parameter_part_selects() {
    for (declaration, selection, expected, width, signed) in [
        ("logic [31:0] W = 18", "W[3:0] + 1", 3, 32, false),
        ("logic [11:4] W = 8'hab", "W[7:4]", 11, 4, false),
        ("logic [4:11] W = 8'hab", "W[8:11]", 11, 4, false),
        ("logic [-1:-8] W = 8'hab", "W[-5:-8]", 11, 4, false),
        ("logic [-8:-1] W = 8'hab", "W[-4:-1]", 11, 4, false),
        ("logic signed [7:0] W = -2", "W[7:0] >> 1", 127, 8, false),
        ("logic [7:0] W = 8'hab", "int'(W[3:0]) + 1", 12, 32, true),
        ("logic [7:0] W = 8'hab", "$clog2(W[7:4])", 4, 32, true),
        ("logic [1:0][7:4] W = 8'hab", "W[0][7:4]", 11, 4, false),
        ("logic [0:1][4:7] W = 8'hab", "W[1][4:7]", 11, 4, false),
        ("logic [2:1][7:4] W = 8'hab", "W[2:2]", 10, 4, false),
        ("logic [1:2][4:7] W = 8'hab", "W[1:1]", 10, 4, false),
        ("logic [1:0][7:4] W = 8'hab", "W[0][5]", 1, 1, false),
    ] {
        let source = format!(
            "module Top; parameter {declaration}; localparam N = {selection}; \
             localparam BITS = $bits(N); endmodule"
        );
        let ir = analyze_source(&source, Path::new("constant_parameter_select.sv"))
            .unwrap_or_else(|error| panic!("{selection}: {error}"));
        let parameters = ir.modules()[0].parameters();
        assert_eq!(
            parameters[1].resolved_value(),
            Some(expected),
            "{selection}"
        );
        assert_eq!(
            parameters[1].resolved_width(),
            Some(width as usize),
            "{selection}"
        );
        assert_eq!(parameters[1].resolved_signed(), Some(signed), "{selection}");
        assert_eq!(parameters[2].resolved_value(), Some(width), "{selection}");
    }
}

#[test]
fn resolves_imported_constant_parameter_part_selects() {
    for (declaration, selection, expected) in [
        ("logic [11:4] D = 8'hab", "[7:4]", 11),
        ("logic [4:11] D = 8'hab", "[8:11]", 11),
        ("logic [11:4] D = 8'hxa", "[7:4]", 10),
    ] {
        for import in ["import cfg::*;", "import cfg::D;", ""] {
            let reference = if import.is_empty() { "cfg::D" } else { "D" };
            let source = format!(
                "package cfg; parameter {declaration}; endpackage \
             module Top; {import} localparam N = {reference}{selection}; \
             localparam BITS = $bits(N); endmodule"
            );
            let ir = analyze_source(&source, Path::new("imported_parameter_select.sv")).unwrap();
            let parameters = ir.modules()[0].parameters();
            assert_eq!(
                parameters[0].resolved_value(),
                Some(expected),
                "{import}: {declaration}"
            );
            assert_eq!(parameters[0].resolved_width(), Some(4), "{import}");
            assert_eq!(parameters[1].resolved_value(), Some(4), "{import}");
        }
    }
    let source = "package cfg; parameter logic [11:4] D = 8'hab; endpackage \
                  module Top #(parameter N = cfg::D[7:4])(output int value); \
                  assign value = N; endmodule";
    let ir = analyze_source(source, Path::new("imported_header_parameter_select.sv")).unwrap();
    assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(11));
    let parsed =
        ParsedSource::parse(source, Path::new("imported_header_parameter_select.sv")).unwrap();
    let packages = ParsedSource::analyze_packages_with_roots(&[&parsed], &["Top"]).unwrap();
    let ir = parsed
        .analyze_module(
            "Top",
            &HashMap::default(),
            &[],
            &[],
            &ModuleInterfaces::default(),
            &packages,
        )
        .unwrap();
    assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(11));
}

#[test]
fn preserves_unknown_out_of_range_constant_parameter_elements() {
    for (declaration, selection, width, is_x) in [
        ("logic [1:0][7:4] W = 8'hab", "W[2][5]", 1, true),
        ("logic [1:0][7:4] W = 8'hab", "W[0][8]", 1, true),
        ("logic [1:0][7:4] W = 8'hab", "W[2]", 4, true),
        ("logic [1:0][63:0] W = 128'h1", "W[2][5]", 1, true),
        ("bit [1:0][7:4] W = 8'hab", "W[2][5]", 1, false),
        ("bit [1:0][7:4] W = 8'hab", "W[0][8]", 1, false),
    ] {
        let source = format!(
            "module Top; parameter {declaration}; \
             localparam N = {selection}; localparam BITS = $bits(N); \
             localparam IS_X = N === 'x; endmodule"
        );
        let ir = analyze_source(&source, Path::new("out_of_range_parameter_select.sv"))
            .unwrap_or_else(|error| panic!("{selection}: {error}"));
        let parameters = ir.modules()[0].parameters();
        assert_eq!(
            parameters[1].resolved_value(),
            if is_x { None } else { Some(0) },
            "{selection}"
        );
        assert_eq!(parameters[1].resolved_width(), Some(width), "{selection}");
        assert_eq!(parameters[1].resolved_signed(), Some(false), "{selection}");
        assert_eq!(parameters[2].resolved_value(), Some(width as i128));
        assert_eq!(
            parameters[3].resolved_value(),
            Some(i128::from(is_x)),
            "{selection}"
        );
    }
}

#[test]
fn preserves_overhanging_constant_parameter_part_selects() {
    for (declaration, selection, expected) in [
        ("logic [7:0] W = 8'hab", "W[8:5]", "4'bx101"),
        ("logic [7:0] W = 8'hab", "W[2:-1]", "4'b011x"),
        ("logic [7:0] W = 8'hab", "W[6 +: 4]", "4'bxx10"),
        ("logic [7:0] W = 8'hab", "W[1 -: 4]", "4'b11xx"),
        ("logic [7:0] W = 8'hab", "W[12:9]", "4'bxxxx"),
        ("bit [7:0] W = 8'hab", "W[8:5]", "4'bx101"),
        ("bit [7:0] W = 8'hab", "W[6 +: 4]", "4'bxx10"),
        ("logic [11:4] W = 8'hab", "W[12:9]", "4'bx101"),
        ("logic [4:11] W = 8'hab", "W[3:6]", "4'bx101"),
        ("logic [1:0][7:4] W = 8'hab", "W[0][8:5]", "4'bx101"),
        ("logic [1:0][7:4] W = 8'hab", "W[0][6 +: 4]", "4'bxx10"),
        ("logic [1:0][7:4] W = 8'hab", "W[2][7:4]", "4'bxxxx"),
        ("logic [1:0][7:4] W = 8'hab", "W[2:1]", "8'bxxxx1010"),
    ] {
        let source = format!(
            "module Top; parameter {declaration}; localparam N = {selection}; \
             localparam SAME = N === {expected}; endmodule"
        );
        let ir = analyze_source(&source, Path::new("overhanging_parameter_select.sv"))
            .unwrap_or_else(|error| panic!("{selection}: {error}"));
        let parameters = ir.modules()[0].parameters();
        assert_eq!(parameters[1].resolved_value(), None, "{selection}");
        let expected_width = expected
            .split('\'')
            .next()
            .unwrap()
            .parse::<usize>()
            .unwrap();
        assert_eq!(
            parameters[1].resolved_width(),
            Some(expected_width),
            "{selection}"
        );
        assert_eq!(parameters[1].resolved_signed(), Some(false), "{selection}");
        assert_eq!(parameters[2].resolved_value(), Some(1), "{selection}");
    }
}

#[test]
fn bounds_constant_parameter_part_select_folding() {
    for selection in [
        "W[65536:0]",
        "W[1000000000:0]",
        "W[0 +: 1000000000]",
        "W[1'bx +: 65537]",
        "W[1'bz -: 1000000000]",
        "W[1'bx +: 0]",
        "W[1'bx +: -1]",
        "W[1'bx +: 1'bx]",
    ] {
        let source = format!(
            "module Top; parameter logic [7:0] W = 0; localparam N = {selection}; endmodule"
        );
        let error = analyze_source(&source, Path::new("oversized_parameter_select.sv"))
            .expect_err("oversized selections must be rejected before allocating their bits");
        assert!(matches!(error, AnalyzerError::Unsupported(_)), "{error}");
    }
    for selection in ["W[65535:0]", "W[1'bx +: 65536]"] {
        let source = format!(
            "module Top; parameter logic [7:0] W = 0; \
            localparam N = {selection}; localparam BITS = $bits(N); endmodule"
        );
        let ir = analyze_source(&source, Path::new("maximum_parameter_select.sv")).unwrap();
        let parameters = ir.modules()[0].parameters();
        assert_eq!(parameters[1].resolved_width(), Some(65_536));
        assert_eq!(parameters[2].resolved_value(), Some(65_536));
    }
}

#[test]
fn rejects_oversized_constant_parameter_operands_before_materialization() {
    for source in [
        "module Top; parameter logic [65536:0] W = 0; localparam N = W[0:0]; endmodule",
        "module Top; parameter logic [1000000000:0] W = 0; localparam N = W[0:0]; endmodule",
        "module Top; parameter logic [1000000000:0] W = '1; localparam N = W[0:0]; endmodule",
        "module Top #(parameter logic [1000000000:0] W = 0, N = W[0:0]); endmodule",
        "package p; parameter logic [1000000000:0] W = 0; endpackage \
         module Top; localparam N = p::W[0:0]; endmodule",
        "module Top; typedef logic [1000000000:0] wide_t; parameter wide_t W = 0; \
         localparam N = W[0:0]; endmodule",
        "module Top; localparam W = 65537'd0; localparam N = W[0:0]; endmodule",
        "module Top; localparam W = 1000000000'd0; localparam N = W[0:0]; endmodule",
        "module Top; localparam W = 1000000000'bx; localparam N = W[0:0]; endmodule",
    ] {
        let error = analyze_source(source, Path::new("oversized_parameter_operand.sv"))
            .expect_err("an oversized operand must not be expanded before a narrow selection");
        assert!(matches!(error, AnalyzerError::Unsupported(_)), "{error}");
        assert!(
            error.to_string().contains("constant materialization limit"),
            "{error}"
        );
    }
    let source = "module Top; parameter logic [65535:0] W = 1; \
        localparam N = W[0:0]; endmodule";
    let ir = analyze_source(source, Path::new("maximum_parameter_operand.sv")).unwrap();
    let parameters = ir.modules()[0].parameters();
    assert_eq!(parameters[0].resolved_width(), Some(65_536));
    assert_eq!(parameters[1].resolved_width(), Some(1));
    assert_eq!(parameters[1].resolved_value(), Some(1));
}

#[test]
fn permits_wide_unevaluated_constant_parameter_query_operands() {
    for (query, expected) in [
        ("$bits(1000000000'd0)", 1_000_000_000),
        ("$bits(1000000000'd0 + 1)", 1_000_000_000),
        ("$bits(65537'bx)", 65_537),
        ("$bits($bits(65537'd0))", 32),
        ("$size(65537'd0)", 65_537),
        ("$bits({2{65537'd0}})", 131_074),
        ("$bits(8'(65537'd0))", 8),
    ] {
        let source = format!("module Top; localparam N = {query}; endmodule");
        let ir = analyze_source(&source, Path::new("wide_query_operand.sv"))
            .unwrap_or_else(|error| panic!("{query}: {error}"));
        assert_eq!(
            ir.modules()[0].parameters()[0].resolved_value(),
            Some(expected),
            "{query}"
        );
    }
    let error = analyze_source(
        "module Top(input logic [3:0] data); localparam N = $size(data, 65537'd1); endmodule",
        Path::new("wide_query_dimension.sv"),
    )
    .expect_err("the evaluated dimension argument must still have a bounded value");
    assert!(
        error.to_string().contains("constant materialization limit"),
        "{error}"
    );
}

#[test]
fn preserves_unknown_constant_parameter_indexed_part_select_bases() {
    for (declaration, selection, expected, number, width) in [
        ("logic [1:0][7:4]", "W[1][1'bx +: 2]", "2'bxx", None, 2),
        ("logic [1:0][7:4]", "W[0][1'bz -: 2]", "2'bxx", None, 2),
        ("logic [1:0][7:4]", "W[I +: 1]", "4'bxxxx", None, 4),
        ("logic [7:0]", "W[I -: 3]", "3'bxxx", None, 3),
        ("logic [7:0]", "{W}[I +: 3]", "3'bxxx", None, 3),
        ("bit [7:0]", "W[1'bx +: 2]", "2'bxx", None, 2),
        ("logic [4:7]", "W[1'bz -: 2]", "2'bxx", None, 2),
        (
            "logic [1:0][7:4]",
            "W[0][(1'bx & 1'b0) + 4 +: 2]",
            "2'b11",
            Some(3),
            2,
        ),
        (
            "logic [7:0]",
            "W[(1'bx === 1'bx) +: 2]",
            "2'b01",
            Some(1),
            2,
        ),
    ] {
        let source = format!(
            "module Top; parameter {declaration} W = 8'hab; localparam I = 1'bx; \
             localparam N = {selection}; localparam SAME = N === {expected}; endmodule"
        );
        let ir = analyze_source(&source, Path::new("unknown_parameter_part_select.sv"))
            .unwrap_or_else(|error| panic!("{declaration} {selection}: {error}"));
        let parameters = ir.modules()[0].parameters();
        assert_eq!(parameters[2].resolved_value(), number, "{selection}");
        assert_eq!(parameters[2].resolved_width(), Some(width), "{selection}");
        assert_eq!(parameters[2].resolved_signed(), Some(false), "{selection}");
        assert_eq!(parameters[3].resolved_value(), Some(1), "{selection}");
    }
}

#[test]
fn bounds_evaluated_constant_parameter_query_subexpressions() {
    for literal_width in [65_537, 1_000_000_000] {
        for query in [
            format!("$bits(W[0 +: {literal_width}'d1])"),
            format!("$bits(W[{literal_width}'d0:0])"),
            format!("$bits(W[{literal_width}'d0])"),
            format!("$bits({{{literal_width}'d1{{W}}}})"),
            format!("$bits({literal_width}'d1'(W))"),
            format!("$bits($size(W, {literal_width}'d1))"),
            format!("$bits({{<<{literal_width}'d1{{W}}}})"),
            format!("$bits(logic [{literal_width}'d0:0])"),
        ] {
            let source = format!(
                "module Top; parameter logic [7:0] W = 0; localparam N = {query}; endmodule"
            );
            let error = analyze_source(&source, Path::new("wide_query_shape.sv"))
                .expect_err("type-shaping values must be bounded even inside unevaluated operands");
            assert!(
                error.to_string().contains("constant materialization limit"),
                "{query}: {error}"
            );
        }
    }
    let ir = analyze_source(
        "module Top; parameter logic [7:0] W = 0; \
         localparam N = $bits(W[0 +: 65536'd1]); \
         localparam COUNT = $bits({65536'd1{W}}); endmodule",
        Path::new("bounded_query_shape.sv"),
    )
    .unwrap();
    let parameters = ir.modules()[0].parameters();
    assert_eq!(parameters[1].resolved_value(), Some(1));
    assert_eq!(parameters[2].resolved_value(), Some(8));
}

#[test]
fn preserves_unknown_constant_parameter_selection_indices() {
    for (kind, selection, expected, number, width) in [
        ("logic", "W[1'bx][5]", "1'bx", None, 1),
        ("logic", "W[1][1'bz]", "1'bx", None, 1),
        ("logic", "W[1'bz][7:4]", "4'bxxxx", None, 4),
        ("logic", "W[1'bx][7:4]", "4'bxxxx", None, 4),
        ("logic", "W[I][5]", "1'bx", None, 1),
        ("bit", "W[1'bx][5]", "1'b0", Some(0), 1),
        ("bit", "W[1'bz][7:4]", "4'b0000", Some(0), 4),
        ("logic", "W[1'bx & 1'b0][4]", "1'b1", Some(1), 1),
        ("logic", "W[1'bx === 1'bx][4]", "1'b0", Some(0), 1),
    ] {
        let source = format!(
            "module Top; parameter {kind} [1:0][7:4] W = 8'hab; \
            localparam I = 1'bx; localparam N = {selection}; \
            localparam SAME = N === {expected}; endmodule"
        );
        let ir = analyze_source(&source, Path::new("unknown_parameter_index.sv"))
            .unwrap_or_else(|error| panic!("{kind} {selection}: {error}"));
        let parameters = ir.modules()[0].parameters();
        assert_eq!(parameters[2].resolved_value(), number, "{kind} {selection}");
        assert_eq!(
            parameters[2].resolved_width(),
            Some(width),
            "{kind} {selection}"
        );
        assert_eq!(
            parameters[3].resolved_value(),
            Some(1),
            "{kind} {selection}"
        );
    }
}

#[test]
fn preserves_signed_named_constant_parameter_elements() {
    let source = "module Top; typedef logic signed [3:0] nibble_t; \
        parameter nibble_t [1:0][1:0] W = '1; localparam N = W[0][0]; \
        localparam SUM = W[0][0] + 1; localparam PART = W[0][0][3:0]; endmodule";
    let ir = analyze_source(source, Path::new("signed_parameter_elements.sv")).unwrap();
    let parameters = ir.modules()[0].parameters();
    assert_eq!(parameters[1].resolved_value(), Some(-1));
    assert_eq!(parameters[1].resolved_width(), Some(4));
    assert_eq!(parameters[1].resolved_signed(), Some(true));
    assert_eq!(parameters[2].resolved_value(), Some(0));
    assert_eq!(parameters[3].resolved_value(), Some(15));
    assert_eq!(parameters[3].resolved_signed(), Some(false));
}

#[test]
fn resolves_dependent_four_state_generate_imports() {
    let source = "package p; parameter Q = 8'hxa; \
        parameter P = Q; endpackage \
        module Top(output logic [3:0] y); if (1) begin : imported \
        import p::P; localparam N = P[3:0]; assign y = N; end endmodule";
    let ir = analyze_source(source, Path::new("dependent_generate_import.sv")).unwrap();
    assert_eq!(
        ir.modules()[0].assignments()[0].rhs(),
        &crate::ir::Expr::Literal("4'd10".into())
    );
}

#[test]
fn rejects_unrepresentable_constant_parameter_selections() {
    for selection in ["W[MISSING:0]", "W[0][3:0]", "W[0][1]"] {
        let source = format!(
            "module Top; parameter logic [7:0] W = 18; localparam N = {selection}; endmodule"
        );
        let error = analyze_source(&source, Path::new("unsupported_parameter_select.sv"))
            .expect_err("an unused unsupported selection must not become the base parameter");
        assert!(matches!(error, AnalyzerError::Unsupported(_)), "{error}");
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
                localparam logic [7:0] X = 8'b10xz_01zx;
                localparam byte unsigned U = unsigned'(byte'(X));
            endmodule
        "#,
        Path::new("two_state_constant_cast.sv"),
    )
    .expect("two-state constant casts should convert unknown bits to zero");
    assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(1));
    assert_eq!(ir.modules()[0].parameters()[2].resolved_value(), Some(0x84));
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

/// Timing constructs that remain unimplemented are rejected with their
/// own construct names, not as a generic timing control.
#[test]
fn rejects_unimplemented_timing_constructs() {
    for (body, construct) in [
        ("initial begin fork #1; join end", "fork-join block"),
        ("initial wait fork;", "wait fork statement"),
        // The `event` data type is rejected by name before its uses.
        ("event e; initial @e;", "named event"),
        ("event e; initial ->e;", "named event"),
        (
            "logic clk, en; initial @(posedge clk iff en);",
            "iff-qualified event",
        ),
        (
            "logic a, b; initial a = #1 b;",
            "intra-assignment timing control",
        ),
        (
            "logic a, b; initial a <= #1 b;",
            "intra-assignment timing control",
        ),
        ("initial begin : b disable b; end", "disable statement"),
        ("initial disable fork;", "disable fork statement"),
        (
            "logic a, y; always_latch y <= a;",
            "nonblocking assignment inside always_latch",
        ),
    ] {
        let code = format!("module Top; {body} endmodule");
        let error = analyze_source(&code, Path::new("timing.sv"))
            .expect_err(&format!("`{body}` must be rejected"));
        assert!(
            matches!(&error, AnalyzerError::Unsupported(text) if text.starts_with(construct)),
            "`{body}`: {error:?}"
        );
    }
    assert_eq!(
        AnalyzerError::Unsupported("iff-qualified event".to_string()).tracking_issue(),
        1444
    );
}

/// `always_comb` and `always_ff` bodies may not suspend (IEEE 1800-2023
/// 9.2.2.2, 9.2.2.4).
#[test]
fn rejects_timing_controls_in_always_comb_and_always_ff() {
    for body in [
        "logic a; always_comb begin #1 a = 1; end",
        "logic clk, a; always_ff @(posedge clk) begin @(a); end",
        "logic clk, a; always_ff @(posedge clk) wait (a);",
    ] {
        let code = format!("module Top; {body} endmodule");
        let error = analyze_source(&code, Path::new("timing.sv"))
            .expect_err(&format!("`{body}` must be rejected"));
        assert!(
            matches!(error, AnalyzerError::TimingControlOutsideProcess { .. }),
            "`{body}`: {error:?}"
        );
    }
}

/// An `always` that is neither combinational nor edge-triggered RTL is a
/// process that repeats its statement.
#[test]
fn analyzes_always_processes() {
    for body in [
        "logic clk; always #5 clk = ~clk;",
        "logic a, b, y; always @(a or b) y = a & b;",
        "logic clk, q; always @(posedge clk) begin #1 q = ~q; end",
        "logic x; always x = ~x;",
    ] {
        let code = format!("module Top; {body} endmodule");
        let ir = analyze_source(&code, Path::new("process.sv"))
            .unwrap_or_else(|error| panic!("`{body}`: {error}"));
        let module = &ir.modules()[0];
        assert!(
            module
                .initial_processes()
                .iter()
                .any(|process| process.repeats()),
            "`{body}` must be an always process"
        );
    }
}

/// Delays, event controls and waits in an `initial` block keep their
/// statement structure in the IR.
#[test]
fn analyzes_timing_controls_in_initial_blocks() {
    let code = "module Top(input logic clk, output logic [7:0] y);\n\
                initial begin #3; @(posedge clk or negedge clk, y); @(edge clk); #(1:2:3); wait (y > 1); end\n\
                endmodule";
    analyze_source(code, Path::new("timing.sv")).expect("timing controls in an initial block");
}

/// `@*` in a process waits on the variables its statement reads (IEEE
/// 1800-2023 9.4.2.2): right-hand sides, conditions, arguments and
/// left-hand-side indices, but not assignment targets or timing controls.
#[test]
fn implicit_event_controls_wait_on_the_reads_of_their_statement() {
    let code = "module Top(input logic [7:0] a, b, c, d, i, output logic [7:0] y, z);\n\
                initial forever @* begin if (c > 0) y = a; else y = b; z[i] = 1; @(d); #(d); end\n\
                endmodule";
    let ir = analyze_source(code, Path::new("implicit.sv")).expect("`@*` in a process");
    let mut waits = Vec::new();
    for process in ir.modules()[0].initial_processes() {
        for stmt in process.body() {
            stmt.walk(&mut |stmt| {
                if let ir::Stmt::WaitEvent(items) = stmt {
                    waits.push(
                        items
                            .iter()
                            .map(|item| match &item.expr {
                                ir::Expr::Ident(name) => name.clone(),
                                expr => panic!("unexpected event expression {expr:?}"),
                            })
                            .collect::<Vec<_>>(),
                    );
                }
            });
        }
    }
    assert_eq!(waits, [vec!["c", "a", "b", "i"], vec!["d"]]);
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
fn analyzes_package_variables_into_a_state_module() {
    // A package variable is one object: the package's state module holds it,
    // and a module's signal for it names that variable.
    let code = "package p; logic [7:0] shared = 8'd3; const int K = 3; endpackage\n\
                module Top(output logic [7:0] y); import p::*; assign y = shared + p::shared; endmodule";
    let packages = analyze_packages(&[(code, Path::new("state.sv"))]).unwrap();
    let states = packages.state_modules().collect::<Vec<_>>();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].name(), "p");
    assert!(
        states[0]
            .signals()
            .iter()
            .any(|signal| signal.name() == "shared")
    );
    let ir = analyze_source(code, Path::new("state.sv")).unwrap();
    let mut bound = ir.modules()[0]
        .signals()
        .iter()
        .filter_map(|signal| Some((signal.name(), signal.package_variable()?)))
        .collect::<Vec<_>>();
    bound.sort();
    assert_eq!(
        bound,
        [("p::shared", ("p", "shared")), ("shared", ("p", "shared"))]
    );
    // Package nets are not supported yet.
    let error =
        analyze_packages(&[("package p; wire w; endpackage", Path::new("net.sv"))]).unwrap_err();
    assert_eq!(
        error,
        AnalyzerError::Unsupported("package net `p::w`".to_string())
    );
    assert_eq!(error.tracking_issue(), 1146);
}

#[test]
fn package_functions_give_shapes_to_instance_connections() {
    // The untyped assignment pattern takes the type of the package
    // function's formal argument.
    let code = "package p;
                  function automatic int sum(input int v [2]); return v[0] + v[1]; endfunction
                endpackage
                module Child(input int x); endmodule
                module Top; Child c(.x(p::sum('{1, 2}))); endmodule";
    let ir = analyze_source(code, Path::new("shapes.sv")).unwrap();
    let top = ir
        .modules()
        .iter()
        .find(|module| module.name() == "Top")
        .unwrap();
    let connection = &top.instances()[0].port_connections()[0];
    assert!(
        matches!(connection.actual_expr(), Some(ir::Expr::Call { callee: ir::Callee::Subroutine(name), .. }) if name == "p::sum"),
        "{connection:?}"
    );
}

#[test]
fn imports_a_dpi_function_declared_in_a_package() {
    let code = "package p; import \"DPI-C\" function int twice(input int x); endpackage\n\
                module Top(input int a, output int y); import p::*; assign y = twice(a); endmodule\n";
    let ir = analyze_source(code, Path::new("dpi.sv")).unwrap();
    let imports = ir.modules()[0].dpi_imports();
    assert_eq!(imports.len(), 1);
    assert_eq!(
        (imports[0].name(), imports[0].c_name()),
        ("p::twice", "twice")
    );
}

fn native_interface_module(source: &str, name: &str) -> ir::Module {
    let parsed = ParsedSource::parse(source, Path::new("interfaces.sv")).unwrap();
    let packages = ParsedSource::analyze_packages(&[&parsed]).unwrap();
    parsed
        .analyze_module(
            name,
            &HashMap::default(),
            &[],
            &[],
            &ModuleInterfaces::default(),
            &packages,
        )
        .unwrap()
        .modules()[0]
        .clone()
}

fn assert_interface_member(
    plan: &NativeInterfacePlan,
    module: &str,
    port: &str,
    member: &str,
    direction: ir::PortDirection,
) {
    let port = plan.modules[module]
        .ports
        .iter()
        .find(|p| p.name == port)
        .unwrap();
    assert!(
        port.members
            .iter()
            .any(|(name, value)| name == member && ir::PortDirection::from(*value) == direction),
        "{port:?}"
    );
}

fn elaborate(source: &str) -> Result<Option<NativeInterfacePlan>, AnalyzerError> {
    let plan = plan_interfaces(&[(source, Path::new("interfaces.sv"))], &[])?;
    Ok((!plan.modules.is_empty()).then_some(plan))
}

fn native_interface_sources(sources: &[(&str, &Path)]) -> Result<Vec<ir::Module>, AnalyzerError> {
    let parsed = sources
        .iter()
        .map(|(code, path)| ParsedSource::parse(code, path))
        .collect::<Result<Vec<_>, _>>()?;
    let packages = ParsedSource::analyze_packages(&parsed.iter().collect::<Vec<_>>())?;
    let mut modules = Vec::new();
    for source in &parsed {
        for name in source.module_names() {
            let ir = source.analyze_module(
                name,
                &HashMap::default(),
                &[],
                &[],
                &ModuleInterfaces::default(),
                &packages,
            )?;
            modules.extend(ir.modules().iter().cloned());
        }
    }
    Ok(modules)
}

#[test]
fn leaves_sources_without_interfaces_unchanged() {
    let source = "module Top(input logic a, output logic y); assign y = a; endmodule";
    assert_eq!(elaborate(source), Ok(None));
}

#[test]
fn expands_interface_instances_ports_and_generic_ports() {
    let source = r#"
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
        "#;
    let plan = plan_interfaces(&[(source, Path::new("interfaces.sv"))], &[]).unwrap();

    assert_interface_member(&plan, "Writer", "bus", "data", ir::PortDirection::Output);
    assert!(plan.modules.contains_key("Pass$Bus"));
    let top = native_interface_module(source, "Top");
    assert_eq!(
        top.signals()
            .iter()
            .find(|s| s.name() == "b$data")
            .unwrap()
            .r#type()
            .resolved_width(),
        Some(6)
    );
    assert_eq!(top.instances()[0].module_name(), "Pass$Bus");
    assert_eq!(
        top.instances()[0].port_connections()[0].actual_expr(),
        Some(&ir::Expr::Ident("b$data".into()))
    );
}

#[test]
fn expands_interface_parameters_functions_and_grouped_ports() {
    let source = r#"
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
        "#;
    let plan = plan_interfaces(&[(source, Path::new("interfaces.sv"))], &[]).unwrap();

    assert_interface_member(&plan, "Loader", "c", "state", ir::PortDirection::Output);
    assert_interface_member(&plan, "Pair", "a", "state", ir::PortDirection::Input);
    assert_interface_member(&plan, "Pair", "b", "state", ir::PortDirection::Input);
    assert!(plan.modules["Scaler"].ports[0].members.is_empty());
    assert_eq!(plan.modules["Scaler"].ports[0].parameters, ["K"]);
    let top = native_interface_module(source, "Top");
    for (name, width) in [("named$state", 8), ("ordered$state", 6)] {
        assert_eq!(
            top.signals()
                .iter()
                .find(|s| s.name() == name)
                .unwrap()
                .r#type()
                .resolved_width(),
            Some(width)
        );
    }
}

#[test]
fn expands_nested_references_functions_and_written_arguments() {
    let source = r#"
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
        "#;
    let plan = plan_interfaces(&[(source, Path::new("interfaces.sv"))], &[]).unwrap();

    assert_interface_member(&plan, "Driver", "l", "i0", ir::PortDirection::Output);
    assert!(
        plan.modules["Reader"].ports[0]
            .functions
            .contains(&"bits".into())
    );
    let top = native_interface_module(source, "Top");
    assert_eq!(
        top.signals()
            .iter()
            .filter(|s| s.name().ends_with("$i0"))
            .count(),
        3
    );
    assert_eq!(
        top.instances()[0].port_connections()[0].actual_expr(),
        Some(&ir::Expr::Ident("rows[2]$i0".into()))
    );
    assert!(
        top.signals()
            .iter()
            .filter(|s| s.name().ends_with("$i0"))
            .all(|s| s.r#type().resolved_width() == Some(4))
    );
}

#[test]
fn infers_writes_through_child_ports_and_scoped_subroutines() {
    let source = r#"
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
        "#;
    let plan = plan_interfaces(&[(source, Path::new("interfaces.sv"))], &[]).unwrap();

    assert_interface_member(&plan, "M", "p", "x", ir::PortDirection::Output);
    assert_interface_member(&plan, "M", "p", "y", ir::PortDirection::Output);
    assert_interface_member(&plan, "M", "p", "z", ir::PortDirection::Input);
    let top = native_interface_module(source, "Top");
    assert!(top.instances().is_empty());
    assert_eq!(top.comb_processes().len(), 5);
    assert!(matches!(
        plan_interfaces(
            &[(
                "interface I; endinterface interface I; endinterface",
                Path::new("duplicate.sv")
            )],
            &[]
        ),
        Err(AnalyzerError::DuplicateModule { .. })
    ));
}

#[test]
fn interface_functions_write_members_through_subroutine_arguments() {
    let source = r#"
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
        "#;
    let plan = plan_interfaces(&[(source, Path::new("interfaces.sv"))], &[]).unwrap();

    assert_interface_member(&plan, "L", "s", "state", ir::PortDirection::Output);
    assert!(plan.modules["L"].ports[0].functions.contains(&"put".into()));
    assert!(
        plan.modules["L"].ports[0]
            .functions
            .contains(&"load".into())
    );
    let module = native_interface_module(source, "L");
    assert_eq!(module.ports()[1].name(), "s$state");
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
    // Interface typedefs retain their declaration scopes even when their
    // unqualified names collide with another interface or the caller.
    let module = native_interface_module(
        r#"
            package pa; typedef logic [7:0] word_t; endpackage
            package pb; typedef logic [3:0] word_t; endpackage
            interface A; import pa::*; word_t d; modport r(input d); endinterface
            interface B; import pb::*; word_t d; modport r(input d); endinterface
            module M(A.r a, B.r b, output logic [7:0] o, output logic [3:0] q);
                assign o = a.d;
                assign q = b.d;
            endmodule
            "#,
        "M",
    );
    for (name, width) in [("a$d", 8), ("b$d", 4)] {
        assert_eq!(
            module
                .ports()
                .iter()
                .find(|port| port.name() == name)
                .unwrap()
                .r#type()
                .resolved_width(),
            Some(width)
        );
    }
    let module = native_interface_module(
        r#"
            package pa; typedef logic [7:0] word_t; endpackage
            interface A; import pa::*; word_t d; modport r(input d); endinterface
            module M(A.r a, output logic [7:0] o);
                typedef logic [3:0] word_t;
                assign o = a.d;
            endmodule
            "#,
        "M",
    );
    assert_eq!(
        module
            .ports()
            .iter()
            .find(|port| port.name() == "a$d")
            .unwrap()
            .r#type()
            .resolved_width(),
        Some(8)
    );
}

#[test]
fn resolves_interface_handles_by_scope_and_carries_unit_imports() {
    let source = r#"
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
        "#;
    let plan = plan_interfaces(&[(source, Path::new("interfaces.sv"))], &[]).unwrap();

    assert_interface_member(&plan, "M", "p", "y", ir::PortDirection::Input);
    let top = native_interface_module(source, "Top");
    assert_eq!(
        top.signals()
            .iter()
            .find(|s| s.name() == "g1.h$x")
            .unwrap()
            .r#type()
            .resolved_width(),
        Some(8)
    );
    assert!(!top.signals().iter().any(|s| s.name().starts_with("g2.")));
    let sources = native_interface_sources(&[
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
    .unwrap();
    let module = sources.iter().find(|module| module.name() == "M").unwrap();
    assert_eq!(
        module
            .ports()
            .iter()
            .find(|p| p.name() == "p$x")
            .unwrap()
            .r#type()
            .resolved_width(),
        Some(6)
    );
}

#[test]
fn keeps_generate_branches_imports_and_uncalled_writers_in_scope() {
    let source = r#"
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
        "#;
    let plan = plan_interfaces(&[(source, Path::new("interfaces.sv"))], &[]).unwrap();

    assert_interface_member(&plan, "R", "p", "x", ir::PortDirection::Input);
    assert_interface_member(&plan, "S", "p", "x", ir::PortDirection::Input);
    assert!(!plan.modules["R"].ports[0].functions.contains(&"set".into()));
    let top = native_interface_module(source, "Top");
    assert_eq!(
        top.signals()
            .iter()
            .filter(|s| s.name().ends_with("$x"))
            .count(),
        3
    );
    let source = r#"
        package pk; typedef logic [5:0] word_t; endpackage
        module M(I p, output logic [5:0] o); assign o = p.x; endmodule
        import pk::*;
        interface I; word_t x; endinterface
        "#;
    let module = native_interface_module(source, "M");
    assert_eq!(
        module
            .ports()
            .iter()
            .find(|p| p.name() == "p$x")
            .unwrap()
            .r#type()
            .resolved_width(),
        Some(6)
    );
}

#[test]
fn resolves_declaration_dependencies_scopes_and_implicit_connections() {
    let source = r#"
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
        "#;
    let plan = plan_interfaces(&[(source, Path::new("interfaces.sv"))], &[]).unwrap();

    assert_interface_member(&plan, "M", "p", "data", ir::PortDirection::Input);
    let module = native_interface_module(source, "M");
    assert_eq!(
        module
            .ports()
            .iter()
            .find(|p| p.name() == "p$data")
            .unwrap()
            .r#type()
            .resolved_width(),
        Some(8)
    );
    assert!(
        module.instances()[0]
            .port_connections()
            .iter()
            .any(|p| p.formal() == "clk")
    );
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
    let source = r#"
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
        "#;
    let plan = plan_interfaces(&[(source, Path::new("interfaces.sv"))], &[]).unwrap();

    assert_interface_member(&plan, "L", "p", "mem", ir::PortDirection::Output);
    assert_interface_member(&plan, "C", "p", "hi", ir::PortDirection::Output);
    assert_interface_member(&plan, "C", "p", "lo", ir::PortDirection::Output);
    assert_interface_member(&plan, "C", "p", "idx", ir::PortDirection::Input);
    assert_interface_member(&plan, "S", "p", "x", ir::PortDirection::Input);
    let source = r#"
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
        "#;
    let plan = plan_interfaces(&[(source, Path::new("interfaces.sv"))], &[]).unwrap();

    assert_interface_member(&plan, "L", "p", "mem", ir::PortDirection::Output);
    assert_eq!(
        elaborate("interface I; logic x; modport m(input x, output x); endinterface"),
        Err(AnalyzerError::DuplicateModportItem {
            interface: "I".into(),
            modport: "m".into(),
            name: "x".into()
        })
    );
}

#[test]
fn resolves_scoped_handles_imports_and_grouped_instances() {
    let source = r#"
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
        "#;
    let plan = plan_interfaces(&[(source, Path::new("interfaces.sv"))], &[]).unwrap();

    assert_interface_member(&plan, "M", "p", "x", ir::PortDirection::Input);
    // The plan above checks scoped imports; grouped instance lowering is
    // independent of procedural package imports (not supported yet).
    let module = native_interface_module(
        "interface F; logic [7:0] y; endinterface module M; F a(), b(); endmodule",
        "M",
    );
    for name in ["a$y", "b$y"] {
        assert!(module.signals().iter().any(|signal| signal.name() == name));
    }
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
    let source = "module bus(input logic a); endmodule interface I; logic x; endinterface module M(I bus); bus u(.a(bus.x)); endmodule";
    let module = native_interface_module(source, "M");
    assert_eq!(module.instances()[0].module_name(), "bus");
    assert_eq!(
        module.instances()[0].port_connections()[0].actual_expr(),
        Some(&ir::Expr::Ident("bus$x".into()))
    );
    let sources = native_interface_sources(&[
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
    .unwrap();
    let module = sources.iter().find(|module| module.name() == "M").unwrap();
    assert_eq!(module.ports()[0].direction(), ir::PortDirection::Input);
    let module = native_interface_module(
        "`define DRIVE assign x = 1;\ninterface I; logic x; `DRIVE endinterface module M(); I h(); endmodule",
        "M",
    );
    assert_eq!(module.assignments().len(), 1);
    // Interface bodies retain the compilation unit that declared them.
    let interface = "function automatic logic source(); return 1; endfunction interface I; logic x; assign x = source(); endinterface";
    for sources in [
        vec![
            (interface, Path::new("interface.sv")),
            ("module M(); I h(); endmodule", Path::new("module.sv")),
        ],
        vec![
            ("module M(); I h(); endmodule", Path::new("module.sv")),
            (interface, Path::new("interface.sv")),
        ],
    ] {
        let modules = native_interface_sources(&sources).unwrap();
        let module = modules.iter().find(|module| module.name() == "M").unwrap();
        assert!(module.signals().iter().any(|signal| signal.name() == "h$x"));
        assert_eq!(module.assignments().len(), 1);
    }
    let module =
        native_interface_module(&format!("module M(); I h(); endmodule\n{interface}"), "M");
    assert_eq!(module.assignments().len(), 1);
}

#[test]
fn resolves_sibling_generic_ports_and_struct_fields() {
    let source = r#"
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
        "#;
    let plan = plan_interfaces(&[(source, Path::new("interfaces.sv"))], &[]).unwrap();

    let generic = plan
        .modules
        .values()
        .find(|module| module.original == "G")
        .unwrap();
    assert!(
        generic
            .ports
            .iter()
            .find(|port| port.name == "p")
            .unwrap()
            .members
            .contains(&("x".into(), ast::PortDirection::Input))
    );
    let module = native_interface_module(source, "S");
    assert_eq!(
        module
            .ports()
            .iter()
            .find(|p| p.name() == "bus$x")
            .unwrap()
            .r#type()
            .resolved_width(),
        Some(8)
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
            "missing interface actual of `C.q`",
        ),
        (
            "interface I; if (1) begin logic x; end logic y; assign y = genblk1.x; endinterface",
            "reference to the implicit generate block name `genblk1` in interface `I`",
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
    let sources = native_interface_sources(&[
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
    .unwrap();
    let module = sources.iter().find(|module| module.name() == "M").unwrap();
    assert!(module.signals().iter().any(|signal| signal.name() == "a$x"));
    assert!(
        module
            .signals()
            .iter()
            .any(|signal| signal.name() == "g.h$x")
    );
    let module = native_interface_module(
        "package pk; typedef logic [3:0] word_t; endpackage import pk::*; interface I; word_t x; endinterface module M(I p, output logic [3:0] o); assign o = p.x; endmodule",
        "M",
    );
    assert_eq!(
        module
            .ports()
            .iter()
            .find(|p| p.name() == "p$x")
            .unwrap()
            .r#type()
            .resolved_width(),
        Some(4)
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
            "module Top(output logic [7:0] o); Bus b [2] (); assign o = b.get(1); endmodule",
            "call of a function of the unselected interface array `b`",
        ),
        (
            "module M(Bus p, output logic [7:0] o); assign o = p.x; endmodule
             module Top(output logic [7:0] o); Bus b(); M u(.p(b.r), .o(o)); endmodule",
            "modport mismatch at `M.p`",
        ),
        (
            "module C(Bus p, input logic [7:0] q); endmodule
             module Top(); Bus i(); C c(.p(i), .q(i)); endmodule",
            "use of interface `i` other than as a port connection or through a member",
        ),
        (
            "module Top(output logic [7:0] o); Bus h(); logic [7:0] h$x; assign o = h.x; endmodule",
            "interface binding collides with declaration `h$x`",
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
        let actual = if message.starts_with("use of interface")
            || message.starts_with("interface binding collides")
        {
            analyze_source(&source, Path::new("interfaces.sv")).map(|_| None)
        } else {
            elaborate(&source)
        };
        assert_eq!(
            actual,
            Err(AnalyzerError::Unsupported(message.to_string())),
            "{body}"
        );
    }
    for (interface, message) in [
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
fn interface_bindings_preserve_local_shadowing_and_escaped_names() {
    let module = native_interface_module(
        r"interface Bus; logic [7:0] x; endinterface
          module M(Bus p, output logic [7:0] o);
            function automatic logic [7:0] f(input logic [7:0] p); return p; endfunction
            assign o = f(p.x);
          endmodule",
        "M",
    );
    let function = module.subroutines().iter().find(|f| f.name == "f").unwrap();
    assert_eq!(function.params[0].source_name, "p");
    assert!(module.ports().iter().any(|p| p.name() == "p$x"));
    let module = native_interface_module(
        r"interface Bus; logic [7:0] x; endinterface
          module Top(output logic [7:0] o); Bus \b.0 (); assign o = \b.0 .x; endmodule",
        "Top",
    );
    assert_eq!(
        module.assignments()[0].rhs(),
        &ir::Expr::Ident(r"\b.0$x".into())
    );
}

#[test]
fn interface_array_functions_bind_their_own_member_symbols() {
    let module = native_interface_module(
        "interface I; logic x; logic y; function automatic logic f(); return x; endfunction
         assign y = f(); endinterface module Top(); I a [2] (); endmodule",
        "Top",
    );
    assert_eq!(module.assignments().len(), 2);
    for (index, assignment) in module.assignments().iter().enumerate() {
        assert!(
            matches!(assignment.rhs(), ir::Expr::Call { callee: ir::Callee::Subroutine(name), .. }
            if name == &format!("a[{index}]$f"))
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

#[test]
fn isunbounded_elaborates_unbounded_parameters() {
    let ir = analyze_source(
        r#"
        module Top(output logic y);
            parameter int U = $;
            localparam Q = $isunbounded(U);
            if ($isunbounded(U)) begin assign y = Q; end else begin assign y = 0; end
        endmodule
    "#,
        Path::new("isunbounded.sv"),
    )
    .unwrap();
    assert_eq!(ir.modules()[0].parameters()[1].resolved_value(), Some(1));
}

#[test]
fn isunbounded_rejects_non_parameter_operands() {
    for operand in [
        "", "P, P", "1", "$", "P + 1", "P[0]", "v", "E", "missing", "f()",
    ] {
        for body in [
            format!("assign y = $isunbounded({operand});"),
            format!("localparam UNUSED = $isunbounded({operand});"),
        ] {
            let source = format!(
                r#"
                module Top(input logic v, output logic y);
                    localparam int P = 3;
                    typedef enum logic {{E}} t;
                    function automatic int f(); return 1; endfunction
                    {body}
                endmodule
            "#
            );
            let error =
                analyze_source(&source, Path::new("isunbounded_invalid.sv")).expect_err(&source);
            assert!(
                error.to_string().contains("isunbounded"),
                "{source}: {error}"
            );
        }
    }
}

#[test]
fn isunbounded_rejects_variables_shadowing_parameters() {
    for body in [
        "logic P; y = $isunbounded(P);",
        "for (int P=0; P<1; P++) y = $isunbounded(P);",
    ] {
        let source = format!(
            "module Top(output logic y); parameter int P = $; always_comb begin {body} end endmodule"
        );
        let error = analyze_source(&source, Path::new("isunbounded_shadow.sv")).unwrap_err();
        assert!(
            error.to_string().contains("isunbounded")
                || error.to_string().contains("parameter name collides"),
            "{error}"
        );
    }
}

#[test]
fn rejects_numeric_use_of_unbounded_parameters() {
    for initializer in ["$ + 1", "U + 1", "U == 0", "U[0]", "!U"] {
        let source = format!(
            "module Top(); parameter int U = $; localparam int BAD = {initializer}; endmodule"
        );
        assert!(
            analyze_source(&source, Path::new("unbounded_numeric.sv")).is_err(),
            "{source}"
        );
    }
}

#[test]
fn isunbounded_rejects_subroutine_arguments_as_parameters() {
    for declaration in [
        "function automatic bit query(input int P); return $isunbounded(P); endfunction assign y = query(1);",
        "function automatic bit query(); int P; return $isunbounded(P); endfunction assign y = query();",
        "task automatic query(input int P); y = $isunbounded(P); endtask always_comb query(1);",
    ] {
        let source =
            format!("module Top(output logic y); parameter int P = $; {declaration} endmodule");
        assert!(
            analyze_source(&source, Path::new("isunbounded_subroutine.sv")).is_err(),
            "{source}"
        );
    }
}

#[test]
fn isunbounded_observes_top_level_parameter_overrides() {
    let ir = analyze_source_with_module_parameter_overrides(
        "module Top #(parameter int P = $, parameter bit Q = $isunbounded(P)) (output logic y); assign y = Q; endmodule",
        Path::new("isunbounded_override.sv"), "Top", &HashMap::from_iter([("P".to_string(), 7)]),
    ).unwrap();
    assert_eq!(ir.modules()[0].parameters()[1].resolved_value(), Some(0));
}

#[test]
fn rejects_unbounded_values_in_aggregate_parameters() {
    for declaration in [
        "parameter logic [1:0][1:0] P = $;",
        "typedef logic [1:0][1:0] T; parameter T P = $;",
        "typedef struct packed {logic [7:0] a;} T; parameter T P = $;",
        "parameter int U = $; typedef struct packed {logic [7:0] a;} T; localparam T P = U;",
    ] {
        let source = format!("module Top(); {declaration} endmodule");
        assert!(
            analyze_source(&source, Path::new("unbounded_type.sv")).is_err(),
            "{source}"
        );
    }
}

#[test]
fn isunbounded_and_dimensions_preserve_parameter_metadata() {
    let ir = analyze_source(
        r#"
        package P;
            parameter logic [0:0] U = $;
            localparam logic [0:0] BOUNDED = 0;
            localparam int RANK = $dimensions(BOUNDED);
        endpackage
        module Top(output logic y);
            import P::*;
            localparam logic [0:0] ALIAS = U;
            localparam int RANK = $dimensions(P::BOUNDED);
            localparam Q = $isunbounded(ALIAS);
            localparam int QRANK = $dimensions(Q);
            localparam int IMPORTED_RANK = P::RANK;
            assign y = Q;
        endmodule
        "#,
        Path::new("isunbounded_dimensions.sv"),
    )
    .unwrap();
    let alias = ir.modules()[0]
        .parameters()
        .iter()
        .find(|p| p.name() == "ALIAS")
        .unwrap();
    assert_eq!(alias.resolved_width(), Some(1));
    for name in ["RANK", "Q", "QRANK", "IMPORTED_RANK"] {
        let parameter = ir.modules()[0]
            .parameters()
            .iter()
            .find(|p| p.name() == name)
            .unwrap();
        assert_eq!(parameter.resolved_value(), Some(1), "{name}");
    }
}

#[test]
fn isunbounded_generate_shadow_does_not_supply_numeric_bounds() {
    let source = "module Top(); parameter int P = 7; if (1) begin : g localparam int P = $; logic [P:0] s; end endmodule";
    assert!(analyze_source(source, Path::new("unbounded_generate_shadow.sv")).is_err());
}

#[test]
fn rejects_unbounded_parameter_operands_of_other_data_queries() {
    for name in [
        "$bits",
        "$size",
        "$dimensions",
        "$unpacked_dimensions",
        "$left",
        "$right",
        "$low",
        "$high",
        "$increment",
    ] {
        for body in [
            format!("localparam BAD = {name}(P);"),
            format!("assign y = {name}(P);"),
            format!("always_comb begin localparam int U = $; y = {name}(U); end"),
        ] {
            let source =
                format!("module Top(output logic y); parameter int P = $; {body} endmodule");
            assert!(
                analyze_source(&source, Path::new("unbounded_query.sv")).is_err(),
                "{source}"
            );
        }
    }
}

#[test]
fn unbounded_data_query_checks_compound_arguments_but_allows_isunbounded() {
    for operand in ["P + 1", "{P, 1'b0}", "P[0]", "$unsigned(P)", "$"] {
        let source = format!(
            "module Top(); parameter int P = $; localparam BAD = $bits({operand}); endmodule"
        );
        assert!(
            analyze_source(&source, Path::new("unbounded_compound_query.sv")).is_err(),
            "{source}"
        );
    }
    let ir = analyze_source(
        "module Top(); parameter int P = $; localparam W = $bits($isunbounded(P)); endmodule",
        Path::new("nested_unbounded_query.sv"),
    )
    .unwrap();
    assert_eq!(ir.modules()[0].parameters()[1].resolved_value(), Some(1));
}

#[test]
fn isunbounded_argument_validation_is_not_skipped_by_bits() {
    for body in [
        "assign y = $bits($isunbounded(v));",
        "always_comb y = $bits($isunbounded(v));",
        "localparam BAD = $bits($isunbounded(v));",
    ] {
        let source = format!("module Top(input int v, output int y); {body} endmodule");
        assert!(
            analyze_source(&source, Path::new("isunbounded_unevaluated.sv")).is_err(),
            "{source}"
        );
    }
}

#[test]
fn constant_branch_pruning_retains_runtime_subroutine_conditions() {
    let ir = analyze_source(
        r#"
            module Top(output logic y);
                function automatic int choose(input int v);
                    $display("choose %0d", v);
                    return 1;
                endfunction
                always_comb if (choose(1)) y = 1; else y = 0;
            endmodule
        "#,
        Path::new("conditional_call.sv"),
    )
    .unwrap();
    assert!(matches!(
        ir.modules()[0].comb_processes()[0].body()[0],
        crate::ir::Stmt::If { .. }
    ));
}

#[test]
fn named_constant_calls_retain_parameter_dependencies() {
    let source = "module Top #(parameter W=8, parameter N=f(.value(W)))(); function automatic integer f(input integer value); return value+1; endfunction endmodule";
    let syntax =
        crate::source_tree::parse_source(source, std::path::Path::new("named_dependency.sv"))
            .unwrap();
    let source = ast::Source::from_syntax(&syntax).unwrap();
    let module = &source.modules()[0];
    let state = module.constant_function_state();
    let expression: ir::ConstExpr = module
        .parameters()
        .iter()
        .find(|param| param.name() == "N")
        .unwrap()
        .value()
        .unwrap()
        .clone()
        .into();
    let values = fxhash::FxHashMap::from_iter([("W".to_string(), 16i128)]);
    assert_eq!(
        typecheck::eval_const_expr_in_env(
            &expression,
            &ast::const_functions::Environment {
                constants: &values,
                functions: &state.const_functions
            }
        ),
        Some(17)
    );
}

#[test]
fn interface_signatures_preserve_declaration_dependencies_without_lowering_processes() {
    let source = r#"
        interface Bus #(parameter W = 8);
            localparam N = W * 2;
            logic [N-1:0] data;
            initial forever #1 data = ~data;
        endinterface
    "#;
    let signatures = crate::interface_prototypes(&[(source, Path::new("bus.sv"))]).unwrap();
    let module = &signatures[0].module;
    let port = &module.ports()[0];
    assert_eq!(port.name(), "h$data");
    let values = fxhash::FxHashMap::from_iter([("h$N".to_string(), 64)]);
    assert_eq!(
        typecheck::eval_const_expr(port.r#type().packed_ranges()[0].left(), &values),
        Some(63),
    );
}

#[test]
fn interface_signatures_only_analyze_requested_declarations() {
    let source = r#"
        interface Selected; logic value; endinterface
        interface Unselected; real value; endinterface
    "#;
    let selected = fxhash::FxHashSet::from_iter(["Selected".to_string()]);
    let signatures =
        crate::interface_prototypes_for(&[(source, Path::new("interfaces.sv"))], Some(&selected))
            .unwrap();
    assert_eq!(signatures.len(), 1);
    assert_eq!(signatures[0].name, "Selected");
}

#[test]
fn native_interface_header_inspection_keeps_driven_nets() {
    let module = native_interface_module(
        "interface I; logic x; endinterface module Top(input logic a, output logic y); I i(); wire w; assign w = a; assign i.x = w; assign y = i.x; endmodule",
        "Top",
    );
    assert_eq!(module.assignments().len(), 3);
}

#[test]
fn native_interface_array_ports_keep_their_bounds() {
    let module = native_interface_module(
        "interface I; logic [7:0] x; modport r(input x); endinterface module M #(parameter N=2)(I.r p[N], output logic [15:0] y); assign y = {p[0].x, p[1].x}; endmodule",
        "M",
    );
    assert!(module.ports().iter().any(|port| port.name() == "p$0$x"));
    assert!(module.ports().iter().any(|port| port.name() == "p$1$x"));
}

#[test]
fn native_interface_array_ports_reject_incompatible_shapes() {
    let source = "interface I; logic x; endinterface module M(I p[2]); endmodule";
    let parsed = ParsedSource::parse(source, Path::new("array-port.sv")).unwrap();
    let packages = ParsedSource::analyze_packages(&[&parsed]).unwrap();
    let error = parsed
        .analyze_module(
            "M",
            &HashMap::from_iter([("p$$dimension0".into(), ir::ConstExpr::Literal("3".into()))]),
            &[],
            &[],
            &ModuleInterfaces::default(),
            &packages,
        )
        .unwrap_err();
    assert_eq!(
        error,
        AnalyzerError::Unsupported("interface array dimension mismatch at `M.p`".into())
    );
    let error = analyze_source(
        "interface I; logic x; endinterface module C(I p[4]); endmodule module Top; I a[2][2](); C c(.p(a)); endmodule", Path::new("array-rank.sv"),
    ).unwrap_err();
    assert_eq!(
        error,
        AnalyzerError::Unsupported("interface array rank mismatch at `C.p`".into())
    );
}

#[test]
fn rejects_mismatched_interface_array_connection_widths_and_shapes() {
    for declaration in [
        "logic [2:0] value; I lanes[1:0](.value(value));",
        "logic [0:0] value [3]; I lanes[1:0](.value(value));",
        "logic value [1][2]; I lanes[1:0](.value(value));",
    ] {
        let text = format!(
            "interface I(input logic value); endinterface module Top; {declaration} endmodule"
        );
        let parsed =
            ParsedSource::parse(&text, Path::new("invalid-interface-array-connection.sv")).unwrap();
        let packages = ParsedSource::analyze_packages(&[&parsed]).unwrap();
        let error = parsed
            .analyze_module(
                "Top",
                &HashMap::default(),
                &[],
                &[],
                &ModuleInterfaces::default(),
                &packages,
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("interface array connection shape/width"),
            "{declaration}: {error}"
        );
    }
}

#[test]
fn native_interface_port_views_do_not_repeat_instance_initializers() {
    let module = native_interface_module(
        "interface I; logic [7:0] x = 8'd42; endinterface module M(I p, output logic [7:0] y); assign y = p.x; endmodule",
        "M",
    );
    assert!(module.initial_processes().is_empty());
}

#[test]
fn native_interface_bindings_reject_authored_subroutine_collisions() {
    for declaration in [
        "function automatic int h$f(); return 99; endfunction",
        "localparam h$f = 99;",
        "typedef logic h$f;",
    ] {
        let source = format!(
            "interface I; function automatic int f(); return 7; endfunction endinterface
             module Top(output int y); I h(); {declaration} assign y = h.f(); endmodule"
        );
        let error = analyze_source(&source, Path::new("collision.sv")).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("interface binding collides with declaration `h$f`"),
            "{error}"
        );
    }
}

#[test]
fn interface_signatures_skip_unrequested_unsupported_syntax() {
    for unselected in [
        "interface Unselected #(parameter type T = logic); T value; endinterface",
        "interface Unselected(input logic clk); logic value; endinterface",
        "interface Unselected; enum { A, B } value; endinterface",
        "interface Unselected; interface Nested; logic value; endinterface endinterface",
    ] {
        let source = format!("interface Selected; logic value; endinterface {unselected}");
        let files = [(source.as_str(), Path::new("selected.sv"))];
        let selected = fxhash::FxHashSet::from_iter(["Selected".to_string()]);
        let signatures = crate::interface_prototypes_for(&files, Some(&selected)).unwrap();
        assert_eq!(signatures.len(), 1);
        assert_eq!(signatures[0].name, "Selected");
        assert!(
            crate::interface_prototypes_for(&files, Some(&Default::default()))
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn native_interface_bindings_reject_enum_constant_collisions() {
    let source = "interface I; parameter f = 7; endinterface
        module Top(output int y); I h(); typedef enum int {h$f = 3} E; assign y = h$f; endmodule";
    let error = analyze_source(source, Path::new("enum_collision.sv")).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("interface binding collides with declaration `h$f`"),
        "{error}"
    );
}

#[test]
fn native_package_bindings_feed_reexports_header_imports_and_unit_visibility() {
    for declaration in ["import p::*;", "import \"DPI-C\" function int f();"] {
        let code = format!(
            "package p; endpackage module M; if(1) {declaration} parameter int N=1; \
             function int local_function(); localparam HIDDEN=2; endfunction endmodule"
        );
        let source = ParsedSource::parse(&code, Path::new("atomic-branch.sv")).unwrap();
        assert_eq!(source.module_interfaces()["M"].parameters, ["N"]);
    }

    let code = r#"
        `define USE(package_name) import package_name::*;
        package p;
            parameter int VALUE = 7;
            typedef logic [VALUE:0] word;
        endpackage
        package q;
            import p::*;
            export p::VALUE, p::word;
        endpackage
        `USE(q)
        module Top import q::VALUE; #(parameter N = VALUE)(output p::word y);
            assign y = N;
            if (1) begin : g
                import q::*;
                word z;
                assign z = VALUE;
            end
        endmodule
    "#;
    let ir = analyze_source(code, Path::new("native-package-bindings.sv")).unwrap();
    let module = &ir.modules()[0];
    assert_eq!(module.parameters()[0].resolved_value(), Some(7));
    assert_eq!(module.ports()[0].r#type().resolved_width(), Some(8));
    assert_eq!(module.signals()[0].r#type().resolved_width(), Some(8));

    let code = r"package \p.q ; parameter int \v.w = 9; endpackage
                 import \p.q ::\v.w ;
                 module Top #(parameter N=\v.w )(); endmodule";
    let ir = analyze_source(code, Path::new("escaped-package-bindings.sv")).unwrap();
    assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(9));
}

#[test]
fn native_package_wildcard_and_all_exports_preserve_transitive_bindings() {
    for export in ["export p::*;", "export *::*;"] {
        let code = format!(
            "package p; parameter int V=11; endpackage \
             package q; import p::*; localparam int used=V; {export} endpackage \
             module Top import q::*; #(parameter N=V)(); endmodule"
        );
        let ir = analyze_source(&code, Path::new("native-reexport.sv")).unwrap();
        assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(11));
    }
}

#[test]
fn native_typedef_names_and_base_types_bind_into_existing_layouts() {
    let declarations = ParsedSource::parse(
        "typedef class C; typedef interface class I; typedef enum E; typedef F; \
         interface Bus; typedef logic[7:0] word; endinterface \
         module M; Bus bus(); typedef bus.word View; endmodule",
        Path::new("forward-and-interface-typedefs.sv"),
    )
    .unwrap();
    assert_eq!(declarations.declarations().type_aliases.len(), 6);

    let code = r#"
        `define WORD(name) typedef logic[7:0] name;
        package p; typedef shortint unsigned half; endpackage
        module Top;
            `WORD(Word)
            typedef p::half Half;
            typedef Word Pair[2];
            typedef enum logic[1:0] { A, B } Choice;
            typedef struct packed { Word high; Half low; } Record;
            Half h; Word w; Pair array; Choice choice; Record record;
        endmodule
    "#;
    let ir = analyze_source(code, Path::new("native-typedefs.sv")).unwrap();
    let signals = ir.modules()[0].signals();
    for (name, width) in [
        ("h", 16),
        ("w", 8),
        ("array", 16),
        ("choice", 2),
        ("record", 24),
    ] {
        let signal = signals.iter().find(|signal| signal.name() == name).unwrap();
        assert_eq!(signal.r#type().resolved_width(), Some(width), "{name}");
    }
    assert!(
        !signals
            .iter()
            .find(|signal| signal.name() == "h")
            .unwrap()
            .r#type()
            .is_signed()
    );
}

#[test]
fn native_time_values_bind_all_statement_groupings_and_macro_literals() {
    for declaration in [
        "timeunit `VALUE; timeprecision 1ps;",
        "timeprecision 1ps; timeunit `VALUE;",
        "timeunit `VALUE/1ps;",
        "timeunit `VALUE; timeprecision 1ps; timeunit `VALUE;",
    ] {
        let code = format!(
            "`define VALUE 10.00ns\n`timescale 100ns/1ns\nmodule Top; {declaration} localparam U=$timeunit(), P=$timeprecision(); endmodule\n`resetall\n"
        );
        let ir = analyze_source(&code, Path::new("native-time-values.sv")).unwrap();
        assert_eq!(
            ir.modules()[0].parameters()[0].resolved_value(),
            Some(-8),
            "{declaration}"
        );
        assert_eq!(
            ir.modules()[0].parameters()[1].resolved_value(),
            Some(-12),
            "{declaration}"
        );
    }
}

#[test]
fn native_typedef_bound_operators_preserve_precedence_and_typed_primaries() {
    for (declaration, width) in [
        ("typedef logic [20-3*4-1:0] T;", 8),
        ("typedef logic [Sizes::N-1:0] T;", 3),
        ("typedef logic [grow(3)-1:0] T;", 5),
        ("typedef logic [int'(7):0] T;", 8),
        ("typedef logic [MASK[0]:0] T;", 2),
        ("typedef logic [$bits(Eight)-1:0] T;", 8),
        (
            "typedef Eight [(Sizes::N==3 ? grow(Sizes::N)+7 : 0)-1:0] T;",
            96,
        ),
        ("typedef logic [2**3-1:0] T;", 8),
        ("typedef logic [(20-3)-4-1:0] T;", 13),
        ("typedef logic [20-(3-4)-1:0] T;", 21),
        ("typedef logic [((1 ? 0 : 1) ? 9 : 4)-1:0] T;", 4),
        ("typedef Eight T[grow(1)+1];", 32),
        ("typedef Eight T[Sizes::N-1:0][1+1];", 48),
    ] {
        let code = format!(
            r#"
            package Sizes; parameter N=3; endpackage
            module Top;
              function automatic int grow(input int n); return n+2; endfunction
              localparam logic [7:0] MASK=8'haf;
              typedef logic [7:0] Eight;
              {declaration}
              T value;
            endmodule
        "#
        );
        let ir = analyze_source(&code, Path::new("native-bound-operators.sv"))
            .unwrap_or_else(|error| panic!("{declaration}: {error}"));
        let signal = ir.modules()[0]
            .signals()
            .iter()
            .find(|signal| signal.name() == "value")
            .unwrap();
        assert_eq!(
            signal.r#type().resolved_width(),
            Some(width),
            "{declaration}"
        );
    }
}

#[test]
fn native_parameter_statements_bind_header_body_generate_and_subroutine_values() {
    let source = r#"
        `define CALC(x) ((x)*3+1)
        localparam ROOT=2;
        module Top #(
          parameter N=`CALC(ROOT),
          parameter bit [3:0] FILL='0
        )(output logic [31:0] value);
          localparam BASE=N+2;
          function automatic int f(input int x);
            localparam K=2*3;
            return x+K;
          endfunction
          if(BASE>0) begin:g
            localparam G=(BASE ? BASE : 0)+1;
            assign value=f(G);
          end
        endmodule
    "#;
    let parsed = ParsedSource::parse(source, Path::new("native-parameter-values.sv")).unwrap();
    let declarations = parsed.declarations();
    assert_eq!(declarations.parameter_declarations.len(), 4);
    let ir = analyze_source(source, Path::new("native-parameter-values.sv")).unwrap();
    for (name, value) in [("N", 7), ("FILL", 0), ("BASE", 9)] {
        let parameter = ir.modules()[0]
            .parameters()
            .iter()
            .find(|parameter| parameter.name() == name)
            .unwrap();
        assert_eq!(parameter.resolved_value(), Some(value), "{name}");
    }
    assert_eq!(
        ir.modules()[0].ports()[0].r#type().resolved_width(),
        Some(32)
    );

    // Class headers remain explicitly outside the parameter-default bridge.
    // Body statements still have native syntax and must not corrupt the next
    // unit's header identity or positional metadata.
    let source = "class Unused #(parameter N=3, type T=logic[2:0]); localparam K=N+1; endclass module M #(parameter P=7); endmodule";
    let parsed = ParsedSource::parse(source, Path::new("unused-class-header.sv")).unwrap();
    assert_eq!(parsed.declarations().parameter_declarations.len(), 2);
    assert_eq!(parsed.module_interfaces()["M"].parameters, ["P"]);
}

#[test]
fn native_continuous_operators_preserve_runtime_groups_concat_and_typed_leaves() {
    let source = r#"
        `define PAIR(x,y) {x,y}
        module Top(input logic [7:0] a,b,c,input logic choose,output logic[7:0] y,z,w);
          assign y=choose ? a-(b-c) : a-b-c,
                 z=(a ~^ b) ^ (~&c),
                 w=`PAIR(a[3:0],4'(b+1));
        endmodule
    "#;
    let parsed = ParsedSource::parse(source, Path::new("native-runtime-operators.sv")).unwrap();
    let statements = &parsed.declarations().assign_statements;
    assert_eq!(statements.len(), 1);
    assert_eq!(statements[0].assignments.len(), 3);
    let ir = analyze_source(source, Path::new("native-runtime-operators.sv")).unwrap();
    let assignments = ir.modules()[0].assignments();
    let ir::Expr::Mux {
        then_expr,
        else_expr,
        ..
    } = assignments[0].rhs()
    else {
        panic!("native conditional")
    };
    let ir::Expr::Binary {
        op: ir::BinaryOp::Sub,
        right,
        ..
    } = &**then_expr
    else {
        panic!("grouped subtraction")
    };
    assert!(matches!(
        &**right,
        ir::Expr::Binary {
            op: ir::BinaryOp::Sub,
            ..
        }
    ));
    let ir::Expr::Binary {
        op: ir::BinaryOp::Sub,
        left,
        ..
    } = &**else_expr
    else {
        panic!("left-associated subtraction")
    };
    assert!(matches!(
        &**left,
        ir::Expr::Binary {
            op: ir::BinaryOp::Sub,
            ..
        }
    ));
    assert!(matches!(
        assignments[1].rhs(),
        ir::Expr::Binary {
            op: ir::BinaryOp::BitXor,
            ..
        }
    ));
    let ir::Expr::Concat(parts) = assignments[2].rhs() else {
        panic!("native concat")
    };
    assert_eq!(parts.len(), 2);
    assert!(matches!(&parts[0], ir::Expr::Select { .. }));
    assert!(matches!(&parts[1], ir::Expr::Resize { width: 4, .. }));

    let syntax=ParsedSource::parse("module M; wire a,b; assign (strong1,pull0) #(1:2:3,4,5) a=b; initial begin assign b=a; end endmodule",Path::new("native-assign-syntax.sv")).unwrap();
    assert_eq!(syntax.declarations().assign_statements.len(), 2);
}

#[test]
fn native_assign_grammar_retains_existing_unsupported_runtime_diagnostics() {
    for (expression, diagnostic) in [
        ("(x+=1)", "assignment used as an expression"),
        ("tagged Value x", "tagged union expression"),
        ("++x", "increment or decrement expression"),
        ("x++", "increment or decrement expression"),
    ] {
        let source =
            format!("module M(input int x,output int y); assign y={expression}; endmodule");
        ParsedSource::parse(&source, Path::new("unsupported-runtime-expression.sv")).unwrap();
        let error =
            analyze_source(&source, Path::new("unsupported-runtime-expression.sv")).unwrap_err();
        assert!(
            matches!(error, AnalyzerError::Unsupported(_)),
            "{expression}: {error}"
        );
        assert!(
            error.to_string().contains(diagnostic),
            "{expression}: {error}"
        );
    }
}

#[test]
fn native_control_values_preserve_case_ranges_loops_waits_and_packed_alias_types() {
    let source = r#"
      module M(input logic[7:0] a,b,c,output logic[7:0] y);
        typedef bit signed[3:0] T;
        initial begin
          case(a+b) b+c:y=a; default:y=b; endcase
          case(a+b) inside [b:c+1]:y=a; default:y=b; endcase
          while(a+b) y=a;
          repeat(a+b) y=b;
          do y=c; while(a+b);
          for(T[1:0] i=0;i<a;i+=1) y=a;
          for(var int j=0,k=1,logic[3:0] p=0;j<a;j+=2,k--,++p) y=b;
          for(y=0;y<a;y+=2) y=b;
          wait(a+b);
        end
      endmodule
    "#;
    let ir = analyze_source(source, Path::new("native-control-values.sv")).unwrap();
    let module = &ir.modules()[0];
    let local = module
        .locals()
        .iter()
        .find(|local| local.source_name == "i")
        .unwrap();
    assert_eq!(local.r#type.resolved_width(), Some(8));
    assert!(!local.r#type.is_signed());
    let mut cases = 0;
    let mut loops = 0;
    let mut waits = 0;
    for process in module.initial_processes() {
        for statement in process.body() {
            statement.walk(&mut |statement| match statement {
                ir::Stmt::Case {
                    selector, items, ..
                } => {
                    assert!(matches!(
                        selector,
                        ir::Expr::Binary {
                            op: ir::BinaryOp::Add,
                            ..
                        }
                    ));
                    match &items[0].labels[0] {
                        ir::CaseLabel::Value(value) => assert!(matches!(
                            value,
                            ir::Expr::Binary {
                                op: ir::BinaryOp::Add,
                                ..
                            }
                        )),
                        ir::CaseLabel::Range { high, .. } => assert!(matches!(
                            high,
                            ir::Expr::Binary {
                                op: ir::BinaryOp::Add,
                                ..
                            }
                        )),
                    }
                    cases += 1;
                }
                ir::Stmt::Loop { .. } => loops += 1,
                ir::Stmt::Wait(condition) => {
                    assert!(matches!(
                        condition,
                        ir::Expr::Binary {
                            op: ir::BinaryOp::Add,
                            ..
                        }
                    ));
                    waits += 1;
                }
                _ => {}
            });
        }
    }
    assert_eq!((cases, loops, waits), (2, 6, 1));
}

#[test]
fn native_local_types_dimensions_and_initializers_reach_procedural_ir() {
    let source = r#"
      module M(input logic[3:0] a,b,output logic[7:0] y);
        typedef bit signed[3:0] T;
        always_comb begin
          automatic T[1:0] pair={a,b};
          automatic logic[3:0] words[2]='{a,b};
          var automatic signed[3:0] value=a-b;
          y=pair[0]+words[1]+value;
        end
      endmodule
    "#;
    let parsed = ParsedSource::parse(source, Path::new("native-local-values.sv")).unwrap();
    assert_eq!(
        parsed
            .declarations()
            .statements
            .iter()
            .filter(|statement| matches!(
                statement.kind,
                celox_sv_syntax::StatementKind::Declaration(
                    celox_sv_syntax::LocalDeclaration::Variables(_)
                )
            ))
            .count(),
        3
    );
    let ir = analyze_source(source, Path::new("native-local-values.sv")).unwrap();
    let locals = ir.modules()[0].locals();
    let pair = locals
        .iter()
        .find(|local| local.source_name == "pair")
        .unwrap();
    assert_eq!(pair.r#type.resolved_width(), Some(8));
    assert!(!pair.r#type.is_signed());
    let words = locals
        .iter()
        .find(|local| local.source_name == "words")
        .unwrap();
    assert_eq!(words.r#type.unpacked_ranges().len(), 1);
    let value = locals
        .iter()
        .find(|local| local.source_name == "value")
        .unwrap();
    assert!(value.r#type.is_signed());
    assert_eq!(value.r#type.resolved_width(), Some(4));
    let mut initialized = 0;
    for statement in ir.modules()[0].comb_processes()[0].body() {
        statement.walk(&mut |statement| {
            if matches!(statement, ir::Stmt::Local { init: Some(_), .. }) {
                initialized += 1;
            }
        });
    }
    assert_eq!(initialized, 3);
}

#[test]
fn native_subroutine_headers_formals_defaults_and_bodies_reach_ir() {
    let source = r#"
      module M(input logic[3:0] a,output logic[7:0] y);
        typedef bit signed[3:0] T;
        function automatic T[1:0] sum(input T[1:0] x,input int bias=1+2);
          automatic T[1:0] value=x+bias;
          if(x>0) return value+1; else return value-1;
        endfunction
        task drive;input logic[3:0] x;output logic[7:0] result;result=sum(x);endtask
        function automatic int count(input int n);
          automatic int total=0;
          for(int i=0;i<n;i++) total+=i;
          return total;
        endfunction
        localparam int N=count(4);
        logic[N-1:0] width_probe;
        always_comb begin drive(a,y);width_probe='0;end
      endmodule
    "#;
    let parsed = ParsedSource::parse(source, Path::new("native-subroutines.sv")).unwrap();
    assert_eq!(parsed.declarations().subroutines.len(), 3);
    let ir = analyze_source(source, Path::new("native-subroutines.sv")).unwrap();
    let module = &ir.modules()[0];
    let sum = module
        .subroutines()
        .iter()
        .find(|routine| routine.name == "sum")
        .unwrap();
    let ty = sum.return_type.as_ref().unwrap();
    assert_eq!(ty.resolved_width(), Some(8));
    assert!(!ty.is_signed());
    assert_eq!(sum.params[0].r#type.resolved_width(), Some(8));
    assert_eq!(sum.params[1].source_name, "bias");
    assert!(sum.params[1].default.is_some());
    let mut locals = 0;
    let mut returns = 0;
    for statement in &sum.body {
        statement.walk(&mut |statement| match statement {
            ir::Stmt::Local { init: Some(_), .. } => locals += 1,
            ir::Stmt::Return(Some(_)) => returns += 1,
            _ => {}
        });
    }
    assert_eq!((locals, returns), (1, 2));
    let drive = module
        .subroutines()
        .iter()
        .find(|routine| routine.name == "drive")
        .unwrap();
    assert!(drive.is_task);
    assert_eq!(drive.params[1].direction, ir::ParamDirection::Output);
    assert_eq!(
        module
            .signals()
            .iter()
            .find(|signal| signal.name() == "width_probe")
            .unwrap()
            .r#type()
            .resolved_width(),
        Some(6)
    );
}

#[test]
fn native_named_call_arguments_reach_production_ir_with_source_order() {
    let source = r#"
      module M(input int a,output int y);
        function automatic int g(input int x);return x;endfunction
        function automatic int f(input int left=1,right=2);return left+right;endfunction
        assign y=f(.right(a+1),.left(g(a*2)));
      endmodule
    "#;
    let ir = analyze_source(source, Path::new("native-named-call-ir.sv")).unwrap();
    let assignment = &ir.modules()[0].assignments()[0];
    let ir::Expr::Call {
        callee,
        args,
        arg_order,
    } = assignment.rhs()
    else {
        panic!("{assignment:?}")
    };
    assert_eq!(callee, &ir::Callee::Subroutine("f".into()));
    assert_eq!(arg_order, &[1, 0]);
    let Some(ir::Expr::Call { callee, args, .. }) = &args[0] else {
        panic!()
    };
    assert_eq!(callee, &ir::Callee::Subroutine("g".into()));
    assert!(matches!(
        &args[0],
        Some(ir::Expr::Binary {
            op: ir::BinaryOp::Mul,
            ..
        })
    ));
}

#[test]
fn native_index_selection_preserves_multidimensional_offsets_and_signed_elements() {
    let source = r#"
      module M(output logic[3:0] y,z);
        typedef bit signed[3:0] T;
        logic [1:0][3:0] x[2];
        T[1:0] a;
        assign y=x[1][0], z=a[1];
      endmodule
    "#;
    let ir = analyze_source(source, Path::new("native-selected-elements.sv")).unwrap();
    for (assignment, high, low, expected_signed) in [
        (&ir.modules()[0].assignments()[0], 11, 8, false),
        (&ir.modules()[0].assignments()[1], 7, 4, true),
    ] {
        let ir::Expr::Select {
            msb, lsb, signed, ..
        } = assignment.rhs()
        else {
            panic!("{assignment:?}")
        };
        assert_eq!(eval_test_const_expr(msb), Some(high));
        assert_eq!(eval_test_const_expr(lsb), Some(low));
        assert_eq!(*signed, expected_signed);
    }
}

#[test]
fn native_cast_targets_preserve_numeric_signing_alias_and_zero_contracts() {
    let source = r#"
      module M(input logic signed[7:0] a,input logic[7:0] b,
        output logic[31:0] x0,x1,x2,x3,x4,x5,x6);
        typedef bit signed[3:0] T;
        localparam WIDTH=5;
        assign x0=4'(a),x1=unsigned'(a),x2=signed'(b),
          x3=T'(b),x4=WIDTH'(b),x5=8'('0),x6=T'(0);
      endmodule
    "#;
    let ir = analyze_source(source, Path::new("native-cast-contracts.sv")).unwrap();
    let assignments = ir.modules()[0].assignments();
    for (index, expected_width, expected_signed) in [
        (0, 4, true),
        (1, 8, false),
        (2, 8, true),
        (3, 4, true),
        (4, 5, false),
        (5, 8, false),
        (6, 4, true),
    ] {
        let ir::Expr::Resize { width, signed, .. } = assignments[index].rhs() else {
            panic!("{:?}", assignments[index].rhs())
        };
        assert_eq!(*width, expected_width, "x{index}");
        assert_eq!(*signed, expected_signed, "x{index}");
    }
    let ir::Expr::Resize { expr, .. } = assignments[3].rhs() else {
        panic!()
    };
    let ir::Expr::Resize { expr, signed, .. } = &**expr else {
        panic!("{expr:?}")
    };
    assert!(!signed);
    assert!(matches!(
        &**expr,
        ir::Expr::Unary {
            op: ir::UnaryOp::ToTwoState,
            ..
        }
    ));
    let ir::Expr::Resize { expr, .. } = assignments[6].rhs() else {
        panic!()
    };
    assert!(matches!(&**expr, ir::Expr::Literal(value) if value == "0"));
}

#[test]
fn native_indexed_ranges_preserve_declared_direction_for_dynamic_bases() {
    let source = "module M(input logic[7:0] d,input logic[0:7] u,input logic[2:0] i,output logic[1:0] y0,y1,y2,y3);assign y0=d[i+:2],y1=d[i-:2],y2=u[i+:2],y3=u[i-:2];endmodule";
    let ir = analyze_source(source, Path::new("native-indexed-direction.sv")).unwrap();
    for (assignment, (high, low)) in
        ir.modules()[0]
            .assignments()
            .iter()
            .zip([(4, 3), (3, 2), (3, 4), (2, 3)])
    {
        let ir::Expr::Select {
            msb, lsb, signed, ..
        } = assignment.rhs()
        else {
            panic!()
        };
        let env = HashMap::from_iter([("i".into(), 3)]);
        assert_eq!(typecheck::eval_const_expr(msb, &env), Some(high));
        assert_eq!(typecheck::eval_const_expr(lsb, &env), Some(low));
        assert!(!signed);
    }
}

#[test]
fn native_system_task_arguments_preserve_strings_omissions_and_query_operands() {
    let source = r#"module Top(input int value);
        initial begin
            $display;
            $display();
            $display("value=%0d\n\"quoted\"",, $bits(value) + value);
            $write("tail\\");
        end
    endmodule"#;
    let ir = analyze_source(source, Path::new("native_system_task_arguments.sv")).unwrap();
    let body = ir.modules()[0].initial_processes()[0].body();
    let crate::ir::Stmt::SystemTask { args, .. } = &body[2] else {
        panic!("expected display");
    };
    assert!(
        matches!(&args[0], crate::ir::SystemTaskArg::Str(text) if text == r#"value=%0d\n\"quoted\""#)
    );
    assert!(matches!(args[1], crate::ir::SystemTaskArg::Empty));
    assert!(matches!(args[2], crate::ir::SystemTaskArg::Expr(_)));
    let crate::ir::Stmt::SystemTask { args, .. } = &body[3] else {
        panic!("expected write");
    };
    assert!(matches!(&args[0], crate::ir::SystemTaskArg::Str(text) if text == r#"tail\\"#));
    for call in ["$display(.value(1));", "$display(1,,.value(2));"] {
        let source = format!("module Top; initial begin {call} end endmodule");
        assert!(analyze_source(&source, Path::new("named_system_task.sv")).is_err());
    }
}

#[test]
fn timed_task_calls_require_a_suspendable_context() {
    for caller in [
        "always_comb outer();",
        "always @* outer();",
        "always_ff @(posedge clk) outer();",
        "function void f(); outer(); endfunction initial f();",
    ] {
        let source = format!(
            "module Top(input bit clk); task automatic pause(); #1; endtask task automatic outer(); pause(); endtask {caller} endmodule"
        );
        let error = analyze_source(&source, Path::new("timed_task_context.sv"))
            .expect_err("a timed task cannot suspend this caller");
        assert!(
            error.to_string().contains("timed task call"),
            "{caller}: {error}"
        );
    }
}

#[test]
fn timed_tasks_reject_static_locals_instead_of_reinitializing_them() {
    for declaration in [
        "task t; int saved = 1; #1; saved = 2; endtask",
        "task automatic t; static int saved = 1; #1; saved = 2; endtask",
    ] {
        let source = format!("module Top; {declaration} initial t(); endmodule");
        let error = analyze_source(&source, Path::new("timed_static.sv"))
            .expect_err("static local storage across timed calls is not lowered yet");
        assert!(
            error.to_string().contains("static local in a timed task"),
            "{error}"
        );
    }
}

#[test]
fn functions_still_reject_direct_timing_controls() {
    for statement in ["#1;", "@(posedge clk);", "wait (clk);"] {
        let source = format!(
            "module Top(input bit clk); function void f(); {statement} endfunction initial f(); endmodule"
        );
        let error = analyze_source(&source, Path::new("function_timing.sv"))
            .expect_err("functions cannot suspend");
        // IEEE 1800-2023 13.4 makes it illegal, not unimplemented.
        assert!(
            matches!(
                error,
                AnalyzerError::TimingControlOutsideProcess {
                    context: "a function",
                    ..
                }
            ),
            "{error:?}"
        );
    }
}

#[test]
fn timed_tasks_reject_nonblocking_updates_to_automatic_storage() {
    let source = "module Top; task automatic t(output int result); #1; result <= 1; endtask int q; initial t(q); endmodule";
    let error = analyze_source(source, Path::new("automatic_task_nba.sv"))
        .expect_err("NBA cannot outlive automatic task storage (IEEE 1800-2023 13.3.2)");
    assert!(
        error.to_string().contains("automatic task variable"),
        "{error}"
    );
}

#[test]
fn timed_tasks_reject_automatic_nbas_in_untimed_callees() {
    for assignment in ["x <= 1;", "{x, y} <= 64'd1;", "if (q == 0) x <= 1;"] {
        let source = format!(
            r#"
            module Top;
                int q, r;
                task automatic leaf(output int x, output int y);
                    {assignment}
                    y = 0;
                endtask
                task automatic middle(output int x, output int y);
                    leaf(x, y);
                endtask
                task automatic outer();
                    #1;
                    middle(q, r);
                endtask
                initial outer();
            endmodule
        "#
        );
        let error = analyze_source(&source, Path::new("untimed_callee_nba.sv"))
            .expect_err("untimed callees cannot enqueue updates to automatic storage");
        assert!(
            error.to_string().contains("automatic task variable"),
            "{assignment}: {error}"
        );
    }
}

#[test]
fn untimed_tasks_also_reject_nbas_to_automatic_locals() {
    let source = "module Top; task automatic t(); int saved[2]; saved[0] <= 1; endtask initial t(); endmodule";
    let error = analyze_source(source, Path::new("untimed_automatic_local_nba.sv"))
        .expect_err("the automatic-storage restriction does not depend on suspension");
    assert!(
        error.to_string().contains("automatic task variable"),
        "{error}"
    );
}

#[test]
fn untimed_callees_may_schedule_nbas_to_module_storage() {
    let source = r#"
        module Top;
            int q, r;
            task automatic leaf(output int x);
                q <= 1;
                x = 7;
            endtask
            task automatic outer(); #1; leaf(r); endtask
            initial outer();
        endmodule
    "#;
    analyze_source(source, Path::new("callee_module_nba.sv"))
        .expect("module storage outlives the automatic task activation");
}

#[test]
fn automatic_task_nbas_are_illegal_even_in_folded_branches() {
    for statement in [
        "if (0) x <= 1;",
        "if (1) ; else {x, y} <= 64'd1;",
        "if (0) begin if (1) x <= 1; end",
    ] {
        let source = format!(
            "module Top; int q, r; task automatic t(output int x, output int y); {statement} endtask initial t(q, r); endmodule"
        );
        let error = analyze_source(&source, Path::new("folded_automatic_nba.sv"))
            .expect_err("automatic NBA legality is independent of reachability");
        assert!(
            error.to_string().contains("automatic task variable"),
            "{error}"
        );
    }
    analyze_source(
        "module Top; task automatic t; if (0) #1; endtask always_comb t(); endmodule",
        Path::new("folded_timing.sv"),
    )
    .expect("unreachable timing must not make a combinational task suspend");
}

#[test]
fn scoped_timed_task_static_formals_require_shared_storage() {
    for scope in ["package p;", ""] {
        let end_scope = if scope.is_empty() { "" } else { "endpackage" };
        let call = if scope.is_empty() { "t(q)" } else { "p::t(q)" };
        for lifetime in ["", "automatic"] {
            let source = format!(
                "{scope} task {lifetime} t(input int x); #1; endtask {end_scope} module Top; int q; initial {call}; endmodule"
            );
            let result = analyze_source(&source, Path::new("scoped_timed_task.sv"));
            if lifetime.is_empty() {
                let error = result.expect_err("scoped static formals are not shared yet");
                assert!(
                    error
                        .to_string()
                        .contains("static arguments in a package or compilation-unit timed task"),
                    "{error}"
                );
            } else {
                result.expect("scoped automatic tasks have per-activation storage");
            }
        }
    }
}

#[test]
fn unpacked_constant_function_bounds_survive_analysis_without_losing_the_contract() {
    let ir = analyze_source(
        r#"
        module Top #(parameter int N = 3);
          function automatic int extent(input int n); return n * 2; endfunction
          logic [7:0] memory [extent(N)];
        endmodule
        "#,
        Path::new("unpacked-constant-function-bounds.sv"),
    )
    .unwrap();
    let ty = ir.modules()[0]
        .signals()
        .iter()
        .find(|signal| signal.name() == "memory")
        .unwrap()
        .r#type();
    let range = &ty.unpacked_ranges()[0];
    assert_eq!(range.resolved_bounds(), Some((0, 5)));
    assert_eq!(ty.resolved_width(), Some(48));
    // Call expressions must remain symbolic for interface declaration
    // contracts, even though lowering no longer owns their function bodies.
    assert!(typecheck::eval_const_expr(range.right(), &HashMap::default()).is_none());
}

#[test]
fn scope_randomize_validates_variable_arguments() {
    for call in [
        "randomize(value)",
        "std::randomize(value)",
        "std::randomize()",
    ] {
        let source =
            format!("module Top; logic [2048:0] value; bit ok; initial ok = {call}; endmodule");
        analyze_source(&source, Path::new("randomize.sv")).unwrap();
    }
    for (declaration, call) in [
        ("bit value;", "randomize(1)"),
        ("bit value;", "randomize(value[0])"),
        ("bit value;", "randomize({value})"),
        ("bit value;", "randomize(.value(value))"),
        ("bit value;", "randomize(value,)"),
        ("localparam value = 1;", "randomize(value)"),
        ("bit value[2];", "randomize(value)"),
        ("wire value;", "randomize(value)"),
        ("const bit value = 1;", "randomize(value)"),
        ("enum bit [1:0] {A=0, B=3} value;", "randomize(value)"),
        (
            "typedef enum bit [1:0] {A=0, B=3} E; E value;",
            "std::randomize(value)",
        ),
        ("bit value;", "std::randomize(value) with {value == 1;}"),
    ] {
        let source = format!("module Top; {declaration} bit ok; initial ok = {call}; endmodule");
        assert!(
            analyze_source(&source, Path::new("invalid_randomize.sv")).is_err(),
            "accepted {source}"
        );
    }
}

#[test]
fn time_literal_delays_reject_scheduler_overflow() {
    let error = analyze_source(
        "module Top; timeunit 1s / 1fs; initial #18446744073709551616s; endmodule",
        Path::new("overflow_delay.sv"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("scheduler range"), "{error}");
}

#[test]
fn time_literal_delays_accept_numeric_separators() {
    let source = "module Top; timeunit 1ns / 1ps; initial #1_000ps; endmodule";
    let ir = analyze_source(source, Path::new("separated_time.sv")).unwrap();
    let ir::Stmt::Delay {
        amount,
        ticks_per_unit,
        ..
    } = &ir.modules()[0].initial_processes()[0].body()[0]
    else {
        panic!("delay");
    };
    assert_eq!(*ticks_per_unit, 1);
    assert!(matches!(amount, ir::Expr::Literal(value) if value == "64'd1000"));
}

#[test]
fn scope_randomize_respects_port_variable_kinds() {
    for source in [
        "module Top(output logic [8:0] a, b); initial void'(std::randomize(a,b)); endmodule",
        "module Top(output var bit a); initial void'(randomize(a)); endmodule",
    ] {
        analyze_source(source, Path::new("randomized_output.sv")).unwrap();
    }
    for source in [
        "module Top(input var bit a); initial void'(randomize(a)); endmodule",
        "module Top(output wire logic a); initial void'(randomize(a)); endmodule",
    ] {
        assert!(
            analyze_source(source, Path::new("randomized_net.sv")).is_err(),
            "accepted {source}"
        );
    }
}

#[test]
fn analyzes_real_storage_literals_and_runtime_conversions() {
    let ir = analyze_source(
        r#"module Top(input logic [63:0] bits, output logic [63:0] result);
            real value;
            shortreal narrow_value;
            realtime alias_value;
            always_comb begin
                value = $bitstoreal(bits) + 1.5;
                narrow_value = shortreal'(value);
                alias_value = real'(narrow_value);
                result = $realtobits(alias_value);
            end
        endmodule"#,
        Path::new("real.sv"),
    )
    .expect("real runtime expressions should analyze");
    let module = &ir.modules()[0];
    assert_eq!(module.signals()[0].r#type().kind(), ir::TypeKind::Real);
    assert_eq!(module.signals()[1].r#type().kind(), ir::TypeKind::Shortreal);
    assert_eq!(module.signals()[2].r#type().kind(), ir::TypeKind::Real);
}

#[test]
fn rejects_integral_operators_and_packed_selects_on_real_values() {
    for statement in [
        "assign y = x & 1;",
        "assign y = ~x;",
        "assign y = x[0];",
        "initial x[0] = 1;",
    ] {
        let source = format!("module Top(output logic y); real x; {statement} endmodule");
        assert!(
            analyze_source(&source, Path::new("real_invalid.sv")).is_err(),
            "{statement}"
        );
    }
}

#[test]
fn analyzes_real_parameter_arithmetic() {
    analyze_source(
        r#"module Top(output logic [63:0] bits);
        localparam real A = 1.5;
        localparam real B = A * 2.0;
        assign bits = $realtobits(B);
    endmodule"#,
        Path::new("real_parameter.sv"),
    )
    .expect("real parameter arithmetic");
}

#[test]
fn analyzes_sized_cast_of_negated_real_local() {
    analyze_source(
        r#"module Top(input logic signed [31:0] a, output logic signed [31:0] result);
        always_comb begin
            real value;
            value = a;
            result = $signed(32'(-value));
        end
    endmodule"#,
        Path::new("real_local_cast.sv"),
    )
    .expect("real local signedness remains available in sized casts");
}

#[test]
fn analyzes_real_type_casts_in_separate_always_comb_bodies() {
    analyze_source(
        r#"module Top (
    input  var int   a       ,
    input  var int   b       ,
    output var int   sum     ,
    output var int   diff    ,
    output var int   product ,
    output var int   quotient,
    output var int   negative,
    output var logic lt      ,
    output var logic eq
);
    real x       ;
    real y       ;
    always_comb x        = real'(a);
    always_comb y        = real'(b);
    always_comb sum      = int'((x + y));
    always_comb diff     = int'((x - y));
    always_comb product  = int'((x * y));
    always_comb quotient = int'((x / y));
    always_comb negative = int'((-x));
    always_comb lt       = x < y;
    always_comb eq       = x == y;
endmodule
//# sourceMappingURL=float.sv.map
"#,
        Path::new("real_emitted.sv"),
    )
    .expect("real casts in module variables");
}

#[test]
fn real_storage_widths_do_not_create_packed_dimensions() {
    let ir = analyze_source(
        r#"module Top;
        real value;
        shortreal narrow;
        real array_value [0:2];
        function automatic real real_result(); return 1.0; endfunction
        localparam real REAL_PARAMETER = 1.5;
        localparam DTYPE = $dimensions(real);
        localparam DSHORT = $dimensions(shortreal);
        localparam DVALUE = $dimensions(value);
        localparam DNARROW = $dimensions(narrow);
        localparam DARRAY = $dimensions(array_value);
        localparam DPARAM = $dimensions(REAL_PARAMETER);
        localparam DFUNC = $dimensions(real_result());
        localparam BREAL = $bits(real);
        localparam BSHORT = $bits(shortreal);
    endmodule"#,
        Path::new("real_dimensions.sv"),
    )
    .expect("real query metadata");
    for (name, expected) in [
        ("DTYPE", 0),
        ("DSHORT", 0),
        ("DVALUE", 0),
        ("DNARROW", 0),
        ("DARRAY", 1),
        ("DPARAM", 0),
        ("DFUNC", 0),
        ("BREAL", 64),
        ("BSHORT", 32),
    ] {
        let parameter = ir.modules()[0]
            .parameters()
            .iter()
            .find(|p| p.name() == name)
            .unwrap();
        assert_eq!(parameter.resolved_value(), Some(expected), "{name}");
    }
}

#[test]
fn real_constant_casts_preserve_precision_and_integral_rounding() {
    let ir = analyze_source(
        r#"module Top;
        localparam shortreal F = shortreal'(-2.25);
        localparam int N = int'(F);
        localparam int A = N + 5;
        localparam int POS_HALF = int'(shortreal'(1.5));
        localparam int NEG_HALF = int'(real'(-1.5));
        localparam int NARROW_ROUND = int'(shortreal'(16777217.0));
        localparam int UNSIGNED_BITS = 8'(real'(-1.5));
    endmodule"#,
        Path::new("real_constant_casts.sv"),
    )
    .expect("real constant casts");
    for (name, expected) in [
        ("N", -2),
        ("A", 3),
        ("POS_HALF", 2),
        ("NEG_HALF", -2),
        ("NARROW_ROUND", 16777216),
        ("UNSIGNED_BITS", -2),
    ] {
        let parameter = ir.modules()[0]
            .parameters()
            .iter()
            .find(|p| p.name() == name)
            .unwrap();
        assert_eq!(parameter.resolved_value(), Some(expected), "{name}");
    }
}

#[test]
fn rejects_real_valued_select_indices() {
    for (declaration, index) in [
        ("real a = 0.5;", "a"),
        ("shortreal a = 0.5;", "a"),
        ("real a = 0.5;", "a + 1"),
        ("", "0.5"),
        ("localparam real a = 0.5;", "a"),
        ("localparam shortreal a = 0.5;", "a"),
        ("localparam real a = 0.5 + 1.0;", "a"),
        ("localparam real a = 0.5;", "a + 1"),
        ("function real a(); return 0.5; endfunction", "a()"),
    ] {
        let source = format!(
            "module top(input wire [3:0] b, output wire c); {declaration} assign c = b[{index}]; endmodule"
        );
        let error = analyze_source(&source, Path::new("real_index.sv")).unwrap_err();
        assert!(
            error.to_string().contains("real-valued select index"),
            "{declaration} {index}: {error}"
        );
    }
}

#[test]
fn accepts_integral_select_indices_including_real_conversion() {
    for index in ["a", "a + 1", "int'(r)", "$rtoi(r)"] {
        let source = format!(
            "module top(input wire [3:0] b, input int a, input real r, output wire c); assign c = b[{index}]; endmodule"
        );
        analyze_source(&source, Path::new("integer_index.sv")).expect(index);
    }
}

#[test]
fn explicit_output_data_types_default_to_variables() {
    let source = analyze_source(
        "module Top(input logic i, output logic q,r, output wire logic w, output [3:0] implicit_o); endmodule",
        Path::new("port_kinds.sv"),
    ).unwrap();
    let ports = source.modules()[0].ports();
    assert!(ports[0].is_net());
    assert!(!ports[1].is_net());
    assert!(!ports[2].is_net());
    assert!(ports[3].is_net());
    assert!(ports[4].is_net());
}

#[test]
fn class_randomize_rejects_unsupported_semantics() {
    for (members, call, expected) in [
        (
            "rand int x; constraint c { x == 1; }",
            "c.randomize()",
            "constraints",
        ),
        ("randc bit [1:0] x;", "c.randomize()", "randc"),
        ("rand int x;", "c.srandom()", "argument"),
        ("rand int x;", "c.srandom(1, 2)", "arguments"),
        (
            "rand int x; extern constraint c;",
            "c.randomize()",
            "constraints",
        ),
        ("rand const int x;", "c.randomize()", "const"),
        ("static rand int x;", "c.randomize()", "static random"),
        ("rand int x;", "C::randomize()", "object receiver"),
        ("rand int x;", "C::srandom(1)", "object receiver"),
        (
            "static function int f(); return randomize(); endfunction",
            "c.f()",
            "object receiver",
        ),
        ("rand string x;", "c.randomize()", "integral non-enum"),
        (
            "rand int x;",
            "c.randomize() with { x == 1; }",
            "unsupported",
        ),
        ("rand int x;", "c.randomize(x + 1)", "arguments"),
        ("rand int x;", "c.randomize(c.x)", "arguments"),
        ("rand int x;", "c.randomize(null, x)", "arguments"),
        (
            "rand int x;",
            "c.randomize(missing)",
            "not an instance property",
        ),
        ("const int x = 1;", "c.randomize(x)", "const"),
        ("local int x;", "c.randomize(x)", "non-public"),
        ("protected int x;", "c.randomize(x)", "non-public"),
        ("rand int x;", "c.rand_mode()", "getter requires"),
        ("int x;", "c.x.rand_mode()", "rand or randc"),
        (
            "rand int x; function void pre_randomize(int arg); endfunction",
            "c.randomize()",
            "hooks",
        ),
        (
            "function int randomize(); return 1; endfunction",
            "c.randomize()",
            "overriding",
        ),
    ] {
        let source = format!(
            "module Top; class C; {members} endclass C c; initial begin c = new; void'({call}); end endmodule"
        );
        let error = analyze_source(&source, Path::new("randomize_unsupported.sv"))
            .unwrap_err()
            .to_string();
        assert!(error.to_lowercase().contains(expected), "{source}: {error}");
    }
}

#[test]
fn rand_mode_setter_cannot_be_used_as_a_value() {
    let source = "module Top; class C; rand int x; endclass C c; int result; initial begin c = new; result = c.x.rand_mode(1); end endmodule";
    let error = analyze_source(source, Path::new("mode_value.sv"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("setter is void"), "{error}");
}

#[test]
fn rand_mode_name_outside_a_class_is_an_ordinary_function() {
    let source = "module Top(output int value); function int rand_mode(); return 42; endfunction initial value = rand_mode(); endmodule";
    analyze_source(source, Path::new("ordinary_rand_mode.sv")).unwrap();
}

#[test]
fn randomization_signatures_do_not_force_unused_interface_class_layouts() {
    for declaration in [
        "interface class I; typedef int T; pure virtual function void f(T x); endclass
         interface class J extends I; pure virtual function void g(T x); endclass",
        "interface class I #(type T = logic); pure virtual function void f(T x); endclass
         interface class J #(type T = logic); pure virtual function void g(T x); endclass
         interface class K #(type U = logic) extends I#(U), J#(U); typedef U T; endclass",
    ] {
        let source = format!("module Top; {declaration} endmodule");
        analyze_source(&source, Path::new("unused_interface_class.sv")).unwrap();
    }
}
