//! Runtime and constant operands bind directly from native expression arenas.
use super::*;
use celox_sv_syntax::{Declarations, DimensionKind, ExprId, ExprKind};
mod calls;
mod direct;
mod lvalues;
#[cfg(test)]
mod selected;
mod strings;
pub(super) use calls::{statement as call_statement, statement_root as call_statement_root};
pub(super) use direct::{
    arguments, bind_value, call_name as native_name, callable_name, cast_target_native, real_value,
    remove_real_division_guards, ungroup,
};
pub(super) use lvalues::{net_lvalue, variable_lvalue};
pub(super) use strings::{
    call as string_call, method as string_method, native_string, to_string as string_operand,
};
fn disagreement() -> AnalyzerError {
    AnalyzerError::Parse("invalid native expression ownership".into())
}
pub(super) fn bind(
    root: ExprId,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    bind_value(root, tree.declarations(), tree, dimensions, None)
}
pub(super) fn expression(
    root: ExprId,
    native: &Declarations,
    lhs: &LValue,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    let shape = match lhs {
        LValue::Ident(name) => dimensions.get(name),
        _ => None,
    };
    let value = bind_value(root, native, tree, dimensions, shape)?;
    // A string literal or integral value assigned to a string becomes its
    // bytes (IEEE 1800-2023 6.16).
    Ok(if shape.is_some_and(|shape| shape.string) {
        string_operand(value, dimensions)
    } else {
        value
    })
}
pub(super) fn value(
    root: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    bind_value(root, native, tree, dimensions, None)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deep_native_runtime_groups_and_native_literal_values() {
        let path = Path::new("native-runtime-groups.sv");
        let tree =
            crate::source_tree::parse_source("module M(output int y); assign y=7; endmodule", path)
                .unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let lhs = LValue::Ident("y".into());
        let source = format!(
            "module M; assign y={}7{}; endmodule",
            "(".repeat(10_000),
            ")".repeat(10_000)
        );
        let native = celox_sv_syntax::Declarations::parse(&source, path).unwrap();
        let root = native.assign_statements[0].assignments[0].right;
        assert_eq!(
            expression(root, &native, &lhs, &tree, &PackedDimensions::default()).unwrap(),
            Expr::Literal("7".into())
        );
        for (text, expected) in [("8", 8), ("7+8", 15)] {
            let native = celox_sv_syntax::Declarations::parse(
                &format!("module M;assign y={text};endmodule"),
                path,
            )
            .unwrap();
            let root = native.assign_statements[0].assignments[0].right;
            let value =
                expression(root, &native, &lhs, &tree, &PackedDimensions::default()).unwrap();
            assert_eq!(
                expr_to_const(&tree.state, value).and_then(|value| eval_ast_const_expr(
                    &tree.state,
                    &value,
                    &HashMap::default()
                )),
                Some(expected)
            );
        }
    }
}

#[cfg(test)]
mod query_depth_tests {
    use super::*;
    #[test]
    fn deeply_nested_bits_uses_native_jobs_on_default_stack() {
        let tree = std::thread::Builder::new()
            .stack_size(128 * 1024 * 1024)
            .spawn(|| {
                let source = format!(
                    "module M;parameter N={}1{};endmodule",
                    "$bits(".repeat(500),
                    ")".repeat(500)
                );
                crate::source_tree::parse_source(&source, Path::new("deep-native-bits.sv")).unwrap()
            })
            .unwrap()
            .join()
            .unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let root = tree
            .declarations()
            .expression_nodes()
            .filter(|(_, expr)| matches!(expr.kind, ExprKind::Call { .. }))
            .min_by_key(|(_, expr)| expr.tokens.start)
            .map(|(id, _)| id)
            .unwrap();
        let context = PackedDimensions {
            scope_types_complete: true,
            ..PackedDimensions::default()
        };
        let value = bind(root, &tree, &context);
        drop(tree);
        assert_eq!(value.unwrap(), Expr::Literal("32".into()));
    }
}
