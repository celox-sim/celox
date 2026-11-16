//! Native packed aggregate fields reduce through explicit type jobs into the
//! existing semantic Type and shared layout kernel. No nested aggregate reparse.
use super::*;
pub(super) fn bind(
    root: TypeId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
    aliases: &TypeAliases,
) -> Converted<Option<Type>> {
    binding::bind_in(
        root,
        native,
        tree,
        session,
        &PackedDimensions::new(HashMap::default(), env, aliases),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_field_order_owns_offsets_in_the_shared_layout() {
        let session = &mut Session::default();
        let tree = crate::source_tree::parse_source(
            "module M;typedef struct packed {logic[3:0]high;bit[1:0]low;} T;T value;endmodule",
            Path::new("native-aggregate-order.sv"),
        )
        .unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let mut native = tree.declarations().clone();
        let celox_sv_syntax::TypeAliasTarget::DataType(root) = native.type_aliases[0].target else {
            panic!()
        };
        if let DataTypeKind::Struct { fields, .. } = &mut native
            .types
            .iter_mut()
            .find(|ty| matches!(ty.kind, DataTypeKind::Struct { .. }))
            .unwrap()
            .kind
        {
            fields.reverse();
        }
        let ty = bind(
            root,
            &native,
            &tree,
            session,
            &ConstEnv::default(),
            &TypeAliases::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(ty.members[0].name(), "low");
        assert_eq!(ty.members[0].offset(), 4);
        assert_eq!(ty.members[1].name(), "high");
        assert_eq!(ty.members[1].offset(), 0);
    }
    #[test]
    fn types_keep_the_semantic_type_of_the_scope_that_declares_them() {
        // IEEE 1800-2023 6.18, 23.9: the names in a type resolve in the scope
        // that declares it. A reader in a scope that does not see `W` gets
        // the type the declaring scope bound.
        let session = &mut Session::default();
        let tree = crate::source_tree::parse_source(
            "module M;typedef logic[W-1:0] T;endmodule",
            Path::new("declared-type.sv"),
        )
        .unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let native = tree.declarations();
        let celox_sv_syntax::TypeAliasTarget::DataType(root) = native.type_aliases[0].target else {
            panic!()
        };
        let declaring = ConstEnv::from_iter([("W".into(), 4)]);
        let ty = bind(
            root,
            native,
            &tree,
            session,
            &declaring,
            &TypeAliases::default(),
        )
        .unwrap()
        .unwrap();
        let declared =
            semantic_types::of_type(&mut session.types, &tree.functions, &ty, &declaring).unwrap();
        let elsewhere = semantic_types::of_type(
            &mut session.types,
            &tree.functions,
            &ty,
            &ConstEnv::default(),
        );
        assert_eq!(elsewhere, Some(declared));
    }
    struct Original(Option<SyntaxTree>);
    impl Drop for Original {
        fn drop(&mut self) {
            if let Some(tree) = self.0.take() {
                std::thread::Builder::new()
                    .stack_size(128 * 1024 * 1024)
                    .spawn(move || drop(tree))
                    .unwrap()
                    .join()
                    .unwrap();
            }
        }
    }
    #[test]
    fn deeply_nested_native_aggregate_type_jobs_use_the_default_stack() {
        let tree = std::thread::Builder::new()
            .stack_size(128 * 1024 * 1024)
            .spawn(|| {
                let source = format!(
                    "module M;typedef {}bit member;{} }}T;endmodule",
                    "struct packed {".repeat(500),
                    "}member;".repeat(499)
                );
                crate::source_tree::parse_source(&source, Path::new("native-deep-aggregate.sv"))
            })
            .unwrap()
            .join()
            .unwrap()
            .unwrap();
        let original = Original(Some(tree));
        let tree = original.0.as_ref().unwrap();
        let tree = AnalysisContext::unspecialized(tree);
        let native = tree.declarations();
        let celox_sv_syntax::TypeAliasTarget::DataType(root) = native.type_aliases[0].target else {
            panic!()
        };
        let result = bind(
            root,
            native,
            &tree,
            &mut Session::default(),
            &ConstEnv::default(),
            &TypeAliases::default(),
        );
        drop(tree);
        drop(original);
        let ty = result.unwrap().unwrap();
        let mut at = &ty;
        let mut depth = 0;
        while let Some(member) = at.members.first() {
            depth += 1;
            at = member.r#type();
        }
        assert_eq!(depth, 500);
        assert_eq!(
            expr_type_from_type(
                &const_functions::ConstantFunctions::default(),
                &ty,
                &ConstEnv::default()
            )
            .unwrap()
            .width,
            1
        );
        drop(ty);
    }
    use std::path::Path;
}
