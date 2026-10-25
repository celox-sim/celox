use super::*;

fn parse(code: &str) -> Declarations {
    Declarations::parse(code, Path::new("headers.sv")).unwrap()
}

#[test]
fn checker_scopes_capture_native_control_and_procedure_ownership_without_leaking_items() {
    let syntax = parse(
        "module M;checker C(input logic a=1);logic x;property P;a;endproperty if(a)begin:g final begin end end else always_latch begin end checker D;initial;endchecker:D endchecker:C wire w;endmodule",
    );
    let BodyItemKind::Unconverted { category, items } =
        &syntax.body_item(syntax.units[0].items[0]).kind
    else {
        panic!()
    };
    assert_eq!(category, "checker");
    assert!(
        matches!(&syntax.body_item(items[0]).kind,BodyItemKind::Unconverted{category,..} if category=="logic")
    );
    assert!(
        matches!(&syntax.body_item(items[1]).kind,BodyItemKind::Unconverted{category,..} if category=="property")
    );
    let BodyItemKind::Conditional {
        then_body,
        otherwise: Some(otherwise),
        ..
    } = syntax.body_item(items[2]).kind
    else {
        panic!()
    };
    let BodyItemKind::Block {
        items: then_items, ..
    } = &syntax.body_item(then_body).kind
    else {
        panic!()
    };
    let BodyItemKind::Procedure(id) = syntax.body_item(then_items[0]).kind else {
        panic!()
    };
    assert_eq!(syntax.procedure(id).kind, ProcedureKind::Final);
    assert!(
        syntax
            .token_text(syntax.procedure(id).tokens)
            .starts_with("final")
    );
    let BodyItemKind::Procedure(id) = syntax.body_item(otherwise).kind else {
        panic!()
    };
    assert_eq!(syntax.procedure(id).kind, ProcedureKind::AlwaysLatch);
    let BodyItemKind::Unconverted {
        category,
        items: nested,
    } = &syntax.body_item(items[3]).kind
    else {
        panic!()
    };
    assert_eq!(category, "checker");
    let BodyItemKind::Procedure(id) = syntax.body_item(nested[0]).kind else {
        panic!()
    };
    assert_eq!(syntax.procedure(id).kind, ProcedureKind::Initial);
    assert_eq!(syntax.units[0].items.len(), 2);
    assert!(syntax.data_declarations.is_empty());
    assert_eq!(syntax.net_declarations.len(), 1);
}

#[test]
fn malformed_checker_boundaries_and_captured_controls_are_rejected() {
    for source in [
        "module M;checker C(input logic a];endchecker endmodule",
        "module M;checker C;if()final begin end endchecker endmodule",
        "module M;checker C;if(1)begin final begin end endchecker endmodule",
        "module M;checker C;endchecker endchecker endmodule",
    ] {
        assert!(
            Declarations::parse(source, Path::new("checker-boundary.sv")).is_err(),
            "{source}"
        );
    }
}

#[test]
fn deep_checker_scope_and_control_arenas_parse_clone_and_drop_on_the_default_stack() {
    let source = format!(
        "module M;{}{}final begin end {}endmodule",
        "checker C;".repeat(10_000),
        "if(1)".repeat(10_000),
        "endchecker ".repeat(10_000)
    );
    let syntax = parse(&source);
    let copy = syntax.clone();
    assert_eq!(syntax.procedures.len(), 1);
    assert_eq!(syntax.units[0].items.len(), 1);
    drop(syntax);
    drop(copy);
}

#[test]
fn opaque_randsequence_owns_code_blocks_and_assignment_event_payloads() {
    let syntax = parse(
        "module M;initial randsequence(main) main:leaf({1'b0,1'b1}) {x=2;};leaf(int a):{x<=repeat(2)@(posedge clk iff a)1;};endsequence endmodule",
    );
    let root = syntax.procedures[0].body;
    let StatementKind::Unconverted {
        category, items, ..
    } = &syntax.statement(root).kind
    else {
        panic!()
    };
    assert_eq!(category, "randsequence");
    assert_eq!(items.len(), 2);
    let StatementKind::Block {
        items: statements, ..
    } = &syntax.statement(items[1]).kind
    else {
        panic!()
    };
    let StatementKind::Assignment {
        timing: Some(range),
        event_control: Some(event),
        nonblocking: true,
        ..
    } = syntax.statement(statements[0]).kind
    else {
        panic!()
    };
    assert!(syntax.token_text(range).starts_with("repeat(2)@"));
    let TimingControlKind::Event(EventControl::Items(events)) = &syntax.timing_control(event).kind
    else {
        panic!()
    };
    assert_eq!(events[0].edge, TimingEdge::Pos);
    assert!(events[0].iff.is_some());
    assert_eq!(
        syntax.statement(root).kind.children().collect::<Vec<_>>(),
        *items
    );
}

#[test]
fn deeply_nested_opaque_code_blocks_parse_clone_and_drop_without_native_stack_recursion() {
    let source = format!(
        "module M;initial {}x=1;{}endmodule",
        "randsequence(main)main:{".repeat(10_000),
        "};endsequence ".repeat(10_000)
    );
    let syntax = parse(&source);
    let copy = syntax.clone();
    assert_eq!(syntax.procedures.len(), 1);
    drop(syntax);
    drop(copy);
}

fn default<'a>(syntax: &'a Declarations, parameter: &Parameter) -> &'a Expr {
    let ParameterKind::Value {
        default: Some(value),
        ..
    } = &parameter.kind
    else {
        panic!("expected a default value")
    };
    syntax.expression(*value)
}

fn parameter_type<'a>(syntax: &'a Declarations, parameter: &Parameter) -> &'a DataType {
    let ParameterKind::Type { default: Some(id) } = parameter.kind else {
        panic!("expected type default")
    };
    syntax.data_type(id)
}

#[test]
fn expression_precedence_and_associativity_are_structural() {
    let syntax = parse("module M #(parameter A=-2**3**2, B=1+2*3<<4, C=a?b:c?d:e); endmodule");
    let parameters = syntax.units[0].parameters.as_ref().unwrap();
    let ExprKind::Binary {
        left,
        operator,
        right,
    } = &default(&syntax, &parameters[0]).kind
    else {
        panic!()
    };
    assert_eq!(operator, "**");
    assert!(matches!(syntax.expression(*right).kind,ExprKind::Literal(ref value) if value=="2"));
    let ExprKind::Binary { left, operator, .. } = &syntax.expression(*left).kind else {
        panic!()
    };
    assert_eq!(operator, "**");
    assert!(
        matches!(syntax.expression(*left).kind,ExprKind::Unary {ref operator,..} if operator=="-")
    );
    let ExprKind::Binary { left, operator, .. } = &default(&syntax, &parameters[1]).kind else {
        panic!()
    };
    assert_eq!(operator, "<<");
    assert!(
        matches!(syntax.expression(*left).kind,ExprKind::Binary {ref operator,..} if operator=="+")
    );
    assert!(
        matches!(&default(&syntax, &parameters[2]).kind,ExprKind::Conditional {else_expression,..} if matches!(syntax.expression(*else_expression).kind,ExprKind::Conditional {..}))
    );
}

#[test]
fn types_dimensions_and_local_parameter_inheritance_are_owned() {
    let syntax = parse(
        "module M #(parameter type T=logic[7:0], localparam type L=T, type K=int, parameter T A[2]='{1,2})(input T [1:0] x, y, Bus.master bus, interface.slave generic); endmodule",
    );
    let unit = &syntax.units[0];
    assert_eq!(
        unit.overridable_parameters()
            .map(|p| p.name.value())
            .collect::<Vec<_>>(),
        ["T", "A"]
    );
    let parameters = unit.parameters.as_ref().unwrap();
    assert!(parameters[1].local && parameters[2].local);
    assert!(
        parameter_type(&syntax, &parameters[0])
            .packed_dimensions
            .len()
            == 1
    );
    assert_eq!(
        syntax
            .data_type(unit.ports[0].data_type.unwrap())
            .packed_dimensions
            .len(),
        1
    );
    assert_eq!(unit.ports[1].data_type, unit.ports[0].data_type);
    assert_eq!(
        unit.interface_references(&syntax)
            .map(Name::value)
            .collect::<Vec<_>>(),
        ["Bus"]
    );
    assert!(
        matches!(&syntax.data_type(unit.ports[3].data_type.unwrap()).kind,DataTypeKind::Interface{name:None,modport:Some(name)} if name.value()=="slave")
    );
}

#[test]
fn casts_patterns_structs_enums_and_real_literals_keep_nodes() {
    let syntax = parse(
        "module M #(parameter type T=struct packed {logic [3:0] a; int b;}, type E=enum logic[1:0]{ZERO,ONE=1}, parameter real R=1.25e-3, parameter int P[2]='{default:int'(f(1,2))}, Q[2]='{2{3}}); endmodule",
    );
    let parameters = syntax.units[0].parameters.as_ref().unwrap();
    assert!(
        matches!(&parameter_type(&syntax,&parameters[0]).kind,DataTypeKind::Struct{fields,..} if fields.len()==2)
    );
    assert!(
        matches!(&parameter_type(&syntax,&parameters[1]).kind,DataTypeKind::Enum{items,..} if items.len()==2)
    );
    assert!(
        matches!(&default(&syntax, &parameters[2]).kind,ExprKind::Literal(value) if value=="1.25e-3")
    );
    assert!(
        matches!(&default(&syntax, &parameters[3]).kind,ExprKind::Pattern{items,..} if items[0].key.is_some() && matches!(syntax.expression(items[0].value).kind,ExprKind::Cast{..}))
    );
    assert!(
        matches!(&default(&syntax, &parameters[4]).kind,ExprKind::Pattern{items,..} if matches!(syntax.expression(items[0].value).kind,ExprKind::Replication{..}))
    );
}

#[test]
fn direct_body_parameters_exclude_nested_scopes_and_header_localizes_them() {
    let syntax = parse(
        "module M; parameter A=1; function int f(); parameter HIDDEN=2; endfunction generate if(1) begin parameter G=3; end endgenerate if(1) parameter C=4; parameter B=5; module N; parameter X=6; endmodule endmodule module E #(); parameter L=7; endmodule",
    );
    assert_eq!(
        syntax.units[0]
            .overridable_parameters()
            .map(|p| p.name.value())
            .collect::<Vec<_>>(),
        ["A", "B"]
    );
    assert_eq!(
        syntax
            .unit(syntax.units[0].children[0])
            .overridable_parameters()
            .map(|p| p.name.value())
            .collect::<Vec<_>>(),
        ["X"]
    );
    assert_eq!(syntax.units[1].overridable_parameters().count(), 0);
    assert!(syntax.units[1].body_parameters[0].local);
}

#[test]
fn macro_headers_retain_original_spelling_and_expansion_chain() {
    let syntax =
        parse("`define PORT(W) input logic [W-1:0] payload\nmodule M(`PORT(8)); endmodule");
    let PortKind::Named(name) = &syntax.units[0].ports[0].kind else {
        panic!()
    };
    assert_eq!(name.value(), "payload");
    assert!(name.origin.expansion.is_some());
    let range = syntax.units[0].ports[0].tokens.range();
    assert!(syntax.tokens[range].iter().any(|token| token.text == "8"));
}

#[test]
fn nonansi_external_and_wildcard_units_are_explicit() {
    let syntax = parse(
        "extern module E(input int x); module N(a,.b({x,y})); output a; endmodule : N module W(.*); endmodule interface Bus; endinterface",
    );
    assert!(syntax.units[0].external);
    assert_eq!(syntax.units[1].port_style, PortStyle::NonAnsi);
    assert_eq!(syntax.units[1].end_label.as_ref().unwrap().value(), "N");
    assert_eq!(syntax.units[2].port_style, PortStyle::Wildcard);
    assert_eq!(syntax.units[3].kind, UnitKind::Interface);
}

#[test]
fn malformed_headers_fail_without_accepting_a_partial_header() {
    for code in [
        "module M #(parameter A=); endmodule",
        "module M(input logic [3:0 x); endmodule",
        "module M(input int x) endmodule",
        "module M;",
    ] {
        assert!(
            Declarations::parse(code, Path::new("bad.sv")).is_err(),
            "{code}"
        );
    }
    assert!(parse("").units.is_empty());
}

