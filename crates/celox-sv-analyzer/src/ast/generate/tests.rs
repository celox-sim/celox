use super::*;
use std::path::Path;

fn analyze(source: &str) -> Result<Source, AnalyzerError> {
    let tree = crate::source_tree::parse_scope_fixture(source, Path::new("generate.sv"))?;
    let tree = AnalysisContext::unspecialized(&tree);
    Source::from_syntax(&tree)
}

#[test]
fn consecutive_parameter_runs_bind_each_prefix_once() {
    let session = &mut Session::default();
    for count in [16, 64, 256] {
        let mut code = String::from("module Top(); if (1) begin : g\n");
        for index in 0..count {
            let value = if index + 1 == count {
                "1".into()
            } else {
                format!("P{} + 1", index + 1)
            };
            use std::fmt::Write;
            writeln!(code, "localparam logic [31:0] P{index} = {value};").unwrap();
        }
        code.push_str("logic [P0-1:0] data; end endmodule");
        let tree = crate::source_tree::parse_scope_fixture(
            &code,
            Path::new("generate_parameter_prefix.sv"),
        )
        .unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        parameters::PARAMETER_BINDINGS.with(|bindings| bindings.set(0));
        let active = items(
            &tree,
            session,
            &ConstEnv::default(),
            &TypeAliases::default(),
        )
        .unwrap();
        let bindings = parameters::PARAMETER_BINDINGS.with(|bindings| bindings.get());
        assert!(
            bindings <= 3 * count,
            "{count} parameters rebound {bindings} times"
        );
        assert_eq!(active.last().unwrap().env["P0"], count as i128);
    }
}

#[test]
fn signals_interrupt_parameter_runs_before_size_queries_resume() {
    let source = analyze("module Top(); if (1) begin : g localparam int A=$bits(s0); logic [3:0] s0; localparam int B=A+2; logic [B-1:0] s1; localparam int C=$bits(s1); localparam int D=C+1; logic [D-1:0] s2; end endmodule").unwrap();
    let ir = crate::analyze::analyze_source(source).unwrap();
    let signals = ir.modules()[0].signals();
    assert_eq!(signals.len(), 3);
    for (signal, width) in signals.iter().zip([4, 6, 7]) {
        assert_eq!(signal.r#type().resolved_width(), Some(width));
    }
}

#[test]
fn cached_generate_types_match_rebuilt_types_for_imports_and_four_state_values() {
    let session = &mut Session::default();
    let syntax = crate::source_tree::SourceTree::empty();
    let tree = AnalysisContext::unspecialized(&syntax);
    let mut base = ConstEnv::from_iter([("pkg::IMPORTED".into(), -1)]);
    insert_parameter_type_markers(
        &mut base,
        "pkg::IMPORTED",
        ExprType {
            width: 8,
            signed: true,
        },
    );
    let mut cached_env = base.clone();
    let mut rebuilt_env = base;
    let mut cached_literals = HashMap::default();
    let mut rebuilt_literals = HashMap::default();
    let mut types = parameter_types_from_const_env(&cached_env);
    for (name, value, width) in [
        ("A", ConstExpr::Ident("pkg::IMPORTED".into()), None),
        ("X", ConstExpr::Literal("8'bx".into()), Some(8)),
        ("W", ConstExpr::Literal("129'b1".into()), Some(129)),
        ("Y", ConstExpr::Ident("X".into()), Some(8)),
        (
            "Z",
            ConstExpr::Binary {
                left: Box::new(ConstExpr::Ident("A".into())),
                op: BinaryOp::Add,
                right: Box::new(ConstExpr::Literal("2".into())),
            },
            Some(32),
        ),
    ] {
        let parameter = Parameter::new(
            name.into(),
            Some(value),
            width,
            width.map(|_| false),
            false,
            width.is_some(),
            true,
        );
        bind_generate_parameter(
            &tree,
            session,
            parameter.clone(),
            &mut rebuilt_env,
            &mut rebuilt_literals,
        );
        constants::bind_generate_parameter_with_types(
            &tree,
            session,
            parameter,
            &mut cached_env,
            &mut cached_literals,
            &mut types,
        );
        assert_eq!(cached_env, rebuilt_env);
        assert_eq!(cached_literals, rebuilt_literals);
        assert_eq!(types, parameter_types_from_const_env(&rebuilt_env));
    }
}

#[test]
fn generated_items_share_names_but_keep_mutations_in_their_own_snapshot() {
    let session = &mut Session::default();
    let tree = crate::source_tree::parse_scope_fixture(
        "module Top(); if (1) begin : g logic x; logic y; end endmodule",
        Path::new("generate_name_snapshots.sv"),
    )
    .unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let active = items(
        &tree,
        session,
        &ConstEnv::default(),
        &TypeAliases::default(),
    )
    .unwrap();
    assert_eq!(active.len(), 2);
    assert!(std::ptr::eq(&*active[0].names, &*active[1].names));
    assert!(Arc::ptr_eq(&active[0].shadowed, &active[1].shadowed));
    let mut local = active[0].clone();
    local.names.remove("x");
    Arc::make_mut(&mut local.shadowed).remove("x");
    assert_eq!(local.name("x"), "x");
    for sibling in &active {
        assert_eq!(sibling.name("x"), "g.x");
        assert_eq!(sibling.name("y"), "g.y");
        assert!(sibling.shadowed.contains("x"));
    }
}

#[test]
fn grouped_parameters_keep_only_the_scheduled_sibling_and_mask_outer_values() {
    for keyword in ["localparam", "parameter"] {
        let code = format!(
            r#"
                module Top #(parameter B=99) (output logic [31:0] y);
                    if (1) begin : g
                        {keyword} int A=B+1, B=C+1, C=2;
                        {keyword} logic [7:0] X='1, Y=X-1;
                        logic [A-1:0] data;
                        assign y=A+B+C+X+Y+$bits(data);
                    end
                endmodule
            "#
        );
        let tree =
            crate::source_tree::parse_scope_fixture(&code, Path::new("scheduled_siblings.sv"))
                .unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let active = items(
            &tree,
            &mut Session::default(),
            &ConstEnv::from_iter([("B".into(), 99)]),
            &TypeAliases::default(),
        )
        .unwrap();
        let assignment = active
            .iter()
            .find(|item| {
                matches!(
                    tree.declarations().body_item(item.id).kind,
                    celox_sv_syntax::BodyItemKind::Assign(_)
                )
            })
            .unwrap();
        for (name, value) in [("A", 4), ("B", 3), ("C", 2), ("X", 255), ("Y", 254)] {
            assert_eq!(assignment.env[name], value, "{keyword} {name}");
        }
        let ir = crate::analyze::analyze_source(Source::from_syntax(&tree).unwrap()).unwrap();
        assert_eq!(ir.modules()[0].signals()[0].name(), "g.data");
        assert_eq!(
            ir.modules()[0].signals()[0].r#type().resolved_width(),
            Some(4)
        );
    }
}

#[test]
fn reports_a_ready_declaration_error_before_a_remaining_cycle() {
    let error = analyze(
        r#"
            module Top();
                if (1) begin : g
                    localparam A=B, B=A;
                    localparam chandle R=null;
                end
            endmodule
        "#,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unsupported parameter data type"),
        "{error}"
    );
}

