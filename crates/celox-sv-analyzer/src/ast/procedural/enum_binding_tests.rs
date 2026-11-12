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
