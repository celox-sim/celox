//! Assignment-pattern structure and values bind native item IDs directly.
use super::*;
use celox_sv_syntax::{ExprId, ExprKind};
fn mismatch() -> AnalyzerError {
    AnalyzerError::Parse("invalid native assignment pattern".into())
}
pub(in crate::ast) enum Kind {
    List,
    Repeat(usize),
    Keyed(Vec<Key>),
}
pub(in crate::ast) struct Plan {
    pub kind: Kind,
    pub bindings: Vec<ExprId>,
}
pub(in crate::ast) fn prepare_native(
    root: ExprId,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
    shape: Option<&VariableDimensions>,
) -> Converted<Plan> {
    let native = tree.declarations();
    let ExprKind::Pattern { items, .. } = &native.expression(root).kind else {
        return Err(mismatch());
    };
    let integer = |id| {
        constant_binding::bind(id, tree, dims)?
            .and_then(|value| eval_ast_const_expr(&tree.state, &value, &dims.const_env))
            .ok_or_else(|| unsupported("nonconstant assignment pattern key or count"))
    };
    if let [item] = items.as_slice()
        && item.key.is_none()
        && let ExprKind::Replication { count, expressions } = &native.expression(item.value).kind
    {
        return Ok(Plan {
            kind: Kind::Repeat(
                usize::try_from(integer(*count)?)
                    .map_err(|_| unsupported("assignment pattern repetition count"))?,
            ),
            bindings: expressions.clone(),
        });
    }
    let bindings = items.iter().map(|item| item.value).collect();
    if items.iter().all(|item| item.key.is_none()) {
        return Ok(Plan {
            kind: Kind::List,
            bindings,
        });
    }
    let mut keys = Vec::new();
    for item in items {
        let id = item.key.ok_or_else(mismatch)?;
        keys.push(match &native.expression(id).kind {
            ExprKind::Literal(text) if text == "default" => Key::Default,
            ExprKind::Name(name) if shape.is_none() => Key::Name {
                name: instances::normalize_identifier(&name.spelling),
                index: integer(id).ok(),
            },
            ExprKind::Name(name)
                if shape.is_some_and(|shape| {
                    shape.unpacked.is_empty()
                        && shape.packed.len() <= 1
                        && (shape.members.iter().any(|member| {
                            member.name() == instances::normalize_identifier(&name.spelling)
                        }) || shape.unpacked_struct.as_ref().is_some_and(|aggregate| {
                            aggregate
                                .member(&instances::normalize_identifier(&name.spelling))
                                .is_some()
                        }))
                }) || !dims
                    .const_env
                    .contains_key(&instances::normalize_identifier(&name.spelling)) =>
            {
                Key::Member(instances::normalize_identifier(&name.spelling))
            }
            ExprKind::Type(_) => return Err(unsupported("assignment pattern type key")),
            _ => Key::Index(integer(id)?),
        });
    }
    Ok(Plan {
        kind: Kind::Keyed(keys),
        bindings,
    })
}
pub(super) fn value(
    root: ExprId,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<Value> {
    value_mode(root, tree, dims, true)
}

pub(super) fn value_mode(
    root: ExprId,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
    constant: bool,
) -> Converted<Value> {
    enum Job {
        Enter(ExprId),
        Finish(Kind, usize),
    }
    let native = tree.declarations();
    let mut jobs = vec![Job::Enter(root)];
    let mut values = Vec::new();
    while let Some(job) = jobs.pop() {
        match job {
            Job::Enter(id) => {
                let mut id = id;
                while let ExprKind::Group(inner) = native.expression(id).kind {
                    id = inner;
                }
                match &native.expression(id).kind {
                    ExprKind::Pattern { target: None, .. } => {
                        let plan = prepare_native(id, tree, dims, None)?;
                        jobs.push(Job::Finish(plan.kind, plan.bindings.len()));
                        jobs.extend(plan.bindings.into_iter().rev().map(Job::Enter));
                    }
                    ExprKind::Pattern { .. } if constant => {
                        values.push(patterns::constant_native(id, tree, dims)?)
                    }
                    _ => {
                        let expr = expression_binding::bind(id, tree, dims)?;
                        let (expr, signed, value_type) = if constant {
                            // Capture the self-determined type before folding the
                            // value into bits, just as for runtime port values.
                            let types = dimensions::query_context_for_operand(id, tree, dims)?;
                            let value_type = super::ports::ValueType::Known(
                                super::ports::native_value_type(id, tree, &types)?,
                            );
                            let signed =
                                dimensions::expression_signedness(&expr, &types, &types.const_env);
                            let expr = frozen(&tree.state, expr, dims)?;
                            let signed = Some(signed.unwrap_or_else(|| {
                                dimensions::expression_signedness(&expr, &types, &types.const_env)
                                    .unwrap_or(false)
                            }));

                            (expr, signed, value_type)
                        } else {
                            let signed =
                                dimensions::expression_signedness(&expr, dims, &dims.const_env);
                            (
                                expr,
                                signed,
                                super::ports::native_value_type_plan(id, tree, dims)?,
                            )
                        };
                        values.push(Value::Expression {
                            expr,
                            signed,
                            value_type,
                        });
                    }
                }
            }
            Job::Finish(kind, count) => {
                let start = values.len().checked_sub(count).ok_or_else(mismatch)?;
                let items = values.split_off(start);
                let items = match kind {
                    Kind::List => Items::List(items),
                    Kind::Repeat(n) => Items::Repeat(n, items),
                    Kind::Keyed(keys) => Items::Keyed(keys.into_iter().zip(items).collect()),
                };
                values.push(Value::Pattern(BoundPattern { items }));
            }
        }
    }
    if values.len() != 1 {
        return Err(mismatch());
    }
    values.pop().ok_or_else(mismatch)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
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
    fn deeply_nested_native_override_pattern_jobs_use_the_default_stack() {
        let tree = std::thread::Builder::new()
            .stack_size(128 * 1024 * 1024)
            .spawn(|| {
                let source = format!(
                    "module M;C#(.P({}1{}))c();endmodule",
                    "'{".repeat(500),
                    "}".repeat(500)
                );
                crate::source_tree::parse_source(&source, Path::new("native-deep-pattern.sv"))
            })
            .unwrap()
            .join()
            .unwrap()
            .unwrap();
        let original = Original(Some(tree));
        let context = AnalysisContext::unspecialized(original.0.as_ref().unwrap());
        let tree = &context;
        let root = tree.declarations().instantiations[0]
            .parameters
            .as_ref()
            .unwrap()[0]
            .value
            .unwrap();
        let dims =
            PackedDimensions::new(HashMap::default(), &HashMap::default(), &HashMap::default());
        let result = super::super::from_param(root, tree, &dims);
        drop(context);
        drop(original);
        let pattern = result.unwrap();
        let mut at = &pattern;
        let mut depth = 0;
        loop {
            let Items::List(items) = &at.items else {
                panic!()
            };
            assert_eq!(items.len(), 1);
            depth += 1;
            match &items[0] {
                Value::Pattern(pattern) => at = pattern,
                Value::Expression {
                    expr: Expr::Literal(value),
                    ..
                } => {
                    assert_eq!(value, "1");
                    break;
                }
                _ => panic!(),
            }
        }
        assert_eq!(depth, 500);
        drop(pattern);
    }
}
