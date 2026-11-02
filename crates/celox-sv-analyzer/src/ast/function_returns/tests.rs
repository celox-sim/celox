use super::*;
use celox_sv_syntax::ExprId;
use std::path::Path;

fn fixture(declarations: &str, call: &str) -> (SyntaxTree, Session, PackedDimensions, ExprId) {
    let source = format!("module M; {declarations} assign out={call}; endmodule");
    let syntax =
        crate::source_tree::parse_scope_fixture(&source, Path::new("return-types.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&syntax);
    let id = tree
        .declarations()
        .assign_statements
        .last()
        .unwrap()
        .assignments[0]
        .right;
    let mut session = Session::default();
    let dims = dimensions::query_operand_dimensions_id(
        id,
        &tree,
        &mut session,
        &PackedDimensions::default(),
    )
    .unwrap();
    (syntax, session, dims, id)
}

#[test]
fn binding_function_bodies_preserves_the_declared_return_id() {
    // IEEE 1800-2023 13.4.1: the declaration determines a function's return
    // type, including identity and array shape, independently of its body.
    for declarations in [
        "function automatic int f(); return 4; endfunction",
        "function automatic f(); return 1; endfunction",
        "function automatic string f(); return \"text\"; endfunction",
        "function automatic real f(); return 1.5; endfunction",
        "function automatic shortreal f(); return 1.5; endfunction",
        "typedef enum logic[1:0] {A,B} E; function automatic E f(); return A; endfunction",
        "class C; endclass function automatic C f(); return null; endfunction",
        "typedef bit[4:0] Words[2]; function automatic Words f(); return '{1,2}; endfunction",
        "typedef bit[1:0][4:0] Packed; function automatic Packed f(); return '0; endfunction",
        "typedef int Q[$]; function automatic Q f(); return '{}; endfunction",
    ] {
        let (syntax, mut session, mut dims, id) = fixture(declarations, "f()");
        let tree = AnalysisContext::unspecialized(&syntax);
        let expected = dims.function_return_types["f"]
            .semantic_type
            .expect(declarations);
        let functions =
            functions::functions_from_scope(&tree, &mut session, &dims.const_env, &dims).unwrap();
        assert_eq!(
            functions["f"].return_semantic_type,
            Some(expected),
            "{declarations}"
        );
        let metadata = FunctionReturnMetadata::from_function(&functions["f"]);
        assert_eq!(metadata.semantic_type, Some(expected));

        // Bound-only contexts have no discovered-signature registry. They
        // must retain every kind of return type without re-inferring flags.
        dims.function_return_types = SharedMap::default();
        dims.functions = Arc::new(functions);
        let sites = session.next_call_site;
        assert_eq!(
            expression_types::type_of(&tree, &mut session, id, tree.declarations(), &dims).unwrap(),
            expected,
            "{declarations}"
        );
        assert_eq!(session.next_call_site, sites);

        // Publishing bound functions must use the same ID, not a lookup of
        // old signatures by function name.
        dims.function_return_types.insert("f".to_string(), metadata);
        assert_eq!(
            expression_types::type_of(&tree, &mut session, id, tree.declarations(), &dims).unwrap(),
            expected
        );
    }
}

#[test]
fn renamed_bound_functions_carry_their_return_type_without_a_signature_lookup() {
    let (syntax, mut session, mut dims, id) = fixture(
        "class C; endclass function automatic C f(); return null; endfunction",
        "g()",
    );
    let tree = AnalysisContext::unspecialized(&syntax);
    let mut functions =
        functions::functions_from_scope(&tree, &mut session, &dims.const_env, &dims).unwrap();
    let original = functions.remove("f").unwrap();
    let expected = original.return_semantic_type.unwrap();
    let names = HashMap::from_iter([("f".to_string(), "g".to_string())]);
    let renamed = scope::Renamer { names: &names }.function(original);
    assert_eq!(renamed.name, "g");
    assert_eq!(
        FunctionReturnMetadata::from_function(&renamed).semantic_type,
        Some(expected)
    );
    dims.function_return_types = SharedMap::default();
    dims.functions = Arc::new(HashMap::from_iter([("g".to_string(), renamed)]));
    let sites = session.next_call_site;
    assert_eq!(
        expression_types::type_of(&tree, &mut session, id, tree.declarations(), &dims).unwrap(),
        expected
    );
    assert_eq!(session.next_call_site, sites);
}

#[test]
fn unresolved_ranges_are_typed_only_after_their_parameter_binding_is_known() {
    // Supply N through the resolved caller environment, as for a signature
    // inherited from a specialized scope. The preliminary query has no N.
    let (syntax, mut session, dims, id) = fixture(
        "function automatic bit[N-1:0] f(); return '0; endfunction",
        "f()",
    );
    let tree = AnalysisContext::unspecialized(&syntax);
    assert!(dims.function_return_types["f"].semantic_type.is_none());
    let mut ids = Vec::new();
    for width in [5, 11] {
        let mut bound = dims.clone();
        bound.const_env.insert("N".to_string(), width);
        let functions =
            functions::functions_from_scope(&tree, &mut session, &bound.const_env, &bound).unwrap();
        bound.functions = Arc::new(functions);
        let sites = session.next_call_site;
        let ty = expression_types::type_of(&tree, &mut session, id, tree.declarations(), &bound)
            .unwrap();
        assert_eq!(session.next_call_site, sites);
        let semantic_types::TyData::Packed(packed) = session.types.data(ty) else {
            panic!("not integral")
        };
        assert_eq!(packed.bits(), width as u128);
        ids.push(ty);
    }
    assert_ne!(ids[0], ids[1]);
}

#[test]
fn dpi_return_ids_are_available_during_discovery_and_final_publication() {
    // IEEE 1800-2023 35.5.6: retain the scalar ABI type in both registries.
    for (declaration, function, width, signed, four_state) in [
        ("bit", "f", 1, false, false),
        ("logic", "f", 1, false, true),
        ("byte", "f", 8, true, false),
        ("shortint unsigned", "f", 16, false, false),
        ("int unsigned", "f", 32, false, false),
        ("longint", "f", 64, true, false),
        ("chandle", "f", 64, false, false),
        ("string", "uvm_dpi_get_tool_name_c", 0, false, false),
    ] {
        let declarations = format!("import \"DPI-C\" function {declaration} {function}();");
        let (syntax, mut session, dims, id) = fixture(&declarations, &format!("{function}()"));
        let tree = AnalysisContext::unspecialized(&syntax);
        let early = &dims.function_return_types[function];
        let expected = early.semantic_type.expect(&declarations);
        let import = dpi::import(
            tree.root_unit().unwrap().dpi_declarations[0],
            tree.declarations(),
        )
        .unwrap();
        let late = FunctionReturnMetadata::from_dpi(&import, &mut session.types).unwrap();
        assert_eq!(&late, early, "{declarations}");
        let sites = session.next_call_site;
        assert_eq!(
            expression_types::type_of(&tree, &mut session, id, tree.declarations(), &dims).unwrap(),
            expected
        );
        assert_eq!(session.next_call_site, sites);
        if declaration == "string" {
            assert_eq!(
                session.types.data(expected),
                &semantic_types::TyData::String
            );
        } else {
            let semantic_types::TyData::Packed(packed) = session.types.data(expected) else {
                panic!("{declarations}")
            };
            assert_eq!(
                (packed.bits(), packed.signed, packed.four_state),
                (width, signed, four_state)
            );
        }
    }
}

#[test]
fn void_dpi_imports_do_not_gain_a_fabricated_value_type() {
    let import = DpiImport::new("f".into(), "f".into(), false, None, Vec::new());
    assert!(
        FunctionReturnMetadata::from_dpi(&import, &mut semantic_types::TyTable::default())
            .is_none()
    );
}