#[test]
fn compiler_directives_and_numeric_forms_preserve_header_boundaries() {
    let syntax = parse(
        "`timescale 1ns / 1ps module M #(parameter A=8 /*gap*/ 'h FF, B=2.5ns, parameter type V=virtual interface Bus.master)(input int x); endmodule",
    );
    let parameters = syntax.units[0].parameters.as_ref().unwrap();
    assert!(
        matches!(&default(&syntax, &parameters[0]).kind,ExprKind::Literal(value) if value=="8'hFF")
    );
    assert!(
        matches!(&default(&syntax, &parameters[1]).kind,ExprKind::Literal(value) if value=="2.5ns")
    );
    assert!(matches!(
        parameter_type(&syntax, &parameters[2]).kind,
        DataTypeKind::VirtualInterface { .. }
    ));
}

#[test]
fn deep_and_long_expressions_parse_clone_and_drop_on_the_default_stack() {
    let depth = 10_000;
    for expression in [
        format!("{}1{}", "(".repeat(depth), ")".repeat(depth)),
        format!("{}1", "!".repeat(depth)),
        vec!["1"; depth].join("->"),
        format!("{}1", "1?1:".repeat(depth)),
        vec!["1"; depth * 2].join("+"),
        format!("{}1{}", "f(".repeat(depth), ")".repeat(depth)),
        format!("{}1{}", "int'(".repeat(depth), ")".repeat(depth)),
        format!("{}1{}", "'{".repeat(depth), "}".repeat(depth)),
    ] {
        let syntax = parse(&format!("module M #(parameter A={expression}); endmodule"));
        assert!(syntax.expressions.len() >= depth);
        let copy = syntax.clone();
        assert_eq!(copy.expressions.len(), syntax.expressions.len());
        // Both the arena and its clone are dropped on this ordinary test thread.
    }
    let invalid = format!(
        "module M #(parameter A={}1{}); endmodule",
        "(".repeat(depth),
        ")".repeat(depth - 1)
    );
    assert!(Declarations::parse(&invalid, Path::new("deep-invalid.sv")).is_err());
}

#[test]
fn mintypmax_is_structural_and_keeps_semantic_rejection_downstream() {
    let syntax = parse("module M #(parameter A=1:2:3, B=(4:5:6)); endmodule");
    let parameters = syntax.units[0].parameters.as_ref().unwrap();
    assert!(matches!(
        default(&syntax, &parameters[0]).kind,
        ExprKind::MinTypMax { .. }
    ));
    let ExprKind::Group(inner) = default(&syntax, &parameters[1]).kind else {
        panic!()
    };
    assert!(matches!(
        syntax.expression(inner).kind,
        ExprKind::MinTypMax { .. }
    ));
}

#[test]
fn deeply_nested_types_and_units_use_flat_arenas() {
    let depth = 2_000;
    let syntax = parse(&format!(
        "module M #(parameter type T={}logic{}); endmodule",
        "struct packed {".repeat(depth),
        " value;}".repeat(depth)
    ));
    assert!(syntax.types.len() > depth);
    drop(syntax.clone());
    let boundary = parse(&format!(
        "module M #(parameter type T={}int{}); endmodule",
        "type(logic[$bits(".repeat(depth),
        "):0])".repeat(depth)
    ));
    assert!(boundary.types.len() > depth);
    let syntax = parse(&format!(
        "{}{}",
        (0..depth)
            .map(|i| format!("module M{i};"))
            .collect::<String>(),
        "endmodule ".repeat(depth)
    ));
    assert_eq!(syntax.all_units().count(), depth);
    drop(syntax.clone());
}

#[test]
fn const_ref_inheritance_and_body_scope_boundaries_are_explicit() {
    let syntax = parse(
        "module M(const ref int a,b); typedef interface class Forward; interface class C; parameter HIDDEN=1; endclass default clocking cb; parameter LOCAL=2; endmodule",
    );
    assert!(
        syntax.units[0]
            .ports
            .iter()
            .all(|port| port.constant && port.direction == Some(Direction::Ref))
    );
    assert_eq!(
        syntax.units[0]
            .body_parameters
            .iter()
            .map(|parameter| parameter.name.value())
            .collect::<Vec<_>>(),
        ["LOCAL"]
    );
}

#[test]
fn dpi_body_prototypes_keep_native_types_defaults_and_macro_origins() {
    let syntax = parse(
        "`define ARG input int unsigned value\nimport \"DPI-C\" function void unit_call(); package P; import \"DPI-C\" pure c_add = function shortint signed add(`ARG, input logic flag); endpackage module M; if(1) begin:g import \"DPI-C\" context task t(output byte q[] = '{1}); end export \"DPI-C\" c_export = function local_f; endmodule",
    );
    assert_eq!(syntax.dpi_declarations.len(), 4);
    let add = &syntax.dpi_declarations[1];
    assert_eq!(add.property, Some(DpiProperty::Pure));
    assert_eq!(add.c_name.as_ref().unwrap().value(), "c_add");
    assert_eq!(syntax.data_type(add.ports[0].data_type).signed, Some(false));
    assert!(
        add.ports[0]
            .name
            .as_ref()
            .unwrap()
            .origin
            .expansion
            .is_some()
    );
    assert_eq!(syntax.units[0].dpi_declarations.len(), 1);
    let task = &syntax.dpi_declarations[2];
    assert_eq!(task.kind, DpiKind::Task);
    assert_eq!(task.ports[0].dimensions.len(), 1);
    assert!(task.ports[0].default.is_some());
    assert!(!syntax.dpi_declarations[3].import);
}

#[test]
fn streaming_with_attaches_to_the_whole_stream_expression() {
    let syntax = parse("module M #(parameter P={<<{a+b with[3:0],c}}); endmodule");
    let parameters = syntax.units[0].parameters.as_ref().unwrap();
    let ExprKind::Streaming { expressions, .. } = &default(&syntax, &parameters[0]).kind else {
        panic!()
    };
    assert_eq!(expressions.len(), 2);
    let ExprKind::StreamSelect {
        expression,
        dimension,
    } = &syntax.expression(expressions[0]).kind
    else {
        panic!()
    };
    assert!(matches!(
        syntax.expression(*expression).kind,
        ExprKind::Binary { .. }
    ));
    assert!(matches!(dimension.kind, DimensionKind::Range { .. }));
}

#[test]
fn dpi_linkage_and_prototype_grammar_rejects_invalid_forms_in_every_scope() {
    for declaration in [
        r#"import "foreign" function int f();"#,
        r#"export "foreign" function f;"#,
        r#"export "DPI-C" pure function f;"#,
        r#"export "DPI-C" context task t;"#,
        r#"import "DPI-C" pure task t();"#,
        r#"export "DPI-C" function f();"#,
        r#"export "DPI-C" task t(input int x);"#,
        r#"import "DPI-C" c$name = function int f();"#,
        r#"import "DPI-C" \c_name = function int f();"#,
        r#"import "DPI-C" function int f(input int = 1);"#,
        r#"import "DPI-C" function int f(input int []);"#,
        r#"import "DPI-C" function int [3:0] f();"#,
        r#"import "DPI-C" function int f(input int [3:0] x);"#,
        r#"import "DPI-C" function int f(input void x);"#,
        r#"import "DPI-C" function real signed f();"#,
    ] {
        for source in [
            declaration.to_owned(),
            format!("module M; {declaration} endmodule"),
            format!("package P; {declaration} endpackage"),
            format!("module M; if(1) begin:g {declaration} end endmodule"),
        ] {
            let error =
                Declarations::parse(&source, Path::new("invalid-dpi.sv")).expect_err(&source);
            assert_eq!(error.kind, ParseErrorKind::MalformedHeader, "{source}");
        }
    }
}

#[test]
fn dpi_valid_but_semantically_unsupported_forms_remain_native_syntax() {
    for declaration in [
        r#"import "DPI" function int f;"#,
        r#"import "DPI-C" context function int f();"#,
        r#"import "DPI-C" context task t(input int x);"#,
        r#"export "DPI-C" c_name = function \local.name ;"#,
        r#"export "DPI-C" task t;"#,
        r#"import "DPI-C" function int f(input int, input named_type);"#,
        r#"import "DPI-C" function int f(const ref int x, output byte q[] = '{1});"#,
        r#"import "DPI-C" function int f(input logic [3:0]);"#,
        // implicit_data_type and the optional identifier can both be empty.
        r#"import "DPI-C" function int f(,);"#,
    ] {
        let syntax = parse(declaration);
        assert_eq!(syntax.dpi_declarations.len(), 1, "{declaration}");
    }
}

#[test]
fn dpi_prototype_bounds_keep_the_explicit_stack_contract() {
    let bound = format!("{}7{}", "(".repeat(10_000), ")".repeat(10_000));
    let syntax = parse(&format!(
        "module M; import \"DPI-C\" function int f(input logic[{bound}:0] value); endmodule"
    ));
    let port = &syntax.dpi_declarations[0].ports[0];
    assert_eq!(syntax.data_type(port.data_type).packed_dimensions.len(), 1);
    drop(syntax.clone());
}

#[test]
fn package_bindings_preserve_header_body_scope_and_macro_origins() {
    let syntax = parse(concat!(
        "`define USE(p) import p::T, p::*;\n",
        "import Root::item; package P; export *::*; endpackage ",
        "module M `USE(P) (); if(1) begin : g import P::T; end ",
        "import \"DPI-C\" function int f(); endmodule ",
        "package Q; export P::T, P::*; endpackage"
    ));
    assert_eq!(syntax.package_bindings.len(), 5);
    let module = &syntax.units[1];
    assert_eq!(module.imports.len(), 1);
    assert_eq!(module.package_bindings.len(), 2);
    let header = syntax.package_binding(module.package_bindings[0]);
    assert_eq!(header.kind, PackageBindingKind::Import);
    let PackageBindingItemKind::Named { package, name } = &header.items[0].kind else {
        panic!()
    };
    assert_eq!(package.spelling, "P");
    assert_eq!(name.spelling, "T");
    assert!(name.origin.expansion.is_some());
    assert!(matches!(
        header.items[1].kind,
        PackageBindingItemKind::Wildcard(_)
    ));
    assert!(matches!(
        syntax.package_bindings[1].items[0].kind,
        PackageBindingItemKind::All
    ));
    assert_eq!(syntax.dpi_declarations.len(), 1);

    let syntax = parse(r"package Q; import \p.q ::\t.x ; export \p.q ::*; endpackage");
    let PackageBindingItemKind::Named { package, name } = &syntax.package_bindings[0].items[0].kind
    else {
        panic!()
    };
    assert_eq!(package.spelling, r"\p.q");
    assert_eq!(name.spelling, r"\t.x");
}

#[test]
fn malformed_package_bindings_never_accept_a_partial_declaration() {
    for declaration in [
        "import p;",
        "import p::;",
        "import p::x,;",
        "import *::*;",
        "export *::* , p::x;",
        "export p::x, *::*;",
        "export *::x;",
        "import p::x endmodule",
        "export p::* unexpected;",
    ] {
        assert!(
            Declarations::parse(
                &format!("module M; {declaration} endmodule"),
                Path::new("invalid.sv")
            )
            .is_err(),
            "{declaration}"
        );
    }
}

