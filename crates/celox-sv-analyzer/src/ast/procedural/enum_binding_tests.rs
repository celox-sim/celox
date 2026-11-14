use super::*;
use std::path::Path;

#[test]
fn local_values_hide_enum_bindings_and_scope_exit_restores_them() {
    let syntax = crate::source_tree::parse_scope_fixture(
        "module M; typedef enum {A,B} E; assign out=A; endmodule",
        Path::new("enum-scope-restore.sv"),
    )
    .unwrap();
    let tree = AnalysisContext::unspecialized(&syntax);
    let id = tree.declarations().assign_statements[0].assignments[0].right;
    let mut session = Session::default();
    let dims = dimensions::query_operand_dimensions_id(
        id,
        &tree,
        &mut session,
        &PackedDimensions::default(),
    )
    .unwrap();
    let original = dims.constant_types["A"].clone();
    let mut locals = Vec::new();
    let mut counter = 0;
    let mut state = BodyState {
        locals: &mut locals,
        counter: &mut counter,
        subroutine_params: &HashMap::default(),
        subroutine_directions: &HashMap::default(),
        automatic: true,
        statics: Vec::new(),
        explicit_statics: HashSet::default(),
        static_initializers: Vec::new(),
        constant_evaluation: false,
    };
    let mut builder = BodyBuilder::new(
        &tree,
        &mut session,
        &dims,
        &mut state,
        system_functions::Body::Initial,
    );
    builder.push_scope();
    builder.declare("A", int_type());
    assert!(!builder.dims.constant_types.contains_key("A"));
    assert!(
        expression_types::constant_binding(
            &tree,
            builder.session,
            id,
            tree.declarations(),
            &builder.dims
        )
        .unwrap()
        .is_none()
    );
    builder.pop_scope();
    assert_eq!(builder.dims.constant_types["A"], original);
    // A localparam/typedef uses the same binding save/restore entry.
    builder.push_scope();
    let entry = builder.take_binding("A", None, 0);
    builder
        .dims
        .const_env
        .insert(parameters::unbounded_parameter_marker("A"), 0);
    builder.names.scopes.last_mut().unwrap().entries.push(entry);
    assert!(
        expression_types::constant_binding(
            &tree,
            builder.session,
            id,
            tree.declarations(),
            &builder.dims
        )
        .unwrap()
        .is_none()
    );
    builder.pop_scope();
    assert_eq!(builder.dims.constant_types["A"], original);
}

#[test]
fn locals_are_bound_where_they_are_declared() {
    // IEEE 1800-2023 6.18: the names in a type resolve in the scope that
    // declares it. A local the body declares, such as a temporary of a type
    // made for it, is bound there, so reading its type, as publishing it
    // does, resolves nothing again.
    let syntax = crate::source_tree::parse_scope_fixture(
        "module M; endmodule",
        Path::new("local-binding.sv"),
    )
    .unwrap();
    let tree = AnalysisContext::unspecialized(&syntax);
    let mut session = Session::default();
    let env = ConstEnv::from_iter([("W".to_string(), 4)]);
    let dims = PackedDimensions::new(HashMap::default(), &env, &TypeAliases::default());
    let mut locals = Vec::new();
    let mut counter = 0;
    let mut state = BodyState {
        locals: &mut locals,
        counter: &mut counter,
        subroutine_params: &HashMap::default(),
        subroutine_directions: &HashMap::default(),
        automatic: true,
        statics: Vec::new(),
        explicit_statics: HashSet::default(),
        static_initializers: Vec::new(),
        constant_evaluation: false,
    };
    let mut builder = BodyBuilder::new(
        &tree,
        &mut session,
        &dims,
        &mut state,
        system_functions::Body::Initial,
    );
    builder.push_scope();
    let mut ty = int_type();
    ty.packed_ranges[0].left = ConstExpr::Ident("W".into());
    builder.declare("x", ty);
    builder.pop_scope();
    drop(builder);
    assert_eq!(session.types.unbound_reads, 0);
}