#[test]
fn coerces_genvar_assignments_before_naming_or_updating_iterations() {
    for (scheme, names) in [
        ("genvar i=32'hffffffff; i!=0; i/=2", vec!["g[-1].tmp"]),
        (
            "genvar i=2147483647; i!=-2147483647; i++",
            vec!["g[-2147483648].tmp", "g[2147483647].tmp"],
        ),
        ("genvar i=1; i!=0; i=64'h100000000", vec!["g[1].tmp"]),
        ("genvar i=1073741824; i!=0; i*=4", vec!["g[1073741824].tmp"]),
    ] {
        let source = analyze(&format!(
            "module Top(); for ({scheme}) begin : g logic tmp; end endmodule"
        ))
        .unwrap();
        let actual: Vec<_> = source.modules()[0]
            .signals()
            .iter()
            .map(|signal| signal.name())
            .collect();
        assert_eq!(actual, names, "{scheme}");
    }
}

#[test]
fn predeclares_generate_functions_before_nested_conditions() {
    let error = analyze(
        r#"
            module Top(output logic y);
                function automatic bit f(); return 1; endfunction
                if (1) begin : g
                    if (f()) assign y=1;
                    else assign y=0;
                    function automatic bit f(); return 0; endfunction
                end
            endmodule
        "#,
    )
    .unwrap_err();
    // IEEE 1800-2023 13.4.3 excludes generate-local constant functions.
    // Never silently select the same-named module function instead.
    assert!(
        error
            .to_string()
            .contains("unknown conditional-generate condition"),
        "{error}"
    );
}

#[test]
fn does_not_use_a_module_function_hidden_by_a_generate_function() {
    let source = r#"
            module Top(output logic y);
                function automatic bit enabled(); return 1; endfunction
                if (1) begin : g
                    function automatic bit enabled(); return 0; endfunction
                    if (enabled()) assign y=1;
                    else assign y=0;
                end
            endmodule
        "#;
    let error = analyze(source).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unknown conditional-generate condition"),
        "{error}"
    );
}

