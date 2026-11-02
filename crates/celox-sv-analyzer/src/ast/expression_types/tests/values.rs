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

#[test]
fn casts_take_their_target_type_signing_or_width() {
    // 6.24.1: a type target gives that type; a signing or width target
    // keeps the rest of the operand's type.
    let declarations = "typedef logic [5:0] six_t; logic signed [3:0] s; bit [3:0] u;";
    let (session, ty) = query(declarations, "int'(u)");
    let TyData::Packed(int) = session.types.data(ty) else {
        panic!("int'(u) is not integral")
    };
    assert_eq!((int.bits(), int.signed, int.four_state), (32, true, false));
    packed(declarations, "six_t'(u)", 6, false, true);
    packed(declarations, "unsigned'(s)", 4, false, true);
    packed(declarations, "signed'(u)", 4, true, false);
    packed(declarations, "const'(s)", 4, true, true);
    packed(declarations, "12'(s)", 12, true, true);
    let (session, ty) = query(declarations, "real'(u)");
    assert_eq!(session.types.data(ty), &TyData::Real);
    // A cast to a declared type needs no type of its operand: a streaming
    // concatenation has none of its own.
    let (session, ty) = query(declarations, "int'({>>{u}})");
    let TyData::Packed(int) = session.types.data(ty) else {
        panic!("int'({{>>{{u}}}}) is not integral")
    };
    assert_eq!((int.bits(), int.signed, int.four_state), (32, true, false));
}

#[test]
fn concatenations_sum_their_items_or_are_strings() {
    // 11.4.12: an unsigned vector of the items' widths; a string item makes
    // a string concatenation (11.4.12.1).
    let declarations = "logic signed [3:0] s; bit [2:0] u; string t;";
    packed(declarations, "{s, u}", 7, false, true);
    packed("bit [2:0] u; bit v;", "{u, v}", 4, false, false);
    let (session, ty) = query(declarations, "{t, \"a\"}");
    assert_eq!(session.types.data(ty), &TyData::String);
}

#[test]
fn replications_multiply_the_width_of_their_items() {
    // 11.4.12.1: a constant count of the concatenation.
    packed("bit [2:0] u; bit v;", "{3{u, v}}", 12, false, false);
    packed("logic [1:0] l;", "{4{l}}", 8, false, true);
}

#[test]
fn conditionals_of_numeric_arms_take_the_larger_vector_or_real() {
    // 11.4.11: arms of other widths give the larger width, signed only if
    // both are; a real arm gives real.
    let declarations = "bit c; logic signed [3:0] s; logic signed [7:0] w; bit [5:0] u; real r;";
    packed(declarations, "c ? s : w", 8, true, true);
    packed(declarations, "c ? s : u", 6, false, true);
    let (session, ty) = query(declarations, "c ? s : r");
    assert_eq!(session.types.data(ty), &TyData::Real);
}

#[test]
fn assignment_expressions_take_their_target_type() {
    // 11.3.6: the value of an assignment expression is that of its target.
    packed("logic [4:0] a; bit [9:0] b;", "(a = b)", 5, false, true);
}

#[test]
fn min_typ_max_expressions_take_the_typical_type() {
    // 11.11: the selected expression gives the type.
    packed(
        "logic [2:0] a; logic [5:0] b; logic [7:0] d;",
        "(a:b:d)",
        6,
        false,
        true,
    );
}

#[test]
fn system_functions_return_their_declared_types() {
    // 20.6.2, 20.7 and 20.8: data queries and math functions return integer;
    // $signed and $unsigned keep the operand's width (11.7); real math
    // functions return real (20.8.2).
    let declarations = "logic [5:0] a; real r;";
    packed(declarations, "$bits(a)", 32, true, true);
    packed(declarations, "$size(a)", 32, true, true);
    packed(declarations, "$clog2(a)", 32, true, true);
    packed(declarations, "$signed(a)", 6, true, true);
    packed(declarations, "$unsigned($signed(a))", 6, false, true);
    let (session, ty) = query(declarations, "$sqrt(r)");
    assert_eq!(session.types.data(ty), &TyData::Real);
}

#[test]
fn inside_gives_one_bit_whatever_its_set() {
    // 11.4.13: the result is a one-bit value, which may be x.
    let declarations = "logic [7:0] v; int lo, hi; int q[$];";
    packed(declarations, "v inside {1, 2}", 1, false, true);
    packed(declarations, "v inside {[lo:hi], 9}", 1, false, true);
    packed(declarations, "v inside {q}", 1, false, true);
}

