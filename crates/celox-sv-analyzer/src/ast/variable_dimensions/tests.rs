use super::*;
use semantic_types::{PackedElement, TyData, TyId, TyTable};
use std::path::Path;

fn symbolic_shape() -> VariableDimensions {
    let mut ty = Type::new(TypeKind::Bit);
    ty.packed_ranges.push(PackedRange::new(
        ConstExpr::Binary {
            left: Box::new(ConstExpr::Ident("N".into())),
            op: BinaryOp::Sub,
            right: Box::new(ConstExpr::Literal("1".into())),
        },
        ConstExpr::Literal("0".into()),
    ));
    procedural::dimensions_from_type(&ty)
}

fn env(width: i128) -> HashMap<String, i128> {
    HashMap::from_iter([("N".into(), width)])
}

fn assert_width(table: &TyTable, id: TyId, width: u128) {
    let TyData::Packed(packed) = table.data(id) else {
        panic!("not integral")
    };
    assert_eq!(packed.bits(), width);
    assert_eq!(packed.element, PackedElement::Bits);
}

#[test]
fn declarations_keep_their_type_when_a_caller_shadows_a_parameter() {
    // 7.4: a declaration's bounds, not the referencing scope's names,
    // determine its array type. Preserve symbolic metadata for lowering.
    let shape = symbolic_shape();
    let mut table = TyTable::default();
    let functions = const_functions::ConstantFunctions::default();
    let id = shape.bind_type(&mut table, &functions, &env(5)).unwrap();
    assert_eq!(shape.type_id(&mut table, &functions, &env(11)), Some(id));
    assert_eq!(shape.bind_type(&mut table, &functions, &env(11)), Some(id));
    assert_width(&table, id, 5);
    assert!(matches!(shape.packed[0].left, ConstExpr::Binary { .. }));

    // A new specialization constructs a new declaration from the same syntax.
    let specialized = VariableDimensions::new(shape.clone_shape());
    let specialized_id = specialized
        .bind_type(&mut table, &functions, &env(11))
        .unwrap();
    assert_width(&table, specialized_id, 11);
    assert!(!shape.same_declaration(&specialized));
}

#[test]
fn unresolved_shapes_are_not_frozen_by_provisional_queries() {
    let shape = symbolic_shape();
    let mut table = TyTable::default();
    let functions = const_functions::ConstantFunctions::default();
    assert!(
        shape
            .bind_type(&mut table, &functions, &HashMap::default())
            .is_none()
    );
    let five = shape.type_id(&mut table, &functions, &env(5)).unwrap();
    let eleven = shape.type_id(&mut table, &functions, &env(11)).unwrap();
    assert_ne!(five, eleven);
    assert_width(&table, eleven, 11);
    shape.bind_type(&mut table, &functions, &env(5)).unwrap();
    assert_eq!(shape.type_id(&mut table, &functions, &env(11)), Some(five));
}

#[test]
fn editing_a_cloned_shape_invalidates_only_its_own_binding() {
    let shape = symbolic_shape();
    let mut table = TyTable::default();
    let functions = const_functions::ConstantFunctions::default();
    let original = shape.bind_type(&mut table, &functions, &env(5)).unwrap();
    let mut edited = shape.clone();
    edited.signed = true;
    let changed = edited.type_id(&mut table, &functions, &env(11)).unwrap();
    assert_width(&table, changed, 11);
    assert!(matches!(table.data(changed), TyData::Packed(packed) if packed.signed));
    assert_eq!(
        shape.type_id(&mut table, &functions, &env(11)),
        Some(original)
    );
    assert!(!shape.same_declaration(&edited));
}

#[test]
fn class_identity_changes_cannot_reuse_a_previous_binding() {
    let mut shape = procedural::dimensions_from_type(&Type::new(TypeKind::Bit));
    shape.class = Some("C".into());
    let mut table = TyTable::default();
    let functions = const_functions::ConstantFunctions::default();
    let first = shape
        .bind_type(&mut table, &functions, &HashMap::default())
        .unwrap();
    shape.class = Some("D".into());
    let second = shape
        .type_id(&mut table, &functions, &HashMap::default())
        .unwrap();
    assert_ne!(first, second);
    assert!(
        matches!(table.data(second), TyData::Class(semantic_types::Decl::Class(name)) if name.as_ref()=="D")
    );
}

#[test]
fn renaming_references_preserves_the_evaluated_declaration_type() {
    let shape = symbolic_shape();
    let mut table = TyTable::default();
    let functions = const_functions::ConstantFunctions::default();
    let id = shape.bind_type(&mut table, &functions, &env(5)).unwrap();
    let names = HashMap::from_iter([("N".into(), "P".into())]);
    let renamed = scope::Renamer { names: &names }.dimensions(shape);
    let caller = HashMap::from_iter([("P".into(), 99)]);
    assert_eq!(renamed.type_id(&mut table, &functions, &caller), Some(id));
    assert_width(&table, id, 5);
}

