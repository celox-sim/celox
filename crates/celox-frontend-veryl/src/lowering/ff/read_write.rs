//! Which variables an expression, call or statement list references, reads or writes.

use super::*;

impl<'a> FfParser<'a> {
    pub(super) fn expression_references_any(
        expr: &Expression,
        candidates: &HashSet<VarId>,
    ) -> bool {
        let input_references = |input: &veryl_analyzer::ir::SystemFunctionInput| {
            Self::expression_references_any(&input.0, candidates)
        };
        match expr {
            Expression::Term(factor) => match factor.as_ref() {
                Factor::Variable(id, index, select, _) => {
                    candidates.contains(id)
                        || index
                            .0
                            .iter()
                            .any(|expr| Self::expression_references_any(expr, candidates))
                        || select
                            .0
                            .iter()
                            .any(|expr| Self::expression_references_any(expr, candidates))
                        || select.1.as_ref().is_some_and(|(_, expr)| {
                            Self::expression_references_any(expr, candidates)
                        })
                }
                Factor::HierVariable(reference) => {
                    reference
                        .index
                        .0
                        .iter()
                        .any(|expr| Self::expression_references_any(expr, candidates))
                        || reference
                            .select
                            .0
                            .iter()
                            .any(|expr| Self::expression_references_any(expr, candidates))
                        || reference.select.1.as_ref().is_some_and(|(_, expr)| {
                            Self::expression_references_any(expr, candidates)
                        })
                }
                Factor::FunctionCall(call) => call
                    .inputs
                    .values()
                    .any(|expr| Self::expression_references_any(expr, candidates)),
                Factor::SystemFunctionCall(call) => match &call.kind {
                    SystemFunctionKind::Display(args) | SystemFunctionKind::Write(args) => {
                        args.iter().any(input_references)
                    }
                    SystemFunctionKind::Assert { cond, args, .. } => {
                        input_references(cond) || args.iter().any(input_references)
                    }
                    SystemFunctionKind::Bits(_) | SystemFunctionKind::Size(..) => false,
                    SystemFunctionKind::Clog2(input)
                    | SystemFunctionKind::Onehot(input)
                    | SystemFunctionKind::Signed(input)
                    | SystemFunctionKind::Unsigned(input) => input_references(input),
                    SystemFunctionKind::Readmemh(_, _) | SystemFunctionKind::Finish => false,
                },
                Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => false,
            },
            Expression::Binary(lhs, _, rhs, _) => {
                Self::expression_references_any(lhs, candidates)
                    || Self::expression_references_any(rhs, candidates)
            }
            Expression::Unary(_, inner, _) => Self::expression_references_any(inner, candidates),
            Expression::Ternary(cond, then_expr, else_expr, _) => {
                Self::expression_references_any(cond, candidates)
                    || Self::expression_references_any(then_expr, candidates)
                    || Self::expression_references_any(else_expr, candidates)
            }
            Expression::Concatenation(items, _) => items.iter().any(|(expr, repeat)| {
                Self::expression_references_any(expr, candidates)
                    || repeat
                        .as_ref()
                        .is_some_and(|repeat| Self::expression_references_any(repeat, candidates))
            }),
            Expression::ArrayLiteral(items, _) => items.iter().any(|item| match item {
                ArrayLiteralItem::Value(expr, repeat) => {
                    Self::expression_references_any(expr, candidates)
                        || repeat.as_ref().is_some_and(|repeat| {
                            Self::expression_references_any(repeat, candidates)
                        })
                }
                ArrayLiteralItem::Defaul(expr) => Self::expression_references_any(expr, candidates),
            }),
            Expression::StructConstructor(_, fields, _) => fields
                .iter()
                .any(|(_, expr)| Self::expression_references_any(expr, candidates)),
        }
    }

