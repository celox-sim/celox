//! Which expressions, calls and statements have runtime effects, need eager evaluation or read nonlocal state.

use super::*;

impl<'a> FfParser<'a> {
    pub(super) fn expression_has_runtime_effect(&self, expr: &Expression) -> bool {
        self.expression_has_runtime_effect_inner(expr, &mut HashSet::default())
    }

    pub(super) fn expression_needs_eager_evaluation(&self, expr: &Expression) -> bool {
        super::expression::expression_has_side_effect(expr)
            || self.expression_has_runtime_effect(expr)
    }

    pub(super) fn expression_needs_runtime_materialization(&self, expr: &Expression) -> bool {
        self.expression_needs_eager_evaluation(expr)
            || self.expression_needs_assignment_snapshot(expr)
    }

    pub(super) fn expression_needs_assignment_snapshot(&self, expr: &Expression) -> bool {
        self.expression_needs_assignment_snapshot_inner(expr, &mut HashSet::default())
    }

    fn expression_needs_assignment_snapshot_inner(
        &self,
        expr: &Expression,
        visiting: &mut HashSet<VarId>,
    ) -> bool {
        let mut input_needs_snapshot = |input: &veryl_analyzer::ir::SystemFunctionInput| {
            self.expression_needs_assignment_snapshot_inner(&input.0, visiting)
        };
        match expr {
            Expression::Term(factor) => match factor.as_ref() {
                Factor::Variable(id, index, select, _) => {
                    self.module.variables[id].affiliation != Affiliation::Function
                        || !index.0.is_empty()
                        || !select.0.is_empty()
                        || select.1.is_some()
                }
                Factor::HierVariable(_) => true,
                Factor::FunctionCall(call) => {
                    call.inputs
                        .values()
                        .any(|expr| self.expression_needs_assignment_snapshot_inner(expr, visiting))
                        || self.function_call_reads_nonlocal(call, visiting)
                }
                Factor::SystemFunctionCall(call) => match &call.kind {
                    SystemFunctionKind::Display(args) | SystemFunctionKind::Write(args) => {
                        args.iter().any(input_needs_snapshot)
                    }
                    SystemFunctionKind::Assert { cond, args, .. } => {
                        input_needs_snapshot(cond) || args.iter().any(input_needs_snapshot)
                    }
                    SystemFunctionKind::Bits(_) | SystemFunctionKind::Size(..) => false,
                    SystemFunctionKind::Clog2(input)
                    | SystemFunctionKind::Onehot(input)
                    | SystemFunctionKind::Signed(input)
                    | SystemFunctionKind::Unsigned(input) => input_needs_snapshot(input),
                    SystemFunctionKind::Readmemh(_, _) | SystemFunctionKind::Finish => false,
                },
                Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => false,
            },
            Expression::Binary(lhs, _, rhs, _) => {
                self.expression_needs_assignment_snapshot_inner(lhs, visiting)
                    || self.expression_needs_assignment_snapshot_inner(rhs, visiting)
            }
            Expression::Unary(_, inner, _) => {
                self.expression_needs_assignment_snapshot_inner(inner, visiting)
            }
            Expression::Ternary(cond, then_expr, else_expr, _) => {
                self.expression_needs_assignment_snapshot_inner(cond, visiting)
                    || self.expression_needs_assignment_snapshot_inner(then_expr, visiting)
                    || self.expression_needs_assignment_snapshot_inner(else_expr, visiting)
            }
            Expression::Concatenation(items, _) => items.iter().any(|(expr, repeat)| {
                self.expression_needs_assignment_snapshot_inner(expr, visiting)
                    || repeat.as_ref().is_some_and(|repeat| {
                        self.expression_needs_assignment_snapshot_inner(repeat, visiting)
                    })
            }),
            Expression::ArrayLiteral(items, _) => items.iter().any(|item| match item {
                ArrayLiteralItem::Value(expr, repeat) => {
                    self.expression_needs_assignment_snapshot_inner(expr, visiting)
                        || repeat.as_ref().is_some_and(|repeat| {
                            self.expression_needs_assignment_snapshot_inner(repeat, visiting)
                        })
                }
                ArrayLiteralItem::Defaul(expr) => {
                    self.expression_needs_assignment_snapshot_inner(expr, visiting)
                }
            }),
            Expression::StructConstructor(_, fields, _) => fields
                .iter()
                .any(|(_, expr)| self.expression_needs_assignment_snapshot_inner(expr, visiting)),
        }
    }