#[test]
fn table_prefix_clones_share_bindings_but_independent_appends_do_not() {
    let functions = const_functions::ConstantFunctions::default();
    let mut original = TyTable::default();
    let mut independent = original.clone();
    let shape = symbolic_shape();
    let id = shape.bind_type(&mut original, &functions, &env(5)).unwrap();
    let mut prefix = original.clone();
    assert_eq!(shape.type_id(&mut prefix, &functions, &env(11)), Some(id));
    assert_width(&prefix, id, 5);
    // Both branches can allocate the same numeric ID to different nodes.
    independent.intern(TyData::String);
    let fresh = shape
        .type_id(&mut independent, &functions, &env(11))
        .unwrap();
    assert_width(&independent, fresh, 11);
    assert_eq!(shape.type_id(&mut original, &functions, &env(11)), Some(id));
}

#[test]
fn name_and_element_queries_read_the_declarations_bound_type() {
    let source = "module M; bit[N-1:0] a[2]; assign out=a[0]; endmodule";
    let syntax =
        crate::source_tree::parse_scope_fixture(source, Path::new("bound-variable.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&syntax);
    let id = tree.declarations().assign_statements[0].assignments[0].right;
    let mut session = Session::default();
    let input = PackedDimensions::new(HashMap::default(), &env(5), &HashMap::default());
    let mut dims =
        dimensions::query_operand_dimensions_id(id, &tree, &mut session, &input).unwrap();
    dims.const_env.insert("N".into(), 11);
    let sites = session.next_call_site;
    let selected =
        expression_types::type_of(&tree, &mut session, id, tree.declarations(), &dims).unwrap();
    assert_width(&session.types, selected, 5);
    assert_eq!(session.next_call_site, sites);
}

#[test]
fn formal_publication_observes_changed_bindings_with_identical_symbolic_ranges() {
    let source = "module M; function int f(input bit[N-1:0] a[2]); return 0; endfunction assign out=f('{default:0}); endmodule";
    let syntax =
        crate::source_tree::parse_scope_fixture(source, Path::new("bound-formal.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&syntax);
    let id = tree.declarations().assign_statements[0].assignments[0].right;
    let mut session = Session::default();
    let input = PackedDimensions::new(HashMap::default(), &env(5), &HashMap::default());
    let mut dims =
        dimensions::query_operand_dimensions_id(id, &tree, &mut session, &input).unwrap();
    let old = dims.subroutine_param_shapes["f"][0].clone();
    dims.const_env.insert("N".into(), 11);
    let rebound = dimensions::query_operand_dimensions_id(id, &tree, &mut session, &dims).unwrap();
    let new = &rebound.subroutine_param_shapes["f"][0];
    assert_eq!(&old, new, "the symbolic shapes themselves did not change");
    assert!(!old.same_declaration(new));
    let new_type = new
        .type_id(&mut session.types, &tree.functions, &env(99))
        .unwrap();
    let TyData::Array { element, .. } = session.types.data(new_type) else {
        panic!("not an array")
    };
    assert_width(&session.types, *element, 11);
}

#[test]
fn many_declarations_share_interned_type_nodes() {
    let functions = const_functions::ConstantFunctions::default();
    let mut table = TyTable::default();
    let shapes = (0..1024).map(|_| symbolic_shape()).collect::<Vec<_>>();
    let first = shapes[0]
        .bind_type(&mut table, &functions, &env(5))
        .unwrap();
    let count = table.len();
    for shape in &shapes {
        assert_eq!(
            shape.bind_type(&mut table, &functions, &env(5)),
            Some(first)
        );
    }
    assert_eq!(table.len(), count);
}

#[test]
fn generated_formals_keep_their_declaration_scope_bounds() {
    let source = "module M; if (1) begin : g localparam int W=7; function int f(input bit[W-1:0] a[2]); return 0; endfunction end endmodule";
    let syntax =
        crate::source_tree::parse_scope_fixture(source, Path::new("generated-formal.sv")).unwrap();
    let tree = AnalysisContext::unspecialized(&syntax);
    let mut session = Session::default();
    let (_, shapes, _) = procedural::subroutine_argument_names(
        &tree,
        &mut session,
        &HashMap::default(),
        &SharedMap::default(),
    )
    .unwrap();
    let formal = &shapes["g.f"][0];
    let caller = HashMap::from_iter([("W".into(), 99)]);
    let id = formal
        .type_id(&mut session.types, &tree.functions, &caller)
        .unwrap();
    let TyData::Array { element, .. } = session.types.data(id) else {
        panic!("not an array")
    };
    assert_width(&session.types, *element, 7);
}