#[test]
fn rejects_recursive_constant_function_expansion() {
    let source = r#"
            module Top(output logic y);
                function automatic int recurse(); return recurse() + recurse(); endfunction
                if (recurse()) assign y=1;
                else assign y=0;
            endmodule
        "#;
    let error = analyze(source).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unknown conditional-generate condition"),
        "{error}"
    );
}

#[test]
fn preserves_constant_function_masks_and_case_context() {
    let source = r#"
            module Top(output logic y);
                function automatic logic unknown(); return 1'bx; endfunction
                function automatic logic [3:0] narrow(); return 4'hf; endfunction
                if (unknown()) begin initial $fatal; end
                else begin
                    case (narrow() + 4'd1)
                        8'd16: assign y=1;
                        default: initial $fatal;
                    endcase
                end
            endmodule
        "#;
    analyze(source).unwrap();
}

#[test]
fn rejects_runtime_function_reads_even_when_generate_locals_shadow_them() {
    let source = r#"
            module Top(input logic a, output logic y);
                function automatic logic read_input(); return a; endfunction
                if (1) begin : g
                    localparam a=1;
                    if (read_input()) assign y=1;
                    else assign y=0;
                end
            endmodule
        "#;
    let error = analyze(source).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unknown conditional-generate condition"),
        "{error}"
    );
}

#[test]
fn retains_loop_ancestry_through_nested_conditional_blocks() {
    let source = r#"
            module Top();
                for (genvar i=0; i<1; i++) begin : g
                    if (1) begin : nested
                        function automatic bit f(); return 1; endfunction
                    end
                end
            endmodule
        "#;
    let error = analyze(source).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("function declaration inside loop-generate"),
        "{error}"
    );
}

#[test]
fn resolves_shared_packed_and_individual_unpacked_dimensions() {
    for declaration in [
        "logic [P-1:0] a[W-1:0], b[N-1:0];",
        "wire [P-1:0] a[W-1:0], b[N-1:0];",
        "T a[W-1:0], b[N-1:0];",
        "logic [P-1:0] a[W-1:0], b[N'(N-1):0];",
    ] {
        let source = format!(
            r#"
                module Top(output logic y);
                    typedef logic [2:0] T;
                    if (1) begin : g
                        localparam N = $size(a);
                        localparam W = 2;
                        localparam P = 3;
                        {declaration}
                        assign a[0] = 0;
                        assign a[1] = 0;
                        assign b[0] = 0;
                        assign b[1] = 0;
                        if (($bits(b) == 6) && ($size(b) == 2)) assign y=1;
                        else begin initial $fatal; end
                    end
                endmodule
            "#
        );
        analyze(&source).unwrap_or_else(|error| panic!("{declaration}: {error}"));
    }
}

#[test]
fn scopes_all_function_return_metadata_and_keeps_hidden_outer_binding() {
    let session = &mut Session::default();
    let tree = crate::source_tree::parse_scope_fixture(
        r#"
            module Top();
                if (1) begin : g
                    function automatic bit signed [3:0] f(); return 0; endfunction
                end
            endmodule
        "#,
        Path::new("generate_metadata.sv"),
    )
    .unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let active = items(
        &tree,
        session,
        &ConstEnv::default(),
        &TypeAliases::default(),
    )
    .unwrap();
    let outer = FunctionReturnMetadata {
        semantic_type: None,
        arguments: None,
        width: Some(8),
        first_packed_dimension_width: Some(2),
        dimensions: Some(2),
        signed: false,
        is_2state: false,
        string: false,
        chandle: false,
        pattern_type: None,
        enum_domain: None,
        virtual_interface: None,
        container: None,
    };
    let inner = FunctionReturnMetadata {
        semantic_type: None,
        arguments: None,
        width: Some(4),
        first_packed_dimension_width: Some(4),
        dimensions: Some(1),
        signed: true,
        is_2state: true,
        string: false,
        chandle: false,
        pattern_type: None,
        enum_domain: None,
        virtual_interface: None,
        container: None,
    };
    let dimensions = PackedDimensions {
        function_return_types: [
            ("f".to_string(), outer.clone()),
            ("g.f".to_string(), inner.clone()),
        ]
        .into_iter()
        .collect(),
        ..PackedDimensions::default()
    };
    let local = active[0].dimensions(&dimensions);
    assert_eq!(local.function_return_types["f"], inner);
    assert_eq!(local.function_return_types["g.f"], inner);
    assert_eq!(
        local.function_return_types[&format!("{OUTER_BINDING}f")],
        outer
    );
    assert_eq!(dimensions.function_return_types["f"], outer);
}

