//! Symbolic execution of the already-lowered native subroutine control tree.
//! No syntax parsing or original statement traversal occurs here.
use super::*;
use crate::procedural::{CaseKind, CaseLabel};
type Values = HashMap<String, Expr>;
struct Outcome {
    values: Values,
    returned: Option<Expr>,
}
struct Branch<'a> {
    condition: Expr,
    statements: &'a [Stmt],
}
enum Job<'a> {
    Sequence {
        statements: Vec<&'a Stmt>,
        values: Values,
    },
    Merge {
        conditions: Vec<Expr>,
        count: usize,
        incoming: Values,
        rest: Vec<&'a Stmt>,
        terminal: bool,
    },
}
/// Validate the complete expression-form class, including unreachable tails,
/// and memoize return containment in one postorder pass.
fn returning(
    statements: &[Stmt],
    types: &HashMap<String, FunctionLocalType>,
    values: &Values,
) -> Option<HashSet<*const Stmt>> {
    let mut jobs = statements
        .iter()
        .map(|stmt| (stmt, false))
        .collect::<Vec<_>>();
    let mut returning = HashSet::default();
    while let Some((stmt, finish)) = jobs.pop() {
        let children = match stmt {
            Stmt::Assign {
                lhs: LValue::Ident(name),
                nonblocking: false,
                ..
            } if types.contains_key(name) || values.contains_key(name) => Vec::new(),
            Stmt::Local { name, .. } if types.contains_key(name) => Vec::new(),
            Stmt::Return(Some(_)) => {
                returning.insert(stmt as *const Stmt);
                continue;
            }
            Stmt::If {
                then_body,
                else_body,
                ..
            } => then_body.iter().chain(else_body).collect::<Vec<_>>(),
            Stmt::Case {
                kind,
                items,
                default,
                ..
            } if !matches!(kind, CaseKind::Inside) => items
                .iter()
                .flat_map(|item| &item.body)
                .chain(default.iter().flatten())
                .collect(),
            _ => return None,
        };
        if finish {
            if children
                .iter()
                .any(|stmt| returning.contains(&(*stmt as *const Stmt)))
            {
                returning.insert(stmt as *const Stmt);
            }
        } else {
            jobs.push((stmt, true));
            jobs.extend(children.into_iter().map(|stmt| (stmt, false)));
        }
    }
    Some(returning)
}
fn has_return(statements: &[Stmt], returning: &HashSet<*const Stmt>) -> bool {
    statements
        .iter()
        .any(|stmt| returning.contains(&(stmt as *const Stmt)))
}
fn fit(expr: Expr, ty: Option<&FunctionLocalType>) -> Expr {
    ty.map(|ty| coerce_function_local_assignment(expr.clone(), *ty))
        .unwrap_or(expr)
}
fn value(outcome: &Outcome, return_var: Option<&str>) -> Option<Expr> {
    outcome
        .returned
        .clone()
        .or_else(|| return_var.and_then(|name| outcome.values.get(name).cloned()))
}
fn sequence(
    statements: &[Stmt],
    values: Values,
    types: &HashMap<String, FunctionLocalType>,
    return_var: Option<&str>,
) -> Option<Outcome> {
    // An expression body cannot represent writes to package/module storage
    // or persistent locals. Keep those functions as calls, preserving effects
    // and the distinct source sites assigned during index conversion.
    let returning = returning(statements, types, &values)?;
    let mut jobs = vec![Job::Sequence {
        statements: statements.iter().collect(),
        values,
    }];
    let mut results: Vec<Outcome> = Vec::new();
    while let Some(job) = jobs.pop() {
        match job {
            Job::Sequence {
                statements,
                mut values,
            } => {
                let mut branched = false;
                for (at, stmt) in statements.iter().enumerate() {
                    let rest = statements[at + 1..].to_vec();
                    let branch = match stmt {
                        Stmt::Assign {
                            lhs: LValue::Ident(name),
                            rhs,
                            nonblocking: false,
                        } => {
                            values.insert(
                                name.clone(),
                                fit(
                                    substitute_expr_idents(rhs.clone(), &values),
                                    types.get(name),
                                ),
                            );
                            continue;
                        }
                        Stmt::Local { name, init } => {
                            let expr = init
                                .as_ref()
                                .map(|init| substitute_expr_idents(init.clone(), &values))
                                .unwrap_or_else(|| Expr::Literal("'x".into()));
                            values.insert(name.clone(), fit(expr, types.get(name)));
                            continue;
                        }
                        Stmt::Return(Some(expr)) => {
                            results.push(Outcome {
                                returned: Some(substitute_expr_idents(expr.clone(), &values)),
                                values: values.clone(),
                            });
                            branched = true;
                            break;
                        }
                        Stmt::If {
                            condition,
                            then_body,
                            else_body,
                        } => {
                            let condition = procedural_truth_condition(substitute_expr_idents(
                                condition.clone(),
                                &values,
                            ));
                            (
                                vec![Branch {
                                    condition,
                                    statements: then_body,
                                }],
                                else_body.as_slice(),
                            )
                        }
                        Stmt::Case {
                            kind,
                            selector,
                            items,
                            default,
                        } if !matches!(kind, CaseKind::Inside) => {
                            let selector = substitute_expr_idents(selector.clone(), &values);
                            let mut branches = Vec::new();
                            for item in items {
                                let condition = item
                                    .labels
                                    .iter()
                                    .map(|label| {
                                        let CaseLabel::Value(label) = label else {
                                            return None;
                                        };
                                        Some(case_item_condition(
                                            selector.clone(),
                                            substitute_expr_idents(label.clone(), &values),
                                            !matches!(kind, CaseKind::Exact),
                                        ))
                                    })
                                    .collect::<Option<Vec<_>>>()?
                                    .into_iter()
                                    .reduce(|left, right| Expr::Binary {
                                        left: Box::new(left),
                                        op: BinaryOp::LogicOr,
                                        right: Box::new(right),
                                    })?;
                                branches.push(Branch {
                                    condition,
                                    statements: &item.body,
                                });
                            }
                            (branches, default.as_deref().unwrap_or_default())
                        }
                        _ => return None,
                    };
                    let (branches, default) = branch;
                    let terminal = branches
                        .iter()
                        .any(|branch| has_return(branch.statements, &returning))
                        || has_return(default, &returning);
                    let conditions = branches
                        .iter()
                        .map(|branch| branch.condition.clone())
                        .collect();
                    jobs.push(Job::Merge {
                        conditions,
                        count: branches.len() + 1,
                        incoming: values.clone(),
                        rest: rest.clone(),
                        terminal,
                    });
                    for body in branches
                        .into_iter()
                        .map(|branch| branch.statements)
                        .chain(std::iter::once(default))
                        .rev()
                    {
                        let mut statements = body.iter().collect::<Vec<_>>();
                        if terminal {
                            statements.extend(rest.iter().copied());
                        }
                        jobs.push(Job::Sequence {
                            statements,
                            values: values.clone(),
                        });
                    }
                    branched = true;
                    break;
                }
                if !branched {
                    results.push(Outcome {
                        values,
                        returned: None,
                    });
                }
            }
            Job::Merge {
                conditions,
                count,
                incoming,
                rest,
                terminal,
            } => {
                let mut branches = results.split_off(results.len().checked_sub(count)?);
                let mut result = branches.pop()?;
                if terminal {
                    let mut result_value = value(&result, return_var)?;
                    for (condition, branch) in conditions.into_iter().zip(branches).rev() {
                        result_value = Expr::Mux {
                            condition: Box::new(condition),
                            then_expr: Box::new(value(&branch, return_var)?),
                            else_expr: Box::new(result_value),
                        };
                    }
                    result.returned = Some(result_value);
                    results.push(result);
                } else {
                    let mut merged = incoming;
                    let mut names = result.values.keys().cloned().collect::<HashSet<_>>();
                    for branch in &branches {
                        names.extend(branch.values.keys().cloned());
                    }
                    for name in names {
                        let mut value = result
                            .values
                            .get(&name)
                            .or_else(|| merged.get(&name))
                            .cloned()
                            .unwrap_or_else(|| Expr::Ident(name.clone()));
                        for (condition, branch) in conditions.iter().zip(&branches).rev() {
                            let branch_value = branch
                                .values
                                .get(&name)
                                .cloned()
                                .unwrap_or_else(|| value.clone());
                            if branch_value != value {
                                value = Expr::Mux {
                                    condition: Box::new(condition.clone()),
                                    then_expr: Box::new(branch_value),
                                    else_expr: Box::new(value),
                                };
                            }
                        }
                        merged.insert(name, value);
                    }
                    jobs.push(Job::Sequence {
                        statements: rest,
                        values: merged,
                    });
                }
            }
        }
    }
    if results.len() != 1 {
        return None;
    }
    results.pop()
}
pub(super) fn body(
    subroutine: &Subroutine,
    locals: &[LocalVariable],
) -> Option<(Expr, Vec<(String, Expr)>)> {
    if !subroutine.persistent_locals.is_empty() {
        return None;
    }
    let mut types = HashMap::default();
    for local in locals {
        if !local.r#type.unpacked_ranges().is_empty() {
            return None;
        }
        types.insert(
            local.name.clone(),
            FunctionLocalType {
                width: local.r#type.resolved_width()?,
                signed: local.r#type.is_signed(),
                is_2state: local.r#type.kind() == crate::ir::TypeKind::Bit,
            },
        );
    }
    let mut values = HashMap::default();
    for param in &subroutine.params {
        values.insert(param.name.clone(), Expr::Ident(param.source_name.clone()));
    }
    if let Some(name) = &subroutine.return_var {
        values.insert(name.clone(), Expr::Literal("'x".into()));
    }
    let outcome = sequence(
        &subroutine.body,
        values,
        &types,
        subroutine.return_var.as_deref(),
    )?;
    let outputs = subroutine
        .params
        .iter()
        .filter(|param| param.direction.is_written())
        .map(|param| {
            Some((
                param.source_name.clone(),
                outcome.values.get(&param.name)?.clone(),
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    if outcome.returned.is_some() && !outputs.is_empty() {
        return None;
    }
    let value = value(&outcome, subroutine.return_var.as_deref())
        .or_else(|| (!outputs.is_empty()).then(|| Expr::Literal("'x".into())))?;
    Some((value, outputs))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source(code: &str) -> Source {
        let tree =
            crate::source_tree::parse_scope_fixture(code, Path::new("native-symbolic.sv")).unwrap();
        Source::from_syntax(&tree).unwrap()
    }
    fn evaluate(expr: Expr, bindings: &HashMap<String, Expr>) -> i128 {
        let expr = substitute_expr_idents(expr, bindings);
        let expr = expr_to_index_const(
            &AnalysisState::default(),
            expr,
            &PackedDimensions::default(),
        )
        .unwrap();
        typecheck::eval_const_expr(&expr.into(), &HashMap::default()).unwrap()
    }
    #[test]
    fn native_case_and_early_returns_preserve_continuations_and_label_priority() {
        let source = source(
            "module M;function automatic int f(input int c);int t;t=1;case(c)0:t=3;1:begin if(c)return 4;end default:t=5;endcase return t+7;endfunction endmodule",
        );
        let function = &source.modules[0].symbolic_functions["f"];
        for (input, expected) in [(0, 10), (1, 4), (2, 12)] {
            assert_eq!(
                evaluate(
                    function.body.clone().unwrap(),
                    &HashMap::from_iter([("c".into(), Expr::Literal(input.to_string()))])
                ),
                expected
            );
        }
    }
    #[test]
    fn native_task_outputs_keep_formal_fit_and_two_state_coercion() {
        let tree = crate::source_tree::parse_scope_fixture(
            "module M;task automatic t(input logic[3:0]x,output bit[2:0]y);y=x+1;endtask endmodule",
            Path::new("native-task-symbolic.sv"),
        )
        .unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let function =
            functions_from_scope(&tree, &HashMap::default(), &PackedDimensions::default())
                .unwrap()
                .remove("t")
                .unwrap();
        assert!(function.body.is_some());
        assert_eq!(function.outputs[0].0, "y");
        for input in ["4'd7", "4'bxxxx"] {
            assert_eq!(
                evaluate(
                    function.outputs[0].1.clone(),
                    &HashMap::from_iter([("x".into(), Expr::Literal(input.into()))])
                ),
                0
            );
        }
    }
    #[test]
    fn non_inlineable_statements_after_a_return_still_keep_signature_only_contract() {
        let source = source(
            "module M;function automatic int f(input int c);return c;for(int i=0;i<2;i++)c=c+1;endfunction endmodule",
        );
        assert!(source.modules[0].symbolic_functions["f"].body.is_none());
    }
    #[test]
    fn deep_normalized_control_uses_jobs_on_the_default_thread_stack() {
        let source = source("module M;function automatic int f();return 7;endfunction endmodule");
        let mut function = source.modules[0].subroutines[0].clone();
        let mut statements = Vec::new();
        for _ in 0..1000 {
            statements = vec![Stmt::If {
                condition: Expr::Literal("1".into()),
                then_body: statements,
                else_body: Vec::new(),
            }];
        }
        statements.push(Stmt::Return(Some(Expr::Literal("7".into()))));
        function.body = statements;
        let (value, _) = body(&function, &[]).unwrap();
        assert_eq!(evaluate(value, &HashMap::default()), 7);
    }
    #[test]
    fn duplicate_native_formals_keep_the_existing_function_argument_diagnostic() {
        let tree=crate::source_tree::parse_scope_fixture("module M(output wire y);function logic f(input logic a,input logic a);return a;endfunction assign y=f(1'b0,1'b1);endmodule",Path::new("native-duplicate-formals.sv")).unwrap();
        let error = Source::from_syntax(&tree).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("duplicate function argument `a`"),
            "{error}"
        );
    }
}