#[test]
fn parameters_have_their_declared_types() {
    // 6.20.2: a typed parameter has its declared type, alone or in an
    // expression. Untyped and qualified constants are evaluated with their
    // scope and are covered by the shared `$cast` cases.
    packed("localparam logic [5:0] P = 3;", "P", 6, false, true);
    // A parameter keeps its declared type, the `int` keyword included.
    let (session, ty) = query("localparam int P = 3;", "P");
    let TyData::Packed(int) = session.types.data(ty) else {
        panic!("P is not integral")
    };
    assert_eq!((int.bits(), int.signed, int.four_state), (32, true, false));
    packed(
        "localparam logic signed [4:0] P = 3;",
        "P + 1",
        32,
        true,
        true,
    );
}

#[test]
fn every_system_function_of_the_catalog_has_a_result_rule() {
    // Clauses 20 and 21 and Annex D: each function form has a declared
    // result, or none of a self-determined type, never an unknown one.
    for entry in crate::system_functions::SYSTEM_TFS {
        if entry.kind != crate::system_functions::SystemTfKind::Task {
            assert!(
                builtins::system_result(entry.name).is_some(),
                "no result rule for `{}`",
                entry.name
            );
        }
    }
}

#[test]
fn operators_and_system_functions_have_their_lrm_result_types() {
    // 11.6.1, 11.8.1 and clauses 20, 21: results by operator or function,
    // whatever the kinds of the operands.
    let declarations = "string s, t; class C; endclass C a, b; real r; logic [3:0] l; \
                        bit [3:0] u; chandle c;";
    for (expression, width, signed, four_state) in [
        ("s == t", 1, false, false),
        ("s != t", 1, false, false),
        ("s < t", 1, false, false),
        ("a == b", 1, false, false),
        ("a != null", 1, false, false),
        ("a === b", 1, false, false),
        ("!a", 1, false, false),
        ("a && b", 1, false, false),
        ("a || u", 1, false, false),
        ("r < 1.0", 1, false, false),
        ("r == r", 1, false, false),
        ("c == null", 1, false, false),
        ("l == u", 1, false, true),
        ("l === u", 1, false, false),
        ("l ==? u", 1, false, true),
        ("u -> u", 1, false, false),
        ("&l", 1, false, true),
        ("^u", 1, false, false),
        ("u inside {1, 2}", 1, false, true),
        ("$cast(a, b)", 32, true, false),
        ("$sscanf(s, \"%d\", u)", 32, true, true),
        ("$fscanf(1, \"%d\", u)", 32, true, true),
        ("$fopen(s)", 32, true, true),
        ("$fgets(s, 1)", 32, true, true),
        ("$test$plusargs(s)", 32, true, true),
        ("$time", 64, false, true),
        ("$stime", 32, false, true),
        ("$random", 32, true, true),
        ("$urandom", 32, false, false),
        ("$countones(l)", 32, true, false),
        ("$onehot(l)", 1, false, false),
        ("$isunknown(l)", 1, false, false),
        ("$realtobits(r)", 64, false, false),
        ("$rtoi(r)", 32, true, true),
        ("$size(l)", 32, true, true),
    ] {
        packed(declarations, expression, width, signed, four_state);
    }
    for (expression, expected) in [
        ("$realtime", TyData::Real),
        ("$itor(u)", TyData::Real),
        ("$sqrt(r)", TyData::Real),
        ("$bitstoshortreal(u)", TyData::Shortreal),
        ("r + 1", TyData::Real),
        ("$sformatf(\"%d\", u)", TyData::String),
        ("$typename(u)", TyData::String),
    ] {
        let (session, ty) = query(declarations, expression);
        assert_eq!(session.types.data(ty), &expected, "{expression}");
    }
}