#[test]
fn rejects_nonconstant_truth_and_unknown_genvar_values() {
    for body in [
        "if (a) assign y=1; else assign y=0;",
        "for (genvar i=0; a; i++) assign y=0;",
        "for (genvar i=1'bx; i<1; i++) assign y=0;",
    ] {
        let source = format!("module Top(input logic a, output logic y); {body} endmodule");
        assert!(
            analyze(&source).is_err(),
            "accepted nonconstant or invalid generate: {body}"
        );
    }
}

#[test]
fn rejects_cyclic_generate_signal_size_dependencies() {
    let source = "module Top(); if (1) begin : g localparam W=$bits(data); logic [W-1:0] data; end endmodule";
    let error = analyze(source).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("cyclic generate-local parameter dependency"),
        "{error}"
    );
}

#[test]
fn genvar_scopes_discard_all_inherited_packed_bounds() {
    let session = &mut Session::default();
    let tree = crate::source_tree::parse_scope_fixture(
            "module Top(output logic [1:0] y); localparam logic signed [2:1][4:0] i=9; for (genvar i=0; i<2; i++) begin : g assign y[i]=i; end endmodule",
            Path::new("genvar_packed_bounds.sv"),
        ).unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let parameters = parameters_from_scope(
        &tree,
        session,
        &TypeAliases::default(),
        &ConstEnv::default(),
        &HashMap::default(),
    )
    .unwrap();
    let mut env = ConstEnv::default();
    extend_const_env_with_parameters(&tree, session, &mut env, &parameters);
    assert_eq!(env[&parameter_dimensions_marker("i")], 2);
    let active = items(
        &tree,
        &mut Session::default(),
        &env,
        &TypeAliases::default(),
    )
    .unwrap();
    let iterations: Vec<_> = active
        .iter()
        .filter(|item| !item.scope.is_empty())
        .collect();
    assert_eq!(iterations.len(), 2);
    for (value, item) in iterations.iter().enumerate() {
        assert_eq!(item.env["i"], value as i128);
        assert_eq!(item.env[&parameter_width_marker("i")], 32);
        assert_eq!(item.env[&parameter_signed_marker("i")], 1);
        assert_eq!(item.env[&parameter_rank_marker("i")], 1);
        assert_eq!(item.env[&parameters::unbounded_parameter_marker("i")], 0);
        assert!(!item.env.contains_key(&parameter_dimensions_marker("i")));
        assert!(!item.parameter_dimensions.contains_key("i"));
        for dimension in 0..2 {
            for bound in ["left", "right"] {
                assert!(
                    !item
                        .env
                        .contains_key(&parameter_dimension_marker("i", dimension, bound))
                );
            }
        }
    }
    assert_eq!(env[&parameter_dimension_marker("i", 0, "left")], 2);
    assert_eq!(env[&parameter_dimension_marker("i", 1, "right")], 0);
}

#[test]
fn folds_shadowing_genvars_in_nested_selects() {
    let source = analyze(
            "module Top #(parameter logic [7:0] i=9)(output logic [2:0] y); for (genvar i=-1; i<2; i++) begin : g if (1) assign y[i+1]=i; end endmodule",
        ).unwrap();
    assert_eq!(source.modules()[0].assignments().len(), 3);
    for (index, assignment) in source.modules()[0].assignments().iter().enumerate() {
        let LValue::Select { lsb, .. } = &assignment.lhs else {
            panic!("{assignment:?}")
        };
        assert_eq!(
            eval_ast_const_expr(
                &const_functions::ConstantFunctions::default(),
                lsb,
                &ConstEnv::default()
            ),
            Some(index as i128),
            "{assignment:?}"
        );
    }
}

#[test]
fn reserves_genvar_in_its_own_block_but_allows_nested_shadowing() {
    for declaration in ["logic i;", "wire i;", "localparam i = 2;"] {
        let source = format!(
            "module Top(); for (genvar i=0; i<1; i++) begin : g {declaration} end endmodule"
        );
        let error = analyze(&source).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("duplicate generate-local declaration `i`"),
            "{error}"
        );
    }
    analyze("module Top(output logic y); for (genvar i=0; i<1; i++) begin : g if (1) begin : inner localparam i=1; assign y=i; end end endmodule").unwrap();
}