#[test]
fn long_package_item_lists_clone_and_drop_without_recursive_ownership() {
    let items = (0..20_000)
        .map(|i| format!("p::item{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let syntax = parse(&format!("module M; import {items}; endmodule"));
    assert_eq!(syntax.package_bindings[0].items.len(), 20_000);
    drop(syntax.clone());
}

#[test]
fn modport_subroutine_imports_and_exports_are_not_package_bindings() {
    let syntax = parse("interface I; import p::T; modport m(import set, export get); endinterface");
    assert_eq!(syntax.package_bindings.len(), 1);
}

#[test]
fn atomic_body_declarations_finish_conditional_and_prototype_state() {
    for declaration in ["import p::*;", "import \"DPI-C\" function int f();"] {
        let syntax = parse(&format!(
            "module M; if(1) {declaration} parameter int N=1; \
             function int local_function(); localparam HIDDEN=2; endfunction endmodule"
        ));
        let names = syntax.units[0]
            .overridable_parameters()
            .map(|parameter| parameter.name.spelling.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["N"], "{declaration}");
    }
}

#[test]
fn typedef_nodes_preserve_forward_interface_and_aggregate_structure() {
    let syntax = parse(concat!(
        "typedef class C; typedef interface class I; typedef enum E; typedef F; ",
        "module M; typedef bus[2].word View; ",
        "typedef struct { rand bit a,b; randc byte c; int d; } Record; ",
        "typedef logic [7:0] Bytes[2]; endmodule"
    ));
    assert_eq!(syntax.type_aliases.len(), 7);
    assert!(matches!(
        syntax.type_aliases[0].target,
        TypeAliasTarget::Forward(Some(ForwardType::Class))
    ));
    assert!(matches!(
        syntax.type_aliases[1].target,
        TypeAliasTarget::Forward(Some(ForwardType::InterfaceClass))
    ));
    assert!(matches!(
        syntax.type_aliases[3].target,
        TypeAliasTarget::Forward(None)
    ));
    let TypeAliasTarget::InterfaceMember(reference) = syntax.type_aliases[4].target else {
        panic!()
    };
    assert!(matches!(
        syntax.expression(reference).kind,
        ExprKind::Member { .. }
    ));
    let TypeAliasTarget::DataType(data) = syntax.type_aliases[5].target else {
        panic!()
    };
    let DataTypeKind::Struct { fields, .. } = &syntax.data_type(data).kind else {
        panic!()
    };
    assert_eq!(fields.len(), 4);
    assert_eq!(fields[0].random, Some(RandomQualifier::Rand));
    assert_eq!(fields[1].random, Some(RandomQualifier::Rand));
    assert_eq!(fields[2].random, Some(RandomQualifier::Randc));
    assert_eq!(fields[3].random, None);
    assert_eq!(syntax.type_aliases[6].dimensions.len(), 1);
    assert_eq!(syntax.units[0].type_aliases.len(), 3);
}

#[test]
fn typedef_macro_origins_and_deep_type_bounds_survive_clone_and_drop() {
    let syntax =
        parse("`define WORD(name) typedef logic[7:0] name;\nmodule M; `WORD(Word) endmodule");
    assert!(syntax.type_aliases[0].name.origin.expansion.is_some());
    let bound = format!("{}7{}", "(".repeat(10_000), ")".repeat(10_000));
    let syntax = parse(&format!(
        "module M; typedef logic[{bound}:0] Word; endmodule"
    ));
    drop(syntax.clone());
    let nested = format!(
        "{}bit value;{}}} T;",
        "struct packed {".repeat(2000),
        "} value;".repeat(1999)
    );
    let syntax = parse(&format!("module M; typedef {nested} endmodule"));
    assert!(syntax.types.len() >= 2000);
    drop(syntax.clone());
}

#[test]
fn malformed_typedefs_fail_at_the_original_source() {
    for declaration in [
        "typedef ;",
        "typedef logic;",
        "typedef logic[7:] T;",
        "typedef struct { bit a } T;",
        "typedef logic T[2] extra;",
        "typedef bus[0].T ;",
    ] {
        assert!(
            Declarations::parse(
                &format!("module M; {declaration} endmodule"),
                Path::new("bad-typedef.sv")
            )
            .is_err(),
            "{declaration}"
        );
    }
}

#[test]
fn time_syntax_preserves_atomic_declarations_directives_and_expansion_origins() {
    let syntax = parse(concat!(
        "`define SETTING 10.00 ns\n",
        "`timescale 10ns / 1ps\n",
        "timeunit 1 ns; timeprecision 1 ps; ",
        "module M; timeprecision 1ps; timeunit `SETTING / 1ps; endmodule\n",
        "`resetall\n"
    ));
    assert_eq!(syntax.time_declarations.len(), 4);
    assert_eq!(syntax.units[0].time_declarations.len(), 2);
    let value = syntax.time_declarations[3].unit.as_ref().unwrap();
    assert_eq!(value.magnitude, "10.00");
    assert_eq!(value.unit, "ns");
    assert!(syntax.tokens[value.tokens.start].origin.expansion.is_some());
    assert_eq!(syntax.time_directives.len(), 2);
    let TimeDirectiveKind::Scale { unit, precision } = &syntax.time_directives[0].kind else {
        panic!()
    };
    assert_eq!((&*unit.magnitude, &*unit.unit), ("10", "ns"));
    assert_eq!((&*precision.magnitude, &*precision.unit), ("1", "ps"));
    assert!(matches!(
        syntax.time_directives[1].kind,
        TimeDirectiveKind::Reset
    ));
}

#[test]
fn malformed_time_syntax_never_consumes_following_source() {
    for declaration in [
        "timeunit;",
        "timeunit 1;",
        "timeprecision 1ns/1ps;",
        "timeunit 1ns/;",
        "timeprecision 1qq;",
        "timeunit 1ns extra;",
    ] {
        assert!(
            Declarations::parse(
                &format!("module M; {declaration} parameter int N=1; endmodule"),
                Path::new("bad-time.sv")
            )
            .is_err(),
            "{declaration}"
        );
    }
    for directive in ["`timescale 1ns 1ps", "`timescale 1ns/1"] {
        assert!(
            Declarations::parse(
                &format!("{directive} module M; endmodule"),
                Path::new("bad-directive.sv")
            )
            .is_err(),
            "{directive}"
        );
    }
}

#[test]
fn parameter_statements_retain_nested_scopes_and_direct_metadata_projection() {
    let source = r#"
        localparam ROOT=5;
        module M #(parameter WIDTH=3);
          parameter DIRECT=WIDTH+1;
          if(1) begin:g localparam G=ROOT*2; end
          function int f(); localparam H=7; return H; endfunction
          class C #(parameter X=8); localparam Z=X+1; endclass
        endmodule
        class Ignored #(parameter N=3); localparam K=4; endclass
    "#;
    let syntax = Declarations::parse(source, Path::new("parameter-statements.sv")).unwrap();
    let names = syntax
        .parameter_declarations
        .iter()
        .flat_map(|declaration| {
            declaration
                .parameters
                .iter()
                .map(|parameter| parameter.name.value())
        })
        .collect::<Vec<_>>();
    assert_eq!(names, ["ROOT", "DIRECT", "G", "H", "X", "Z", "N", "K"]);
    assert_eq!(syntax.units[0].parameter_declarations.len(), 4);
    assert_eq!(
        syntax.units[0]
            .body_parameters
            .iter()
            .map(|parameter| parameter.name.value())
            .collect::<Vec<_>>(),
        ["DIRECT"]
    );
    assert!(syntax.units[0].body_parameters[0].local);
    assert!(!syntax.parameter_declarations[1].parameters[0].local);
    for declaration in &syntax.parameter_declarations {
        let text = syntax.token_text(declaration.tokens);
        assert!(text.ends_with(';') || (text.starts_with('(') && text.ends_with(')')));
    }
}

#[test]
fn malformed_body_parameter_statement_cannot_consume_following_syntax() {
    for source in [
        "module M; if(1) begin localparam X=1 end endmodule",
        "module M; function int f(); localparam X=; endfunction endmodule",
        "localparam X=1 module M; endmodule",
    ] {
        assert!(
            Declarations::parse(source, Path::new("bad-parameter.sv")).is_err(),
            "{source}"
        );
    }
}

#[test]
fn assign_syntax_preserves_strength_delay_lists_and_expression_nodes() {
    let source = r#"
        `define VALUE(x) ((x)+1)
        module M(input logic a,b,output logic x,y);
          assign (strong1,pull0) #(1:2:3,4,5) x=`VALUE(a)*2,y=b?{a,b}:2'b00;
          initial begin assign x=a; end
        endmodule
    "#;
    let syntax = Declarations::parse(source, Path::new("assign-nodes.sv")).unwrap();
    assert_eq!(syntax.assign_statements.len(), 2);
    let statement = &syntax.assign_statements[0];
    let strength = statement.strength.as_ref().unwrap();
    assert_eq!((&*strength.first, &*strength.second), ("strong1", "pull0"));
    let delay = statement.delay.as_ref().unwrap();
    assert!(delay.parenthesized);
    assert_eq!(delay.values.len(), 3);
    assert!(matches!(
        syntax.expression(delay.values[0]).kind,
        ExprKind::MinTypMax { .. }
    ));
    assert_eq!(statement.assignments.len(), 2);
    assert!(
        matches!(&syntax.expression(statement.assignments[0].right).kind,ExprKind::Binary {operator,..} if operator=="*")
    );
    assert!(matches!(
        &syntax.expression(statement.assignments[1].right).kind,
        ExprKind::Conditional { .. }
    ));
    assert!(
        syntax.tokens[syntax
            .expression(statement.assignments[0].right)
            .tokens
            .range()]
        .iter()
        .any(|token| token.origin.expansion.is_some())
    );
    assert!(syntax.assign_statements[1].strength.is_none());
    assert!(syntax.assign_statements[1].delay.is_none());
}

#[test]
fn malformed_assign_syntax_stops_at_its_own_statement() {
    for source in [
        "module M; assign =1; endmodule",
        "module M; assign x=; endmodule",
        "module M; assign x=1,y=; endmodule",
        "module M; assign #(1,2,3,4) x=1; endmodule",
        "module M; assign x=1 endmodule",
    ] {
        assert!(
            Declarations::parse(source, Path::new("bad-assign.sv")).is_err(),
            "{source}"
        );
    }
}

#[test]
fn assignment_and_tagged_expressions_keep_valid_unsupported_syntax() {
    let syntax = Declarations::parse(
        "module M; assign a=(b+=1),c=tagged Value (b+1),d=tagged Empty; endmodule",
        Path::new("unsupported-expression-nodes.sv"),
    )
    .unwrap();
    let assignments = &syntax.assign_statements[0].assignments;
    assert!(
        matches!(&syntax.expression(assignments[0].right).kind,ExprKind::Assignment {operator,..} if operator=="+=")
    );
    assert!(
        matches!(&syntax.expression(assignments[1].right).kind,ExprKind::Tagged {member,value:Some(_)} if member.value()=="Value")
    );
    assert!(
        matches!(&syntax.expression(assignments[2].right).kind,ExprKind::Tagged {member,value:None} if member.value()=="Empty")
    );
    for source in [
        "module M; assign 7=8; endmodule",
        "module M; assign a=(1+=2); endmodule",
    ] {
        assert!(Declarations::parse(source, Path::new("bad-lvalue.sv")).is_err());
    }
}

#[test]
fn procedural_control_syntax_uses_flat_nodes_and_retains_nested_declarations() {
    let source = r#"
      module M(input logic a,b,output logic y);
        always_comb begin : body
          localparam K=3;
          if(a) y=b;
          else if(b) y=0;
          else begin : fallback y=1; end : fallback
          case(y) 0,1: y=~a; default: y=b; endcase
          for(int i=0;i<K;i++) y^=a;
          do y=b; while(a);
        end : body
        initial begin repeat(2) #1 y=0; wait(a) y=1; void'($bits(a)); end
      endmodule
    "#;
    let syntax = Declarations::parse(source, Path::new("procedural-nodes.sv")).unwrap();
    assert_eq!(syntax.procedures.len(), 2);
    assert_eq!(syntax.units[0].procedures.len(), 2);
    assert_eq!(syntax.parameter_declarations.len(), 1);
    assert!(syntax.units[0].body_parameters.is_empty());
    let StatementKind::Block {
        label,
        end_label,
        items,
    } = &syntax.statement(syntax.procedures[0].body).kind
    else {
        panic!("procedure block")
    };
    assert_eq!(label.as_ref().unwrap().value(), "body");
    assert_eq!(end_label.as_ref().unwrap().value(), "body");
    let StatementKind::Conditional {
        branches,
        otherwise,
    } = &syntax.statement(items[1]).kind
    else {
        panic!("conditional chain")
    };
    assert_eq!(branches.len(), 2);
    assert!(otherwise.is_some());
    assert!(syntax.statements.iter().any(|statement| matches!(
        statement.kind,
        StatementKind::Loop {
            kind: LoopSyntax::DoWhile(_),
            ..
        }
    )));
}

#[test]
fn raw_statement_leaves_reject_eof_with_unclosed_delimiters() {
    for body in ["force y=(a;", "let identity(x)=(x;", "C value=new(a;"] {
        let source = format!("module M;initial begin {body} end endmodule");
        assert!(
            Declarations::parse(&source, Path::new("unclosed-raw-statement.sv")).is_err(),
            "{body}"
        );
    }
}

#[test]
fn wildcard_event_controls_are_contextual_attribute_delimiters() {
    for control in ["@*", "@ *", "@(*)", "@( *)", "@( * )"] {
        let syntax = Declarations::parse(
            &format!("module M; always {control} (* keep = 1 *) y=a; endmodule"),
            Path::new("wildcard.sv"),
        )
        .unwrap();
        let StatementKind::Timing { body, .. } = syntax.statement(syntax.procedures[0].body).kind
        else {
            panic!("timing control")
        };
        assert!(syntax.statement(body).attributes.is_some());
        assert!(matches!(
            syntax.statement(body).kind,
            StatementKind::Assignment { .. }
        ));
    }
}

#[test]
fn deep_procedural_syntax_parse_clone_and_drop_uses_the_default_stack() {
    for (prefix, suffix) in [
        ("begin ", " end"),
        ("if(a) ", ""),
        ("case(a) 0:", " endcase"),
        ("for(int i=0;;i++) ", ""),
    ] {
        let source = format!(
            "module M; initial {}y=1;{} endmodule",
            prefix.repeat(10_000),
            suffix.repeat(10_000)
        );
        let syntax = Declarations::parse(&source, Path::new("deep-statements.sv")).unwrap();
        assert_eq!(syntax.statements.len(), 10_001);
        drop(syntax.clone());
        drop(syntax);
    }
    let malformed = format!("module M; initial {}y=; endmodule", "if(a) ".repeat(10_000));
    assert!(Declarations::parse(&malformed, Path::new("deep-malformed.sv")).is_err());
}

#[test]
fn for_headers_retain_typed_groups_empty_clauses_and_ordered_updates() {
    let syntax = parse(
        "module M; initial begin for(var int i=a,j=b,logic[3:0] k=0;i<n;i+=2,j--,++k) y=i; for(a=0,b[2]=1;;a*=2) ; for(;;) ; end endmodule",
    );
    let headers = syntax
        .statements
        .iter()
        .filter_map(|statement| match &statement.kind {
            StatementKind::Loop {
                kind: LoopSyntax::For(header),
                ..
            } => Some(header),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(headers.len(), 3);
    let ForInitialization::Declarations(groups) = headers[0].initialization.as_ref().unwrap()
    else {
        panic!("typed initialization")
    };
    assert_eq!(groups.len(), 2);
    assert!(groups[0].var);
    assert_eq!(
        groups[0]
            .variables
            .iter()
            .map(|value| value.name.value())
            .collect::<Vec<_>>(),
        ["i", "j"]
    );
    assert_eq!(groups[1].variables[0].name.value(), "k");
    assert!(headers[0].condition.is_some());
    assert_eq!(headers[0].steps.len(), 3);
    assert!(matches!(headers[0].steps[0],ForStep::Assignment {ref operator,..} if operator=="+="));
    assert!(
        matches!(headers[1].initialization,Some(ForInitialization::Assignments(ref values)) if values.len()==2)
    );
    assert!(headers[1].condition.is_none());
    assert!(
        headers[2].initialization.is_none()
            && headers[2].condition.is_none()
            && headers[2].steps.is_empty()
    );
    for header in [
        "int i=;i<2;i++",
        "int i=0,j;i<2;i++",
        "i<=2;;",
        "int i=0;i<2;7+=1",
    ] {
        assert!(
            Declarations::parse(
                &format!("module M;initial for({header}) ; endmodule"),
                Path::new("bad-for.sv")
            )
            .is_err(),
            "{header}"
        );
    }
}

#[test]
fn local_variable_nodes_preserve_qualifiers_dimensions_and_macro_origins() {
    let syntax = parse(
        r#"
      `define ID \local.k
      module M;initial begin
        const var automatic logic[7:0] `ID [2]='{1,2},other[2]='{3,4};
        var signed[3:0] value=1;
        var implicit_value=0;
        static T[1:0] pair;
      end endmodule
    "#,
    );
    let declarations = syntax
        .statements
        .iter()
        .filter_map(|statement| match &statement.kind {
            StatementKind::Declaration(LocalDeclaration::Variables(value)) => Some(value),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(declarations.len(), 4);
    assert!(declarations[0].constant && declarations[0].var);
    assert_eq!(declarations[0].lifetime.as_deref(), Some("automatic"));
    assert_eq!(declarations[0].variables[0].name.value(), "local.k");
    assert!(declarations[0].variables[0].name.origin.expansion.is_some());
    assert_eq!(declarations[0].variables[0].dimensions.len(), 1);
    assert!(declarations[0].variables[0].value.is_some());
    let signed = syntax.data_type(declarations[1].data_type);
    assert!(matches!(signed.kind, DataTypeKind::Implicit));
    assert_eq!(signed.signed, Some(true));
    assert_eq!(signed.packed_dimensions.len(), 1);
    assert!(matches!(
        syntax.data_type(declarations[2].data_type).kind,
        DataTypeKind::Implicit
    ));
    assert_eq!(declarations[3].lifetime.as_deref(), Some("static"));
    for body in ["logic value=;", "logic value[);", "logic value,;"] {
        assert!(
            Declarations::parse(
                &format!("module M;initial begin {body} end endmodule"),
                Path::new("bad-local.sv")
            )
            .is_err(),
            "{body}"
        );
    }
}

#[test]
fn deep_local_declarations_clone_and_drop_through_flat_statement_arenas() {
    let source = format!(
        "module M;initial {} ;{} endmodule",
        "begin int x=0; ".repeat(10_000),
        "end ".repeat(10_000)
    );
    let syntax = parse(&source);
    assert_eq!(syntax.statements.len(), 20_001);
    drop(syntax.clone());
    drop(syntax);
}

#[test]
fn subroutine_headers_and_ordered_bodies_preserve_authored_structure() {
    let syntax = parse(
        r#"
      `define NAME sum
      package P;
        function automatic logic signed[7:0] `NAME(input logic[3:0] a,b=1+2,output var bit[7:0] result);
          const int bias=2; result=a+b+bias; return result;
        endfunction:sum
        task drive; input logic[3:0] a,b; output logic[3:0] c; c=a+b; endtask:drive
        function [3:0] f; f=3; endfunction
      endpackage
      class C; extern function int method(input int a); function new(int a);super.new(a);endfunction endclass
    "#,
    );
    assert_eq!(syntax.subroutines.len(), 5);
    let sum = &syntax.subroutines[0];
    assert_eq!(sum.kind, SubroutineKind::Function);
    assert_eq!(sum.name.value(), "sum");
    assert!(sum.name.origin.expansion.is_some());
    assert_eq!(sum.lifetime.as_deref(), Some("automatic"));
    let ports = sum.ports.as_ref().unwrap();
    assert_eq!(ports.len(), 3);
    assert_eq!(ports[1].name.as_ref().unwrap().value(), "b");
    assert!(
        matches!(syntax.expression(ports[1].default.unwrap()).kind,ExprKind::Binary{ref operator,..} if operator=="+")
    );
    assert!(ports[2].variable);
    assert_eq!(ports[2].direction, Some(Direction::Output));
    assert_eq!(sum.items.len(), 3);
    assert!(matches!(
        syntax.statement(sum.items[0]).kind,
        StatementKind::Declaration(LocalDeclaration::Variables(_))
    ));
    assert_eq!(sum.end_label.as_ref().unwrap().value(), "sum");
    assert_eq!(syntax.subroutines[1].port_declarations.len(), 2);
    assert!(syntax.subroutines[3].prototype);
    assert!(syntax.subroutines[3].items.is_empty());
    assert_eq!(syntax.subroutines[4].kind, SubroutineKind::Constructor);
    assert_eq!(syntax.units[0].subroutines.len(), 3);
}

#[test]
fn malformed_subroutines_do_not_consume_later_source_or_accept_partial_headers() {
    for code in [
        "function int f(input int);endfunction",
        "function int f(input int a=);endfunction",
        "function int f();return (1;endfunction",
        "task t;input a,;endtask",
        "function int f();f=1;",
        "function int f();begin endfunction",
    ] {
        assert!(
            Declarations::parse(code, Path::new("bad-subroutine.sv")).is_err(),
            "{code}"
        );
    }
    let syntax = parse(
        "class C;covergroup cg with function sample(input int x);endgroup function int f();return 1;endfunction endclass",
    );
    assert_eq!(syntax.subroutines.len(), 1);
    assert_eq!(syntax.subroutines[0].name.value(), "f");
    let syntax = parse(
        "checker C(input logic a);assert property(a);endchecker function int f();return 1;endfunction module M;endmodule",
    );
    assert_eq!(syntax.subroutines.len(), 1);
    assert_eq!(syntax.subroutines[0].name.value(), "f");
}

#[test]
fn deep_subroutine_types_and_bodies_clone_and_drop_on_the_default_stack() {
    let depth = 10_000;
    let source = format!(
        "module M;function logic[{}1{}:0] f(input int a={}1{});{}return a;{}endfunction endmodule",
        "(".repeat(depth),
        ")".repeat(depth),
        "(".repeat(depth),
        ")".repeat(depth),
        "begin ".repeat(depth),
        "end ".repeat(depth)
    );
    let syntax = parse(&source);
    assert_eq!(syntax.subroutines.len(), 1);
    assert_eq!(syntax.statements.len(), depth + 1);
    drop(syntax.clone());
    drop(syntax);
}

#[test]
fn body_data_items_preserve_boundaries_types_initializers_and_generate_scopes() {
    let syntax = parse(
        r#"
        module M(a);
          input logic a; wire logic n; Child c[2](); alias n=a; defparam c.P=1;
          (* keep=1 *) logic[7:0] x=1,y[3];
          if(1) begin:g int z; end:g
          int q;
          function int f(); int local_value; return local_value; endfunction
          generate for(genvar i=0;i<2;i++) begin:h int v; end endgenerate
          case(1) 1:logic selected; default:logic other; endcase
        endmodule
    "#,
    );
    let names = syntax
        .data_declarations
        .iter()
        .map(|declaration| {
            let DataDeclaration::Variables(declaration) = declaration else {
                panic!()
            };
            declaration
                .variables
                .iter()
                .map(|variable| variable.name.value())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            vec!["x", "y"],
            vec!["z"],
            vec!["q"],
            vec!["v"],
            vec!["selected"],
            vec!["other"]
        ]
    );
    assert_eq!(syntax.units[0].data_declarations.len(), 6);
    let DataDeclaration::Variables(first) = &syntax.data_declarations[0] else {
        panic!()
    };
    assert!(first.variables[0].value.is_some());
    assert_eq!(first.variables[1].dimensions.len(), 1);
    assert_eq!(syntax.data_type(first.data_type).packed_dimensions.len(), 1);
    assert!(syntax.token_text(first.tokens).starts_with("logic[7:0]"));
}

#[test]
fn body_alias_variables_and_constructed_forms_have_distinct_nodes() {
    let syntax = parse(
        "module M;typedef logic[7:0] T; T a=1,b[3]; C object=new(); int dynamic_array[]=new[3];endmodule",
    );
    assert!(
        matches!(&syntax.data_declarations[0],DataDeclaration::Variables(value) if value.variables.len()==2)
    );
    assert!(matches!(
        syntax.data_declarations[1],
        DataDeclaration::Constructed(_)
    ));
    assert!(matches!(
        syntax.data_declarations[2],
        DataDeclaration::Constructed(_)
    ));
}

#[test]
fn generate_body_items_retain_control_blocks_labels_and_singleton_arms() {
    let syntax = parse(
        "module M;generate if(A) begin:t logic x;end:t else if(B) logic y;else logic z;for(genvar i=0;i<2;i++) begin:g Child c();end:g case(X)1,2:begin:q int v;end default:;endcase endgenerate endmodule",
    );
    let BodyItemKind::Region(items) = &syntax.body_item(syntax.units[0].items[0]).kind else {
        panic!()
    };
    assert_eq!(items.len(), 3);
    let BodyItemKind::Conditional {
        then_body,
        otherwise: Some(otherwise),
        ..
    } = &syntax.body_item(items[0]).kind
    else {
        panic!()
    };
    assert!(
        matches!(&syntax.body_item(*then_body).kind,BodyItemKind::Block{label:Some(name),end_label:Some(end),items,..}if name.value()=="t"&&end.value()=="t"&&items.len()==1)
    );
    assert!(matches!(
        syntax.body_item(*otherwise).kind,
        BodyItemKind::Conditional {
            otherwise: Some(_),
            ..
        }
    ));
    let BodyItemKind::Loop {
        declared,
        variable,
        step,
        body,
        ..
    } = &syntax.body_item(items[1]).kind
    else {
        panic!()
    };
    assert!(*declared);
    assert_eq!(variable.value(), "i");
    assert!(matches!(step, ForStep::Expression(_)));
    assert!(
        matches!(&syntax.body_item(*body).kind,BodyItemKind::Block{label:Some(name),..}if name.value()=="g")
    );
    let BodyItemKind::Case { arms, .. } = &syntax.body_item(items[2]).kind else {
        panic!()
    };
    assert_eq!(arms[0].labels.len(), 2);
    assert!(arms[1].labels.is_empty());
    assert!(arms[1].colon);
}

#[test]
fn deep_generate_bodies_parse_clone_and_drop_on_the_default_stack() {
    for blocks in [false, true] {
        let prefix = if blocks { "if(1) begin:g " } else { "if(1) " };
        let suffix = if blocks {
            "end ".repeat(10_000)
        } else {
            String::new()
        };
        let syntax = parse(&format!(
            "module M;{}logic x;{suffix}endmodule",
            prefix.repeat(10_000)
        ));
        assert_eq!(
            syntax.body_items.len(),
            if blocks { 20_001 } else { 10_001 }
        );
        assert_eq!(syntax.units[0].items.len(), 1);
        drop(syntax.clone());
        drop(syntax);
    }
}

#[test]
fn deep_generate_blocks_declare_implicit_nets_on_the_default_stack() {
    let syntax = parse(&format!(
        "module M;logic a;{}assign x=a;{}endmodule",
        "if(1) begin:g ".repeat(10_000),
        "end ".repeat(10_000)
    ));
    let nets: Vec<_> = syntax
        .net_declarations
        .iter()
        .flat_map(|net| &net.variables)
        .map(|variable| variable.name.value())
        .collect();
    assert_eq!(nets, ["x"]);
}

#[test]
fn continuous_assignment_targets_declare_implicit_nets_in_their_scope() {
    let syntax = parse(
        "module M(input a);wire d;assign {p,q[0]}=a;assign d=a;\
         if(1) begin:g assign w=a;assign p=a; end endmodule",
    );
    let names = |items: &[BodyItemId]| -> Vec<String> {
        items
            .iter()
            .filter_map(|&item| match syntax.body_item(item).kind {
                BodyItemKind::Net(id) => Some(
                    syntax.net_declarations[id.0].variables[0]
                        .name
                        .value()
                        .to_string(),
                ),
                _ => None,
            })
            .collect()
    };
    let unit = &syntax.units[0];
    assert_eq!(names(&unit.items), ["p", "q", "d"]);
    let block = unit
        .items
        .iter()
        .find_map(|&item| match &syntax.body_item(item).kind {
            BodyItemKind::Conditional { then_body, .. } => Some(*then_body),
            _ => None,
        })
        .unwrap();
    let BodyItemKind::Block { items, .. } = &syntax.body_item(block).kind else {
        panic!("generate block");
    };
    assert_eq!(names(items), ["w"]);
}

#[test]
fn ordinary_net_declarations_retain_qualifiers_types_dimensions_and_values() {
    let code = "module M;wire signed[7:0] a[2:1],b=3+2;trireg(small)scalared[3:0]c;tri(strong0,weak1)vectored logic[4:0]#(1:2:3,4,5)d;wire T[3:0]#1 e;input wire p;C child();if(1)begin:g uwire bit q;end endmodule interface I;tri t;endinterface";
    let native = Declarations::parse(code, Path::new("nets.sv")).unwrap();
    assert_eq!(native.net_declarations.len(), 6);
    let a = &native.net_declarations[0];
    assert_eq!(a.net_type, "wire");
    let ty = native.data_type(a.data_type);
    assert!(matches!(ty.kind, DataTypeKind::Implicit));
    assert_eq!(ty.signed, Some(true));
    assert_eq!(ty.packed_dimensions.len(), 1);
    assert_eq!(a.variables.len(), 2);
    assert_eq!(a.variables[0].dimensions.len(), 1);
    assert!(a.variables[1].value.is_some());
    assert!(
        matches!(native.net_declarations[1].strength, Some(NetStrength::Charge { ref name, .. }) if name == "small")
    );
    let d = &native.net_declarations[2];
    assert_eq!(d.vector.as_deref(), Some("vectored"));
    assert!(matches!(d.strength, Some(NetStrength::Drive(_))));
    assert_eq!(d.delay.as_ref().unwrap().values.len(), 3);
    assert_eq!(native.units[0].net_declarations.len(), 5);
    for code in [
        "module M;wire a=;endmodule",
        "module M;wire[7:0 a;endmodule",
        "module M;tri(small,large)a;endmodule",
        "module M;wire#(1,2,3,4)a;endmodule",
    ] {
        assert!(
            Declarations::parse(code, Path::new("bad-net.sv")).is_err(),
            "{code}"
        );
    }
}

#[test]
fn instance_syntax_retains_parameters_arrays_and_connection_forms() {
    let native = Declarations::parse("module M;C#(.N(1:2:3),.T(logic[7:0]),.E())a[3:0](.x(foo+1),.y,.z(),.*),b(.x(2));D#(int,4)c(,(* keep=1 *)foo,);typedef int T;T v[2];and gate_(a,b,c);modport mp(input a);if(1)C g();endmodule", Path::new("native-instances.sv")).unwrap();
    assert_eq!(native.instantiations.len(), 3);
    let c = &native.instantiations[0];
    assert_eq!(c.name.spelling, "C");
    assert_eq!(c.parameters.as_ref().unwrap().len(), 3);
    let arguments = c.parameters.as_ref().unwrap();
    assert!(matches!(
        native.expression(arguments[0].value.unwrap()).kind,
        ExprKind::MinTypMax { .. }
    ));
    assert!(matches!(
        native.expression(arguments[1].value.unwrap()).kind,
        ExprKind::Type(_)
    ));
    assert!(arguments[2].value.is_none());
    assert_eq!(c.instances.len(), 2);
    assert_eq!(c.instances[0].dimensions.len(), 1);
    assert!(matches!(
        c.instances[0].connections[0].kind,
        InstanceConnectionKind::Named {
            value: ConnectionValue::Expression(_),
            ..
        }
    ));
    assert!(matches!(
        c.instances[0].connections[1].kind,
        InstanceConnectionKind::Named {
            value: ConnectionValue::Implicit,
            ..
        }
    ));
    assert!(matches!(
        c.instances[0].connections[2].kind,
        InstanceConnectionKind::Named {
            value: ConnectionValue::Empty,
            ..
        }
    ));
    assert!(matches!(
        c.instances[0].connections[3].kind,
        InstanceConnectionKind::Wildcard
    ));
    assert_eq!(native.instantiations[1].instances[0].connections.len(), 3);
    for code in [
        "module M;C x(.a(1),2);endmodule",
        "module M;C#(1,.N(2))x();endmodule",
        "module M;C x(.a(;endmodule",
        "module M;C x[3:0](1,,;endmodule",
    ] {
        assert!(
            Declarations::parse(code, Path::new("bad-instance.sv")).is_err(),
            "{code}"
        );
    }
}

#[test]
fn timing_controls_have_typed_delay_events_edges_and_iff_operands() {
    let syntax = parse(
        "module M;initial begin #12; #(1:2+3:4); #tick; @done; @(*); @((posedge a or negedge b),edge c,d iff en,((a+b)&&c)); end endmodule",
    );
    assert_eq!(syntax.timing_controls.len(), 6);
    assert!(matches!(
        syntax.timing_controls[0].kind,
        TimingControlKind::Delay {
            parenthesized: false,
            ..
        }
    ));
    let TimingControlKind::Delay {
        value,
        parenthesized: true,
    } = syntax.timing_controls[1].kind
    else {
        panic!()
    };
    let ExprKind::Group(value) = syntax.expression(value).kind else {
        panic!()
    };
    assert!(matches!(
        syntax.expression(value).kind,
        ExprKind::MinTypMax { .. }
    ));
    assert!(matches!(
        syntax.timing_controls[3].kind,
        TimingControlKind::Event(EventControl::Named(_))
    ));
    assert!(matches!(
        syntax.timing_controls[4].kind,
        TimingControlKind::Event(EventControl::Wildcard)
    ));
    let TimingControlKind::Event(EventControl::Items(items)) = &syntax.timing_controls[5].kind
    else {
        panic!()
    };
    assert_eq!(
        items.iter().map(|item| item.edge).collect::<Vec<_>>(),
        [
            TimingEdge::Pos,
            TimingEdge::Neg,
            TimingEdge::Both,
            TimingEdge::Any,
            TimingEdge::Any
        ]
    );
    assert!(items[3].iff.is_some());
    let mut expression = items[4].expression;
    while let ExprKind::Group(inner) = syntax.expression(expression).kind {
        expression = inner;
    }
    assert!(
        matches!(&syntax.expression(expression).kind,ExprKind::Binary{operator,..}if operator=="&&")
    );
    let control = &syntax.timing_controls[5];
    assert!(items.iter().all(
        |item| item.tokens.start > control.tokens.start && item.tokens.end < control.tokens.end
    ));
}
#[test]
fn malformed_timing_operands_do_not_become_opaque_acceptance() {
    for control in [
        "@(posedge)",
        "@(a or)",
        "@(a iff)",
        "#()",
        "@((a or b)",
        "@(a,)",
    ] {
        let source = format!("module M;initial {control};endmodule");
        assert!(
            Declarations::parse(&source, Path::new("bad-timing.sv")).is_err(),
            "{control}"
        );
    }
}
#[test]
fn deep_event_groups_unions_and_delay_expressions_use_flat_arenas() {
    let depth = 10_000;
    for event in [
        format!("{}a{}", "(".repeat(depth), ")".repeat(depth)),
        format!(
            "{}posedge a or negedge b{}",
            "(".repeat(depth),
            ")".repeat(depth)
        ),
        std::iter::repeat_n("posedge a", depth)
            .collect::<Vec<_>>()
            .join(","),
    ] {
        let syntax = parse(&format!("module M;initial @({event});endmodule"));
        let TimingControlKind::Event(EventControl::Items(items)) = &syntax.timing_controls[0].kind
        else {
            panic!()
        };
        assert_eq!(
            items.len(),
            if event.contains(',') {
                depth
            } else if event.contains("negedge") {
                2
            } else {
                1
            }
        );
        let copied = syntax.clone();
        drop(syntax);
        drop(copied);
    }
    let syntax = parse(&format!(
        "module M;initial #({}1{});endmodule",
        "(".repeat(depth),
        ")".repeat(depth)
    ));
    drop(syntax.clone());
    drop(syntax);
}

#[test]
fn foreach_headers_retain_native_array_paths_and_optional_index_slots() {
    for (array, expected) in [
        ("a", vec![None, Some("j"), None]),
        ("pkg::a", vec![Some("i"), None]),
        ("$unit::a", vec![Some("i"), None]),
        ("$root.top.g[1+2][3].a", vec![Some("i"), None]),
        ("C#(int,4)::a", vec![Some("i"), None]),
    ] {
        let variables = if expected.len() == 3 { ",j," } else { "i," };
        let syntax = parse(&format!(
            "module M;initial foreach({array}[{variables}]) begin end endmodule"
        ));
        let header = syntax
            .statements
            .iter()
            .find_map(|stmt| match &stmt.kind {
                StatementKind::Loop {
                    kind: LoopSyntax::Foreach(header),
                    ..
                } => Some(header),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            header
                .variables
                .iter()
                .map(|name| name.as_ref().map(|name| name.spelling.as_str()))
                .collect::<Vec<_>>(),
            expected
        );
        assert!(header.tokens.start <= syntax.expression(header.array).tokens.start);
        assert!(syntax.expression(header.array).tokens.end < header.tokens.end);
        assert!(matches!(
            syntax.expression(header.array).kind,
            ExprKind::Name(_) | ExprKind::Member { .. }
        ));
    }
    let syntax = parse("module M;initial foreach(a[]) begin end endmodule");
    let header = syntax
        .statements
        .iter()
        .find_map(|stmt| match &stmt.kind {
            StatementKind::Loop {
                kind: LoopSyntax::Foreach(header),
                ..
            } => Some(header),
            _ => None,
        })
        .unwrap();
    assert_eq!(header.variables, [None]);
}
#[test]
fn malformed_foreach_headers_have_no_opaque_acceptance_path() {
    for header in [
        "a",
        "a[i+1]",
        "a[f()]",
        "a[0][i]",
        "a[i,j].b[k]",
        "a[(i)]",
        "a[i",
        "a[i]tail",
    ] {
        let source = format!("module M;initial foreach({header}) begin end endmodule");
        assert!(
            Declarations::parse(&source, Path::new("bad-foreach.sv")).is_err(),
            "{header}"
        );
    }
}
#[test]
fn deep_foreach_statements_and_long_slot_lists_parse_clone_and_drop_on_default_stack() {
    let depth = 10_000;
    let syntax = parse(&format!(
        "module M;initial {}begin end endmodule",
        "foreach(a[i]) ".repeat(depth)
    ));
    assert_eq!(
        syntax
            .statements
            .iter()
            .filter(|stmt| matches!(
                stmt.kind,
                StatementKind::Loop {
                    kind: LoopSyntax::Foreach(_),
                    ..
                }
            ))
            .count(),
        depth
    );
    drop(syntax.clone());
    drop(syntax);
    let syntax = parse(&format!(
        "module M;initial foreach(a[{}]) begin end endmodule",
        ",".repeat(depth)
    ));
    let header = syntax
        .statements
        .iter()
        .find_map(|stmt| match &stmt.kind {
            StatementKind::Loop {
                kind: LoopSyntax::Foreach(header),
                ..
            } => Some(header),
            _ => None,
        })
        .unwrap();
    assert_eq!(header.variables.len(), depth + 1);
    drop(syntax.clone());
    drop(syntax);
}

#[test]
fn assertion_headers_distinguish_typed_immediate_predicates_from_property_boundaries() {
    let syntax = parse(
        "module M;initial begin assert(a+1);assume #0(b);cover final(c);assert property(@(posedge clk)a|->b);cover sequence(a##1b);expect(a);end endmodule",
    );
    let headers = syntax
        .statements
        .iter()
        .filter_map(|stmt| match &stmt.kind {
            StatementKind::Assertion { header, .. } => Some(header),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(headers.len(), 6);
    for (header, expected) in headers.iter().take(3).zip([
        None,
        Some(AssertionDeferral::Zero),
        Some(AssertionDeferral::Final),
    ]) {
        let AssertionHeader::Immediate {
            condition,
            deferred,
            tokens,
        } = header
        else {
            panic!()
        };
        assert_eq!(*deferred, expected);
        let expression = syntax.expression(*condition);
        assert!(tokens.start < expression.tokens.start && expression.tokens.end < tokens.end);
    }
    assert!(
        headers
            .iter()
            .skip(3)
            .all(|header| matches!(header, AssertionHeader::Unconverted(_)))
    );
}
#[test]
fn malformed_immediate_predicates_cannot_use_an_opaque_header_path() {
    for assertion in [
        "assert();",
        "assert(a+);",
        "assume(a,b);",
        "assert #1(a);",
        "assert #0 final(a);",
        "cover final;",
    ] {
        let source = format!("module M;initial {assertion} endmodule");
        assert!(
            Declarations::parse(&source, Path::new("bad-assertion.sv")).is_err(),
            "{assertion}"
        );
    }
}
#[test]
fn deep_immediate_assertion_actions_and_predicates_parse_clone_and_drop_on_default_stack() {
    let depth = 10_000;
    let syntax = parse(&format!(
        "module M;initial {};endmodule",
        "assert(a) ".repeat(depth)
    ));
    assert_eq!(
        syntax
            .statements
            .iter()
            .filter(|stmt| matches!(
                stmt.kind,
                StatementKind::Assertion {
                    header: AssertionHeader::Immediate { .. },
                    ..
                }
            ))
            .count(),
        depth
    );
    drop(syntax.clone());
    drop(syntax);
    let syntax = parse(&format!(
        "module M;initial assert({}a{});endmodule",
        "(".repeat(depth),
        ")".repeat(depth)
    ));
    drop(syntax.clone());
    drop(syntax);
}

#[test]
fn native_statement_children_follow_authored_order_and_reach_each_body_once() {
    let syntax = parse(
        "module M;initial begin if(a) begin y=1;end else case(a) 1:y=2;default:fork y=3;y=4;join_none endcase repeat(2) @(a) wait(a);assert(a) y=1;else y=2;wait_order(a,b) y=3;else y=4;end endmodule",
    );
    let mut pending = vec![syntax.procedures[0].body];
    let mut seen = vec![false; syntax.statements.len()];
    while let Some(id) = pending.pop() {
        assert!(!seen[id.0]);
        seen[id.0] = true;
        let statement = syntax.statement(id);
        let children = statement.kind.children().collect::<Vec<_>>();
        assert!(
            children
                .windows(2)
                .all(|pair| syntax.statement(pair[0]).tokens.start
                    < syntax.statement(pair[1]).tokens.start)
        );
        for child in &children {
            let tokens = syntax.statement(*child).tokens;
            assert!(statement.tokens.start < tokens.start && tokens.end <= statement.tokens.end);
        }
        pending.extend(children);
    }
    assert!(seen.into_iter().all(|seen| seen));
}

#[test]
fn opaque_class_members_keep_explicit_nested_owners_in_units_and_sources() {
    for source in [
        "module M;class C;function int f();return 1;endfunction class D;task t;endtask endclass:D endclass:C endmodule",
        "class C;function int f();return 1;endfunction class D;task t;endtask endclass:D endclass:C module M;endmodule",
    ] {
        let syntax = parse(source);
        let class = if source.starts_with("module") {
            syntax.units[0].items[0]
        } else {
            let SourceItemKind::Declaration(id) = syntax.source_items[0].kind else {
                panic!()
            };
            id
        };
        let BodyItemKind::Unconverted { items, .. } = &syntax.body_item(class).kind else {
            panic!()
        };
        assert_eq!(items.len(), 2);
        let BodyItemKind::Subroutine(f) = syntax.body_item(items[0]).kind else {
            panic!()
        };
        assert_eq!(syntax.subroutine(f).name.spelling, "f");
        let BodyItemKind::Unconverted { items, category } = &syntax.body_item(items[1]).kind else {
            panic!()
        };
        assert_eq!(category, "class");
        let BodyItemKind::Subroutine(t) = syntax.body_item(items[0]).kind else {
            panic!()
        };
        assert_eq!(syntax.subroutine(t).name.spelling, "t");
    }
}

#[test]
fn builtin_datatype_suffixes_are_validated_in_nested_and_ordinary_contexts() {
    for datatype in [
        "byte",
        "shortint",
        "int",
        "longint",
        "integer",
        "time",
        "shortreal",
        "real",
        "realtime",
        "string",
        "chandle",
        "event",
        "void",
    ] {
        let invalid = format!("{datatype} [3:0]");
        for source in [
            format!("module M; {invalid} x; endmodule"),
            format!("typedef {invalid} T;"),
            format!("typedef struct packed {{ {invalid} x; }} T;"),
            format!("module M #(parameter type T={invalid}); endmodule"),
            format!("module M; function {invalid} f(); endfunction endmodule"),
        ] {
            assert!(
                Declarations::parse(&source, Path::new("invalid-type.sv")).is_err(),
                "{source}"
            );
        }
    }
    for datatype in [
        "shortreal",
        "real",
        "realtime",
        "string",
        "chandle",
        "event",
        "void",
    ] {
        for signing in ["signed", "unsigned"] {
            let source = format!("typedef struct {{ {datatype} {signing} x; }} T;");
            assert!(
                Declarations::parse(&source, Path::new("invalid-type.sv")).is_err(),
                "{source}"
            );
        }
    }
}

#[test]
fn builtin_type_grammar_keeps_signing_vectors_and_unpacked_declarators() {
    for datatype in ["byte", "shortint", "int", "longint", "integer", "time"] {
        for signing in ["", "signed", "unsigned"] {
            parse(&format!("module M; {datatype} {signing} x[3:0]; endmodule"));
            parse(&format!("typedef struct {{ {datatype} {signing} x; }} T;"));
        }
    }
    for datatype in ["bit", "logic", "reg"] {
        for signing in ["", "signed", "unsigned"] {
            parse(&format!("typedef {datatype} {signing} [3:0][1:0] T;"));
            parse(&format!("typedef {datatype} {signing} [] T;"));
        }
    }
    for datatype in [
        "shortreal",
        "real",
        "realtime",
        "string",
        "chandle",
        "event",
    ] {
        parse(&format!("module M; {datatype} x[3:0]; endmodule"));
        parse(&format!("typedef struct {{ {datatype} x[3:0]; }} T;"));
    }
    parse("module M; function void f(); endfunction endmodule");
}

#[test]
fn malformed_attribute_lists_and_values_are_not_opaque_acceptance() {
    for attribute in [
        "(* *)",
        "(* , keep *)",
        "(* keep, *)",
        "(* keep other *)",
        "(* keep = *)",
        "(* keep = 1 + *)",
        "(* keep = (1 *)",
        "(* keep = 1; other = 2 *)",
        "(* keep = 1, = 2 *)",
        "(* keep = (x = 1) *)",
        "(* keep = x++ *)",
        "(* keep = 1 + (* nested *) 2 *)",
    ] {
        for source in [
            format!("{attribute} module M; endmodule"),
            format!("module M; {attribute} logic x; endmodule"),
            format!("module M; initial {attribute} x=1; endmodule"),
            format!("typedef struct {{ {attribute} int x; }} T;"),
            format!("module M #(parameter P=1 + {attribute} 2); endmodule"),
        ] {
            assert!(
                Declarations::parse(&source, Path::new("invalid-attribute.sv")).is_err(),
                "{source}"
            );
        }
    }
}

#[test]
fn attributes_keep_lists_symbols_strings_and_operator_placement() {
    let attribute = r#"(* flag, name="value", value=1+(2*3), symbol=unknown, escaped=\identifier.with.dots  *) (* again *)"#;
    for source in [
        format!("{attribute} module M; endmodule"),
        format!("module M; {attribute} logic x; endmodule"),
        format!("module M; initial {attribute} x=1; endmodule"),
        format!("typedef struct {{ {attribute} int x; }} T;"),
        format!("module M #(parameter P=1 + {attribute} 2); endmodule"),
    ] {
        parse(&source);
    }
}

#[test]
fn deeply_grouped_attribute_values_use_the_expression_worklist() {
    let value = format!("{}1{}", "(".repeat(10_000), ")".repeat(10_000));
    let syntax = parse(&format!("(* keep={value} *) module M; endmodule"));
    drop(syntax.clone());
    drop(syntax);
}

#[test]
fn datatype_contexts_reject_general_dimension_and_signing_grammar() {
    for datatype in [
        "logic [4]",
        "logic [*]",
        "logic [$]",
        "logic [0+:4]",
        "T signed",
        "type(x) unsigned",
        "type(x) [3:0]",
        "enum {A} signed",
        "struct signed {int x;}",
        "struct packed {int x;} unsigned",
        "struct tagged {int x;}",
        "struct soft {int x;}",
        "union soft signed {int x;}",
        "union soft tagged {int x;}",
        "union tagged soft {int x;}",
        "struct packed {}",
        "union {}",
        "enum real {A}",
        "enum struct packed {int x;} {A}",
        "enum logic[1:0][1:0] {A}",
        "enum P::T {A}",
        "enum {A[N]}",
        "enum {A[1+1]}",
        "enum {A[(2)]}",
        "enum {A[2][3]}",
        "enum {A[1.5]}",
        "enum {A[2ns]}",
        "enum {A['1]}",
        "enum {A[-1:2]}",
    ] {
        let source = format!("typedef {datatype} Result;");
        assert!(
            Declarations::parse(&source, Path::new("invalid-type-context.sv")).is_err(),
            "{source}"
        );
    }
}

#[test]
fn datatype_contexts_preserve_enum_ranges_and_aggregate_qualifiers() {
    for datatype in [
        "logic [3:0]",
        "T [3:0][]",
        "type(x)",
        "struct {int x;}",
        "struct packed signed {logic[3:0] x;}",
        "union packed unsigned {int x; bit[31:0] y;}",
        "union tagged {int x; void nothing;}",
        "union soft {int x; bit y;}",
        "union soft packed signed {int x; bit y;}",
        "enum {A}",
        "enum int unsigned {A}",
        "enum logic [3:0] {A}",
        "enum T [3:0] {A}",
        "enum {A[2], B[1:3], C[4'h2], D['d2]}",
    ] {
        parse(&format!("typedef {datatype} Result;"));
    }
}

#[test]
fn reserved_keywords_cannot_supply_declaration_or_expression_identifiers() {
    for source in [
        "module module; endmodule",
        "module M; logic always; endmodule",
        "typedef logic while;",
        "module M #(parameter if=1); endmodule",
        "module M; initial x=module; endmodule",
        "package P; import module::*; endpackage",
        "(* module *) module M; endmodule",
        "module M; function int if(); endfunction endmodule",
    ] {
        assert!(
            Declarations::parse(source, Path::new("invalid-keyword.sv")).is_err(),
            "{source}"
        );
    }
}

#[test]
fn keyword_directive_scopes_and_escaped_identifiers_preserve_names() {
    parse("module \\module ; logic \\always ; assign \\always =1; endmodule");
    parse("`begin_keywords \"1364-1995\" module logic; reg checker; endmodule `end_keywords");
    parse(
        "`begin_keywords \"1364-2001-noconfig\" module config; wire library; endmodule `end_keywords",
    );
    parse("`begin_keywords \"1800-2005\" module checker; logic nettype; endmodule `end_keywords");
    parse("`begin_keywords \"1800-2009\" module nettype; logic soft; endmodule `end_keywords");
    parse(
        "`begin_keywords \"1364-1995\" module logic; endmodule `begin_keywords \"1800-2005\" module checker; endmodule `end_keywords module logic; endmodule `end_keywords",
    );
    for version in [
        "1364-1995",
        "1364-2001-noconfig",
        "1364-2001",
        "1364-2005",
        "1800-2005",
        "1800-2009",
        "1800-2012",
        "1800-2017",
        "1800-2023",
    ] {
        parse(&format!(
            "`begin_keywords \"{version}\" module M; endmodule `end_keywords"
        ));
    }
    for source in [
        "`begin_keywords \"1800-2005\" module logic; endmodule `end_keywords",
        "`begin_keywords \"1800-2012\" module nettype; endmodule `end_keywords",
        "`begin_keywords \"1364-2001\" module config; endmodule `end_keywords",
        "`begin_keywords \"1364-1995\" module logic; endmodule `end_keywords module logic; endmodule",
        "`begin_keywords \"not-a-version\" module M; endmodule",
    ] {
        assert!(
            Declarations::parse(source, Path::new("invalid-keyword-scope.sv")).is_err(),
            "{source}"
        );
    }
    parse("class C; function new(); super.new(); endfunction:new endclass");
    parse("module M; initial begin x=this.x; y=super.x; z=a.and(); q=a.unique(); end endmodule");
}

#[test]
fn bare_identifiers_are_not_source_or_module_declarations() {
    parse("var source_value; module M; var value; initial begin var local_value; end endmodule");
    for source in [
        "bogus;",
        "\\bogus ;",
        "module M; nonsense; endmodule",
        "package P; nonsense; endpackage",
        "module M; if(1) begin:g nonsense; end endmodule",
    ] {
        assert!(
            Declarations::parse(source, Path::new("invalid-bare-declaration.sv")).is_err(),
            "{source}"
        );
    }
    parse("class C; typedef int T; T value; endclass module M; endmodule");
    parse("module M; checker C; T value; endchecker endmodule");
    parse("typedef int T; T value; module M; T x; endmodule");
}

#[test]
fn constant_grammar_operands_exclude_assignment_and_increment_operators() {
    for expression in [
        "(x=1)",
        "++x",
        "x++",
        "1+(x+=2)",
        "1 ? x++ : 2",
        "'{(x=1)}",
        "{x++}",
        "int'(x++)",
    ] {
        for source in [
            format!("module M #(parameter P={expression}); endmodule"),
            format!("module M; localparam P={expression}; endmodule"),
            format!("typedef logic [{expression}:0] T;"),
            format!("typedef enum {{A={expression}}} T;"),
            format!("(* keep={expression} *) module M; endmodule"),
        ] {
            assert!(
                Declarations::parse(&source, Path::new("invalid-constant-operator.sv")).is_err(),
                "{source}"
            );
        }
    }
    // The grammar of call arguments/type references remains ordinary expression,
    // independent of later constant evaluation or unsupported-feature checks.
    parse("module M #(parameter P=f(x++), Q=$bits((x=1)), R=type(x++)); endmodule");
    parse("(* keep=$bits((x=1)) *) module M; endmodule");
    parse("module M; initial begin x++; x=(y=1); end endmodule");
    parse("module M; Child #(.P((x=1))) child(); endmodule");
}

#[test]
fn class_headers_validate_names_parameters_and_inheritance_without_rejecting_unused_classes() {
    for header in [
        "class C",
        "virtual class C",
        "class automatic C",
        "class static C",
        "class C#(parameter N=3, type T=logic[2:0])",
        "class C extends P::Base#(.N(3))() implements P::I#(3), J",
        "class C extends Base(1, .x(2))",
        "interface class I#(type T=int) extends P::Base#(T), Other",
    ] {
        parse(&format!("{header}; endclass module M; endmodule"));
        parse(&format!("module M; {header}; endclass endmodule"));
    }
    for header in [
        "class",
        "class module",
        "class C extends",
        "class C extends int",
        "class C extends Base[3:0]",
        "class C implements",
        "class C implements int",
        "class C implements I,",
        "class C#(parameter N=)",
        "class C extends Base(1, .x()) implements I,",
        "interface class I extends A()",
    ] {
        for source in [
            format!("{header}; endclass module M; endmodule"),
            format!("module M; {header}; endclass endmodule"),
        ] {
            assert!(
                Declarations::parse(&source, Path::new("invalid-class-header.sv")).is_err(),
                "{source}"
            );
        }
    }
}

#[test]
fn numeric_literal_grammar_rejects_invalid_digits_and_empty_based_values() {
    for literal in [
        "8'hxyz", "2'b2", "8'o8", "8'd12x", "'h", "'sh", "'d_1", "8'b_1", "8'dxx", "8'dx1", "0'h1",
        "01'h1", "1e3ns",
    ] {
        let source = format!("module M #(parameter P={literal}); endmodule");
        assert!(
            Declarations::parse(&source, Path::new("invalid-number.sv")).is_err(),
            "{source}"
        );
    }
}

#[test]
fn numeric_literal_grammar_keeps_integer_real_time_and_macro_base_spellings() {
    for literal in [
        "0",
        "01",
        "1_000",
        "1_",
        "8'hff",
        "8 'h ff",
        "'shff",
        "8'sB 1x_z?",
        "'d x___",
        "'dx",
        "'dz",
        "'d?",
        "'b10?x",
        "'0",
        "'1",
        "'X",
        "'Z",
        "1.25",
        "1.2e-3",
        "1E+3",
        "1_2.3_4E5_6",
        "1ns",
        "1.25ns",
    ] {
        parse(&format!("module M #(parameter P={literal}); endmodule"));
    }
    parse("`define BASE 'h ff\nmodule M #(parameter P=8 `BASE); endmodule");
    for (definitions, literal) in [
        ("`define W 4", "5'd`W"),
        ("`define V ff", "8'h`V"),
        ("`define V 1x", "'sb`V"),
        ("`define V 0f", "8'h`V"),
    ] {
        parse(&format!(
            "{definitions}\nmodule M #(parameter P={literal}); endmodule"
        ));
    }
}

#[test]
fn expression_statements_require_calls_or_incdec_with_lvalues() {
    for statement in [
        "3;",
        "1+2;",
        "(f());",
        "f()();",
        "f[0];",
        "void'(3);",
        "int'(f());",
        "this;",
        "++3;",
        "3++;",
        "f()++;",
        "++(x+1);",
    ] {
        let source = format!("module M; initial {statement} endmodule");
        assert!(
            Declarations::parse(&source, Path::new("invalid-expression-statement.sv")).is_err(),
            "{source}"
        );
    }
    for statement in [
        "f;",
        "f();",
        "$finish;",
        "$display(\"x\");",
        "obj.f;",
        "obj[0].f();",
        "f().method();",
        "super.new();",
        "x++;",
        "++x;",
        "void'(f());",
        "void'(f);",
    ] {
        parse(&format!("module M; initial {statement} endmodule"));
    }
    for expression in ["++3", "3++", "f()++"] {
        let source = format!("module M; assign x={expression}; endmodule");
        assert!(
            Declarations::parse(&source, Path::new("invalid-incdec-lvalue.sv")).is_err(),
            "{source}"
        );
    }
}

#[test]
fn unknown_identifier_declarations_cannot_use_the_balanced_item_scanner() {
    for declaration in [
        "nonsense + ;",
        "nonsense();",
        "nonsense = 1;",
        "nonsense . field;",
        "\\nonsense + ;",
    ] {
        for source in [
            declaration.to_owned(),
            format!("module M; {declaration} endmodule"),
            format!("package P; {declaration} endpackage"),
            format!("module M; if(1) begin:g {declaration} end endmodule"),
        ] {
            assert!(
                Declarations::parse(&source, Path::new("invalid-unknown-item.sv")).is_err(),
                "{source}"
            );
        }
    }
    parse(
        "typedef int T; T value; module M; T x; Other child(); if(1) begin:g Other child(); end endmodule",
    );
}

#[test]
fn compiler_directive_operands_validate_values_without_consuming_following_source() {
    for net in [
        "wire", "tri", "tri0", "tri1", "wand", "triand", "wor", "trior", "trireg", "uwire", "none",
    ] {
        parse(&format!("`default_nettype {net} module M; endmodule"));
    }
    for pull in ["pull0", "pull1"] {
        parse(&format!(
            "`unconnected_drive {pull} module M; endmodule `nounconnected_drive"
        ));
    }
    for directive in [
        "`default_nettype nonsense",
        "`default_nettype 1",
        "`default_nettype \"wire\"",
        "`default_nettype",
        "`unconnected_drive nonsense",
        "`unconnected_drive tri0",
        "`unconnected_drive",
    ] {
        assert!(
            Declarations::parse(
                &format!("{directive}\nmodule M; endmodule"),
                Path::new("invalid-directive-value.sv")
            )
            .is_err(),
            "{directive}"
        );
    }
}

#[test]
fn declaration_dimensions_distinguish_variable_arrays_from_unpacked_constant_shapes() {
    for dimension in ["[0+:4]", "[x++]", "[(x=1)]", "[$:x++]"] {
        for source in [
            format!("typedef int T{dimension};"),
            format!("typedef struct {{ int x{dimension}; }} T;"),
            format!("module M; int x{dimension}; endmodule"),
            format!("module M; function int f(input int x{dimension}); endfunction endmodule"),
        ] {
            assert!(
                Declarations::parse(&source, Path::new("invalid-variable-dimension.sv")).is_err(),
                "{source}"
            );
        }
    }
    for dimension in ["[]", "[*]", "[$]", "[int]", "[$:3]"] {
        for source in [
            format!("module M #(parameter P{dimension}=1); endmodule"),
            format!("module M; wire w{dimension}; endmodule"),
            format!("module M; Other child{dimension}(); endmodule"),
        ] {
            assert!(
                Declarations::parse(&source, Path::new("invalid-unpacked-dimension.sv")).is_err(),
                "{source}"
            );
        }
        parse(&format!("typedef int T{dimension};"));
    }
    parse(
        "module M #(parameter P[2][3:0]='{default:0}); wire w[2]; Other child[3:0](); initial y=x[0+:4]; endmodule",
    );
}

#[test]
fn constructed_declarations_validate_types_dimensions_and_allocation_operands() {
    for declaration in [
        "int a[]=new[+];",
        "int a[]=new[];",
        "int a[]=new[2:0];",
        "int a[]=new[2](1,2);",
        "C x=new(+);",
        "C x=new other+;",
        "int a[0+:4][];",
        "int [3:0] a[];",
    ] {
        for source in [
            format!("module M; {declaration} endmodule"),
            format!("module M; initial begin {declaration} end endmodule"),
        ] {
            assert!(
                Declarations::parse(&source, Path::new("invalid-constructed-declaration.sv"))
                    .is_err(),
                "{source}"
            );
        }
    }
    for declaration in [
        "int a[];",
        "int a[]=new[2];",
        "int a[]=new[2](old_array);",
        "C x=new;",
        "C x=new();",
        "C x=new(1,.x(2));",
        "C x=new old_object;",
        "C x=P::C::new();",
    ] {
        let syntax = parse(&format!("module M; {declaration} endmodule"));
        assert!(
            matches!(syntax.data_declarations[0], DataDeclaration::Constructed(_)),
            "{declaration}"
        );
    }
}

#[test]
fn class_property_payloads_validate_while_opaque_nonproperty_items_keep_boundaries() {
    let members = "rand protected int x=1; static const int y=2; const protected static int z=3; C object=new old_object; int data[]=new[2]; constraint c { x > 0; } int after_constraint=1; extern constraint forward; nettype int N with resolver; covergroup cg; coverpoint x; endgroup int after_covergroup=2; virtual function int f(); return x; endfunction extern function int g(); function new(); super.new(); endfunction";
    parse(&format!("class C; {members} endclass module M; endmodule"));
    parse(&format!("module M; class C; {members} endclass endmodule"));
    for declaration in [
        "int x=+;",
        "logic[3] x;",
        "C object=new(+);",
        "invalid;",
        "T x[x++];",
        "(* keep=1+ *) int x;",
        "constraint c { x > 0; } int x=+;",
        "signed [3:0] x;",
    ] {
        for source in [
            format!("class C; {declaration} endclass module M; endmodule"),
            format!("module M; class C; {declaration} endclass endmodule"),
        ] {
            assert!(
                Declarations::parse(&source, Path::new("invalid-class-property.sv")).is_err(),
                "{source}"
            );
        }
    }
}

#[test]
fn constraint_declarations_validate_control_sets_ranges_and_prototypes() {
    for declaration in [
        "constraint c;",
        "extern constraint c;",
        "pure static constraint c;",
        "static constraint c {}",
        "constraint c { solve this.x, y[0] before z; soft x inside {[0:3],7}; unique {x,y[0],[1:3]}; disable soft this.x; }",
        "constraint c { x dist {1:=2, [2:4]:/3}; if(x) {y>0; z<3;} else y==0; foreach(this.array[i,]) array[i]>0; }",
        "constraint c { x -> { y>0; if(z) q>0; } a <-> b; a -> b -> c; soft a -> b; }",
    ] {
        parse(&format!(
            "class C; {declaration} int after=1; endclass module M; endmodule"
        ));
        parse(&format!(
            "module M; class C; {declaration} int after=1; endclass endmodule"
        ));
    }
    for declaration in [
        "constraint c {x==;}",
        "constraint c {solve x before ;}",
        "constraint c {solve x+1 before y;}",
        "constraint c {unique {};}",
        "constraint c {unique {[1]};}",
        "constraint c {x dist {};}",
        "constraint c {x dist {1:=};}",
        "constraint c {disable x;}",
        "constraint c {if() x;}",
        "constraint c {x -> {y+;}}",
        "constraint c {foreach(a[i+1]) x;}",
        "extern constraint c {}",
        "constraint c {if(x) {solve a before b;}}",
        "constraint c {;}",
        "constraint c {x dist {1,};}",
    ] {
        for source in [
            format!("class C; {declaration} endclass module M; endmodule"),
            format!("module M; class C; {declaration} endclass endmodule"),
        ] {
            assert!(
                Declarations::parse(&source, Path::new("invalid-constraint.sv")).is_err(),
                "{source}"
            );
        }
    }
}

#[test]
fn deep_constraint_sets_and_conditionals_use_native_worklists() {
    parse("class C; constraint c { `default_nettype none x>0; } endclass module M; endmodule");

    let depth = 10_000;
    for body in [
        format!("{}x>0;{}", "if(x){".repeat(depth), "}".repeat(depth)),
        format!("{}x>0;", "x -> ".repeat(depth)),
    ] {
        let syntax = parse(&format!(
            "class C; constraint c {{{body}}} endclass module M; endmodule"
        ));
        drop(syntax.clone());
    }
    parse("class C; function void f(); foreach(this.super.array[i]) x++; endfunction endclass");
}

#[test]
fn checker_formal_prefixes_validate_names_types_dimensions_and_directions() {
    for header in [
        "checker C",
        "checker C()",
        "checker C(input logic a=1, output bit b)",
        "checker C(a, input signed[3:0] b, P::T x[3])",
        "checker C(input property p=a |-> ##1 b, sequence s=(a ##1 b), untyped x=posedge clk)",
    ] {
        parse(&format!("{header}; endchecker module M; endmodule"));
        parse(&format!("module M; {header}; endchecker endmodule"));
    }
    for header in [
        "checker C(input +)",
        "checker C(input int)",
        "checker C(,)",
        "checker C(input logic a,)",
        "checker C(inout logic a)",
        "checker C(ref int a)",
        "checker C(input logic a=)",
        "checker C(input logic a[0+:4])",
        "checker C(input int[3:0] a)",
        "checker C(input logic a=(1])",
        "checker C(input logic a=1;)",
    ] {
        for source in [
            format!("{header}; endchecker module M; endmodule"),
            format!("module M; {header}; endchecker endmodule"),
        ] {
            assert!(
                Declarations::parse(&source, Path::new("invalid-checker-formal.sv")).is_err(),
                "{source}"
            );
        }
    }
}

#[test]
fn covergroup_headers_and_item_prefixes_validate_without_leaking_sampling_functions() {
    for coverage in [
        "covergroup cg; coverpoint x; endgroup",
        "covergroup cg; option: coverpoint x; type_option: cross a,b; endgroup",
        "covergroup cg(input int x) @(posedge clk or negedge rst); option.per_instance=1; type_option.weight=2; cp: coverpoint x iff(enable) { bins b[]={0,[1:3]}; bins transition=(1=>2[*3]); } cx: cross cp,other iff(enable) { bins all=binsof(cp) intersect {0}; function int helper(); typedef int T; return 1; endfunction } endgroup:cg",
        "covergroup cg with function sample(input int x); logic[3:0] cp: coverpoint x; endgroup",
        "covergroup cg @P::cb; cross a,b; endgroup",
        "covergroup cg @@(begin M.task_name or end P::C::f); coverpoint x; endgroup",
    ] {
        let source = parse(&format!(
            "class C; {coverage} endclass function int global_f(); return 1; endfunction module M; endmodule"
        ));
        assert_eq!(source.subroutines.len(), 1, "{coverage}");
        parse(&format!("module M; class C; {coverage} endclass endmodule"));
    }
    for coverage in [
        "covergroup ; endgroup",
        "covergroup cg with function wrong(); endgroup",
        "covergroup cg @(*); endgroup",
        "covergroup cg; coverpoint ; endgroup",
        "covergroup cg; cp: coverpoint x+; endgroup",
        "covergroup cg; cross x; endgroup",
        "covergroup cg; coverpoint x iff(); endgroup",
        "covergroup cg; option.weight=; endgroup",
        "covergroup cg; type_option.weight=x++; endgroup",
        "covergroup cg; logic[3] cp:coverpoint x; endgroup",
        "covergroup cg @@(begin); endgroup",
        "covergroup cg; nonsense; endgroup",
    ] {
        for source in [
            format!("{coverage} module M; endmodule"),
            format!("module M; class C; {coverage} endclass endmodule"),
        ] {
            assert!(
                Declarations::parse(&source, Path::new("invalid-coverage-prefix.sv")).is_err(),
                "{source}"
            );
        }
    }
}

#[test]
fn class_header_defaults_keep_captured_native_ownership() {
    for source in [
        "class C#(parameter P=3, type T=int); localparam Q=P+1; endclass module M; endmodule",
        "module M; class C#(parameter P=3, type T=int); localparam Q=P+1; endclass endmodule",
    ] {
        let syntax = parse(source);
        let class = if let SourceItemKind::Declaration(id) = syntax.source_items[0].kind {
            id
        } else {
            syntax.units[0].items[0]
        };
        let BodyItemKind::Unconverted { items, .. } = &syntax.body_item(class).kind else {
            panic!("class owner missing")
        };
        let BodyItemKind::Parameter(header) = syntax.body_item(items[0]).kind else {
            panic!("header parameter owner missing")
        };
        let header = syntax.parameter_declaration(header);
        assert_eq!(header.parameters.len(), 2);
        assert_eq!(header.parameters[0].name.value(), "P");
        assert!(matches!(
            header.parameters[0].kind,
            ParameterKind::Value {
                default: Some(_),
                ..
            }
        ));
        assert_eq!(syntax.units[0].parameters, None);
        assert_eq!(items.len(), 2);
    }
}

#[test]
fn constructed_declarations_retain_ordinary_sibling_operands() {
    let syntax = parse(
        "module M; int dynamic_array[]=new[2],ordinary=3; initial begin int local_array[]=new[4],local_value=7; end endmodule",
    );
    let DataDeclaration::Constructed(body) = &syntax.data_declarations[0] else {
        panic!()
    };
    assert_eq!(
        body.variables
            .iter()
            .map(|variable| variable.name.spelling.as_str())
            .collect::<Vec<_>>(),
        ["dynamic_array", "ordinary"]
    );
    assert!(
        matches!(&syntax.expression(body.variables[1].value.unwrap()).kind,ExprKind::Literal(value) if value=="3")
    );
    let local = syntax
        .statements
        .iter()
        .find_map(|statement| match &statement.kind {
            StatementKind::Declaration(LocalDeclaration::Constructed(local)) => Some(local),
            _ => None,
        })
        .unwrap();
    assert_eq!(local.variables[1].name.spelling, "local_value");
    assert!(
        matches!(&syntax.expression(local.variables[1].value.unwrap()).kind,ExprKind::Literal(value) if value=="7")
    );
}

#[test]
fn parameter_identity_survives_owner_metadata_clones_and_distinguishes_namesakes() {
    let syntax = parse(
        "module A #(parameter type T, parameter P=1); localparam Q=2; endmodule module B #(parameter type T); localparam Q=3; endmodule",
    );
    let mut ids = fxhash::FxHashSet::default();
    for unit in &syntax.units {
        for parameter in unit.parameters.iter().flatten() {
            assert!(ids.insert(parameter.id));
        }
        for parameter in &unit.body_parameters {
            assert!(ids.insert(parameter.id));
            let owned = syntax
                .parameter_declarations
                .iter()
                .flat_map(|declaration| &declaration.parameters)
                .find(|owned| owned.id == parameter.id)
                .unwrap();
            assert_eq!(owned, parameter);
        }
    }
    assert_eq!(ids.len(), 5);
}

#[test]
fn resetall_is_only_legal_outside_design_elements() {
    assert!(
        Declarations::parse(
            "`resetall\nmodule M; endmodule\n`resetall",
            Path::new("reset.sv")
        )
        .is_ok()
    );
    assert!(Declarations::parse("module M; `resetall\nendmodule", Path::new("reset.sv")).is_err());
}

#[test]
fn based_literal_expression_values_are_compact_without_losing_source_tokens() {
    let native = parse("`define W 4\nmodule M; initial x=5'd`W; endmodule");
    assert!(
        native.expressions.iter().any(
            |expression| matches!(&expression.kind, ExprKind::Literal(text) if text == "5'd4")
        )
    );
    for (authored, value) in [("'h x", "'hx"), ("8'd 6", "8'd6"), ("4 'h f", "4'hf")] {
        let source = format!("module M; initial x={authored}; endmodule");
        let native = parse(&source);
        assert!(native.expressions.iter().any(
            |expression| matches!(&expression.kind, ExprKind::Literal(text) if text == value)
        ));
        assert_eq!(
            native
                .tokens
                .iter()
                .map(|token| token.text.as_str())
                .collect::<String>(),
            source
        );
    }
}

#[test]
fn specparam_retains_its_body_item_boundary() {
    let native = parse("module M; specparam delay=50; endmodule");
    assert!(
        matches!(&native.body_item(native.units[0].items[0]).kind, BodyItemKind::Unconverted { category, .. } if category == "specparam")
    );
}

#[test]
fn module_deferred_assertions_do_not_become_final_procedures() {
    for keyword in ["assert", "assume", "cover"] {
        for deferral in ["#0", "final"] {
            let native = parse(&format!("module M; {keyword} {deferral} (1); endmodule"));
            assert!(native.procedures.is_empty());
            assert!(
                matches!(&native.body_item(native.units[0].items[0]).kind, BodyItemKind::Unconverted { category, .. } if category == keyword)
            );
        }
    }
}

#[test]
fn class_declarations_record_names_bases_properties_and_method_qualifiers() {
    for wrap in [
        ("module M;", "endmodule"),
        ("package P;", "endpackage"),
        ("", ""),
    ] {
        let syntax = parse(&format!(
            "{} virtual class B; int b; pure virtual function int f(); endclass \
             class C extends B(3); static int count = 1; local logic [3:0] x, y; \
             function new(int v); x = v; endfunction \
             virtual function int f(); return x; endfunction \
             static task t(); endtask extern function void g(); endclass:C {}",
            wrap.0, wrap.1
        ));
        let [base, class] = &syntax.classes[..] else {
            panic!("{wrap:?}: {:?}", syntax.classes)
        };
        assert_eq!(base.name.spelling, "B");
        assert!(base.virtual_class && base.base.is_none());
        assert!(base.methods[0].qualifiers.is_pure && base.methods[0].qualifiers.is_virtual);
        assert_eq!(class.name.spelling, "C");
        assert!(!class.virtual_class);
        let extends = class.base.as_ref().unwrap();
        assert_eq!(extends.arguments.as_ref().unwrap().len(), 1);
        assert_eq!(class.properties.len(), 2);
        assert!(class.properties[0].qualifiers.is_static);
        assert!(class.properties[1].qualifiers.local);
        assert_eq!(class.properties[1].declaration.variables.len(), 2);
        let methods: Vec<_> = class
            .methods
            .iter()
            .map(|method| {
                let subroutine = syntax.subroutine(method.subroutine);
                (subroutine.name.spelling.as_str(), method.qualifiers)
            })
            .collect();
        assert_eq!(methods.len(), 4);
        assert_eq!(methods[0].0, "new");
        assert!(methods[1].1.is_virtual && !methods[1].1.is_static);
        assert!(methods[2].1.is_static);
        assert!(methods[3].1.is_extern);
        let text = syntax.token_text(class.tokens);
        assert!(
            text.starts_with("class C") && text.ends_with("endclass"),
            "{text}"
        );
    }
}

#[test]
fn class_random_qualifiers_and_constraints_are_preserved() {
    let syntax = parse(
        "class C; rand bit x; randc bit y; extern constraint limits; endclass module M; endmodule",
    );
    let class = &syntax.classes[0];
    assert!(class.has_constraints);
    assert!(class.properties[0].qualifiers.rand);
    assert!(!class.properties[0].qualifiers.randc);
    assert!(class.properties[1].qualifiers.randc);
    assert!(!class.properties[1].qualifiers.rand);
}