    fn assignment_destination_reads_nonlocal(
        &self,
        dst: &AssignDestination,
        visiting: &mut HashSet<VarId>,
    ) -> bool {
        dst.index
            .0
            .iter()
            .any(|expr| self.expression_needs_assignment_snapshot_inner(expr, visiting))
            || dst
                .select
                .0
                .iter()
                .any(|expr| self.expression_needs_assignment_snapshot_inner(expr, visiting))
            || dst.select.1.as_ref().is_some_and(|(_, expr)| {
                self.expression_needs_assignment_snapshot_inner(expr, visiting)
            })
    }

    fn function_call_reads_nonlocal(
        &self,
        call: &veryl_analyzer::ir::FunctionCall,
        visiting: &mut HashSet<VarId>,
    ) -> bool {
        if !visiting.insert(call.id) {
            return false;
        }
        let reads = self
            .module
            .functions
            .get(&call.id)
            .and_then(|function| {
                if let Some(index) = &call.index {
                    function.get_function(index)
                } else {
                    function.get_function(&[])
                }
            })
            .is_none_or(|body| self.statements_read_nonlocal(&body.statements, visiting));
        visiting.remove(&call.id);
        reads
    }

    fn statements_read_nonlocal(
        &self,
        statements: &[Statement],
        visiting: &mut HashSet<VarId>,
    ) -> bool {
        statements.iter().any(|statement| match statement {
            Statement::Assign(assign) => {
                self.expression_needs_assignment_snapshot_inner(&assign.expr, visiting)
                    || assign
                        .dst
                        .iter()
                        .any(|dst| self.assignment_destination_reads_nonlocal(dst, visiting))
            }
            Statement::If(statement) => {
                self.expression_needs_assignment_snapshot_inner(&statement.cond, visiting)
                    || self.statements_read_nonlocal(&statement.true_side, visiting)
                    || self.statements_read_nonlocal(&statement.false_side, visiting)
            }
            Statement::Case(statement) => {
                self.expression_needs_assignment_snapshot_inner(&statement.case_target, visiting)
                    || statement.arms.iter().any(|arm| {
                        arm.patterns.iter().any(|pattern| match pattern {
                            CasePattern::Eq(expr) => {
                                self.expression_needs_assignment_snapshot_inner(expr, visiting)
                            }
                            CasePattern::Range { lo, hi, .. } => {
                                self.expression_needs_assignment_snapshot_inner(lo, visiting)
                                    || self.expression_needs_assignment_snapshot_inner(hi, visiting)
                            }
                        }) || self.statements_read_nonlocal(&arm.body, visiting)
                    })
                    || self.statements_read_nonlocal(&statement.default, visiting)
            }
            Statement::For(statement) => {
                let (start, end) = match &statement.range {
                    ForRange::Forward { start, end, .. }
                    | ForRange::Reverse { start, end, .. }
                    | ForRange::Stepped { start, end, .. } => (start, end),
                };
                [start, end].into_iter().any(|bound| match bound {
                    ForBound::Const(..) => false,
                    ForBound::Expression(expr) => {
                        self.expression_needs_assignment_snapshot_inner(expr, visiting)
                    }
                }) || self.statements_read_nonlocal(&statement.body, visiting)
            }
            Statement::FunctionCall(call) => {
                call.inputs
                    .values()
                    .any(|expr| self.expression_needs_assignment_snapshot_inner(expr, visiting))
                    || call
                        .outputs
                        .values()
                        .flatten()
                        .any(|dst| self.assignment_destination_reads_nonlocal(dst, visiting))
                    || self.function_call_reads_nonlocal(call, visiting)
            }
            Statement::SystemFunctionCall(call) => match &call.kind {
                SystemFunctionKind::Display(args) | SystemFunctionKind::Write(args) => args
                    .iter()
                    .any(|arg| self.expression_needs_assignment_snapshot_inner(&arg.0, visiting)),
                SystemFunctionKind::Assert { cond, args, .. } => {
                    self.expression_needs_assignment_snapshot_inner(&cond.0, visiting)
                        || args.iter().any(|arg| {
                            self.expression_needs_assignment_snapshot_inner(&arg.0, visiting)
                        })
                }
                SystemFunctionKind::Bits(_) | SystemFunctionKind::Size(..) => false,
                SystemFunctionKind::Clog2(input)
                | SystemFunctionKind::Onehot(input)
                | SystemFunctionKind::Signed(input)
                | SystemFunctionKind::Unsigned(input) => {
                    self.expression_needs_assignment_snapshot_inner(&input.0, visiting)
                }
                SystemFunctionKind::Readmemh(_, _) | SystemFunctionKind::Finish => false,
            },
            Statement::IfReset(statement) => {
                self.statements_read_nonlocal(&statement.true_side, visiting)
                    || self.statements_read_nonlocal(&statement.false_side, visiting)
            }
            Statement::TbMethodCall(_)
            | Statement::Break
            | Statement::Unsupported(_)
            | Statement::Null => false,
        })
    }

