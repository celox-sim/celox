use super::*;
#[test]
fn source_items_retain_native_payload_order_attributes_and_macro_origins() {
    let tree=Declarations::parse("`define V(x) ((x)+1)\ntypedef logic[3:0]T;(* keep=1 *) T g[2]=`V(5);module M;endmodule function automatic int f();return 1;endfunction",Path::new("source-items.sv")).unwrap();
    assert_eq!(tree.source_items.len(), 4);
    let SourceItemKind::Declaration(alias) = tree.source_items[0].kind else {
        panic!()
    };
    assert!(matches!(
        tree.body_item(alias).kind,
        BodyItemKind::TypeAlias(_)
    ));
    let SourceItemKind::Declaration(global) = tree.source_items[1].kind else {
        panic!()
    };
    let BodyItemKind::Data(data) = tree.body_item(global).kind else {
        panic!()
    };
    let DataDeclaration::Variables(data) = tree.data_declaration(data) else {
        panic!()
    };
    assert_eq!(data.variables[0].name.spelling, "g");
    assert_eq!(data.variables[0].dimensions.len(), 1);
    assert!(
        tree.token_text(tree.source_items[1].tokens)
            .starts_with("(* keep=1 *)")
    );
    let value = tree.expression(data.variables[0].value.unwrap());
    assert!(
        tree.tokens[value.tokens.range()]
            .iter()
            .any(|token| token.origin.expansion.is_some())
    );
    let SourceItemKind::Unit(unit) = tree.source_items[2].kind else {
        panic!()
    };
    assert_eq!(tree.source_unit(unit).name.spelling, "M");
    assert_eq!(
        tree.all_units()
            .map(|unit| unit.name.spelling.as_str())
            .collect::<Vec<_>>(),
        ["M"]
    );
    let SourceItemKind::Declaration(function) = tree.source_items[3].kind else {
        panic!()
    };
    assert!(matches!(
        tree.body_item(function).kind,
        BodyItemKind::Subroutine(_)
    ));
}
#[test]
fn opaque_source_scopes_keep_metadata_without_leaking_global_declarations() {
    let tree=Declarations::parse("class C;int member;typedef int T;function void method();endfunction endclass:C int g=1;module M;endmodule",Path::new("source-scopes.sv")).unwrap();
    assert_eq!(tree.source_items.len(), 3);
    let SourceItemKind::Declaration(class) = tree.source_items[0].kind else {
        panic!()
    };
    assert!(
        matches!(&tree.body_item(class).kind,BodyItemKind::Unconverted{category,..} if category=="class")
    );
    assert!(
        tree.token_text(tree.source_items[0].tokens)
            .ends_with("endclass:C")
    );
    assert_eq!(tree.data_declarations.len(), 1);
    assert_eq!(tree.type_aliases.len(), 1);
    assert_eq!(tree.subroutines.len(), 1);
}
#[test]
fn source_declarations_reject_partial_ranges_and_preserve_prototype_boundaries() {
    for source in [
        "int g=",
        "int g[2;module M;endmodule",
        "class C;int member;",
        "bind M N n(;",
    ] {
        assert!(
            Declarations::parse(source, Path::new("bad-source.sv")).is_err(),
            "{source}"
        );
    }
    let tree = Declarations::parse(
        "extern function int f(input int x);int g=1;module M;endmodule",
        Path::new("source-prototype.sv"),
    )
    .unwrap();
    assert_eq!(tree.source_items.len(), 3);
    assert!(tree.subroutines[0].prototype);
    assert!(
        tree.token_text(tree.source_items[0].tokens)
            .starts_with("extern function")
    );
}
#[test]
fn deep_source_scopes_and_global_expression_clone_and_drop_on_the_default_stack() {
    let source = format!(
        "{}{}int g={}1{};module M;endmodule",
        "class C;".repeat(10_000),
        "endclass ".repeat(10_000),
        "(".repeat(10_000),
        ")".repeat(10_000)
    );
    let tree = Declarations::parse(&source, Path::new("deep-source.sv")).unwrap();
    assert_eq!(tree.source_items.len(), 3);
    assert_eq!(tree.data_declarations.len(), 1);
    let copy = tree.clone();
    drop(tree);
    drop(copy);
}

#[test]
fn opaque_covergroup_sampling_header_does_not_consume_the_next_global_function() {
    let tree=Declarations::parse("covergroup cg with function sample(int x);coverpoint x;endgroup function int f();return 1;endfunction",Path::new("source-sampling.sv")).unwrap();
    assert_eq!(tree.source_items.len(), 2);
    assert_eq!(tree.subroutines.len(), 1);
    assert_eq!(tree.subroutines[0].name.spelling, "f");
}