    pub(super) fn expression_contains_function_call(expr: &Expression) -> bool {
        let input_contains = |input: &veryl_analyzer::ir::SystemFunctionInput| {
            Self::expression_contains_function_call(&input.0)
        };
        match expr {
            Expression::Term(factor) => {
                match factor.as_ref() {
                    Factor::FunctionCall(_) => true,
                    Factor::Variable(_, index, select, _) => {
                        index.0.iter().any(Self::expression_contains_function_call)
                            || select.0.iter().any(Self::expression_contains_function_call)
                            || select.1.as_ref().is_some_and(|(_, expr)| {
                                Self::expression_contains_function_call(expr)
                            })
                    }
                    Factor::HierVariable(reference) => {
                        reference
                            .index
                            .0
                            .iter()
                            .any(Self::expression_contains_function_call)
                            || reference
                                .select
                                .0
                                .iter()
                                .any(Self::expression_contains_function_call)
                            || reference.select.1.as_ref().is_some_and(|(_, expr)| {
                                Self::expression_contains_function_call(expr)
                            })
                    }
                    Factor::SystemFunctionCall(call) => match &call.kind {
                        SystemFunctionKind::Display(args) | SystemFunctionKind::Write(args) => {
                            args.iter().any(input_contains)
                        }
                        SystemFunctionKind::Assert { cond, args, .. } => {
                            input_contains(cond) || args.iter().any(input_contains)
                        }
                        SystemFunctionKind::Bits(_) | SystemFunctionKind::Size(..) => false,
                        SystemFunctionKind::Clog2(input)
                        | SystemFunctionKind::Onehot(input)
                        | SystemFunctionKind::Signed(input)
                        | SystemFunctionKind::Unsigned(input) => input_contains(input),
                        SystemFunctionKind::Readmemh(_, _) | SystemFunctionKind::Finish => false,
                    },
                    Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => false,
                }
            }
            Expression::Binary(lhs, _, rhs, _) => {
                Self::expression_contains_function_call(lhs)
                    || Self::expression_contains_function_call(rhs)
            }
            Expression::Unary(_, inner, _) => Self::expression_contains_function_call(inner),
            Expression::Ternary(cond, then_expr, else_expr, _) => {
                Self::expression_contains_function_call(cond)
                    || Self::expression_contains_function_call(then_expr)
                    || Self::expression_contains_function_call(else_expr)
            }
            Expression::Concatenation(items, _) => items.iter().any(|(expr, repeat)| {
                Self::expression_contains_function_call(expr)
                    || repeat
                        .as_ref()
                        .is_some_and(Self::expression_contains_function_call)
            }),
            Expression::ArrayLiteral(items, _) => items.iter().any(|item| match item {
                ArrayLiteralItem::Value(expr, repeat) => {
                    Self::expression_contains_function_call(expr)
                        || repeat
                            .as_ref()
                            .is_some_and(|repeat| Self::expression_contains_function_call(repeat))
                }
                ArrayLiteralItem::Defaul(expr) => Self::expression_contains_function_call(expr),
            }),
            Expression::StructConstructor(_, fields, _) => fields
                .iter()
                .any(|(_, expr)| Self::expression_contains_function_call(expr)),
        }
    }