    pub(super) fn expression_has_runtime_effect_inner(
        &self,
        expr: &Expression,
        visiting: &mut HashSet<VarId>,
    ) -> bool {
        match expr {
            Expression::Term(factor) => {
                match factor.as_ref() {
                    Factor::Variable(_, index, select, _) => {
                        index
                            .0
                            .iter()
                            .any(|expr| self.expression_has_runtime_effect_inner(expr, visiting))
                            || select.0.iter().any(|expr| {
                                self.expression_has_runtime_effect_inner(expr, visiting)
                            })
                            || select.1.as_ref().is_some_and(|(_, expr)| {
                                self.expression_has_runtime_effect_inner(expr, visiting)
                            })
                    }
                    Factor::HierVariable(reference) => {
                        reference
                            .index
                            .0
                            .iter()
                            .any(|expr| self.expression_has_runtime_effect_inner(expr, visiting))
                            || reference.select.0.iter().any(|expr| {
                                self.expression_has_runtime_effect_inner(expr, visiting)
                            })
                            || reference.select.1.as_ref().is_some_and(|(_, expr)| {
                                self.expression_has_runtime_effect_inner(expr, visiting)
                            })
                    }
                    Factor::FunctionCall(call) => {
                        call.inputs
                            .values()
                            .any(|expr| self.expression_has_runtime_effect_inner(expr, visiting))
                            || call.outputs.values().flatten().any(|dst| {
                                self.module.variables[&dst.id].affiliation != Affiliation::Function
                                    || self.assignment_destination_has_runtime_effect(dst, visiting)
                            })
                            || self.function_call_has_runtime_effect(call, visiting)
                    }
                    Factor::SystemFunctionCall(call) => match &call.kind {
                        veryl_analyzer::ir::SystemFunctionKind::Bits(_)
                        | veryl_analyzer::ir::SystemFunctionKind::Size(..) => false,
                        veryl_analyzer::ir::SystemFunctionKind::Clog2(input)
                        | veryl_analyzer::ir::SystemFunctionKind::Onehot(input)
                        | veryl_analyzer::ir::SystemFunctionKind::Signed(input)
                        | veryl_analyzer::ir::SystemFunctionKind::Unsigned(input) => {
                            self.expression_has_runtime_effect_inner(&input.0, visiting)
                        }
                        _ => true,
                    },
                    Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => false,
                }
            }
            Expression::Binary(lhs, _, rhs, _) => {
                self.expression_has_runtime_effect_inner(lhs, visiting)
                    || self.expression_has_runtime_effect_inner(rhs, visiting)
            }
            Expression::Unary(_, inner, _) => {
                self.expression_has_runtime_effect_inner(inner, visiting)
            }
            Expression::Ternary(cond, then_expr, else_expr, _) => {
                self.expression_has_runtime_effect_inner(cond, visiting)
                    || self.expression_has_runtime_effect_inner(then_expr, visiting)
                    || self.expression_has_runtime_effect_inner(else_expr, visiting)
            }
            Expression::Concatenation(items, _) => items.iter().any(|(expr, repeat)| {
                self.expression_has_runtime_effect_inner(expr, visiting)
                    || repeat.as_ref().is_some_and(|repeat| {
                        self.expression_has_runtime_effect_inner(repeat, visiting)
                    })
            }),
            Expression::ArrayLiteral(items, _) => items.iter().any(|item| match item {
                ArrayLiteralItem::Value(expr, repeat) => {
                    self.expression_has_runtime_effect_inner(expr, visiting)
                        || repeat.as_ref().is_some_and(|repeat| {
                            self.expression_has_runtime_effect_inner(repeat, visiting)
                        })
                }
                ArrayLiteralItem::Defaul(expr) => {
                    self.expression_has_runtime_effect_inner(expr, visiting)
                }
            }),
            Expression::StructConstructor(_, fields, _) => fields
                .iter()
                .any(|(_, expr)| self.expression_has_runtime_effect_inner(expr, visiting)),
        }
    }

    pub(super) fn function_call_has_runtime_effect(
        &self,
        call: &veryl_analyzer::ir::FunctionCall,
        visiting: &mut HashSet<VarId>,
    ) -> bool {
        if !visiting.insert(call.id) {
            return false;
        }
        let result = self
            .module
            .functions
            .get(&call.id)
            .and_then(|function| {
                if let Some(index) = &call.index {
                    function.get_function(index)
                } else {
                    function.get_function(&[])
                }
            })
            .is_some_and(|body| self.statements_have_runtime_effect(&body.statements, visiting));
        visiting.remove(&call.id);
        result
    }