#[test]
fn rejects_cyclic_generate_localparams_even_when_outer_names_exist() {
    for declarations in ["localparam A=B; localparam B=A;", "localparam A=A;"] {
        let source =
            format!("module Top(); localparam A=1; if (1) begin : g {declarations} end endmodule");
        let error = analyze(&source).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cyclic generate-local parameter dependency"),
            "{error}"
        );
    }
}

#[test]
fn gives_nested_instances_and_signals_distinct_scopes() {
    let source = analyze(
        r#"
            module Child(input logic a, output logic y); assign y = a; endmodule
            module Top(input logic [3:0] a, output logic [3:0] y);
                for (genvar i = 0; i < 2; i++) begin : rows
                    for (genvar j = 0; j < 2; j++) begin : columns
                        logic tmp;
                        Child child(.a(a[i*2+j]), .y(tmp));
                        assign y[i*2+j] = tmp;
                    end
                end
            endmodule
        "#,
    )
    .unwrap();
    let top = source.modules().iter().find(|m| m.name() == "Top").unwrap();
    let names: Vec<_> = top.signals().iter().map(|s| s.name()).collect();
    assert_eq!(
        names,
        [
            "rows[0].columns[0].tmp",
            "rows[0].columns[1].tmp",
            "rows[1].columns[0].tmp",
            "rows[1].columns[1].tmp"
        ]
    );
    assert_eq!(top.instances()[3].name(), "rows[1].columns[1].child");
}

#[test]
fn case_generate_uses_case_equality_and_selects_only_one_branch() {
    for selector in ["8'hff", "'1", "4'bxz01"] {
        let source = format!(
            r#"
                module Top(output logic y);
                    case ({selector})
                        8'hff, 4'bxz01: assign y = 1'b1;
                        default: initial $fatal;
                    endcase
                endmodule
            "#
        );
        analyze(&source).unwrap_or_else(|error| panic!("selector {selector}: {error}"));
    }
    analyze(
        r#"
            module Top(output logic y);
                case (3)
                    1: initial $fatal;
                    2: initial $fatal;
                endcase
                assign y = 1'b1;
            endmodule
        "#,
    )
    .unwrap();
}

#[test]
fn directly_nested_conditionals_share_the_outer_generate_scope() {
    let source = analyze(
        r#"
            module Child(); endmodule
            module Top();
                if (0) begin : chosen Child child(); end
                else if (1) begin : chosen Child child(); end
            endmodule
        "#,
    )
    .unwrap();
    assert_eq!(source.modules()[1].instances()[0].name(), "chosen.child");
}

#[test]
fn propagates_case_width_before_arithmetic_and_preserves_self_determined_operands() {
    for (selector, label) in [
        ("4'hf + 4'h1", "8'h10"),
        ("4'hf << 2", "8'h3c"),
        ("&4'hf", "8'h01"),
        ("$clog2(8) + 1", "32'd4"),
    ] {
        let source = format!(
            "module Top(output logic y); case ({selector}) {label}: assign y=1; default: initial $fatal; endcase endmodule"
        );
        analyze(&source).unwrap_or_else(|error| panic!("{selector}: {error}"));
    }
}

#[test]
fn validates_only_active_loop_branches_and_preserves_genvar_type() {
    analyze(
        r#"
            module Top(output logic y);
                for (genvar i = -1; i < 0; i++) begin
                    if (&i) assign y = 1'b1;
                    else begin
                        function automatic logic f(); return 0; endfunction
                        initial $fatal;
                    end
                end
            endmodule
        "#,
    )
    .unwrap();
}

#[test]
fn rejects_invalid_generate_bindings_and_nonconstant_cases() {
    for (body, expected) in [
        (
            "if (1) begin logic tmp; logic tmp; end",
            "duplicate generate-local declaration",
        ),
        (
            "if (1) begin localparam P = 0; localparam P = 1; end",
            "duplicate generate-local declaration",
        ),
        (
            "if (1) begin localparam P = 0; logic P; end",
            "duplicate generate-local declaration",
        ),
        (
            "for (genvar i=0; i<1; j++) assign y=0;",
            "genvar update targets a different variable",
        ),
        (
            "case(a) default: assign y=0; endcase",
            "unknown case-generate selector",
        ),
        (
            "case(0) default: assign y=0; default: assign y=1; endcase",
            "duplicate case-generate default",
        ),
        (
            "if (1) begin typedef logic T; typedef bit T; end",
            "duplicate generate-local declaration",
        ),
        (
            "if (1) begin typedef enum logic { A,B } T; typedef enum logic { A,C } U; end",
            "duplicate generate-local declaration",
        ),
        (
            "if (1) begin typedef logic T; logic T; end",
            "duplicate generate-local declaration",
        ),
        (
            "if (1) begin typedef enum logic { A,B } T; localparam A=1; end",
            "duplicate generate-local declaration",
        ),
        (
            "if (1) begin parameter type T=logic; end",
            "generate-local type parameter",
        ),
        (
            "for (genvar i=0; i<1; i++) begin function automatic logic f(); return 0; endfunction end",
            "function declaration inside loop-generate",
        ),
    ] {
        let source = format!("module Top(input logic a, output logic y); {body} endmodule");
        let error = analyze(&source)
            .err()
            .unwrap_or_else(|| panic!("unexpected success: {body}"));
        assert!(error.to_string().contains(expected), "{body}: {error}");
    }
}

