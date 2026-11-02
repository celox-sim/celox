use super::*;

fn packed(declarations: &str, expression: &str, width: u128, signed: bool, four_state: bool) {
    let (session, ty) = query(declarations, expression);
    let TyData::Packed(packed) = session.types.data(ty) else {
        panic!("`{expression}` is not integral")
    };
    assert_eq!(
        (packed.bits(), packed.signed, packed.four_state),
        (width, signed, four_state),
        "{expression}"
    );
    assert_eq!(packed.element, PackedElement::Bits, "{expression}");
    assert_eq!(packed.keyword, None, "{expression}");
}

#[test]
fn literals_use_the_existing_width_and_sign_rules_without_expanding_sized_values() {
    // 5.7.1 and 11.6.1; a huge sized value needs only its size/sign prefix.
    for (expression, width, signed) in [
        ("1", 32, true),
        ("'hff", 32, false),
        ("'shff", 32, true),
        ("8'd1", 8, false),
        ("7'shx", 7, true),
        ("'1", 1, false),
        ("'x", 1, false),
        ("1000000000'b1", 1_000_000_000, false),
        ("\"A\\n\"", 16, false),
        ("\"\"", 8, false),
        ("\"\"\"a\nb\"\"\"", 24, false),
    ] {
        packed("", expression, width, signed, true);
    }
    for expression in ["1.25", "2e3", "1_000.5"] {
        let (session, ty) = query("", expression);
        assert_eq!(session.types.data(ty), &TyData::Real);
    }
}

#[test]
fn unary_operators_apply_the_expression_rules_to_numeric_operands() {
    // 6.19.4, 11.6.1 and 11.8.1: operators yield integral values, not enum identity.
    let declarations = "logic signed [6:0] a; typedef enum logic signed[6:0] {A, B} E; E e;";
    for expression in ["+a", "-a", "~a", "a++", "--a", "-e"] {
        packed(declarations, expression, 7, true, true);
    }
    for operator in ["!", "&", "~&", "|", "~|", "^", "~^", "^~"] {
        packed(declarations, &format!("{operator}a"), 1, false, true);
    }
    packed("bit signed[4:0] a;", "~a", 5, true, false);
}

#[test]
fn arithmetic_and_bitwise_results_use_both_operands_but_no_assignment_context() {
    // Table 11-21 and 11.8.1: largest width; unsigned if either operand is unsigned.
    let declarations = "logic signed[4:0] a; logic signed[8:0] b; bit[2:0] u;";
    for operator in ["+", "-", "*", "/", "%", "&", "|", "^", "^~", "~^"] {
        packed(declarations, &format!("a {operator} b"), 9, true, true);
        packed(declarations, &format!("a {operator} u"), 5, false, true);
    }
    packed(
        "bit signed[3:0] a; bit signed[7:0] b;",
        "a + b",
        8,
        true,
        false,
    );
    packed("int a, b;", "a / b", 32, true, true);
    packed("int a, b;", "a % b", 32, true, true);
}

#[test]
fn shift_counts_and_exponents_do_not_widen_or_unsign_the_left_operand() {
    // Table 11-21: the exponent and shift count are self-determined.
    for operator in ["<<", ">>", "<<<", ">>>", "**"] {
        packed(
            "logic signed[4:0] a; logic[127:0] count;",
            &format!("a {operator} count"),
            5,
            true,
            true,
        );
    }
}

#[test]
fn logical_and_comparison_results_are_single_unsigned_bits() {
    // 11.4.6–7, Table 11-21: case equality cannot produce an unknown.
    for operator in [
        "==", "!=", "==?", "!=?", "<", "<=", ">", ">=", "&&", "||", "->", "<->", "===", "!==",
    ] {
        packed(
            "logic[7:0] a; logic signed[31:0] b;",
            &format!("a {operator} b"),
            1,
            false,
            !matches!(operator, "===" | "!=="),
        );
    }
    packed("bit[7:0] a, b;", "a && b", 1, false, false);
}

#[test]
fn real_and_shortreal_arithmetic_keep_the_required_promotion() {
    // 11.3.1: real takes precedence over shortreal; logical results are one bit.
    for (expression, expected) in [
        ("s + i", TyData::Shortreal),
        ("s * s", TyData::Shortreal),
        ("s + r", TyData::Real),
        ("i ** r", TyData::Real),
        ("-s", TyData::Shortreal),
        ("+r", TyData::Real),
    ] {
        let (session, ty) = query("real r; shortreal s; int i;", expression);
        assert_eq!(session.types.data(ty), &expected, "{expression}");
    }
    packed("real r; shortreal s;", "r < s", 1, false, false);
    packed("real r;", "!r", 1, false, false);
}

#[test]
fn numeric_queries_do_not_bind_function_calls_or_evaluate_values() {
    // A type query must neither execute next() nor fold division by zero.
    let declarations =
        "int calls; function logic signed[4:0] next(); calls++; return calls; endfunction";
    packed(declarations, "next() / 5'sd0", 5, true, true);
    packed(declarations, "0 && next()", 1, false, true);
}

#[test]
fn unsupported_or_unresolved_operands_are_not_hidden_by_boolean_results() {
    for expression in [
        "0 && missing",
        "1 || missing",
        "0 -> missing",
        "missing ** 0",
        "1.0 % 2",
        "~1.0",
        "\"s\" + null",
    ] {
        let source = format!("module M; assign out={expression}; endmodule");
        let tree =
            crate::source_tree::parse_scope_fixture(&source, Path::new("unknown-numeric.sv"))
                .unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let id = tree.declarations().assign_statements[0].assignments[0].right;
        let mut session = Session::default();
        assert!(
            type_of(
                &tree,
                &mut session,
                id,
                tree.declarations(),
                &PackedDimensions::default()
            )
            .is_err(),
            "{expression}"
        );
        assert_eq!(session.next_call_site, 0);
    }
}

#[test]
fn deep_numeric_expressions_use_a_worklist() {
    std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(|| {
            let expression = std::iter::repeat_n("a", 4096).collect::<Vec<_>>().join("+");
            packed("logic signed[4:0] a;", &expression, 5, true, true);
            packed(
                "logic signed[4:0] a;",
                &format!("{}a", "~".repeat(4096)),
                5,
                true,
                true,
            );
        })
        .unwrap()
        .join()
        .unwrap();
}
