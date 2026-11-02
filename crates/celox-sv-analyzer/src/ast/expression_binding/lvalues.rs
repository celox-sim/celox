//! Assignment targets bind native name/member/selection identities directly.
use super::*;
/// The variable target `root` of a procedural write, which may be a chandle
/// if `chandle`: an assignment with `=` writes one (IEEE 1800-2023 6.14).
pub(in crate::ast) fn variable_lvalue(
    root: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
    chandle: bool,
) -> Converted<LValue> {
    if matches!(
        native.expression(ungroup(root, native)).kind,
        ExprKind::Select { .. }
    ) {
        unpacked_structs::check_operands(ungroup(root, native), native, tree, session, dimensions)?;
    }
    chandles::check_target(chandle, root, tree, native, dimensions, session)?;
    if let Some(place) = direct::object_place(root, native, tree, session, dimensions)? {
        return Ok(place);
    }
    direct::selected_lvalue(root, native, tree, session, dimensions, false)
}
pub(in crate::ast) fn net_lvalue(
    root: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<LValue> {
    if matches!(
        native.expression(ungroup(root, native)).kind,
        ExprKind::Select { .. }
    ) {
        unpacked_structs::check_operands(ungroup(root, native), native, tree, session, dimensions)?;
    }
    // No continuous assignment drives a chandle (6.14).
    chandles::check_target(false, root, tree, native, dimensions, session)?;
    if direct::member_context(root, native, tree, session, dimensions)?
        .is_some_and(|(guards, _)| !guards.is_empty())
    {
        return Err(unsupported("continuous driver of a tagged union member"));
    }
    let target = direct::selected_lvalue(root, native, tree, session, dimensions, true)?;
    crate::ast::virtual_interfaces::check_target(&target)?;
    Ok(target)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deep_native_target_range_bounds_and_widths_use_worklists() {
        let session = &mut Session::default();
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
                    net_lvalue(root, &native, &tree, session, &PackedDimensions::default())
                } else {
                    variable_lvalue(
                        root,
                        &native,
                        &tree,
                        session,
                        &PackedDimensions::default(),
                        false,
                    )
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
        let session = &mut Session::default();
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
                net_lvalue(root, &native, &tree, session, &PackedDimensions::default())
            } else {
                variable_lvalue(
                    root,
                    &native,
                    &tree,
                    session,
                    &PackedDimensions::default(),
                    false,
                )
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
