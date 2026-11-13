//! Conditions and loop headers: constant conditionals, `for` headers and `foreach` nests.
use super::*;

impl BodyBuilder<'_, '_, '_> {
    pub(super) fn fold_conditional(
        &mut self,
        branches: Vec<(Expr, Vec<Stmt>)>,
        mut else_body: Vec<Stmt>,
    ) -> Vec<Stmt> {
        for (condition, then_body) in branches.into_iter().rev() {
            // A generate index or parameter can make a procedural branch
            // unreachable. Remove its writes before checking process drivers.
            let constant = if inlining::calls_subroutine(&condition) {
                // Even a constant return value does not make the run-time
                // call's display, output or module-variable effects optional.
                None
            } else {
                expr_to_const(self.session, condition.clone()).and_then(|condition| {
                    eval_ast_const_expr(&self.tree.functions, &condition, &self.dims.const_env)
                })
            };
            if let Some(value) = constant {
                // Implicit sensitivity is syntactic even for unreachable
                // branches. Preserve reads separately from executable writes.
                self.sensitivity_body.push(Stmt::Eval(condition.clone()));
                if value != 0 {
                    self.sensitivity_body.extend(else_body);
                    else_body = then_body;
                } else {
                    self.sensitivity_body.extend(then_body);
                }
                continue;
            }
            else_body = vec![Stmt::If {
                condition,
                then_body,
                else_body,
            }];
        }
        else_body
    }

    pub(super) fn cond_predicate(
        &mut self,
        predicate: &celox_sv_syntax::Predicate,
    ) -> Result<Expr, AnalyzerError> {
        let [item] = predicate.items.as_slice() else {
            return Err(unsupported("`&&&` in a condition"));
        };
        if item.pattern.is_some() {
            return Err(unsupported("pattern matching in a condition"));
        }
        // A Boolean test, which may be of a chandle (IEEE 1800-2023 6.14).
        let condition = expression_binding::bind_chandle_value(
            item.expression,
            self.tree.declarations(),
            self.tree,
            self.session,
            &self.dims,
            None,
        )?;
        let mut condition =
            statements::simplify_condition(self.tree, self.session, condition, &self.dims);
        self.rename_expr(&mut condition);
        Ok(condition)
    }

    pub(super) fn for_header(
        &mut self,
        header: &celox_sv_syntax::ForHeader,
    ) -> Result<ForHeader, AnalyzerError> {
        use celox_sv_syntax::{ForInitialization, ForStep};
        let mut init = Vec::new();
        match &header.initialization {
            None => {}
            Some(ForInitialization::Declarations(groups)) => {
                for group in groups {
                    let ty = type_binding::procedural_type(
                        group.data_type,
                        self.tree.declarations(),
                        self.tree,
                        self.session,
                        &self.dims.const_env,
                        &self.type_aliases,
                    )?
                    .ok_or_else(|| unsupported("for-loop variable type"))?;
                    for variable in &group.variables {
                        init.extend(self.effects(&[variable.value])?);
                        let source = instances::normalize_identifier(&variable.name.spelling);
                        let target = LValue::Ident(source.clone());
                        let mut scoped = self.dims.clone();
                        scoped.insert(source.clone(), dimensions_from_type(&ty));
                        expression_binding::check_chandle_assignment(
                            ty.is_chandle() && ty.unpacked_ranges.is_empty(),
                            variable.value,
                            self.tree,
                            self.tree.declarations(),
                            &self.dims,
                            self.session,
                        )?;
                        let mut value = expression_binding::expression(
                            variable.value,
                            self.tree.declarations(),
                            &target,
                            self.tree,
                            self.session,
                            &scoped,
                        )?;
                        self.rename_expr(&mut value);
                        let name = self.declare(&source, ty.clone());
                        init.push(Stmt::Local {
                            name,
                            init: Some(value),
                        });
                    }
                }
            }
            Some(ForInitialization::Assignments(values)) => {
                for value in values {
                    init.extend(self.effects(&[value.left, value.right])?);
                    match self.aggregate_property_assignment("=", false, value.left, value.right)? {
                        Some(stmts) => init.extend(stmts),
                        None => init.extend(self.assignment_value(
                            "=",
                            false,
                            (value.left, value.right),
                        )?),
                    }
                }
            }
        }
        let (condition, prologue) = match header.condition {
            None => (None, Vec::new()),
            Some(id) => match self.loop_condition(id)? {
                Some(prologue) => (None, prologue),
                None => (Some(self.native_chandle_value(id)?), Vec::new()),
            },
        };
        let mut steps = Vec::new();
        for step in &header.steps {
            match step {
                ForStep::Assignment {
                    left,
                    operator,
                    right,
                    ..
                } => {
                    steps.extend(self.effects(&[*left, *right])?);
                    match self.aggregate_property_assignment(operator, false, *left, *right)? {
                        Some(stmts) => steps.extend(stmts),
                        None => {
                            steps.extend(self.assignment_value(operator, false, (*left, *right))?)
                        }
                    }
                }
                ForStep::Expression(id) => steps.extend(
                    self.update_statement(*id)?
                        .ok_or_else(|| unsupported("subroutine call as a for-loop step"))?,
                ),
            }
        }
        Ok(ForHeader {
            init,
            condition,
            prologue,
            steps,
        })
    }

    pub(super) fn foreach_bound_indices(
        &mut self,
        array: String,
        variables: Vec<Option<String>>,
    ) -> Result<Vec<ForeachLoop>, AnalyzerError> {
        let dimensions = self
            .dims
            .get(&array)
            .cloned()
            .ok_or_else(|| unsupported(format!("foreach over `{array}`")))?;
        let bounds: Vec<(ConstExpr, ConstExpr)> = dimensions
            .unpacked
            .iter()
            .map(|dimension| (dimension.left.clone(), dimension.right.clone()))
            .chain(
                dimensions
                    .packed
                    .iter()
                    .map(|dimension| (dimension.left.clone(), dimension.right.clone())),
            )
            .collect();
        let mut loops = Vec::new();
        for (position, variable) in variables.into_iter().enumerate() {
            let Some(variable) = variable else {
                continue;
            };
            let (left, right) = bounds
                .get(position)
                .ok_or_else(|| unsupported("foreach index beyond the array dimensions"))?;
            let left = eval_ast_const_expr(&self.tree.functions, left, &self.dims.const_env)
                .ok_or_else(|| unsupported("foreach bound"))?;
            let right = eval_ast_const_expr(&self.tree.functions, right, &self.dims.const_env)
                .ok_or_else(|| unsupported("foreach bound"))?;
            let name = self.declare(&variable, foreach_index_type(left, right));
            loops.push(ForeachLoop::Static(name, left, right));
        }
        Ok(loops)
    }
    /// The nested loops of a foreach, the first dimension outermost. With
    /// `stop`, a `break` of the body sets that local, after which each outer
    /// loop breaks too.
    pub(super) fn fold_foreach(
        loops: Vec<ForeachLoop>,
        mut body: Vec<Stmt>,
        stop: Option<String>,
    ) -> Vec<Stmt> {
        if let Some(stop) = &stop {
            body = control::mark_breaks(body, stop);
        }
        let mut innermost = true;
        for bound in loops.into_iter().rev() {
            if let Some(stop) = stop.as_ref().filter(|_| !innermost) {
                body.push(Stmt::If {
                    condition: Expr::Ident(stop.clone()),
                    then_body: vec![Stmt::Break],
                    else_body: Vec::new(),
                });
            }
            innermost = false;
            let (name, left, right) = match bound {
                ForeachLoop::Static(name, left, right) => (name, left, right),
                ForeachLoop::String { array, index } => {
                    let index_expr = Expr::Ident(index.clone());
                    body = vec![Stmt::Loop {
                        kind: LoopKind::For,
                        init: vec![Stmt::Local {
                            name: index.clone(),
                            init: Some(Expr::Literal("0".into())),
                        }],
                        condition: Some(Expr::Binary {
                            left: Box::new(index_expr.clone()),
                            op: BinaryOp::Lt,
                            right: Box::new(expression_binding::string_call(
                                StringOp::Len,
                                vec![array],
                            )),
                        }),
                        step: vec![Stmt::Assign {
                            lhs: LValue::Ident(index),
                            rhs: Expr::Binary {
                                left: Box::new(index_expr),
                                op: BinaryOp::Add,
                                right: Box::new(Expr::Literal("1".into())),
                            },
                            nonblocking: false,
                        }],
                        body,
                    }];
                    continue;
                }
                ForeachLoop::Container {
                    array,
                    index,
                    layout,
                    status,
                } => {
                    let index_expr = Expr::Ident(index.clone());
                    let (init, condition, step) = if let Some(status) = status {
                        let call = |op| {
                            containers::call(op, layout, vec![array.clone(), index_expr.clone()])
                        };
                        (
                            vec![
                                Stmt::Local {
                                    name: index,
                                    init: None,
                                },
                                Stmt::Local {
                                    name: status.clone(),
                                    init: Some(call(ContainerOp::First)),
                                },
                            ],
                            Expr::Ident(status.clone()),
                            vec![Stmt::Assign {
                                lhs: LValue::Ident(status),
                                rhs: call(ContainerOp::Next),
                                nonblocking: false,
                            }],
                        )
                    } else {
                        (
                            vec![Stmt::Local {
                                name: index.clone(),
                                init: Some(Expr::Literal("0".into())),
                            }],
                            Expr::Binary {
                                left: Box::new(index_expr.clone()),
                                op: BinaryOp::Lt,
                                right: Box::new(containers::call(
                                    ContainerOp::Size,
                                    layout,
                                    vec![array],
                                )),
                            },
                            vec![Stmt::Assign {
                                lhs: LValue::Ident(index),
                                rhs: Expr::Binary {
                                    left: Box::new(index_expr),
                                    op: BinaryOp::Add,
                                    right: Box::new(Expr::Literal("1".into())),
                                },
                                nonblocking: false,
                            }],
                        )
                    };
                    body = vec![Stmt::Loop {
                        kind: LoopKind::For,
                        init,
                        condition: Some(condition),
                        step,
                        body,
                    }];
                    continue;
                }
            };
            let index = Expr::Ident(name.clone());
            // A bound outside `int` keeps its value (see `foreach_index_type`).
            let bound = |value: i128| match i32::try_from(value) {
                Ok(value) => Expr::Literal(value.to_string()),
                Err(_) => Expr::Literal(format!("128'sh{:x}", value as u128)),
            };
            let (compare, step) = if left <= right {
                (BinaryOp::Le, BinaryOp::Add)
            } else {
                (BinaryOp::Ge, BinaryOp::Sub)
            };
            body = vec![Stmt::Loop {
                kind: LoopKind::For,
                init: vec![Stmt::Local {
                    name: name.clone(),
                    init: Some(bound(left)),
                }],
                condition: Some(Expr::Binary {
                    left: Box::new(index.clone()),
                    op: compare,
                    right: Box::new(bound(right)),
                }),
                step: vec![Stmt::Assign {
                    lhs: LValue::Ident(name),
                    rhs: Expr::Binary {
                        left: Box::new(index),
                        op: step,
                        right: Box::new(Expr::Literal("1".to_string())),
                    },
                    nonblocking: false,
                }],
                body,
            }];
        }
        match stop {
            Some(stop) => {
                let mut stmts = vec![Stmt::Local {
                    name: stop,
                    init: Some(Expr::Literal("0".into())),
                }];
                stmts.extend(body);
                stmts
            }
            None => body,
        }
    }
}

/// The type of the index of a foreach over a fixed range from `left` to
/// `right`: an `int` (IEEE 1800-2023 12.7.3), or a signed vector as wide as
/// bounds outside its range need, so the index takes each of them.
pub(super) fn foreach_index_type(left: i128, right: i128) -> Type {
    if i32::try_from(left).is_ok() && i32::try_from(right).is_ok() {
        return int_type();
    }
    // The bits of the bound farthest from zero, and a sign bit.
    let magnitude = |value: i128| 128 - value.unsigned_abs().leading_zeros() + 1;
    let width = magnitude(left).max(magnitude(right)).clamp(33, 128);
    let mut ty = classes::unsigned_type(width as usize);
    ty.is_signed = true;
    ty
}