#[test]
fn reference_scan_covers_nested_branches_arrays_packages_and_escaped_names() {
    let code = r#"
            module Top;
                localparam UNUSED = missing(ONLY_INITIALIZER);
                localparam A = 'x;
                localparam B = A;
                localparam logic [7:0] ARR [0:0] = '{ARRAY_INPUT};
                function automatic logic f(); return FUNCTION_INPUT; endfunction
                generate
                    if (0) begin : inactive
                        assign out = INACTIVE_INPUT;
                    end
                    for (genvar i = 0; i < LOOP_BOUND; i++) begin : loop_scope
                        localparam C = NESTED_INPUT;
                    end
                endgenerate
                assign out = \escaped.name ;
            endmodule
            package pkg;
                localparam UNUSED = only_in_initializer;
                function automatic logic f(); return PACKAGE_INPUT; endfunction
            endpackage
        "#;
    let tree =
        crate::source_tree::parse_scope_fixture(code, Path::new("literal_references.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    for handle in tree.root_scopes().unwrap() {
        let selected = tree.scoped(handle).unwrap();
        if selected.root_unit().unwrap().kind == celox_sv_syntax::UnitKind::Module {
            let refs = parameter_literal_references(
                &AnalysisContext::unspecialized(&selected),
                LiteralUse::All,
            )
            .unwrap();
            for name in [
                "ARRAY_INPUT",
                "FUNCTION_INPUT",
                "INACTIVE_INPUT",
                "LOOP_BOUND",
                "NESTED_INPUT",
                "\\escaped.name",
            ] {
                assert!(refs.contains(name), "{name}: {refs:?}");
            }
            assert!(!refs.contains("ONLY_INITIALIZER"));
        } else if selected.root_unit().unwrap().kind == celox_sv_syntax::UnitKind::Package {
            let refs = parameter_literal_references(
                &AnalysisContext::unspecialized(&selected),
                LiteralUse::All,
            )
            .unwrap();
            assert!(refs.contains("PACKAGE_INPUT"));
            assert!(!refs.contains("only_in_initializer"));
        }
    }
}

#[test]
fn unused_unresolved_chains_do_not_populate_item_literals() {
    let session = &mut Session::default();
    use std::fmt::Write;

    for count in [16, 64, 256] {
        for doubling in [false, true] {
            let declaration = if doubling {
                // Expanding the previous unresolved value twice at every
                // declaration would make the unused expression trees grow
                // exponentially before this function is installed.
                "function automatic int next_value(input int left, right); return left + right; endfunction\n"
            } else {
                "function automatic int next_value(input int value); return value + 1; endfunction\n"
            };
            let mut code = format!("module Top; {declaration}");
            for i in 0..count {
                let arg = match (i, doubling) {
                    (0, false) => "0".into(),
                    (0, true) => "1, 1".into(),
                    (_, false) => format!("P{}", i - 1),
                    (_, true) => format!("P{0}, P{0}", i - 1),
                };
                writeln!(code, "localparam int P{i} = next_value({arg});").unwrap();
            }
            code.push_str("endmodule\n");
            let tree = crate::source_tree::parse_scope_fixture(&code, Path::new("unused_chain.sv"))
                .unwrap();
            let tree = AnalysisContext::unspecialized(&tree);
            let active = items(
                &tree,
                session,
                &ConstEnv::default(),
                &TypeAliases::default(),
            )
            .unwrap();
            assert!(active.iter().all(|item| item.literals.is_empty()));
            let ir = crate::analyze::analyze_source(Source::from_syntax(&tree).unwrap()).unwrap();
            for (i, parameter) in ir.modules()[0].parameters().iter().enumerate() {
                let expected = if !doubling {
                    i as i128 + 1
                } else if i >= 31 {
                    0
                } else {
                    (1u32 << (i + 1)) as i32 as i128
                };
                assert_eq!(parameter.resolved_value(), Some(expected));
            }
        }
    }
}

#[test]
fn referenced_four_state_dependencies_keep_their_full_values() {
    let session = &mut Session::default();
    let code = "module Top(output logic [7:0] out); localparam logic [7:0] A=8'bxz010101; localparam logic [7:0] B=A; localparam logic [7:0] UNUSED='x; assign out=B; endmodule";
    let tree =
        crate::source_tree::parse_scope_fixture(code, Path::new("needed_literals.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let env = ConstEnv::default();
    let aliases = TypeAliases::default();
    let parameters =
        parameters_from_scope(&tree, session, &aliases, &env, &HashMap::default()).unwrap();
    let full = parameter_value_env(&tree, session, &parameters, &env);
    let active = items(&tree, session, &env, &aliases).unwrap();
    for item in active {
        assert_eq!(item.literals.get("B"), full.get("B"));
        assert_eq!(item.literals.get("A"), full.get("A"));
        assert!(!item.literals.contains_key("UNUSED"));
    }
}

#[test]
fn subroutine_reference_scan_excludes_rtl_only_bindings() {
    let code = r#"
            module Top;
                localparam INITIALIZER = ONLY_INITIALIZER;
                function automatic logic f(); return FUNCTION_INPUT; endfunction
                task t(input logic [TASK_WIDTH-1:0] value); endtask
                assign out = RTL_INPUT;
                always_comb out = ALWAYS_INPUT;
                initial out = INITIAL_INPUT;
                initial begin : named logic [PROCEDURAL_WIDTH-1:0] local_signal; end
                logic [RTL_WIDTH-1:0] signal;
                if (CONDITION) begin : g
                    localparam P = LOCAL_INPUT;
                    logic [SIGNAL_WIDTH-1:0] x;
                    assign x = GENERATE_INPUT;
                end
                generate
                    assign other = REGION_INPUT;
                endgenerate
            endmodule
            package pkg;
                localparam INITIALIZER = ONLY_INITIALIZER;
                function automatic logic f(); return PACKAGE_INPUT; endfunction
            endpackage
        "#;
    let tree = crate::source_tree::parse_scope_fixture(code, Path::new("subroutine_references.sv"))
        .unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    for handle in tree.root_scopes().unwrap() {
        let selected = tree.scoped(handle).unwrap();
        if selected.root_unit().unwrap().kind == celox_sv_syntax::UnitKind::Module {
            let refs = parameter_literal_references(
                &AnalysisContext::unspecialized(&selected),
                LiteralUse::Subroutines,
            )
            .unwrap();
            for name in [
                "FUNCTION_INPUT",
                "TASK_WIDTH",
                "RTL_WIDTH",
                "PROCEDURAL_WIDTH",
                "CONDITION",
                "LOCAL_INPUT",
                "SIGNAL_WIDTH",
                "GENERATE_INPUT",
                "REGION_INPUT",
            ] {
                assert!(refs.contains(name), "{name}: {refs:?}");
            }
            for name in [
                "ONLY_INITIALIZER",
                "RTL_INPUT",
                "ALWAYS_INPUT",
                "INITIAL_INPUT",
            ] {
                assert!(!refs.contains(name), "{name}: {refs:?}");
            }
        } else if selected.root_unit().unwrap().kind == celox_sv_syntax::UnitKind::Package {
            let refs = parameter_literal_references(
                &AnalysisContext::unspecialized(&selected),
                LiteralUse::Subroutines,
            )
            .unwrap();
            assert!(refs.contains("PACKAGE_INPUT"));
            assert!(!refs.contains("ONLY_INITIALIZER"));
        }
    }
}

#[test]
fn subroutine_literals_preserve_functions_tasks_and_generate_scopes() {
    let session = &mut Session::default();
    let code = r#"
            module Top(output logic [7:0] out);
                localparam logic [7:0] A = 8'bxz010101;
                localparam logic [7:0] B = A;
                localparam logic [7:0] RTL_ONLY = 'x;
                function automatic logic [7:0] f(); return B; endfunction
                task t(output logic [7:0] result); result = A; endtask
                if (1) begin : g
                    localparam logic [7:0] C = B;
                    logic [7:0] x;
                    assign x = C;
                end
                assign out = RTL_ONLY;
            endmodule
        "#;
    let tree =
        crate::source_tree::parse_scope_fixture(code, Path::new("subroutine_literals.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&tree);
    let env = ConstEnv::default();
    let aliases = TypeAliases::default();
    let full = items(&tree, session, &env, &aliases).unwrap();
    let projected = subroutine_items(&tree, session, &env, &aliases).unwrap();
    assert_eq!(projected.len(), full.len());
    for (full, projected) in full.iter().zip(&projected) {
        assert_eq!(projected.env, full.env);
        assert_eq!(projected.names, full.names);
        assert_eq!(projected.shadowed, full.shadowed);
        assert_eq!(projected.parameter_dimensions, full.parameter_dimensions);
        assert_eq!(projected.scope, full.scope);
        for name in ["A", "B", "C"] {
            assert_eq!(projected.literals.get(name), full.literals.get(name));
        }
        assert!(!projected.literals.contains_key("RTL_ONLY"));
        assert!(full.literals.contains_key("RTL_ONLY"));
    }
    // The emitted RTL still uses the ordinary collector's complete roots.
    let ir = crate::analyze::analyze_source(Source::from_syntax(&tree).unwrap()).unwrap();
    let crate::ir::Expr::Literal(value) = ir.modules()[0].assignments().last().unwrap().rhs()
    else {
        panic!("RTL-only four-state parameter must remain a literal")
    };
    let value = typecheck::parse_integral_literal(value).unwrap();
    assert_eq!(value.width, 8);
    assert_eq!(value.mask, 255u32.into());
}

#[test]
fn rtl_referenced_unresolved_chains_do_not_expand_during_subroutine_discovery() {
    let session = &mut Session::default();
    use std::fmt::Write;

    for count in [16, 64, 256] {
        for doubling in [false, true] {
            let declaration = if doubling {
                "function automatic int next_value(input int left, right); return left + right; endfunction\n"
            } else {
                "function automatic int next_value(input int value); return value + 1; endfunction\n"
            };
            let mut code = format!("module Top(output int result); {declaration}");
            for i in 0..count {
                let arg = match (i, doubling) {
                    (0, false) => "0".into(),
                    (0, true) => "1, 1".into(),
                    (_, false) => format!("P{}", i - 1),
                    (_, true) => format!("P{0}, P{0}", i - 1),
                };
                writeln!(code, "localparam int P{i} = next_value({arg});").unwrap();
            }
            writeln!(code, "assign result = P{}; endmodule", count - 1).unwrap();
            let tree =
                crate::source_tree::parse_scope_fixture(&code, Path::new("rtl_chain.sv")).unwrap();
            let tree = AnalysisContext::unspecialized(&tree);
            let active = subroutine_items(
                &tree,
                session,
                &ConstEnv::default(),
                &TypeAliases::default(),
            )
            .unwrap();
            assert!(active.iter().all(|item| item.literals.is_empty()));
            let ir = crate::analyze::analyze_source(Source::from_syntax(&tree).unwrap()).unwrap();
            let parameters = ir.modules()[0].parameters();
            assert_eq!(parameters.len(), count);
            for (i, parameter) in parameters.iter().enumerate() {
                let expected = if !doubling {
                    i as i128 + 1
                } else if i >= 31 {
                    0
                } else {
                    (1u32 << (i + 1)) as i32 as i128
                };
                assert_eq!(parameter.resolved_value(), Some(expected));
            }
            let crate::ir::Expr::Literal(value) = ir.modules()[0].assignments()[0].rhs() else {
                panic!("resolved chain must reach RTL as a literal")
            };
            let value = typecheck::parse_integral_literal(value).unwrap();
            assert_eq!(
                value.value,
                if doubling {
                    if count >= 32 { 0 } else { 1u32 << count }
                } else {
                    count as u32
                }
                .into()
            );
            assert_eq!(value.mask, 0u32.into());
            assert_eq!(value.width, 32);
        }
    }
}

#[test]
fn subroutine_projection_preserves_generate_diagnostics() {
    let session = &mut Session::default();
    for body in [
        "if (UNKNOWN) begin : g logic x; end",
        "if (1) begin : g parameter type T=logic; end",
        "for (genvar i=0; i<1; i++) begin : g function logic f(); return 0; endfunction end",
        "if (1) begin : g localparam P=Q; localparam Q=P; end",
    ] {
        let code = format!("module Top; {body} endmodule");
        let tree =
            crate::source_tree::parse_scope_fixture(&code, Path::new("subroutine_diagnostics.sv"))
                .unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let full = items(
            &tree,
            session,
            &ConstEnv::default(),
            &TypeAliases::default(),
        )
        .err()
        .expect(body);
        let projected = subroutine_items(
            &tree,
            session,
            &ConstEnv::default(),
            &TypeAliases::default(),
        )
        .err()
        .expect(body);
        assert_eq!(projected.to_string(), full.to_string(), "{body}");
    }
}