    fn assignment_destination_has_runtime_effect(
        &self,
        dst: &AssignDestination,
        visiting: &mut HashSet<VarId>,
    ) -> bool {
        dst.index
            .0
            .iter()
            .any(|expr| self.expression_has_runtime_effect_inner(expr, visiting))
            || dst
                .select
                .0
                .iter()
                .any(|expr| self.expression_has_runtime_effect_inner(expr, visiting))
            || dst
                .select
                .1
                .as_ref()
                .is_some_and(|(_, expr)| self.expression_has_runtime_effect_inner(expr, visiting))
    }

    pub(super) fn assignment_destination_needs_eager_evaluation(
        &self,
        dst: &AssignDestination,
    ) -> bool {
        dst.index
            .0
            .iter()
            .any(|expr| self.expression_needs_eager_evaluation(expr))
            || dst
                .select
                .0
                .iter()
                .any(|expr| self.expression_needs_eager_evaluation(expr))
            || dst
                .select
                .1
                .as_ref()
                .is_some_and(|(_, expr)| self.expression_needs_eager_evaluation(expr))
    }

    pub(super) fn statements_have_runtime_effect(
        &self,
        statements: &[Statement],
        visiting: &mut HashSet<VarId>,
    ) -> bool {
        statements.iter().any(|statement| match statement {
            Statement::SystemFunctionCall(_) => true,
            Statement::Assign(assign) => {
                let writes_nonlocal = assign
                    .dst
                    .iter()
                    .any(|dst| self.module.variables[&dst.id].affiliation != Affiliation::Function);
                let destination_effect = assign
                    .dst
                    .iter()
                    .any(|dst| self.assignment_destination_has_runtime_effect(dst, visiting));
                writes_nonlocal
                    || destination_effect
                    || self.expression_has_runtime_effect_inner(&assign.expr, visiting)
            }
            Statement::If(statement) => {
                self.expression_has_runtime_effect_inner(&statement.cond, visiting)
                    || self.statements_have_runtime_effect(&statement.true_side, visiting)
                    || self.statements_have_runtime_effect(&statement.false_side, visiting)
            }
            Statement::Case(statement) => {
                let pattern_effect = statement.arms.iter().any(|arm| {
                    arm.patterns.iter().any(|pattern| match pattern {
                        CasePattern::Eq(expr) => {
                            self.expression_has_runtime_effect_inner(expr, visiting)
                        }
                        CasePattern::Range { lo, hi, .. } => {
                            self.expression_has_runtime_effect_inner(lo, visiting)
                                || self.expression_has_runtime_effect_inner(hi, visiting)
                        }
                    })
                });
                self.expression_has_runtime_effect_inner(&statement.case_target, visiting)
                    || pattern_effect
                    || statement
                        .arms
                        .iter()
                        .any(|arm| self.statements_have_runtime_effect(&arm.body, visiting))
                    || self.statements_have_runtime_effect(&statement.default, visiting)
            }
            Statement::For(statement) => {
                let (start, end) = match &statement.range {
                    ForRange::Forward { start, end, .. }
                    | ForRange::Reverse { start, end, .. }
                    | ForRange::Stepped { start, end, .. } => (start, end),
                };
                let bound_effect = [start, end].into_iter().any(|bound| match bound {
                    ForBound::Const(..) => false,
                    ForBound::Expression(expr) => {
                        self.expression_has_runtime_effect_inner(expr, visiting)
                    }
                });
                bound_effect || self.statements_have_runtime_effect(&statement.body, visiting)
            }
            Statement::FunctionCall(call) => {
                call.inputs
                    .values()
                    .any(|expr| self.expression_has_runtime_effect_inner(expr, visiting))
                    || call.outputs.values().flatten().any(|dst| {
                        self.module.variables[&dst.id].affiliation != Affiliation::Function
                            || self.assignment_destination_has_runtime_effect(dst, visiting)
                    })
                    || self.function_call_has_runtime_effect(call, visiting)
            }
            Statement::IfReset(statement) => {
                self.statements_have_runtime_effect(&statement.true_side, visiting)
                    || self.statements_have_runtime_effect(&statement.false_side, visiting)
            }
            Statement::TbMethodCall(_)
            | Statement::Break
            | Statement::Unsupported(_)
            | Statement::Null => false,
        })
    }
}