    pub(super) fn collect_expression_read_variables(
        &self,
        expr: &Expression,
        variables: &mut HashSet<VarId>,
        arrays_only: bool,
    ) {
        let collect_input = |input: &veryl_analyzer::ir::SystemFunctionInput,
                             variables: &mut HashSet<VarId>| {
            self.collect_expression_read_variables(&input.0, variables, arrays_only)
        };
        match expr {
            Expression::Term(factor) => match factor.as_ref() {
                Factor::Variable(id, index, select, _) => {
                    if !arrays_only
                        || self
                            .module
                            .variables
                            .get(id)
                            .is_some_and(|variable| !variable.r#type.array.is_empty())
                    {
                        variables.insert(*id);
                    }
                    for expr in &index.0 {
                        self.collect_expression_read_variables(expr, variables, arrays_only);
                    }
                    for expr in &select.0 {
                        self.collect_expression_read_variables(expr, variables, arrays_only);
                    }
                    if let Some((_, expr)) = &select.1 {
                        self.collect_expression_read_variables(expr, variables, arrays_only);
                    }
                }
                Factor::HierVariable(reference) => {
                    for expr in &reference.index.0 {
                        self.collect_expression_read_variables(expr, variables, arrays_only);
                    }
                    for expr in &reference.select.0 {
                        self.collect_expression_read_variables(expr, variables, arrays_only);
                    }
                    if let Some((_, expr)) = &reference.select.1 {
                        self.collect_expression_read_variables(expr, variables, arrays_only);
                    }
                }
                Factor::FunctionCall(call) => {
                    for expr in call.inputs.values() {
                        self.collect_expression_read_variables(expr, variables, arrays_only);
                    }
                    for dst in call.outputs.values().flatten() {
                        // An output actual writes its base variable; only its
                        // index/select expressions contribute read dependencies.
                        for expr in &dst.index.0 {
                            self.collect_expression_read_variables(expr, variables, arrays_only);
                        }
                        for expr in &dst.select.0 {
                            self.collect_expression_read_variables(expr, variables, arrays_only);
                        }
                        if let Some((_, expr)) = &dst.select.1 {
                            self.collect_expression_read_variables(expr, variables, arrays_only);
                        }
                    }
                }
                Factor::SystemFunctionCall(call) => match &call.kind {
                    SystemFunctionKind::Display(args) | SystemFunctionKind::Write(args) => {
                        for arg in args {
                            collect_input(arg, variables);
                        }
                    }
                    SystemFunctionKind::Assert { cond, args, .. } => {
                        collect_input(cond, variables);
                        for arg in args {
                            collect_input(arg, variables);
                        }
                    }
                    SystemFunctionKind::Bits(_) | SystemFunctionKind::Size(..) => {}
                    SystemFunctionKind::Clog2(input)
                    | SystemFunctionKind::Onehot(input)
                    | SystemFunctionKind::Signed(input)
                    | SystemFunctionKind::Unsigned(input) => collect_input(input, variables),
                    SystemFunctionKind::Readmemh(_, _) | SystemFunctionKind::Finish => {}
                },
                Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => {}
            },
            Expression::Binary(lhs, _, rhs, _) => {
                self.collect_expression_read_variables(lhs, variables, arrays_only);
                self.collect_expression_read_variables(rhs, variables, arrays_only);
            }
            Expression::Unary(_, inner, _) => {
                self.collect_expression_read_variables(inner, variables, arrays_only)
            }
            Expression::Ternary(cond, then_expr, else_expr, _) => {
                self.collect_expression_read_variables(cond, variables, arrays_only);
                self.collect_expression_read_variables(then_expr, variables, arrays_only);
                self.collect_expression_read_variables(else_expr, variables, arrays_only);
            }
            Expression::Concatenation(items, _) => {
                for (expr, repeat) in items {
                    self.collect_expression_read_variables(expr, variables, arrays_only);
                    if let Some(repeat) = repeat {
                        self.collect_expression_read_variables(repeat, variables, arrays_only);
                    }
                }
            }
            Expression::ArrayLiteral(items, _) => {
                for item in items {
                    match item {
                        ArrayLiteralItem::Value(expr, repeat) => {
                            self.collect_expression_read_variables(expr, variables, arrays_only);
                            if let Some(repeat) = repeat {
                                self.collect_expression_read_variables(
                                    repeat,
                                    variables,
                                    arrays_only,
                                );
                            }
                        }
                        ArrayLiteralItem::Defaul(expr) => {
                            self.collect_expression_read_variables(expr, variables, arrays_only);
                        }
                    }
                }
            }
            Expression::StructConstructor(_, fields, _) => {
                for (_, expr) in fields {
                    self.collect_expression_read_variables(expr, variables, arrays_only);
                }
            }
        }
    }

    pub(super) fn expression_writes_any(
        &self,
        expr: &Expression,
        candidates: &HashSet<VarId>,
    ) -> bool {
        self.expression_writes_any_inner(expr, candidates, &mut HashSet::default())
    }

    fn expression_writes_any_inner(
        &self,
        expr: &Expression,
        candidates: &HashSet<VarId>,
        visiting: &mut HashSet<VarId>,
    ) -> bool {
        match expr {
            Expression::Term(factor) => {
                match factor.as_ref() {
                    Factor::FunctionCall(call) => {
                        self.function_call_writes_any(call, candidates, visiting)
                    }
                    Factor::Variable(_, index, select, _) => {
                        index.0.iter().any(|expr| {
                            self.expression_writes_any_inner(expr, candidates, visiting)
                        }) || select.0.iter().any(|expr| {
                            self.expression_writes_any_inner(expr, candidates, visiting)
                        }) || select.1.as_ref().is_some_and(|(_, expr)| {
                            self.expression_writes_any_inner(expr, candidates, visiting)
                        })
                    }
                    Factor::HierVariable(reference) => {
                        reference.index.0.iter().any(|expr| {
                            self.expression_writes_any_inner(expr, candidates, visiting)
                        }) || reference.select.0.iter().any(|expr| {
                            self.expression_writes_any_inner(expr, candidates, visiting)
                        }) || reference.select.1.as_ref().is_some_and(|(_, expr)| {
                            self.expression_writes_any_inner(expr, candidates, visiting)
                        })
                    }
                    Factor::SystemFunctionCall(call) => match &call.kind {
                        SystemFunctionKind::Display(args) | SystemFunctionKind::Write(args) => {
                            args.iter().any(|arg| {
                                self.expression_writes_any_inner(&arg.0, candidates, visiting)
                            })
                        }
                        SystemFunctionKind::Assert { cond, args, .. } => {
                            self.expression_writes_any_inner(&cond.0, candidates, visiting)
                                || args.iter().any(|arg| {
                                    self.expression_writes_any_inner(&arg.0, candidates, visiting)
                                })
                        }
                        SystemFunctionKind::Bits(_) | SystemFunctionKind::Size(..) => false,
                        SystemFunctionKind::Clog2(input)
                        | SystemFunctionKind::Onehot(input)
                        | SystemFunctionKind::Signed(input)
                        | SystemFunctionKind::Unsigned(input) => {
                            self.expression_writes_any_inner(&input.0, candidates, visiting)
                        }
                        SystemFunctionKind::Readmemh(_, _) | SystemFunctionKind::Finish => false,
                    },
                    Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => false,
                }
            }
            Expression::Binary(lhs, _, rhs, _) => {
                self.expression_writes_any_inner(lhs, candidates, visiting)
                    || self.expression_writes_any_inner(rhs, candidates, visiting)
            }
            Expression::Unary(_, inner, _) => {
                self.expression_writes_any_inner(inner, candidates, visiting)
            }
            Expression::Ternary(cond, then_expr, else_expr, _) => {
                self.expression_writes_any_inner(cond, candidates, visiting)
                    || self.expression_writes_any_inner(then_expr, candidates, visiting)
                    || self.expression_writes_any_inner(else_expr, candidates, visiting)
            }
            Expression::Concatenation(items, _) => items.iter().any(|(expr, repeat)| {
                self.expression_writes_any_inner(expr, candidates, visiting)
                    || repeat.as_ref().is_some_and(|repeat| {
                        self.expression_writes_any_inner(repeat, candidates, visiting)
                    })
            }),
            Expression::ArrayLiteral(items, _) => items.iter().any(|item| match item {
                ArrayLiteralItem::Value(expr, repeat) => {
                    self.expression_writes_any_inner(expr, candidates, visiting)
                        || repeat.as_ref().is_some_and(|repeat| {
                            self.expression_writes_any_inner(repeat, candidates, visiting)
                        })
                }
                ArrayLiteralItem::Defaul(expr) => {
                    self.expression_writes_any_inner(expr, candidates, visiting)
                }
            }),
            Expression::StructConstructor(_, fields, _) => fields
                .iter()
                .any(|(_, expr)| self.expression_writes_any_inner(expr, candidates, visiting)),
        }
    }

    fn function_call_writes_any(
        &self,
        call: &veryl_analyzer::ir::FunctionCall,
        candidates: &HashSet<VarId>,
        visiting: &mut HashSet<VarId>,
    ) -> bool {
        if call.outputs.values().flatten().any(|dst| {
            candidates.contains(&dst.id)
                || self.assignment_destination_writes_any_inner(dst, candidates, visiting)
        }) || call
            .inputs
            .values()
            .any(|expr| self.expression_writes_any_inner(expr, candidates, visiting))
        {
            return true;
        }
        if !visiting.insert(call.id) {
            return false;
        }
        let writes = self
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
            .is_some_and(|body| self.statements_write_any(&body.statements, candidates, visiting));
        visiting.remove(&call.id);
        writes
    }

    pub(super) fn statements_write_any(
        &self,
        statements: &[Statement],
        candidates: &HashSet<VarId>,
        visiting: &mut HashSet<VarId>,
    ) -> bool {
        statements.iter().any(|statement| match statement {
            Statement::Assign(assign) => {
                assign.dst.iter().any(|dst| {
                    candidates.contains(&dst.id)
                        || self.assignment_destination_writes_any_inner(dst, candidates, visiting)
                }) || self.expression_writes_any_inner(&assign.expr, candidates, visiting)
            }
            Statement::If(statement) => {
                self.expression_writes_any_inner(&statement.cond, candidates, visiting)
                    || self.statements_write_any(&statement.true_side, candidates, visiting)
                    || self.statements_write_any(&statement.false_side, candidates, visiting)
            }
            Statement::Case(statement) => {
                self.expression_writes_any_inner(&statement.case_target, candidates, visiting)
                    || statement.arms.iter().any(|arm| {
                        arm.patterns.iter().any(|pattern| match pattern {
                            CasePattern::Eq(expr) => {
                                self.expression_writes_any_inner(expr, candidates, visiting)
                            }
                            CasePattern::Range { lo, hi, .. } => {
                                self.expression_writes_any_inner(lo, candidates, visiting)
                                    || self.expression_writes_any_inner(hi, candidates, visiting)
                            }
                        }) || self.statements_write_any(&arm.body, candidates, visiting)
                    })
                    || self.statements_write_any(&statement.default, candidates, visiting)
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
                        self.expression_writes_any_inner(expr, candidates, visiting)
                    }
                }) || self.statements_write_any(&statement.body, candidates, visiting)
            }
            Statement::FunctionCall(call) => {
                self.function_call_writes_any(call, candidates, visiting)
            }
            Statement::SystemFunctionCall(call) => match &call.kind {
                SystemFunctionKind::Display(args) | SystemFunctionKind::Write(args) => args
                    .iter()
                    .any(|arg| self.expression_writes_any_inner(&arg.0, candidates, visiting)),
                SystemFunctionKind::Assert { cond, args, .. } => {
                    self.expression_writes_any_inner(&cond.0, candidates, visiting)
                        || args.iter().any(|arg| {
                            self.expression_writes_any_inner(&arg.0, candidates, visiting)
                        })
                }
                SystemFunctionKind::Bits(_) | SystemFunctionKind::Size(..) => false,
                SystemFunctionKind::Clog2(input)
                | SystemFunctionKind::Onehot(input)
                | SystemFunctionKind::Signed(input)
                | SystemFunctionKind::Unsigned(input) => {
                    self.expression_writes_any_inner(&input.0, candidates, visiting)
                }
                SystemFunctionKind::Readmemh(_, _) | SystemFunctionKind::Finish => false,
            },
            Statement::IfReset(statement) => {
                self.statements_write_any(&statement.true_side, candidates, visiting)
                    || self.statements_write_any(&statement.false_side, candidates, visiting)
            }
            Statement::TbMethodCall(_)
            | Statement::Break
            | Statement::Unsupported(_)
            | Statement::Null => false,
        })
    }

    pub(super) fn assignment_destination_writes_any(
        &self,
        dst: &AssignDestination,
        candidates: &HashSet<VarId>,
    ) -> bool {
        self.assignment_destination_writes_any_inner(dst, candidates, &mut HashSet::default())
    }

    fn assignment_destination_writes_any_inner(
        &self,
        dst: &AssignDestination,
        candidates: &HashSet<VarId>,
        visiting: &mut HashSet<VarId>,
    ) -> bool {
        dst.index
            .0
            .iter()
            .any(|expr| self.expression_writes_any_inner(expr, candidates, visiting))
            || dst
                .select
                .0
                .iter()
                .any(|expr| self.expression_writes_any_inner(expr, candidates, visiting))
            || dst.select.1.as_ref().is_some_and(|(_, expr)| {
                self.expression_writes_any_inner(expr, candidates, visiting)
            })
    }
}