#[test]
fn deep_one_bit_expressions_use_the_worklist() {
    // Logical, relational and reduction operands are queried on the same
    // worklist as arithmetic ones, so depth does not consume the host stack.
    std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(|| {
            let declarations = "logic [3:0] a; class C; endclass C h;";
            packed(
                declarations,
                &format!("{}a", "!".repeat(10_000)),
                1,
                false,
                true,
            );
            let chain = std::iter::repeat_n("a", 10_000)
                .collect::<Vec<_>>()
                .join(" && ");
            packed(declarations, &chain, 1, false, true);
            let handles = std::iter::repeat_n("(h == null)", 4096)
                .collect::<Vec<_>>()
                .join(" || ");
            packed(declarations, &handles, 1, false, false);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn array_reductions_have_their_element_type() {
    // 7.12.3: sum, product, and, or and xor give a value of the element
    // type, the table binding uses deciding which methods reduce.
    let declarations = "logic signed [5:0] a[3]; bit [3:0] q[$];";
    for method in ["sum", "product", "and", "or", "xor"] {
        packed(declarations, &format!("a.{method}()"), 6, true, true);
        packed(declarations, &format!("q.{method}()"), 4, false, false);
    }
}

#[test]
fn structure_members_have_their_declared_types_through_any_base() {
    // 7.2: a member has its declared type, packed or unpacked, whatever
    // gives the structure: a variable, an element, a member, a property or
    // a call result.
    let declarations = "typedef struct packed { logic [3:0] value; bit signed [2:0] tiny; } P; \
                        typedef struct { bit [5:0] wide; P inner; } U; \
                        class C; P p; endclass \
                        P s; U u; P ps[2]; U us[3]; C h; \
                        function automatic P make(); return s; endfunction";
    for (expression, width, signed, four_state) in [
        ("s.value", 4, false, true),
        ("s.tiny", 3, true, false),
        ("u.wide", 6, false, false),
        ("u.inner.value", 4, false, true),
        ("ps[1].value", 4, false, true),
        ("us[2].inner.tiny", 3, true, false),
        ("h.p.value", 4, false, true),
        ("make().value", 4, false, true),
        ("s.value + 1", 32, false, true),
    ] {
        packed(declarations, expression, width, signed, four_state);
    }
}

#[test]
fn selects_of_packed_values_follow_their_packed_dimensions() {
    // 11.5.1, 11.8.1: a bit- or part-select of the bits of any packed
    // integral value is unsigned, of the selected width, 4-state iff the
    // value is; only an index of a packed-array dimension gives its element.
    let declarations = "typedef enum logic signed [3:0] {A = 1, B = 2} E; \
                        typedef enum bit [2:0] {X = 1, Y = 2} F; \
                        typedef struct packed { logic [3:0] value; bit [1:0] tag; } S; \
                        typedef union packed { logic [5:0] a; bit [5:0] b; } U; \
                        E e; F f; S s; U u; E [2:0] ea; S [1:0] sa; logic [1:0][3:0] v;";
    for (expression, width, signed, four_state) in [
        ("e[0]", 1, false, true),
        ("e[2:1]", 2, false, true),
        ("e[0+:3]", 3, false, true),
        ("f[1]", 1, false, false),
        ("s[0]", 1, false, true),
        ("s[5:2]", 4, false, true),
        ("u[3]", 1, false, true),
        ("u[5:0]", 6, false, true),
        ("ea[1][0]", 1, false, true),
        ("ea[1][3:2]", 2, false, true),
        ("sa[0][1]", 1, false, true),
        ("v[1]", 4, false, true),
        ("v[1][2]", 1, false, true),
        ("v[1][2:0]", 3, false, true),
    ] {
        packed(declarations, expression, width, signed, four_state);
    }
    // An index of a packed array of enums or of packed structures gives
    // the element type.
    let (session, ty) = query(declarations, "ea[1]");
    let TyData::Packed(element) = session.types.data(ty) else {
        panic!("ea[1] is not integral")
    };
    assert!(matches!(
        element.element,
        PackedElement::Enum { array: false, .. }
    ));
    assert_eq!(element.bits(), 4);
    let (session, ty) = query(declarations, "sa[1]");
    let TyData::Packed(element) = session.types.data(ty) else {
        panic!("sa[1] is not integral")
    };
    assert!(matches!(element.element, PackedElement::Aggregate(_)));
    assert_eq!(element.bits(), 6);
    packed(declarations, "sa[1].value", 4, false, true);
}

#[test]
fn equalities_of_aggregates_are_four_state_iff_an_element_is() {
    // 11.4.5, 6.11: an equality of arrays, containers or structures with a
    // 4-state element or member may give x.
    let declarations = "logic a[2], b[2]; bit c[2], d[2]; logic q[$], r[$]; \
                        typedef struct { bit x; logic y; } U; U u, v; \
                        typedef struct { bit x; } B; B s, t;";
    packed(declarations, "a == b", 1, false, true);
    packed(declarations, "c == d", 1, false, false);
    packed(declarations, "q != r", 1, false, true);
    packed(declarations, "u == v", 1, false, true);
    packed(declarations, "s == t", 1, false, false);
}
