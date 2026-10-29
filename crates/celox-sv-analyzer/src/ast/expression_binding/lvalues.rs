//! Assignment targets bind native name/member/selection identities directly.
use super::*;
pub(in crate::ast) fn variable_lvalue(
    root: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
) -> Converted<LValue> {
    direct::selected_lvalue(root, native, tree, dimensions, false)
}
pub(in crate::ast) fn net_lvalue(
    root: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
) -> Converted<LValue> {
    if direct::member_context(root, native, tree, dimensions)?
        .is_some_and(|(guards, _)| !guards.is_empty())
    {
        return Err(unsupported("continuous driver of a tagged union member"));
    }
    let target = direct::selected_lvalue(root, native, tree, dimensions, true)?;
    crate::ast::virtual_interfaces::check_target(&target)?;
    Ok(target)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deep_native_target_range_bounds_and_widths_use_worklists() {
        let path = Path::new("deep-native-target-range.sv");
        for continuous in [false, true] {
            let prefix = if continuous { "assign" } else { "initial" };
            let grouped =
                |text: &str| format!("{}{text}{}", "(".repeat(10_000), ")".repeat(10_000));
            for (target, deep_target) in [
                ("a[7:4]", format!("a[{}:{}]", grouped("7"), grouped("4"))),
                ("a[4+:4]", format!("a[{}+:{}]", grouped("4"), grouped("4"))),
            ] {
                let tree = crate::source_tree::parse_source(
                    &format!("module M;{prefix} {target}=1;endmodule"),
                    path,
                )
                .unwrap();
                let tree = AnalysisContext::unspecialized(&tree);
                let native = Declarations::parse(
                    &format!("module M;{prefix} {deep_target}=1;endmodule"),
                    path,
                )
                .unwrap();
                let root = if continuous {
                    native.assign_statements[0].assignments[0].left
                } else {
                    native
                        .statements
                        .iter()
                        .find_map(|s| match s.kind {
                            celox_sv_syntax::StatementKind::Assignment { left, .. } => Some(left),
                            _ => None,
                        })
                        .unwrap()
                };
                let target = if continuous {
                    net_lvalue(root, &native, &tree, &PackedDimensions::default())
                } else {
                    variable_lvalue(root, &native, &tree, &PackedDimensions::default())
                }
                .unwrap();
                let LValue::Select { msb, lsb, .. } = target else {
                    panic!()
                };
                let evaluate = |value: ConstExpr| {
                    let value: crate::ir::ConstExpr = value.into();
                    crate::typecheck::eval_const_expr(&value, &HashMap::default()).unwrap()
                };
                assert_eq!((evaluate(msb), evaluate(lsb)), (7, 4));
            }
        }
    }
    #[test]
    fn deep_native_target_indices_bind_without_recursive_native_traversal() {
        let path = Path::new("deep-native-target.sv");
        for continuous in [false, true] {
            let prefix = if continuous { "assign" } else { "initial" };
            let tree = crate::source_tree::parse_source(
                &format!("module M;{prefix} a[7]=1;endmodule"),
                path,
            )
            .unwrap();
            let tree = AnalysisContext::unspecialized(&tree);
            let native = Declarations::parse(
                &format!(
                    "module M;{prefix} a[{}7{}]=1;endmodule",
                    "(".repeat(10_000),
                    ")".repeat(10_000)
                ),
                path,
            )
            .unwrap();
            let root = if continuous {
                native.assign_statements[0].assignments[0].left
            } else {
                native
                    .statements
                    .iter()
                    .find_map(|statement| match statement.kind {
                        celox_sv_syntax::StatementKind::Assignment { left, .. } => Some(left),
                        _ => None,
                    })
                    .unwrap()
            };
            let target = if continuous {
                net_lvalue(root, &native, &tree, &PackedDimensions::default())
            } else {
                variable_lvalue(root, &native, &tree, &PackedDimensions::default())
            }
            .unwrap();
            let LValue::Select { msb, lsb, .. } = target else {
                panic!()
            };
            assert_eq!(msb, ConstExpr::Literal("7".into()));
            assert_eq!(msb, lsb);
        }
    }
}
