use super::*;
use crate::ast::Source;
use celox_sv_syntax::{BodyItemKind, DataTypeKind};

#[test]
fn native_dpi_prototype_supplies_abi_and_query_return_metadata() {
    let mut tree = parse_source(
        "module M;import \"DPI-C\" function int f(input int value);parameter int N=$bits(f(1));endmodule",
        Path::new("native-dpi-metadata.sv"),
    )
    .unwrap();
    let dpi = &mut tree.declarations.dpi_declarations[0];
    dpi.property = Some(celox_sv_syntax::DpiProperty::Pure);
    dpi.c_name = Some(dpi.name.clone());
    dpi.c_name.as_mut().unwrap().spelling = "external_f".into();
    let types = [dpi.return_type.unwrap(), dpi.ports[0].data_type];
    let ranges = types.map(|id| tree.declarations.data_type(id).tokens);
    for ty in &mut tree.declarations.types {
        if ranges.contains(&ty.tokens) {
            ty.kind = DataTypeKind::Builtin("byte".into());
            ty.signed = Some(false);
        }
    }
    let source = Source::from_syntax(&tree).unwrap();
    let import = &source.modules()[0].dpi_imports()[0];
    assert!(import.is_pure());
    assert_eq!(import.c_name(), "external_f");
    assert_eq!(import.return_type().unwrap().width(), 8);
    assert!(!import.arguments()[0].r#type().is_signed());
    let ir = crate::analyze::analyze_source(source).unwrap();
    assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(8));
}

#[test]
fn native_body_ids_select_dpi_declarations_without_original_prototype_pairing() {
    let mut tree = parse_source(
        "module M;import \"DPI-C\" function int first();import \"DPI-C\" function byte second();endmodule",
        Path::new("native-dpi-body-identities.sv"),
    )
    .unwrap();
    let bodies = tree
        .declarations
        .body_items
        .iter()
        .filter_map(|item| match item.kind {
            BodyItemKind::Dpi(id) => Some(id),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(bodies.len(), 2);
    for item in &mut tree.declarations.body_items {
        if let BodyItemKind::Dpi(id) = &mut item.kind {
            *id = if *id == bodies[0] {
                bodies[1]
            } else {
                bodies[0]
            };
        }
    }
    let source = Source::from_syntax(&tree).unwrap();
    assert_eq!(
        source.modules()[0]
            .dpi_imports()
            .iter()
            .map(|import| import.name())
            .collect::<Vec<_>>(),
        ["second", "first"]
    );
}

#[test]
fn dpi_queries_keep_root_and_generated_lexical_scopes_separate() {
    let code = r#"module M;
        import "DPI-C" function int f();
        parameter int ROOT = $bits(f());
        if (1) begin : active
            import "DPI-C" function byte f();
            localparam int N = $bits(f());
            logic [N-1:0] x;
        end else begin : inactive
            import "DPI-C" context function longint f();
        end
        if (0) begin : sibling
            import "DPI-C" context function shortint f();
        end
        endmodule"#;
    let ir = crate::analyze_source(code, Path::new("native-dpi-lexical-scopes.sv")).unwrap();
    let module = &ir.modules()[0];
    assert_eq!(module.parameters()[0].resolved_value(), Some(32));
    let x = module
        .signals()
        .iter()
        .find(|signal| signal.name() == "active.x")
        .unwrap();
    assert_eq!(x.r#type().resolved_width(), Some(8));
    assert_eq!(module.dpi_imports().len(), 2);
}

#[test]
fn dpi_packages_generate_loops_and_compilation_unit_diagnostics_are_preserved() {
    let code = r#"package p;import "DPI-C" function byte f();endpackage
        module M;import p::*;
        for(genvar i=0;i<2;i++) begin:g
            import "DPI-C" function int local_f(input int value);
        end
        endmodule"#;
    let ir = crate::analyze_source(code, Path::new("native-dpi-package-loop.sv")).unwrap();
    let imports = ir.modules()[0].dpi_imports();
    assert_eq!(
        imports
            .iter()
            .map(|import| import.name())
            .collect::<Vec<_>>(),
        ["local_f", "local_f", "p::f"]
    );
    let error = crate::analyze_source(
        "`define DPI import \"DPI-C\" function int f();\n`DPI\nmodule M;endmodule",
        Path::new("native-dpi-unit.sv"),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("DPI-C import or export at compilation-unit scope"),
        "{error}"
    );
}

#[test]
fn native_dpi_name_supplies_package_visibility_and_import_binding() {
    let mut tree = parse_source(
        "package p;import \"DPI-C\" function int old_name();endpackage module M;import p::new_name;endmodule",
        Path::new("native-dpi-package-name.sv"),
    )
    .unwrap();
    tree.declarations.dpi_declarations[0].name.spelling = "new_name".into();
    let source = Source::from_syntax(&tree).unwrap();
    assert_eq!(source.modules()[0].dpi_imports()[0].name(), "p::new_name");
}
