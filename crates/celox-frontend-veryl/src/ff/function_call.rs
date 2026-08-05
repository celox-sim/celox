use super::{Domain, FfParser};
use crate::{
    HashMap, HashSet, LoweringPhase, ParserError,
    bitaccess::{
        build_dynamic_partial_assign_expr, build_partial_assign_expr, get_access_width,
        is_static_access,
    },
    case::case_arm_condition_expr,
    context_width::expression_signed,
    function_call_arg, resolve_total_width,
};
use celox_design::VarAtomBase;
use celox_sir::{
    RegisterId, RegisterType, SIRBuilder, SIRInstruction, SIRTerminator, SIRValue, UnaryOp,
};
use num_traits::ToPrimitive;
use veryl_analyzer::ir::{
    ArrayLiteralItem, AssignDestination, AssignStatement, CasePattern, CaseStatement, Comptime,
    Expression, Factor, ForBound, ForRange, Op, Shape, Statement, SystemFunctionCall,
    SystemFunctionKind, Type, TypeKind, ValueVariant, VarId, VarIndex, VarSelect,
};
use veryl_analyzer::symbol::Affiliation;
use veryl_parser::token_range::TokenRange;

#[derive(Clone)]
enum FunctionPathCondition {
    Always,
    Never,
    Conditional(Box<Expression>),
}

fn function_call_arg_mut<'a, T>(
    args: &'a mut [(veryl_analyzer::ir::VarPath, T)],
    path: &veryl_analyzer::ir::VarPath,
) -> Option<&'a mut T> {
    args.iter_mut()
        .find_map(|(candidate, value)| (candidate == path).then_some(value))
}

impl<'a> FfParser<'a> {
    pub(super) fn expression_has_runtime_effect(&self, expr: &Expression) -> bool {
        self.expression_has_runtime_effect_inner(expr, &mut HashSet::default())
    }

    fn expression_needs_eager_evaluation(&self, expr: &Expression) -> bool {
        super::expression::expression_has_side_effect(expr)
            || self.expression_has_runtime_effect(expr)
    }

    fn expression_needs_runtime_materialization(&self, expr: &Expression) -> bool {
        self.expression_needs_eager_evaluation(expr)
            || self.expression_needs_assignment_snapshot(expr)
    }

    fn expression_needs_assignment_snapshot(&self, expr: &Expression) -> bool {
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
                    SystemFunctionKind::Bits(_) | SystemFunctionKind::Size(_) => false,
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
                    ForBound::Const(_) => false,
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
                SystemFunctionKind::Bits(_) | SystemFunctionKind::Size(_) => false,
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

    fn expression_has_runtime_effect_inner(
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
                        | veryl_analyzer::ir::SystemFunctionKind::Size(_) => false,
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

    fn function_call_has_runtime_effect(
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

    fn assignment_destination_needs_eager_evaluation(&self, dst: &AssignDestination) -> bool {
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

    fn assignment_destination_needs_direct_runtime_copyout(&self, dst: &AssignDestination) -> bool {
        self.module.variables[&dst.id].affiliation != Affiliation::Function
            && !is_static_access(&dst.index, &dst.select)
    }

    fn retain_direct_runtime_copyouts(
        &self,
        call: &mut veryl_analyzer::ir::FunctionCall,
        retain_direct: bool,
    ) {
        // Dynamic nonlocal destinations cannot be represented by the
        // whole-variable symbolic state. Leave them on the runtime call so
        // normal store lowering computes their offsets. CallArgs preserves
        // argument positions, so filtered entries remain as empty vectors and
        // output consumers skip them.
        for dsts in call.outputs.values_mut() {
            dsts.retain(|dst| {
                self.assignment_destination_needs_direct_runtime_copyout(dst) == retain_direct
            });
        }
    }

    fn function_call_emits_nonlocal_runtime_write(
        &self,
        call: &veryl_analyzer::ir::FunctionCall,
    ) -> bool {
        if call
            .outputs
            .values()
            .flatten()
            .any(|dst| self.assignment_destination_needs_direct_runtime_copyout(dst))
        {
            return true;
        }

        let nonlocal_ids: HashSet<_> = self
            .module
            .variables
            .iter()
            .filter_map(|(id, variable)| {
                (variable.affiliation != Affiliation::Function).then_some(*id)
            })
            .collect();
        self.module
            .functions
            .get(&call.id)
            .and_then(|function| {
                if let Some(index) = &call.index {
                    function.get_function(index)
                } else {
                    function.get_function(&[])
                }
            })
            .is_some_and(|body| {
                self.statements_write_any(&body.statements, &nonlocal_ids, &mut HashSet::default())
            })
    }

    fn mark_emitted_call_state(
        &self,
        call: &veryl_analyzer::ir::FunctionCall,
        state: &mut HashMap<VarId, Expression>,
    ) {
        // The retained runtime call has already performed its body-side
        // nonlocal writes and dynamic copy-outs. Preserve their expressions
        // for subsequent symbolic reads, but mark them so the enclosing call
        // does not store them again. Nonlocal outputs filtered out of the
        // runtime call still require an ordinary symbolic copy-out.
        let symbolic_nonlocal_outputs: HashSet<_> = call
            .outputs
            .values()
            .flatten()
            .filter_map(|dst| {
                (self.module.variables[&dst.id].affiliation != Affiliation::Function)
                    .then_some(dst.id)
            })
            .collect();
        for (id, expr) in state {
            if self.module.variables[id].affiliation != Affiliation::Function
                && !symbolic_nonlocal_outputs.contains(id)
            {
                Self::mark_emitted_function_state(expr);
            }
        }
    }

    fn statements_have_runtime_effect(
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
                    ForBound::Const(_) => false,
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

    fn statement_is_function_return(statement: &Statement, ret_id: Option<VarId>) -> bool {
        let Some(ret_id) = ret_id else {
            return false;
        };
        let Statement::Assign(assign) = statement else {
            return false;
        };
        assign.dst.len() == 1
            && assign.dst[0].id == ret_id
            && assign.dst[0].index.0.is_empty()
            && assign.dst[0].select.0.is_empty()
            && assign.dst[0].select.1.is_none()
    }

    fn function_state_comptime(&self, id: VarId) -> Comptime {
        let token = TokenRange::default();
        let mut comptime = Comptime::create_unknown(token);
        if let Some(variable) = self.module.variables.get(&id) {
            comptime.r#type = variable.r#type.clone();
            comptime.expr_context.width = variable.r#type.total_width().unwrap_or_default();
            comptime.expr_context.signed = variable.r#type.signed;
        }
        comptime
    }

    fn function_state_merge_comptime(&self, id: VarId) -> Comptime {
        let mut comptime = self.function_state_comptime(id);
        // Token::default() allocates a fresh token id, so equality with a
        // default TokenRange cannot identify synthetic expressions reliably.
        // Source lines cannot use this value; keep it as an internal marker
        // for procedural state merges that may be lowered into guarded writes.
        comptime.token.beg.line = u32::MAX;
        comptime.token.end.line = u32::MAX;
        comptime
    }

    fn function_state_base_comptime(&self, id: VarId) -> Comptime {
        let mut comptime = self.function_state_comptime(id);
        Self::mark_function_state_base_comptime(&mut comptime);
        comptime
    }

    fn mark_function_state_base_comptime(comptime: &mut Comptime) {
        comptime.token.beg.line = u32::MAX - 2;
        comptime.token.end.line = u32::MAX - 2;
    }

    pub(super) fn is_function_state_base(comptime: &Comptime) -> bool {
        comptime.token.beg.line == u32::MAX - 2 && comptime.token.end.line == u32::MAX - 2
    }

    fn is_function_state_merge(comptime: &Comptime) -> bool {
        comptime.token.beg.line == u32::MAX && comptime.token.end.line == u32::MAX
    }

    fn mark_emitted_function_state(expr: &mut Expression) {
        expr.comptime_mut().token.beg.line = u32::MAX - 1;
        expr.comptime_mut().token.end.line = u32::MAX - 1;
    }

    fn is_emitted_function_state(expr: &Expression) -> bool {
        expr.comptime().token.beg.line == u32::MAX - 1
            && expr.comptime().token.end.line == u32::MAX - 1
    }

    fn state_value_expr(&self, id: VarId, state: &HashMap<VarId, Expression>) -> Expression {
        state.get(&id).cloned().unwrap_or_else(|| {
            Expression::Term(Box::new(Factor::Variable(
                id,
                VarIndex::default(),
                VarSelect::default(),
                self.function_state_base_comptime(id),
            )))
        })
    }

    pub(super) fn coerce_function_state_assignment(
        &self,
        expr: Expression,
        dst: &AssignDestination,
    ) -> Result<Expression, ParserError> {
        let variable = &self.module.variables[&dst.id];
        let is_whole_var =
            dst.index.0.is_empty() && dst.select.0.is_empty() && dst.select.1.is_none();
        // Unpacked arrays are shape-checked and lowered element-wise elsewhere;
        // a scalar `as` cast cannot represent their assignment conversion.
        if !variable.r#type.array.is_empty() && is_whole_var {
            return Ok(expr);
        }

        let target_type = if is_whole_var {
            variable.r#type.clone()
        } else {
            let width = get_access_width(self.module, dst.id, &dst.index, &dst.select)?;
            let mut ty = Type::new(if variable.r#type.is_2state() {
                TypeKind::Bit
            } else {
                TypeKind::Logic
            });
            // Selecting only an unpacked array element preserves the packed
            // element's signedness. Packed selections are always unsigned.
            ty.signed = variable.r#type.signed && dst.select.0.is_empty() && dst.select.1.is_none();
            ty.set_concrete_width(Shape::new(vec![Some(width)]));
            ty
        };

        Ok(Self::coerce_function_expression_to_type(expr, &target_type))
    }

    fn coerce_function_expression_to_type(expr: Expression, target_type: &Type) -> Expression {
        let source_type = &expr.comptime().r#type;
        if source_type.total_width() == target_type.total_width()
            && source_type.signed == target_type.signed
            && source_type.is_2state() == target_type.is_2state()
        {
            return expr;
        }

        let token = expr.token_range();
        let cast_target = Expression::Term(Box::new(Factor::Value(Comptime {
            value: ValueVariant::Type(target_type.clone()),
            r#type: Type::new(TypeKind::Type),
            is_const: true,
            is_global: true,
            token,
            ..Default::default()
        })));
        let mut comptime = Comptime::create_unknown(token);
        comptime.r#type = target_type.clone();
        comptime.expr_context.width = target_type.total_width().unwrap_or_default();
        comptime.expr_context.signed = target_type.signed;
        Expression::Binary(
            Box::new(expr),
            Op::As,
            Box::new(cast_target),
            Box::new(comptime),
        )
    }

    fn coerce_function_input_expression(&self, expr: Expression, formal_id: VarId) -> Expression {
        let formal = &self.module.variables[&formal_id];
        if formal.r#type.array.is_empty() {
            Self::coerce_function_expression_to_type(expr, &formal.r#type)
        } else {
            expr
        }
    }

    fn merge_expression_states(
        &self,
        condition: &Expression,
        base: &HashMap<VarId, Expression>,
        then_state: &HashMap<VarId, Expression>,
        else_state: &HashMap<VarId, Expression>,
    ) -> HashMap<VarId, Expression> {
        let ids: HashSet<VarId> = base
            .keys()
            .chain(then_state.keys())
            .chain(else_state.keys())
            .copied()
            .collect();
        ids.into_iter()
            .map(|id| {
                let then_expr = then_state
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| self.state_value_expr(id, base));
                let else_expr = else_state
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| self.state_value_expr(id, base));
                (
                    id,
                    Expression::Ternary(
                        Box::new(Self::normalize_function_control_condition(
                            condition.clone(),
                        )),
                        Box::new(then_expr),
                        Box::new(else_expr),
                        Box::new(self.function_state_merge_comptime(id)),
                    ),
                )
            })
            .collect()
    }

    fn apply_state_transition_on_path(
        &self,
        path: &FunctionPathCondition,
        base: &HashMap<VarId, Expression>,
        transitioned: HashMap<VarId, Expression>,
    ) -> HashMap<VarId, Expression> {
        match path {
            FunctionPathCondition::Always => transitioned,
            FunctionPathCondition::Never => base.clone(),
            FunctionPathCondition::Conditional(condition) => {
                self.merge_expression_states(condition, base, &transitioned, base)
            }
        }
    }

    fn capture_nested_function_outputs_with_states(
        &self,
        expr: &Expression,
        state: &mut HashMap<VarId, Expression>,
    ) -> Result<(Expression, HashMap<TokenRange, HashMap<VarId, Expression>>), ParserError> {
        let mut expression_states = HashMap::default();
        let expr =
            self.capture_nested_function_outputs_inner(expr, state, &mut expression_states)?;
        Ok((expr, expression_states))
    }

    fn apply_statement_function_call_to_state(
        &self,
        call: &veryl_analyzer::ir::FunctionCall,
        state: &HashMap<VarId, Expression>,
    ) -> Result<HashMap<VarId, Expression>, ParserError> {
        let Some(function) = self.module.functions.get(&call.id) else {
            return Err(ParserError::unsupported(
                43,
                LoweringPhase::FfLowering,
                "function call",
                format!("unknown function id: {:?}", call.id),
                Some(&call.comptime.token),
            ));
        };

        let ordered_paths: Vec<_> = function
            .args
            .iter()
            .flat_map(|arg| arg.members.iter().map(|(path, _, _)| path.clone()))
            .collect();
        let mut call = call.clone();
        let mut next = state.clone();
        for path in ordered_paths {
            if let Some(input) = function_call_arg_mut(&mut call.inputs, &path) {
                let input_state = next.clone();
                let captured = self
                    .capture_nested_function_outputs_with_states(input, &mut next)?
                    .0;
                *input = self.substitute_function_expr(&captured, &input_state);
            }
        }
        self.apply_function_call_to_state(&call, &next)
    }

    fn capture_nested_function_outputs_inner(
        &self,
        expr: &Expression,
        state: &mut HashMap<VarId, Expression>,
        expression_states: &mut HashMap<TokenRange, HashMap<VarId, Expression>>,
    ) -> Result<Expression, ParserError> {
        expression_states.insert(expr.token_range(), state.clone());
        Ok(match expr {
            Expression::Term(factor) => match factor.as_ref() {
                // Keep the formal access intact. Runtime-event lowering binds it
                // against the state captured for this argument, which both
                // preserves left-to-right snapshots and applies the formal's
                // declared width, signedness, and state kind.
                Factor::Variable(id, index, select, comptime) => {
                    let mut index = index.clone();
                    for expr in &mut index.0 {
                        *expr = self.capture_nested_function_outputs_inner(
                            expr,
                            state,
                            expression_states,
                        )?;
                    }
                    let mut select = select.clone();
                    for expr in &mut select.0 {
                        *expr = self.capture_nested_function_outputs_inner(
                            expr,
                            state,
                            expression_states,
                        )?;
                    }
                    if let Some((_, expr)) = &mut select.1 {
                        *expr = self.capture_nested_function_outputs_inner(
                            expr,
                            state,
                            expression_states,
                        )?;
                    }
                    Expression::Term(Box::new(Factor::Variable(
                        *id,
                        index,
                        select,
                        comptime.clone(),
                    )))
                }
                Factor::FunctionCall(call) => {
                    let mut call = call.clone();
                    let emits_nonlocal_runtime_write =
                        self.function_call_emits_nonlocal_runtime_write(&call);
                    let Some(function) = self.module.functions.get(&call.id) else {
                        return Err(ParserError::unsupported(
                            43,
                            LoweringPhase::FfLowering,
                            "function call",
                            format!("unknown function id: {:?}", call.id),
                            Some(&call.comptime.token),
                        ));
                    };
                    let ordered_paths: Vec<_> = function
                        .args
                        .iter()
                        .flat_map(|arg| arg.members.iter().map(|(path, _, _)| path.clone()))
                        .collect();
                    for path in ordered_paths {
                        if let Some(input) = function_call_arg_mut(&mut call.inputs, &path) {
                            let input_state = state.clone();
                            let captured = self.capture_nested_function_outputs_inner(
                                input,
                                state,
                                expression_states,
                            )?;
                            *input = self.substitute_function_expr(&captured, &input_state);
                        }
                    }
                    let mut state_call = call.clone();
                    self.retain_direct_runtime_copyouts(&mut state_call, false);
                    let mut transitioned = self.apply_function_call_to_state(&state_call, state)?;
                    if emits_nonlocal_runtime_write {
                        self.mark_emitted_call_state(&state_call, &mut transitioned);
                    }
                    *state = transitioned;
                    self.retain_direct_runtime_copyouts(&mut call, true);
                    Expression::Term(Box::new(Factor::FunctionCall(call)))
                }
                Factor::SystemFunctionCall(call) => {
                    let (call, next, nested_states) =
                        self.prepare_system_function_call(call, state)?;
                    expression_states.extend(nested_states);
                    *state = next;
                    Expression::Term(Box::new(Factor::SystemFunctionCall(call)))
                }
                Factor::HierVariable(reference) => {
                    let mut reference = reference.as_ref().clone();
                    for expr in &mut reference.index.0 {
                        *expr = self.capture_nested_function_outputs_inner(
                            expr,
                            state,
                            expression_states,
                        )?;
                    }
                    for expr in &mut reference.select.0 {
                        *expr = self.capture_nested_function_outputs_inner(
                            expr,
                            state,
                            expression_states,
                        )?;
                    }
                    if let Some((_, expr)) = &mut reference.select.1 {
                        *expr = self.capture_nested_function_outputs_inner(
                            expr,
                            state,
                            expression_states,
                        )?;
                    }
                    Expression::Term(Box::new(Factor::HierVariable(Box::new(reference))))
                }
                Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => expr.clone(),
            },
            Expression::Binary(lhs, op, rhs, comptime) => {
                let lhs =
                    self.capture_nested_function_outputs_inner(lhs, state, expression_states)?;
                let base = state.clone();
                let mut rhs_state = base.clone();
                let rhs = self.capture_nested_function_outputs_inner(
                    rhs,
                    &mut rhs_state,
                    expression_states,
                )?;
                *state = match op {
                    Op::LogicAnd => {
                        let rhs_runs = Self::function_control_may_be_true(lhs.clone());
                        self.merge_expression_states(&rhs_runs, &base, &rhs_state, &base)
                    }
                    Op::LogicOr => self.merge_expression_states(&lhs, &base, &base, &rhs_state),
                    _ => rhs_state,
                };
                Expression::Binary(Box::new(lhs), *op, Box::new(rhs), comptime.clone())
            }
            Expression::Unary(op, inner, comptime) => Expression::Unary(
                *op,
                Box::new(self.capture_nested_function_outputs_inner(
                    inner,
                    state,
                    expression_states,
                )?),
                comptime.clone(),
            ),
            Expression::Ternary(condition, then_expr, else_expr, comptime) => {
                let condition = self.capture_nested_function_outputs_inner(
                    condition,
                    state,
                    expression_states,
                )?;
                let base = state.clone();
                let mut then_state = base.clone();
                let then_expr = self.capture_nested_function_outputs_inner(
                    then_expr,
                    &mut then_state,
                    expression_states,
                )?;
                // A true condition skips the else arm, a false condition starts
                // it from `base`, and an unknown condition evaluates it after
                // the then arm. The true-path entry is irrelevant because the
                // final merge discards the hypothetical else transition there.
                let then_may_run = Self::function_control_may_be_true(condition.clone());
                let mut else_state =
                    self.merge_expression_states(&then_may_run, &base, &then_state, &base);
                let else_expr = self.capture_nested_function_outputs_inner(
                    else_expr,
                    &mut else_state,
                    expression_states,
                )?;
                *state = self.merge_expression_states(&condition, &base, &then_state, &else_state);
                Expression::Ternary(
                    Box::new(condition),
                    Box::new(then_expr),
                    Box::new(else_expr),
                    comptime.clone(),
                )
            }
            Expression::Concatenation(parts, comptime) => {
                let mut rewritten = Vec::with_capacity(parts.len());
                for (expr, repeat) in parts {
                    let expr =
                        self.capture_nested_function_outputs_inner(expr, state, expression_states)?;
                    let repeat = repeat
                        .as_ref()
                        .map(|repeat| {
                            self.capture_nested_function_outputs_inner(
                                repeat,
                                state,
                                expression_states,
                            )
                        })
                        .transpose()?;
                    rewritten.push((expr, repeat));
                }
                Expression::Concatenation(rewritten, comptime.clone())
            }
            Expression::ArrayLiteral(items, comptime) => {
                let mut rewritten = Vec::with_capacity(items.len());
                for item in items {
                    rewritten.push(match item {
                        ArrayLiteralItem::Value(expr, repeat) => ArrayLiteralItem::Value(
                            Box::new(self.capture_nested_function_outputs_inner(
                                expr,
                                state,
                                expression_states,
                            )?),
                            repeat
                                .as_ref()
                                .map(|repeat| {
                                    self.capture_nested_function_outputs_inner(
                                        repeat,
                                        state,
                                        expression_states,
                                    )
                                })
                                .transpose()?
                                .map(Box::new),
                        ),
                        ArrayLiteralItem::Defaul(expr) => ArrayLiteralItem::Defaul(Box::new(
                            self.capture_nested_function_outputs_inner(
                                expr,
                                state,
                                expression_states,
                            )?,
                        )),
                    });
                }
                Expression::ArrayLiteral(rewritten, comptime.clone())
            }
            Expression::StructConstructor(ty, fields, comptime) => {
                let mut rewritten = Vec::with_capacity(fields.len());
                for (name, expr) in fields {
                    rewritten.push((
                        *name,
                        self.capture_nested_function_outputs_inner(expr, state, expression_states)?,
                    ));
                }
                Expression::StructConstructor(ty.clone(), rewritten, comptime.clone())
            }
        })
    }

    fn prepare_system_function_call(
        &self,
        call: &SystemFunctionCall,
        initial: &HashMap<VarId, Expression>,
    ) -> Result<
        (
            SystemFunctionCall,
            HashMap<VarId, Expression>,
            HashMap<TokenRange, HashMap<VarId, Expression>>,
        ),
        ParserError,
    > {
        let mut call = call.clone();
        let mut state = initial.clone();
        let mut arg_states = HashMap::default();
        let capture = |parser: &Self,
                       input: &mut veryl_analyzer::ir::SystemFunctionInput,
                       state: &mut HashMap<VarId, Expression>,
                       arg_states: &mut HashMap<TokenRange, HashMap<VarId, Expression>>|
         -> Result<(), ParserError> {
            input.0 = parser.capture_nested_function_outputs_inner(&input.0, state, arg_states)?;
            Ok(())
        };
        match &mut call.kind {
            SystemFunctionKind::Display(args) | SystemFunctionKind::Write(args) => {
                for input in args {
                    capture(self, input, &mut state, &mut arg_states)?;
                }
            }
            SystemFunctionKind::Assert { cond, args, .. } => {
                capture(self, cond, &mut state, &mut arg_states)?;
                for input in args {
                    capture(self, input, &mut state, &mut arg_states)?;
                }
            }
            SystemFunctionKind::Readmemh(_, _) | SystemFunctionKind::Finish => {}
            SystemFunctionKind::Bits(_) | SystemFunctionKind::Size(_) => {}
            SystemFunctionKind::Clog2(input)
            | SystemFunctionKind::Onehot(input)
            | SystemFunctionKind::Signed(input)
            | SystemFunctionKind::Unsigned(input) => {
                capture(self, input, &mut state, &mut arg_states)?;
            }
        }
        Ok((call, state, arg_states))
    }

    fn apply_statements_to_function_state(
        &self,
        statements: &[Statement],
        initial: &HashMap<VarId, Expression>,
    ) -> Result<HashMap<VarId, Expression>, ParserError> {
        let mut state = initial.clone();
        for statement in statements {
            state = self.apply_statement_to_function_state(statement, &state)?;
        }
        Ok(state)
    }

    fn apply_statements_to_function_state_in_path(
        &self,
        statements: &[Statement],
        ret_id: Option<VarId>,
        initial: &HashMap<VarId, Expression>,
        mut active: FunctionPathCondition,
    ) -> Result<(HashMap<VarId, Expression>, FunctionPathCondition), ParserError> {
        let mut state = initial.clone();
        for statement in statements {
            if matches!(active, FunctionPathCondition::Never) {
                break;
            }
            match statement {
                Statement::Assign(assign) => {
                    let base = state.clone();
                    let mut transitioned_base = base.clone();
                    // State-only evaluation must still apply output bindings
                    // nested in an assignment RHS.
                    let (rhs, _) = self.capture_nested_function_outputs_with_states(
                        &assign.expr,
                        &mut transitioned_base,
                    )?;
                    let transitioned =
                        self.apply_assignment_to_function_state(assign, &rhs, &transitioned_base)?;
                    state = self.apply_state_transition_on_path(&active, &base, transitioned);
                    if Self::statement_is_function_return(statement, ret_id) {
                        active = FunctionPathCondition::Never;
                    }
                }
                Statement::If(statement) => {
                    let base = state.clone();
                    let mut condition_base = base.clone();
                    // State-only evaluation must still apply output bindings
                    // nested in the predicate before deriving either branch.
                    let (condition, _) = self.capture_nested_function_outputs_with_states(
                        &statement.cond,
                        &mut condition_base,
                    )?;
                    let condition = self.substitute_function_expr(&condition, &condition_base);
                    let condition_base =
                        self.apply_state_transition_on_path(&active, &base, condition_base);
                    let true_active = Self::function_path_and(active.clone(), condition.clone());
                    let false_active =
                        Self::function_path_and_not(active.clone(), condition.clone());
                    let (then_state, then_active) = self
                        .apply_statements_to_function_state_in_path(
                            &statement.true_side,
                            ret_id,
                            &condition_base,
                            true_active,
                        )?;
                    let (else_state, else_active) = self
                        .apply_statements_to_function_state_in_path(
                            &statement.false_side,
                            ret_id,
                            &condition_base,
                            false_active,
                        )?;
                    state = self.merge_expression_states(
                        &condition,
                        &condition_base,
                        &then_state,
                        &else_state,
                    );
                    active = Self::function_path_or(then_active, else_active);
                }
                Statement::Case(statement) => {
                    let control_base = state.clone();
                    let case_target =
                        self.substitute_function_expr(&statement.case_target, &control_base);
                    let mut evaluated_state = control_base.clone();
                    let (case_target, _) = self.capture_nested_function_outputs_with_states(
                        &case_target,
                        &mut evaluated_state,
                    )?;
                    state = self.apply_state_transition_on_path(
                        &active,
                        &control_base,
                        evaluated_state,
                    );
                    let case_base = state.clone();
                    let mut remaining = active.clone();
                    let mut arm_states = Vec::with_capacity(statement.arms.len());
                    let mut live_paths = FunctionPathCondition::Never;
                    for arm in &statement.arms {
                        let mut pattern_remaining = remaining.clone();
                        let mut arm_active = FunctionPathCondition::Never;
                        let mut arm_condition = None;
                        for pattern in &arm.patterns {
                            let mut pattern = pattern.clone();
                            let expressions: Vec<&mut Box<Expression>> = match &mut pattern {
                                CasePattern::Eq(expr) => vec![expr],
                                CasePattern::Range { lo, hi, .. } => vec![lo, hi],
                            };
                            for expr in expressions {
                                let pattern_base = state.clone();
                                let frozen = self.substitute_function_expr(expr, &pattern_base);
                                let mut evaluated_state = pattern_base.clone();
                                let (captured, _) = self
                                    .capture_nested_function_outputs_with_states(
                                        &frozen,
                                        &mut evaluated_state,
                                    )?;
                                **expr = captured;
                                state = self.apply_state_transition_on_path(
                                    &pattern_remaining,
                                    &pattern_base,
                                    evaluated_state,
                                );
                            }
                            let condition = case_arm_condition_expr(
                                &case_target,
                                std::slice::from_ref(&pattern),
                            );
                            let matched = Self::function_path_and(
                                pattern_remaining.clone(),
                                condition.clone(),
                            );
                            arm_condition = Some(match arm_condition {
                                None => condition.clone(),
                                Some(previous) => Expression::Binary(
                                    Box::new(previous),
                                    Op::LogicOr,
                                    Box::new(condition.clone()),
                                    Box::new(Comptime::create_unknown(TokenRange::default())),
                                ),
                            });
                            arm_active = Self::function_path_or(arm_active, matched);
                            pattern_remaining =
                                Self::function_path_and_not(pattern_remaining, condition);
                        }
                        let base = state.clone();
                        let (arm_state, arm_live) = self
                            .apply_statements_to_function_state_in_path(
                                &arm.body, ret_id, &base, arm_active,
                            )?;
                        arm_states.push((
                            arm_condition.expect("Case arm must have at least one pattern"),
                            arm_state,
                        ));
                        live_paths = Self::function_path_or(live_paths, arm_live);
                        remaining = pattern_remaining;
                    }
                    let base = state.clone();
                    let (mut merged, default_live) = self
                        .apply_statements_to_function_state_in_path(
                            &statement.default,
                            ret_id,
                            &base,
                            remaining,
                        )?;
                    for (condition, arm_state) in arm_states.into_iter().rev() {
                        merged = self
                            .merge_expression_states(&condition, &case_base, &arm_state, &merged);
                    }
                    state = merged;
                    active = Self::function_path_or(live_paths, default_live);
                }
                Statement::SystemFunctionCall(call) => {
                    let base = state.clone();
                    let (_, transitioned, _) = self.prepare_system_function_call(call, &base)?;
                    state = self.apply_state_transition_on_path(&active, &base, transitioned);
                }
                Statement::FunctionCall(call) => {
                    let base = state.clone();
                    let transitioned = self.apply_statement_function_call_to_state(call, &base)?;
                    state = self.apply_state_transition_on_path(&active, &base, transitioned);
                }
                Statement::Null => {}
                Statement::For(statement) => {
                    return Err(ParserError::unsupported(
                        43,
                        LoweringPhase::FfLowering,
                        "for loop in function body",
                        "for loop".to_string(),
                        Some(&statement.token),
                    ));
                }
                Statement::IfReset(statement) => {
                    return Err(ParserError::unsupported(
                        43,
                        LoweringPhase::FfLowering,
                        "function body control flow",
                        format!("{statement}"),
                        Some(&statement.token),
                    ));
                }
                Statement::TbMethodCall(_) | Statement::Break | Statement::Unsupported(_) => {
                    return Err(ParserError::unsupported(
                        43,
                        LoweringPhase::FfLowering,
                        "function body control flow",
                        format!("{statement}"),
                        None,
                    ));
                }
            }
        }
        Ok((state, active))
    }

    fn apply_statement_to_function_state(
        &self,
        statement: &Statement,
        state: &HashMap<VarId, Expression>,
    ) -> Result<HashMap<VarId, Expression>, ParserError> {
        match statement {
            Statement::Assign(assign) => {
                self.apply_assignment_to_function_state(assign, &assign.expr, state)
            }
            Statement::If(statement) => {
                let mut condition_state = state.clone();
                let (condition, _) = self.capture_nested_function_outputs_with_states(
                    &statement.cond,
                    &mut condition_state,
                )?;
                let condition = self.substitute_function_expr(&condition, &condition_state);
                let then_state = self
                    .apply_statements_to_function_state(&statement.true_side, &condition_state)?;
                let else_state = self
                    .apply_statements_to_function_state(&statement.false_side, &condition_state)?;
                Ok(self.merge_expression_states(
                    &condition,
                    &condition_state,
                    &then_state,
                    &else_state,
                ))
            }
            Statement::Case(statement) => self.apply_case_to_function_state(statement, 0, state),
            Statement::FunctionCall(call) => {
                self.apply_statement_function_call_to_state(call, state)
            }
            Statement::SystemFunctionCall(call) => {
                let (_, next, _) = self.prepare_system_function_call(call, state)?;
                Ok(next)
            }
            Statement::Null => Ok(state.clone()),
            Statement::For(statement) => Err(ParserError::unsupported(
                66,
                LoweringPhase::FfLowering,
                "for loop before runtime effect in function body",
                "for loop".to_string(),
                Some(&statement.token),
            )),
            Statement::IfReset(statement) => Err(ParserError::unsupported(
                66,
                LoweringPhase::FfLowering,
                "if_reset before runtime effect in function body",
                format!("{statement}"),
                Some(&statement.token),
            )),
            Statement::TbMethodCall(_) | Statement::Break | Statement::Unsupported(_) => {
                Err(ParserError::unsupported(
                    66,
                    LoweringPhase::FfLowering,
                    "statement before runtime effect in function body",
                    format!("{statement}"),
                    None,
                ))
            }
        }
    }

    fn apply_assignment_to_function_state(
        &self,
        assign: &AssignStatement,
        rhs: &Expression,
        state: &HashMap<VarId, Expression>,
    ) -> Result<HashMap<VarId, Expression>, ParserError> {
        if assign.dst.len() != 1 {
            return Err(ParserError::unsupported(
                43,
                LoweringPhase::FfLowering,
                "function body assignment shape",
                format!("{}", Statement::Assign(assign.clone())),
                Some(&assign.token),
            ));
        }
        let dst = self.substitute_assignment_destination(&assign.dst[0], state);
        let rhs = self.substitute_function_expr(rhs, state);
        let rhs = self.coerce_function_state_assignment(rhs, &dst)?;
        let mut next = state.clone();
        let is_whole_var =
            dst.index.0.is_empty() && dst.select.0.is_empty() && dst.select.1.is_none();
        if is_whole_var {
            next.insert(dst.id, rhs);
        } else if is_static_access(&dst.index, &dst.select) {
            let old_value = self.state_value_expr(dst.id, state);
            next.insert(
                dst.id,
                build_partial_assign_expr(self.module, &dst, rhs, old_value)?,
            );
        } else if self.module.variables[&dst.id].affiliation != Affiliation::Function {
            let old_value = self.state_value_expr(dst.id, state);
            next.insert(
                dst.id,
                build_dynamic_partial_assign_expr(self.module, &dst, rhs, old_value)?,
            );
        } else {
            return Err(ParserError::unsupported(
                66,
                LoweringPhase::FfLowering,
                "dynamic assignment before runtime effect in function body",
                format!("{}", Statement::Assign(assign.clone())),
                Some(&assign.token),
            ));
        }
        Ok(next)
    }

    fn apply_case_to_function_state(
        &self,
        statement: &CaseStatement,
        arm_index: usize,
        state: &HashMap<VarId, Expression>,
    ) -> Result<HashMap<VarId, Expression>, ParserError> {
        let Some(arm) = statement.arms.get(arm_index) else {
            return self.apply_statements_to_function_state(&statement.default, state);
        };
        let condition = self.substitute_function_expr(
            &case_arm_condition_expr(&statement.case_target, &arm.patterns),
            state,
        );
        let then_state = self.apply_statements_to_function_state(&arm.body, state)?;
        let else_state = self.apply_case_to_function_state(statement, arm_index + 1, state)?;
        Ok(self.merge_expression_states(&condition, state, &then_state, &else_state))
    }

    fn function_path_and(
        path: FunctionPathCondition,
        condition: Expression,
    ) -> FunctionPathCondition {
        let condition = Self::normalize_function_control_condition(condition);
        match path {
            FunctionPathCondition::Always => {
                FunctionPathCondition::Conditional(Box::new(condition))
            }
            FunctionPathCondition::Never => FunctionPathCondition::Never,
            FunctionPathCondition::Conditional(path) => {
                FunctionPathCondition::Conditional(Box::new(Expression::Binary(
                    path,
                    Op::LogicAnd,
                    Box::new(condition),
                    Box::new(Comptime::create_unknown(TokenRange::default())),
                )))
            }
        }
    }

    fn function_path_and_not(
        path: FunctionPathCondition,
        condition: Expression,
    ) -> FunctionPathCondition {
        let condition = Expression::Unary(
            Op::LogicNot,
            Box::new(Self::normalize_function_control_condition(condition)),
            Box::new(Comptime::create_unknown(TokenRange::default())),
        );
        Self::function_path_and(path, condition)
    }

    fn function_path_or(
        lhs: FunctionPathCondition,
        rhs: FunctionPathCondition,
    ) -> FunctionPathCondition {
        match (lhs, rhs) {
            (FunctionPathCondition::Always, _) | (_, FunctionPathCondition::Always) => {
                FunctionPathCondition::Always
            }
            (FunctionPathCondition::Never, rhs) => rhs,
            (lhs, FunctionPathCondition::Never) => lhs,
            (FunctionPathCondition::Conditional(lhs), FunctionPathCondition::Conditional(rhs)) => {
                FunctionPathCondition::Conditional(Box::new(Expression::Binary(
                    lhs,
                    Op::LogicOr,
                    rhs,
                    Box::new(Comptime::create_unknown(TokenRange::default())),
                )))
            }
        }
    }

    fn parse_function_path_condition<A>(
        &mut self,
        condition: &Expression,
        state: &HashMap<VarId, Expression>,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<RegisterId, ParserError> {
        self.function_arg_stack.push(state.clone());
        self.function_array_view_stack.push(HashMap::default());
        self.function_array_view_enabled_stack.push(false);
        let result = self.parse_expression(
            condition, targets, domain, convert, sources, ir_builder, None,
        );
        self.function_array_view_enabled_stack.pop();
        self.function_array_view_stack.pop();
        self.function_arg_stack.pop();
        result?;
        let condition = self
            .stack
            .pop_back()
            .expect("Function path condition evaluation failed");
        Ok(self.lower_procedural_condition(condition, ir_builder))
    }

    fn coerce_register_to_type<A>(
        &self,
        reg: RegisterId,
        ty: &RegisterType,
        signed: bool,
        ir_builder: &mut SIRBuilder<A>,
    ) -> RegisterId {
        match ty {
            RegisterType::Bit {
                width,
                signed: result_signed,
            } => self.coerce_register_to_formal(
                ir_builder,
                reg,
                *width,
                signed,
                *result_signed,
                true,
            ),
            RegisterType::Logic { width } => {
                let widened = self.cast_reg_width_ext(ir_builder, reg, *width, signed);
                if matches!(ir_builder.register(&widened), RegisterType::Logic { .. }) {
                    widened
                } else {
                    let logic = ir_builder.alloc_logic(*width);
                    ir_builder.emit(SIRInstruction::Unary(logic, UnaryOp::Ident, widened));
                    logic
                }
            }
        }
    }

    fn coerce_register_to_variable_type<A>(
        &self,
        reg: RegisterId,
        var_id: VarId,
        extend_signed: bool,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<RegisterId, ParserError> {
        let variable = &self.module.variables[&var_id];
        let width = resolve_total_width(self.module, variable)?;
        Ok(self.coerce_register_to_formal(
            ir_builder,
            reg,
            width,
            extend_signed,
            variable.r#type.signed,
            variable.r#type.is_2state(),
        ))
    }

    fn is_whole_variable_reference(expr: &Expression, var_id: VarId) -> bool {
        matches!(
            expr,
            Expression::Term(factor)
                if matches!(
                    factor.as_ref(),
                    Factor::Variable(id, index, select, _)
                        if *id == var_id
                            && index.0.is_empty()
                            && select.0.is_empty()
                            && select.1.is_none()
                )
        )
    }

    fn collect_guarded_nonlocal_writes(
        var_id: VarId,
        expr: &Expression,
        active: FunctionPathCondition,
        writes: &mut Vec<(VarId, Expression, FunctionPathCondition)>,
    ) {
        if matches!(active, FunctionPathCondition::Never)
            || Self::is_whole_variable_reference(expr, var_id)
            || Self::is_emitted_function_state(expr)
        {
            return;
        }
        if let Expression::Ternary(condition, then_expr, else_expr, comptime) = expr
            && Self::is_function_state_merge(comptime)
        {
            Self::collect_guarded_nonlocal_writes(
                var_id,
                then_expr,
                Self::function_path_and(active.clone(), condition.as_ref().clone()),
                writes,
            );
            Self::collect_guarded_nonlocal_writes(
                var_id,
                else_expr,
                Self::function_path_and_not(active, condition.as_ref().clone()),
                writes,
            );
        } else {
            writes.push((var_id, expr.clone(), active));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_nonlocal_function_state_writes<A>(
        &mut self,
        state: &HashMap<VarId, Expression>,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        // Function-local state is consumed while lowering the enclosing call.
        // Any other entry was introduced by a nested output actual (or a
        // direct write in the function body) and needs an actual store in the
        // caller's FF region.
        let mut symbolic_writes = Vec::new();
        for (&var_id, expr) in state {
            let variable = &self.module.variables[&var_id];
            if variable.affiliation == Affiliation::Function {
                continue;
            }
            Self::collect_guarded_nonlocal_writes(
                var_id,
                expr,
                FunctionPathCondition::Always,
                &mut symbolic_writes,
            );
        }

        let mut writes = Vec::new();
        for (var_id, expr, active) in symbolic_writes {
            let variable = &self.module.variables[&var_id];
            let dst = AssignDestination {
                id: var_id,
                path: variable.path.clone(),
                index: VarIndex::default(),
                select: VarSelect::default(),
                comptime: Comptime {
                    r#type: variable.r#type.clone(),
                    ..Default::default()
                },
                token: TokenRange::default(),
            };
            let guard = if let FunctionPathCondition::Conditional(condition) = active {
                self.parse_expression(
                    &condition, targets, domain, convert, sources, ir_builder, None,
                )?;
                let condition = self
                    .stack
                    .pop_back()
                    .expect("Nonlocal function state guard evaluation failed");
                Some(self.lower_procedural_condition(condition, ir_builder))
            } else {
                None
            };
            self.parse_expression(&expr, targets, domain, convert, sources, ir_builder, None)?;
            let value = self
                .stack
                .pop_back()
                .expect("Nonlocal function state evaluation failed");
            let value = self.coerce_register_to_variable_type(
                value,
                var_id,
                expression_signed(&expr),
                ir_builder,
            )?;
            writes.push((dst, value, guard));
        }

        // Resolve every RHS against the caller's pre-copy-out state before
        // mutating any nonlocal destination. `state` is a hash map, so
        // interleaving evaluation and stores would make the result depend on
        // its iteration order when one final expression reads another target.
        for (dst, value, guard) in writes {
            let merge_block = if let Some(guard) = guard {
                let write_block = ir_builder.new_block();
                let merge_block = ir_builder.new_block();
                ir_builder.seal_block(SIRTerminator::Branch {
                    cond: guard,
                    true_block: (write_block, vec![]),
                    false_block: (merge_block, vec![]),
                });
                ir_builder.switch_to_block(write_block);
                Some(merge_block)
            } else {
                None
            };
            self.emit_multi_dst_assign(
                value,
                std::slice::from_ref(&dst),
                targets,
                domain,
                convert,
                sources,
                ir_builder,
            )?;
            if let Some(merge_block) = merge_block {
                ir_builder.seal_block(SIRTerminator::Jump(merge_block, vec![]));
                ir_builder.switch_to_block(merge_block);
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn flush_captured_nonlocal_state_before_call<A>(
        &mut self,
        call: &veryl_analyzer::ir::FunctionCall,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        if !self.function_call_emits_nonlocal_runtime_write(call) {
            return Ok(());
        }
        let Some(state) = self
            .get_bound_function_event_arg_state(call.comptime.token)
            .cloned()
        else {
            return Ok(());
        };
        if !state
            .keys()
            .any(|id| self.module.variables[id].affiliation != Affiliation::Function)
        {
            return Ok(());
        }
        self.emit_nonlocal_function_state_writes(
            &state, targets, domain, convert, sources, ir_builder,
        )
    }

    fn materialize_function_runtime_expression<A>(
        &mut self,
        expr: &Expression,
        state: &mut HashMap<VarId, Expression>,
        active: &FunctionPathCondition,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<Expression, ParserError> {
        let guard_state = state.clone();
        let mut evaluated_state = state.clone();
        let (expr, expression_states) =
            self.capture_nested_function_outputs_with_states(expr, &mut evaluated_state)?;
        let value = match active {
            FunctionPathCondition::Never => return Ok(expr),
            FunctionPathCondition::Always => {
                *state = evaluated_state;
                self.function_arg_stack.push(state.clone());
                self.function_array_view_stack.push(HashMap::default());
                self.function_array_view_enabled_stack.push(false);
                self.function_event_arg_state_stack
                    .push(expression_states.clone());
                let result = self
                    .parse_expression(&expr, targets, domain, convert, sources, ir_builder, None);
                self.function_event_arg_state_stack.pop();
                self.function_array_view_enabled_stack.pop();
                self.function_array_view_stack.pop();
                self.function_arg_stack.pop();
                result?;
                self.stack
                    .pop_back()
                    .expect("Function runtime expression evaluation failed")
            }
            FunctionPathCondition::Conditional(condition) => {
                let pre_defined = self.defined_ranges.clone();
                let pre_dynamic = self.dynamic_defined_vars.clone();
                *state = self.merge_expression_states(
                    condition,
                    &guard_state,
                    &evaluated_state,
                    &guard_state,
                );
                let condition = self.parse_function_path_condition(
                    condition,
                    &guard_state,
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )?;
                let width = self.get_expression_width(&expr);
                let signed = expression_signed(&expr);
                let result = if expr.comptime().r#type.is_2state() {
                    ir_builder.alloc_bit(width, signed)
                } else {
                    ir_builder.alloc_logic(width)
                };
                let result_type = ir_builder.register(&result).clone();
                let effect_block = ir_builder.new_block();
                let skip_block = ir_builder.new_block();
                let merge_block = ir_builder.new_block_with(vec![result]);
                ir_builder.seal_block(SIRTerminator::Branch {
                    cond: condition,
                    true_block: (effect_block, vec![]),
                    false_block: (skip_block, vec![]),
                });

                ir_builder.switch_to_block(effect_block);
                self.function_arg_stack.push(evaluated_state.clone());
                self.function_array_view_stack.push(HashMap::default());
                self.function_array_view_enabled_stack.push(false);
                self.function_event_arg_state_stack
                    .push(expression_states.clone());
                let parse_result = self
                    .parse_expression(&expr, targets, domain, convert, sources, ir_builder, None);
                self.function_event_arg_state_stack.pop();
                self.function_array_view_enabled_stack.pop();
                self.function_array_view_stack.pop();
                self.function_arg_stack.pop();
                parse_result?;
                let effect_value = self
                    .stack
                    .pop_back()
                    .expect("Guarded function runtime expression evaluation failed");
                let effect_value =
                    self.coerce_register_to_type(effect_value, &result_type, signed, ir_builder);
                let effect_defined =
                    std::mem::replace(&mut self.defined_ranges, pre_defined.clone());
                let effect_dynamic =
                    std::mem::replace(&mut self.dynamic_defined_vars, pre_dynamic.clone());
                ir_builder.seal_block(SIRTerminator::Jump(merge_block, vec![effect_value]));

                ir_builder.switch_to_block(skip_block);
                let dummy = ir_builder.alloc_reg(result_type);
                ir_builder.emit(SIRInstruction::Imm(dummy, SIRValue::new(0u8)));
                ir_builder.seal_block(SIRTerminator::Jump(merge_block, vec![dummy]));
                ir_builder.switch_to_block(merge_block);
                self.defined_ranges = self.intersect_defined_states(pre_defined, effect_defined);
                self.dynamic_defined_vars =
                    self.intersect_dynamic_vars(pre_dynamic, effect_dynamic);
                result
            }
        };
        self.function_expression_value_stack
            .last_mut()
            .expect("Function expression value scope is active")
            .insert(expr.token_range(), value);
        Ok(expr)
    }

    fn emit_function_system_task<A>(
        &mut self,
        call: &SystemFunctionCall,
        state: &mut HashMap<VarId, Expression>,
        active: &FunctionPathCondition,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        if matches!(active, FunctionPathCondition::Never) {
            return Ok(());
        }
        let guard_state = state.clone();
        let (call, next_state, arg_states) = self.prepare_system_function_call(call, state)?;
        *state = self.apply_state_transition_on_path(active, &guard_state, next_state);
        let pre_defined = self.defined_ranges.clone();
        let pre_dynamic = self.dynamic_defined_vars.clone();
        let merge_block = if let FunctionPathCondition::Conditional(condition) = active {
            let condition = self.parse_function_path_condition(
                condition,
                &guard_state,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
            )?;
            let effect_block = ir_builder.new_block();
            let merge_block = ir_builder.new_block();
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: condition,
                true_block: (effect_block, vec![]),
                false_block: (merge_block, vec![]),
            });
            ir_builder.switch_to_block(effect_block);
            Some(merge_block)
        } else {
            None
        };

        self.function_event_arg_state_stack.push(arg_states);
        let result =
            self.parse_system_task_statement(&call, targets, domain, convert, sources, ir_builder);
        self.function_event_arg_state_stack.pop();
        result?;

        if let Some(merge_block) = merge_block {
            let effect_defined = std::mem::replace(&mut self.defined_ranges, pre_defined.clone());
            let effect_dynamic =
                std::mem::replace(&mut self.dynamic_defined_vars, pre_dynamic.clone());
            ir_builder.seal_block(SIRTerminator::Jump(merge_block, vec![]));
            ir_builder.switch_to_block(merge_block);
            self.defined_ranges = self.intersect_defined_states(pre_defined, effect_defined);
            self.dynamic_defined_vars = self.intersect_dynamic_vars(pre_dynamic, effect_dynamic);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_dynamic_nonlocal_function_assignment<A>(
        &mut self,
        assign: &AssignStatement,
        state: &mut HashMap<VarId, Expression>,
        active: &FunctionPathCondition,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        if matches!(active, FunctionPathCondition::Never) {
            return Ok(());
        }

        let mut assign = assign.clone();
        let guard_state = state.clone();
        if state
            .iter()
            .any(|(id, _)| self.module.variables[id].affiliation != Affiliation::Function)
        {
            let flushed_state = state.clone();
            self.emit_nonlocal_function_state_writes(
                &flushed_state,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
            )?;
            assign.expr = self.substitute_function_expr(&assign.expr, &flushed_state);
            assign.dst = assign
                .dst
                .iter()
                .map(|dst| self.substitute_assignment_destination(dst, &flushed_state))
                .collect();
            state.retain(|id, _| self.module.variables[id].affiliation == Affiliation::Function);
        }
        if self.expression_needs_runtime_materialization(&assign.expr) {
            assign.expr = self.materialize_function_runtime_expression(
                &assign.expr,
                state,
                active,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
            )?;
        }

        let pre_defined = self.defined_ranges.clone();
        let pre_dynamic = self.dynamic_defined_vars.clone();
        let merge_block = if let FunctionPathCondition::Conditional(condition) = active {
            let condition = self.parse_function_path_condition(
                condition,
                &guard_state,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
            )?;
            let store_block = ir_builder.new_block();
            let merge_block = ir_builder.new_block();
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: condition,
                true_block: (store_block, vec![]),
                false_block: (merge_block, vec![]),
            });
            ir_builder.switch_to_block(store_block);
            Some(merge_block)
        } else {
            None
        };

        self.function_arg_stack.push(state.clone());
        self.function_array_view_stack.push(HashMap::default());
        self.function_array_view_enabled_stack.push(false);
        let result =
            self.parse_assign_statement(&assign, targets, domain, convert, sources, ir_builder);
        self.function_array_view_enabled_stack.pop();
        self.function_array_view_stack.pop();
        self.function_arg_stack.pop();
        result?;

        if let Some(merge_block) = merge_block {
            let store_defined = std::mem::replace(&mut self.defined_ranges, pre_defined.clone());
            let store_dynamic =
                std::mem::replace(&mut self.dynamic_defined_vars, pre_dynamic.clone());
            ir_builder.seal_block(SIRTerminator::Jump(merge_block, vec![]));
            ir_builder.switch_to_block(merge_block);
            self.defined_ranges = self.intersect_defined_states(pre_defined, store_defined);
            self.dynamic_defined_vars = self.intersect_dynamic_vars(pre_dynamic, store_dynamic);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_dynamic_nonlocal_statement_call<A>(
        &mut self,
        call: &veryl_analyzer::ir::FunctionCall,
        state: &mut HashMap<VarId, Expression>,
        active: &FunctionPathCondition,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        if matches!(active, FunctionPathCondition::Never) {
            return Ok(());
        }

        let mut call = call.clone();
        let guard_state = state.clone();
        if state
            .iter()
            .any(|(id, _)| self.module.variables[id].affiliation != Affiliation::Function)
        {
            let flushed_state = state.clone();
            self.emit_nonlocal_function_state_writes(
                &flushed_state,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
            )?;
            for input in call.inputs.values_mut() {
                *input = self.substitute_function_expr(input, &flushed_state);
            }
            for dsts in call.outputs.values_mut() {
                *dsts = dsts
                    .iter()
                    .map(|dst| self.substitute_assignment_destination(dst, &flushed_state))
                    .collect();
            }
            state.retain(|id, _| self.module.variables[id].affiliation == Affiliation::Function);
        }

        let pre_defined = self.defined_ranges.clone();
        let pre_dynamic = self.dynamic_defined_vars.clone();
        let merge_block = if let FunctionPathCondition::Conditional(condition) = active {
            let condition = self.parse_function_path_condition(
                condition,
                &guard_state,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
            )?;
            let call_block = ir_builder.new_block();
            let merge_block = ir_builder.new_block();
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: condition,
                true_block: (call_block, vec![]),
                false_block: (merge_block, vec![]),
            });
            ir_builder.switch_to_block(call_block);
            Some(merge_block)
        } else {
            None
        };

        self.function_arg_stack.push(state.clone());
        self.function_array_view_stack.push(HashMap::default());
        self.function_array_view_enabled_stack.push(false);
        let result = self
            .parse_function_call_statement(&call, targets, domain, convert, sources, ir_builder);
        self.function_array_view_enabled_stack.pop();
        self.function_array_view_stack.pop();
        self.function_arg_stack.pop();
        result?;

        if let Some(merge_block) = merge_block {
            let call_defined = std::mem::replace(&mut self.defined_ranges, pre_defined.clone());
            let call_dynamic =
                std::mem::replace(&mut self.dynamic_defined_vars, pre_dynamic.clone());
            ir_builder.seal_block(SIRTerminator::Jump(merge_block, vec![]));
            ir_builder.switch_to_block(merge_block);
            self.defined_ranges = self.intersect_defined_states(pre_defined, call_defined);
            self.dynamic_defined_vars = self.intersect_dynamic_vars(pre_dynamic, call_dynamic);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_function_runtime_effects_in_path<A>(
        &mut self,
        statements: &[Statement],
        ret_id: Option<VarId>,
        bindings: &HashMap<VarId, Expression>,
        mut active: FunctionPathCondition,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(HashMap<VarId, Expression>, FunctionPathCondition), ParserError> {
        let mut state = bindings.clone();
        for statement in statements {
            if matches!(active, FunctionPathCondition::Never) {
                break;
            }
            let is_return = Self::statement_is_function_return(statement, ret_id);
            match statement {
                Statement::Assign(assign) => {
                    let direct_dynamic_nonlocal = assign.dst.len() == 1
                        && self.assignment_destination_needs_direct_runtime_copyout(&assign.dst[0]);
                    if direct_dynamic_nonlocal
                        && !self.assignment_destination_needs_eager_evaluation(&assign.dst[0])
                    {
                        self.emit_dynamic_nonlocal_function_assignment(
                            assign, &mut state, &active, targets, domain, convert, sources,
                            ir_builder,
                        )?;
                        continue;
                    }
                    if assign
                        .dst
                        .iter()
                        .any(|dst| self.assignment_destination_needs_eager_evaluation(dst))
                    {
                        return Err(ParserError::unsupported(
                            66,
                            LoweringPhase::FfLowering,
                            "effectful assignment destination in function body",
                            format!("{statement}"),
                            Some(&assign.token),
                        ));
                    }
                    let materialized_rhs =
                        if self.expression_needs_runtime_materialization(&assign.expr) {
                            Some(self.materialize_function_runtime_expression(
                                &assign.expr,
                                &mut state,
                                &active,
                                targets,
                                domain,
                                convert,
                                sources,
                                ir_builder,
                            )?)
                        } else {
                            None
                        };
                    let base = state.clone();
                    let transitioned = if let Some(rhs) = materialized_rhs.as_ref() {
                        self.apply_assignment_to_function_state(assign, rhs, &base)?
                    } else {
                        self.apply_statement_to_function_state(statement, &base)?
                    };
                    state = self.apply_state_transition_on_path(&active, &base, transitioned);
                    if is_return {
                        active = FunctionPathCondition::Never;
                    }
                }
                Statement::SystemFunctionCall(call) => {
                    self.emit_function_system_task(
                        call, &mut state, &active, targets, domain, convert, sources, ir_builder,
                    )?;
                }
                Statement::Null => {}
                Statement::If(statement) => {
                    let condition =
                        if self.expression_needs_runtime_materialization(&statement.cond) {
                            self.materialize_function_runtime_expression(
                                &statement.cond,
                                &mut state,
                                &active,
                                targets,
                                domain,
                                convert,
                                sources,
                                ir_builder,
                            )?
                        } else {
                            self.substitute_function_expr(&statement.cond, &state)
                        };
                    let base = state.clone();
                    let true_active = Self::function_path_and(active.clone(), condition.clone());
                    let false_active =
                        Self::function_path_and_not(active.clone(), condition.clone());
                    let (then_state, then_active) = self.emit_function_runtime_effects_in_path(
                        &statement.true_side,
                        ret_id,
                        &base,
                        true_active,
                        targets,
                        domain,
                        convert,
                        sources,
                        ir_builder,
                    )?;
                    let (else_state, else_active) = self.emit_function_runtime_effects_in_path(
                        &statement.false_side,
                        ret_id,
                        &base,
                        false_active,
                        targets,
                        domain,
                        convert,
                        sources,
                        ir_builder,
                    )?;
                    state =
                        self.merge_expression_states(&condition, &base, &then_state, &else_state);
                    active = Self::function_path_or(then_active, else_active);
                }
                Statement::Case(statement) => {
                    let pattern_needs_eager_evaluation = statement.arms.iter().any(|arm| {
                        arm.patterns.iter().any(|pattern| match pattern {
                            CasePattern::Eq(expr) => self.expression_needs_eager_evaluation(expr),
                            CasePattern::Range { lo, hi, .. } => {
                                self.expression_needs_eager_evaluation(lo)
                                    || self.expression_needs_eager_evaluation(hi)
                            }
                        })
                    });
                    if pattern_needs_eager_evaluation
                        || self.expression_needs_runtime_materialization(&statement.case_target)
                    {
                        self.materialize_function_runtime_expression(
                            &statement.case_target,
                            &mut state,
                            &active,
                            targets,
                            domain,
                            convert,
                            sources,
                            ir_builder,
                        )?;
                    }
                    let case_base = state.clone();
                    let mut remaining = active.clone();
                    let mut live_paths = FunctionPathCondition::Never;
                    let mut arm_states = Vec::with_capacity(statement.arms.len());
                    for arm in &statement.arms {
                        let last_effectful_pattern =
                            arm.patterns.iter().rposition(|pattern| match pattern {
                                CasePattern::Eq(expr) => {
                                    self.expression_needs_eager_evaluation(expr)
                                }
                                CasePattern::Range { lo, hi, .. } => {
                                    self.expression_needs_eager_evaluation(lo)
                                        || self.expression_needs_eager_evaluation(hi)
                                }
                            });
                        let mut pattern_remaining = remaining.clone();
                        let mut arm_active = FunctionPathCondition::Never;
                        let mut arm_condition = None;
                        for (pattern_index, pattern) in arm.patterns.iter().enumerate() {
                            let snapshot_for_later_effect =
                                last_effectful_pattern.is_some_and(|last| pattern_index < last);
                            match pattern {
                                CasePattern::Eq(expr) => {
                                    if snapshot_for_later_effect
                                        || self.expression_needs_runtime_materialization(expr)
                                    {
                                        self.materialize_function_runtime_expression(
                                            expr,
                                            &mut state,
                                            &pattern_remaining,
                                            targets,
                                            domain,
                                            convert,
                                            sources,
                                            ir_builder,
                                        )?;
                                    }
                                }
                                CasePattern::Range { lo, hi, .. } => {
                                    let hi_is_effectful =
                                        self.expression_needs_eager_evaluation(hi);
                                    if snapshot_for_later_effect
                                        || hi_is_effectful
                                        || self.expression_needs_runtime_materialization(lo)
                                    {
                                        self.materialize_function_runtime_expression(
                                            lo,
                                            &mut state,
                                            &pattern_remaining,
                                            targets,
                                            domain,
                                            convert,
                                            sources,
                                            ir_builder,
                                        )?;
                                    }
                                    if snapshot_for_later_effect
                                        || hi_is_effectful
                                        || self.expression_needs_assignment_snapshot(hi)
                                    {
                                        let lower_condition = self.substitute_function_expr(
                                            &Expression::Binary(
                                                lo.clone(),
                                                Op::LessEq,
                                                statement.case_target.clone(),
                                                Box::new(Comptime::create_unknown(
                                                    TokenRange::default(),
                                                )),
                                            ),
                                            &state,
                                        );
                                        let upper_active = Self::function_path_and(
                                            pattern_remaining.clone(),
                                            Self::function_control_may_be_true(lower_condition),
                                        );
                                        self.materialize_function_runtime_expression(
                                            hi,
                                            &mut state,
                                            &upper_active,
                                            targets,
                                            domain,
                                            convert,
                                            sources,
                                            ir_builder,
                                        )?;
                                    }
                                }
                            }
                            let pattern_base = state.clone();
                            let condition = self.substitute_function_expr(
                                &case_arm_condition_expr(
                                    &statement.case_target,
                                    std::slice::from_ref(pattern),
                                ),
                                &pattern_base,
                            );
                            let matched = Self::function_path_and(
                                pattern_remaining.clone(),
                                condition.clone(),
                            );
                            arm_condition = Some(match arm_condition {
                                None => condition.clone(),
                                Some(previous) => Expression::Binary(
                                    Box::new(previous),
                                    Op::LogicOr,
                                    Box::new(condition.clone()),
                                    Box::new(Comptime::create_unknown(TokenRange::default())),
                                ),
                            });
                            arm_active = Self::function_path_or(arm_active, matched);
                            pattern_remaining =
                                Self::function_path_and_not(pattern_remaining, condition);
                        }
                        let base = state.clone();
                        let (_, arm_live) = self.emit_function_runtime_effects_in_path(
                            &arm.body, ret_id, &base, arm_active, targets, domain, convert,
                            sources, ir_builder,
                        )?;
                        // The emitted state includes the case-selection guard,
                        // which can retain a self-reference for output formals.
                        // Recompute without that guard, but preserve the outer
                        // live-path condition from preceding returns. Case
                        // priority is applied once below.
                        let (arm_state, _) = self.apply_statements_to_function_state_in_path(
                            &arm.body,
                            ret_id,
                            &base,
                            active.clone(),
                        )?;
                        arm_states.push((
                            arm_condition.expect("Case arm must have at least one pattern"),
                            arm_state,
                        ));
                        live_paths = Self::function_path_or(live_paths, arm_live);
                        remaining = pattern_remaining;
                    }
                    let base = state.clone();
                    let (_, default_live) = self.emit_function_runtime_effects_in_path(
                        &statement.default,
                        ret_id,
                        &base,
                        remaining,
                        targets,
                        domain,
                        convert,
                        sources,
                        ir_builder,
                    )?;
                    let (default_state, _) = self.apply_statements_to_function_state_in_path(
                        &statement.default,
                        ret_id,
                        &base,
                        active.clone(),
                    )?;
                    state = arm_states.into_iter().rev().fold(
                        default_state,
                        |merged, (condition, arm_state)| {
                            self.merge_expression_states(
                                &condition, &case_base, &arm_state, &merged,
                            )
                        },
                    );
                    active = Self::function_path_or(live_paths, default_live);
                }
                Statement::For(statement) => {
                    return Err(ParserError::unsupported(
                        66,
                        LoweringPhase::FfLowering,
                        "control flow around runtime effect in function body",
                        "for loop".to_string(),
                        Some(&statement.token),
                    ));
                }
                Statement::FunctionCall(call) => {
                    if call
                        .outputs
                        .values()
                        .flatten()
                        .any(|dst| self.assignment_destination_needs_eager_evaluation(dst))
                    {
                        return Err(ParserError::unsupported(
                            66,
                            LoweringPhase::FfLowering,
                            "effectful function call output destination in function body",
                            format!("{statement}"),
                            Some(&call.comptime.token),
                        ));
                    }
                    if self.function_call_has_runtime_effect(call, &mut HashSet::default()) {
                        return Err(ParserError::unsupported(
                            66,
                            LoweringPhase::FfLowering,
                            "nested runtime effect in function body",
                            format!("{statement}"),
                            Some(&call.comptime.token),
                        ));
                    }
                    let Some(function) = self.module.functions.get(&call.id) else {
                        return Err(ParserError::unsupported(
                            43,
                            LoweringPhase::FfLowering,
                            "function call",
                            format!("unknown function id: {:?}", call.id),
                            Some(&call.comptime.token),
                        ));
                    };
                    let ordered_inputs: Vec<_> = function
                        .args
                        .iter()
                        .flat_map(|arg| arg.members.iter().map(|(path, _, _)| path))
                        .filter_map(|path| function_call_arg(&call.inputs, path).cloned())
                        .collect();
                    let last_effectful = ordered_inputs
                        .iter()
                        .rposition(|expr| self.expression_needs_eager_evaluation(expr));
                    if let Some(last_effectful) = last_effectful {
                        for expr in &ordered_inputs[..=last_effectful] {
                            self.materialize_function_runtime_expression(
                                expr, &mut state, &active, targets, domain, convert, sources,
                                ir_builder,
                            )?;
                        }
                    }
                    let mut state_call = call.clone();
                    self.retain_direct_runtime_copyouts(&mut state_call, false);
                    let mut direct_call = call.clone();
                    self.retain_direct_runtime_copyouts(&mut direct_call, true);
                    let emitted_direct_call =
                        direct_call.outputs.values().any(|dsts| !dsts.is_empty());
                    if emitted_direct_call {
                        self.emit_dynamic_nonlocal_statement_call(
                            &direct_call,
                            &mut state,
                            &active,
                            targets,
                            domain,
                            convert,
                            sources,
                            ir_builder,
                        )?;
                    }
                    let base = state.clone();
                    let mut transitioned = self.apply_function_call_to_state(&state_call, &base)?;
                    if emitted_direct_call {
                        self.mark_emitted_call_state(&state_call, &mut transitioned);
                    }
                    state = self.apply_state_transition_on_path(&active, &base, transitioned);
                }
                Statement::IfReset(statement) => {
                    return Err(ParserError::unsupported(
                        66,
                        LoweringPhase::FfLowering,
                        "control flow around runtime effect in function body",
                        format!("{statement}"),
                        Some(&statement.token),
                    ));
                }
                Statement::TbMethodCall(_) | Statement::Break | Statement::Unsupported(_) => {
                    return Err(ParserError::unsupported(
                        66,
                        LoweringPhase::FfLowering,
                        "runtime effect in function body",
                        format!("{statement}"),
                        None,
                    ));
                }
            }
        }
        Ok((state, active))
    }

    fn emit_function_runtime_effects<A>(
        &mut self,
        statements: &[Statement],
        ret_id: Option<VarId>,
        bindings: &HashMap<VarId, Expression>,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<HashMap<VarId, Expression>, ParserError> {
        let (state, _) = self.emit_function_runtime_effects_in_path(
            statements,
            ret_id,
            bindings,
            FunctionPathCondition::Always,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
        )?;
        Ok(state)
    }

    fn normalize_function_control_condition(condition: Expression) -> Expression {
        let token = TokenRange::default();
        let already_one_bit = condition.comptime().r#type.total_width() == Some(1)
            || matches!(
                &condition,
                Expression::Binary(
                    _,
                    Op::Eq
                        | Op::EqWildcard
                        | Op::Ne
                        | Op::NeWildcard
                        | Op::Less
                        | Op::LessEq
                        | Op::Greater
                        | Op::GreaterEq
                        | Op::LogicAnd
                        | Op::LogicOr,
                    _,
                    _,
                ) | Expression::Unary(
                    Op::BitAnd
                        | Op::BitOr
                        | Op::BitXor
                        | Op::BitNand
                        | Op::BitNor
                        | Op::BitXnor
                        | Op::LogicNot,
                    _,
                    _,
                )
            );
        let truth = if already_one_bit {
            condition
        } else {
            Expression::Unary(
                Op::BitOr,
                Box::new(condition),
                Box::new(Comptime::create_unknown(token)),
            )
        };
        let mut bit_type = Type::new(TypeKind::Bit);
        bit_type.set_concrete_width(Shape::new(vec![Some(1)]));
        let cast_target = Expression::Term(Box::new(Factor::Value(Comptime {
            value: ValueVariant::Type(bit_type),
            r#type: Type::new(TypeKind::Type),
            is_const: true,
            is_global: true,
            token,
            ..Default::default()
        })));
        Expression::Binary(
            Box::new(truth),
            Op::As,
            Box::new(cast_target),
            Box::new(Comptime::create_unknown(token)),
        )
    }

    fn function_control_may_be_true(condition: Expression) -> Expression {
        let token = TokenRange::default();
        let definitely_false = Self::normalize_function_control_condition(Expression::Unary(
            Op::LogicNot,
            Box::new(condition),
            Box::new(Comptime::create_unknown(token)),
        ));
        Expression::Unary(
            Op::LogicNot,
            Box::new(definitely_false),
            Box::new(Comptime::create_unknown(token)),
        )
    }

    fn default_expr_matches_formal(expr: &Expression, formal_shape: &[usize]) -> bool {
        Self::expr_shape_matches_formal(expr, formal_shape)
            || (!formal_shape.is_empty() && expr.comptime().r#type.array.is_empty())
    }

    fn expr_shape_matches_formal(expr: &Expression, formal_shape: &[usize]) -> bool {
        match expr {
            Expression::Term(factor)
                if matches!(factor.as_ref(), Factor::Anonymous(_) | Factor::Unknown(_)) =>
            {
                false
            }
            Expression::ArrayLiteral(items, _) => {
                let Some((&formal_len, formal_tail)) = formal_shape.split_first() else {
                    return false;
                };
                let mut explicit_len = 0usize;
                let mut saw_default = false;

                for item in items {
                    match item {
                        ArrayLiteralItem::Value(inner, repeat) => {
                            let rep_count = if let Some(rep_expr) = repeat {
                                match crate::bitaccess::eval_constexpr(rep_expr)
                                    .and_then(|v| v.to_u64())
                                {
                                    Some(v) => v as usize,
                                    None => return false,
                                }
                            } else {
                                1
                            };
                            explicit_len += rep_count;
                            if explicit_len > formal_len {
                                return false;
                            }
                            if !Self::expr_shape_matches_formal(inner, formal_tail) {
                                return false;
                            }
                        }
                        ArrayLiteralItem::Defaul(inner) => {
                            if saw_default {
                                return false;
                            }
                            saw_default = true;
                            if !Self::default_expr_matches_formal(inner, formal_tail) {
                                return false;
                            }
                        }
                    }
                }

                if saw_default {
                    explicit_len <= formal_len
                } else {
                    explicit_len == formal_len
                }
            }
            _ => {
                let shape: Option<Vec<usize>> =
                    expr.comptime().r#type.array.iter().copied().collect();
                shape.unwrap_or_default() == formal_shape
            }
        }
    }

    fn actual_matches_formal_shape(
        &self,
        formal: &veryl_analyzer::ir::Variable,
        expr: &Expression,
    ) -> bool {
        let formal_shape: Option<Vec<usize>> = formal.r#type.array.iter().copied().collect();
        let formal_shape = formal_shape.unwrap_or_default();
        if formal_shape.is_empty() {
            return true;
        }
        Self::expr_shape_matches_formal(expr, &formal_shape)
    }

    fn validate_function_call_bindings(
        &self,
        call: &veryl_analyzer::ir::FunctionCall,
        function_body: &veryl_analyzer::ir::FunctionBody,
    ) -> Result<(), ParserError> {
        for (arg_path, arg_id) in &function_body.arg_map {
            let Some(arg_expr) = function_call_arg(&call.inputs, arg_path) else {
                continue;
            };
            let formal = &self.module.variables[arg_id];
            if !self.actual_matches_formal_shape(formal, arg_expr) {
                return Err(ParserError::unsupported(
                    43,
                    LoweringPhase::FfLowering,
                    "function call argument shape",
                    format!(
                        "actual expression shape does not match unpacked array formal `{}`",
                        formal.path
                    ),
                    Some(&call.comptime.token),
                ));
            }
        }
        Ok(())
    }

    fn substitute_assignment_destination(
        &self,
        dst: &AssignDestination,
        defs: &HashMap<VarId, Expression>,
    ) -> AssignDestination {
        let mut dst = dst.clone();
        for expr in &mut dst.index.0 {
            *expr = self.substitute_function_expr(expr, defs);
        }
        for expr in &mut dst.select.0 {
            *expr = self.substitute_function_expr(expr, defs);
        }
        if let Some((_, expr)) = &mut dst.select.1 {
            *expr = self.substitute_function_expr(expr, defs);
        }
        dst
    }

    fn apply_function_call_to_state(
        &self,
        call: &veryl_analyzer::ir::FunctionCall,
        state: &HashMap<VarId, Expression>,
    ) -> Result<HashMap<VarId, Expression>, ParserError> {
        let Some(function) = self.module.functions.get(&call.id) else {
            return Err(ParserError::unsupported(
                43,
                LoweringPhase::FfLowering,
                "function call",
                format!("unknown function id: {:?}", call.id),
                Some(&call.comptime.token),
            ));
        };

        let Some(function_body) = (if let Some(index) = &call.index {
            function.get_function(index)
        } else {
            function.get_function(&[])
        }) else {
            return Err(ParserError::unsupported(
                62,
                LoweringPhase::FfLowering,
                "function call specialization",
                format!("{call}"),
                Some(&call.comptime.token),
            ));
        };

        self.validate_function_call_bindings(call, &function_body)?;

        let mut bindings: HashMap<VarId, Expression> = HashMap::default();
        for (arg_path, arg_id) in &function_body.arg_map {
            if let Some(arg_expr) = function_call_arg(&call.inputs, arg_path) {
                let arg_expr = self.substitute_function_expr(arg_expr, state);
                bindings.insert(
                    *arg_id,
                    self.coerce_function_input_expression(arg_expr, *arg_id),
                );
            }
        }
        let nonlocal_ids: HashSet<VarId> = self
            .module
            .variables
            .iter()
            .filter_map(|(id, variable)| {
                (variable.affiliation != Affiliation::Function).then_some(*id)
            })
            .collect();
        let writes_nonlocal = self.statements_write_any(
            &function_body.statements,
            &nonlocal_ids,
            &mut HashSet::default(),
        );
        let function_state = if call.outputs.is_empty() && !writes_nonlocal {
            None
        } else {
            Some(
                self.apply_statements_to_function_state_in_path(
                    &function_body.statements,
                    function_body.ret,
                    &bindings,
                    FunctionPathCondition::Always,
                )?
                .0,
            )
        };

        // Function outputs are copied out together. Resolve every formal's
        // final value against the pre-call caller state before mutating any
        // actual destination, so the result is independent of map order and
        // overlapping input/output bindings observe their call-time values.
        let ordered_paths = function
            .args
            .iter()
            .flat_map(|arg| arg.members.iter().map(|(path, _, _)| path));
        let mut output_values = Vec::with_capacity(call.outputs.len());
        for arg_path in ordered_paths {
            let Some(dsts) = function_call_arg(&call.outputs, arg_path) else {
                continue;
            };
            if dsts.is_empty() {
                continue;
            }
            let Some(arg_id) = function_body.arg_map.get(arg_path) else {
                return Err(ParserError::unsupported(
                    61,
                    LoweringPhase::FfLowering,
                    "function call missing argument",
                    format!("{call}"),
                    Some(&call.comptime.token),
                ));
            };

            if dsts.len() != 1 {
                return Err(ParserError::unsupported(
                    60,
                    LoweringPhase::FfLowering,
                    "function body call output assignment shape",
                    format!("{call}"),
                    Some(&call.comptime.token),
                ));
            }

            let dst = &dsts[0];
            let is_whole_var =
                dst.index.0.is_empty() && dst.select.0.is_empty() && dst.select.1.is_none();

            let expr = function_state
                .as_ref()
                .and_then(|state| state.get(arg_id))
                .cloned()
                .ok_or_else(|| {
                    ParserError::unsupported(
                        43,
                        LoweringPhase::FfLowering,
                        "function return expression",
                        format!("function target var id: {arg_id:?}"),
                        Some(&call.comptime.token),
                    )
                })?;
            let expr = self.substitute_function_expr(&expr, state);
            let expr = self.coerce_function_state_assignment(expr, dst)?;
            output_values.push((dst, is_whole_var, expr));
        }

        let mut next = state.clone();
        if let Some(function_state) = &function_state {
            for (&id, expr) in function_state {
                if nonlocal_ids.contains(&id) {
                    next.insert(id, self.substitute_function_expr(expr, state));
                }
            }
        }
        for (dst, is_whole_var, expr) in output_values {
            if is_whole_var {
                next.insert(dst.id, expr);
            } else if is_static_access(&dst.index, &dst.select) {
                let old_value = self.state_value_expr(dst.id, &next);
                let merged = build_partial_assign_expr(self.module, dst, expr, old_value)?;
                next.insert(dst.id, merged);
            } else if self.module.variables[&dst.id].affiliation != Affiliation::Function {
                let old_value = self.state_value_expr(dst.id, &next);
                let merged = build_dynamic_partial_assign_expr(self.module, dst, expr, old_value)?;
                next.insert(dst.id, merged);
            } else {
                return Err(ParserError::unsupported(
                    60,
                    LoweringPhase::FfLowering,
                    "function body call output non-whole assignment (dynamic index)",
                    format!("{call}"),
                    Some(&call.comptime.token),
                ));
            }
        }

        Ok(next)
    }

    pub(super) fn get_bound_function_arg_expr(&self, var_id: VarId) -> Option<&Expression> {
        self.function_arg_stack
            .iter()
            .rev()
            .find_map(|bindings| bindings.get(&var_id))
    }

    pub(super) fn get_bound_function_arg_value(&self, var_id: VarId) -> Option<&RegisterId> {
        self.function_arg_value_stack
            .iter()
            .rev()
            .find_map(|bindings| bindings.get(&var_id))
    }

    pub(super) fn get_bound_function_expression_value(
        &self,
        token: TokenRange,
    ) -> Option<&RegisterId> {
        self.function_expression_value_stack
            .iter()
            .rev()
            .find_map(|bindings| bindings.get(&token))
    }

    pub(super) fn get_bound_function_event_arg_state(
        &self,
        token: TokenRange,
    ) -> Option<&HashMap<VarId, Expression>> {
        self.function_event_arg_state_stack
            .iter()
            .rev()
            .find_map(|states| states.get(&token))
    }

    fn expression_references_any(expr: &Expression, candidates: &HashSet<VarId>) -> bool {
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
                    SystemFunctionKind::Bits(_) | SystemFunctionKind::Size(_) => false,
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

    fn collect_expression_read_variables(
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
                    SystemFunctionKind::Bits(_) | SystemFunctionKind::Size(_) => {}
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

    fn expression_writes_any(&self, expr: &Expression, candidates: &HashSet<VarId>) -> bool {
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
                        SystemFunctionKind::Bits(_) | SystemFunctionKind::Size(_) => false,
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

    fn statements_write_any(
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
                    ForBound::Const(_) => false,
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
                SystemFunctionKind::Bits(_) | SystemFunctionKind::Size(_) => false,
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

    fn assignment_destination_writes_any(
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

    #[allow(clippy::too_many_arguments)]
    fn materialize_array_actual_items<A>(
        &mut self,
        expr: &Expression,
        force_materialization: bool,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        // Array views deliberately defer pure elements until the callee reads
        // them. An unused formal therefore never creates a view, but its
        // effectful actual elements must still run at the call site. A pure
        // item before an effectful one must also be snapshotted before that
        // effect changes its dependencies. Preserve every materialized value
        // under its token range so a later view access reuses it instead of
        // replaying the effect or observing a newer value.
        if let Expression::ArrayLiteral(items, _) = expr {
            let last_effectful_item = items.iter().rposition(|item| {
                let item_expr = match item {
                    ArrayLiteralItem::Value(expr, _) | ArrayLiteralItem::Defaul(expr) => expr,
                };
                self.expression_needs_eager_evaluation(item_expr)
            });
            for (item_index, item) in items.iter().enumerate() {
                let item_expr = match item {
                    ArrayLiteralItem::Value(expr, _) | ArrayLiteralItem::Defaul(expr) => expr,
                };
                self.materialize_array_actual_items(
                    item_expr,
                    force_materialization
                        || last_effectful_item.is_some_and(|last| item_index <= last),
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )?;
            }
            return Ok(());
        }

        if !force_materialization && !self.expression_needs_eager_evaluation(expr) {
            return Ok(());
        }

        self.parse_expression(expr, targets, domain, convert, sources, ir_builder, None)?;
        let value = self
            .stack
            .pop_back()
            .expect("Array literal item materialization failed");
        self.function_expression_value_stack
            .last_mut()
            .expect("Function expression value scope is active")
            .insert(expr.token_range(), value);
        Ok(())
    }

    fn materialize_function_inputs<A>(
        &mut self,
        call: &veryl_analyzer::ir::FunctionCall,
        function_body: &veryl_analyzer::ir::FunctionBody,
        ordered_arg_paths: &[veryl_analyzer::ir::VarPath],
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<HashMap<VarId, RegisterId>, ParserError> {
        let mut values = HashMap::default();
        let output_ids: HashSet<VarId> = call
            .outputs
            .values()
            .flat_map(|destinations| destinations.iter().map(|dst| dst.id))
            .collect();
        let has_effectful_output_destination = call
            .outputs
            .values()
            .flatten()
            .any(|dst| self.assignment_destination_needs_eager_evaluation(dst));
        let last_effectful_arg = ordered_arg_paths.iter().rposition(|arg_path| {
            function_call_arg(&call.inputs, arg_path).is_some_and(|actual| {
                super::expression::expression_has_side_effect(actual)
                    || self.expression_has_runtime_effect(actual)
            })
        });
        for (arg_index, arg_path) in ordered_arg_paths.iter().enumerate() {
            let Some(arg_id) = function_body.arg_map.get(arg_path) else {
                continue;
            };
            let Some(actual) = function_call_arg(&call.inputs, arg_path) else {
                continue;
            };
            let must_snapshot_for_order = last_effectful_arg.is_some_and(|last| arg_index <= last);
            let formal = &self.module.variables[arg_id];
            let mut dependencies = HashSet::default();
            self.collect_expression_read_variables(actual, &mut dependencies, false);
            let callee_writes_dependency = self.statements_write_any(
                &function_body.statements,
                &dependencies,
                &mut HashSet::default(),
            );
            if !formal.r#type.array.is_empty() {
                let mut array_variables = HashSet::default();
                self.collect_expression_read_variables(actual, &mut array_variables, true);
                let aliases_later_write = ordered_arg_paths
                    .iter()
                    .skip(arg_index + 1)
                    .filter_map(|path| function_call_arg(&call.inputs, path))
                    .any(|expr| self.expression_writes_any(expr, &array_variables));
                let aliases_output_write = !dependencies.is_disjoint(&output_ids)
                    || call
                        .outputs
                        .values()
                        .flatten()
                        .any(|dst| self.assignment_destination_writes_any(dst, &dependencies));
                let callee_writes_array = self.statements_write_any(
                    &function_body.statements,
                    &array_variables,
                    &mut HashSet::default(),
                );
                let cannot_snapshot_before_callee_write =
                    callee_writes_dependency && !matches!(actual, Expression::ArrayLiteral(_, _));
                if aliases_later_write
                    || aliases_output_write
                    || callee_writes_array
                    || cannot_snapshot_before_callee_write
                {
                    return Err(ParserError::unsupported(
                        43,
                        LoweringPhase::FfLowering,
                        "unpacked function argument aliases later effect",
                        format!("{actual}"),
                        Some(&call.comptime.token),
                    ));
                }
                self.materialize_array_actual_items(
                    actual,
                    (must_snapshot_for_order || callee_writes_dependency)
                        && matches!(actual, Expression::ArrayLiteral(_, _)),
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )?;
                // Array arguments are represented by call-scoped views. The
                // view snapshots each observed element in source order while
                // leaving unobserved, side-effect-free elements lazy; a scalar
                // register cannot preserve that unpacked behavior.
                continue;
            }
            if !must_snapshot_for_order
                && !has_effectful_output_destination
                && !Self::expression_references_any(actual, &output_ids)
                && !callee_writes_dependency
            {
                continue;
            }
            let formal_width = resolve_total_width(self.module, formal)?;
            self.parse_expression(
                actual,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
                Some(formal_width),
            )?;
            let actual_reg = self.stack.pop_back().unwrap();
            let formal_reg = self.coerce_register_to_formal(
                ir_builder,
                actual_reg,
                formal_width,
                actual.comptime().r#type.signed,
                formal.r#type.signed,
                formal.r#type.is_2state(),
            );
            values.insert(*arg_id, formal_reg);
        }
        Ok(values)
    }

    pub(super) fn get_bound_function_array_view(&self, var_id: VarId) -> Option<VarId> {
        for frame in (0..self.function_arg_stack.len()).rev() {
            if self.function_array_view_enabled_stack[frame]
                && self.function_arg_stack[frame].contains_key(&var_id)
            {
                return self.function_array_view_stack[frame]
                    .get(&var_id)
                    .filter(|view| view.initialized.is_none())
                    .map(|view| view.backing_var_id);
            }
        }
        None
    }

    pub(super) fn substitute_function_expr(
        &self,
        expr: &Expression,
        defs: &HashMap<VarId, Expression>,
    ) -> Expression {
        self.substitute_function_expr_inner(expr, defs, &mut HashSet::default())
    }

    fn substitute_function_expr_inner(
        &self,
        expr: &Expression,
        defs: &HashMap<VarId, Expression>,
        expanding: &mut HashSet<VarId>,
    ) -> Expression {
        if self
            .get_bound_function_expression_value(expr.token_range())
            .is_some()
        {
            return expr.clone();
        }
        match expr {
            Expression::Term(factor) => match factor.as_ref() {
                Factor::Variable(var_id, index, select, comptime) => {
                    if Self::is_function_state_base(comptime) {
                        return expr.clone();
                    }
                    let is_whole = index.0.is_empty() && select.0.is_empty() && select.1.is_none();
                    if is_whole && let Some(bound) = defs.get(var_id) {
                        if expanding.insert(*var_id) {
                            let result =
                                self.substitute_function_expr_inner(bound, defs, expanding);
                            expanding.remove(var_id);
                            return result;
                        }
                    }
                    if !defs.contains_key(var_id)
                        && self.module.variables[var_id].affiliation != Affiliation::Function
                    {
                        let mut comptime = comptime.clone();
                        Self::mark_function_state_base_comptime(&mut comptime);
                        return Expression::Term(Box::new(Factor::Variable(
                            *var_id,
                            index.clone(),
                            select.clone(),
                            comptime,
                        )));
                    }
                    expr.clone()
                }
                Factor::FunctionCall(call) => {
                    let mut call = call.clone();
                    for input_expr in call.inputs.values_mut() {
                        *input_expr =
                            self.substitute_function_expr_inner(input_expr, defs, expanding);
                    }
                    Expression::Term(Box::new(Factor::FunctionCall(call)))
                }
                Factor::SystemFunctionCall(call) => {
                    let mut call = call.clone();
                    match &mut call.kind {
                        // These functions evaluate their operand, so carry the
                        // assignment-time symbolic state through them. `$bits`
                        // and `$size` inspect a type/shape and intentionally keep
                        // their operand intact.
                        SystemFunctionKind::Clog2(input)
                        | SystemFunctionKind::Onehot(input)
                        | SystemFunctionKind::Signed(input)
                        | SystemFunctionKind::Unsigned(input) => {
                            input.0 =
                                self.substitute_function_expr_inner(&input.0, defs, expanding);
                        }
                        _ => {}
                    }
                    Expression::Term(Box::new(Factor::SystemFunctionCall(call)))
                }
                _ => expr.clone(),
            },
            Expression::Binary(lhs, op, rhs, comptime) => Expression::Binary(
                Box::new(self.substitute_function_expr_inner(lhs, defs, expanding)),
                *op,
                Box::new(self.substitute_function_expr_inner(rhs, defs, expanding)),
                comptime.clone(),
            ),
            Expression::Unary(op, inner, comptime) => Expression::Unary(
                *op,
                Box::new(self.substitute_function_expr_inner(inner, defs, expanding)),
                comptime.clone(),
            ),
            Expression::Ternary(cond, then_expr, else_expr, comptime) => Expression::Ternary(
                Box::new(self.substitute_function_expr_inner(cond, defs, expanding)),
                Box::new(self.substitute_function_expr_inner(then_expr, defs, expanding)),
                Box::new(self.substitute_function_expr_inner(else_expr, defs, expanding)),
                comptime.clone(),
            ),
            Expression::Concatenation(parts, comptime) => Expression::Concatenation(
                parts
                    .iter()
                    .map(|(x, rep)| {
                        (
                            self.substitute_function_expr_inner(x, defs, expanding),
                            rep.as_ref()
                                .map(|r| self.substitute_function_expr_inner(r, defs, expanding)),
                        )
                    })
                    .collect(),
                comptime.clone(),
            ),
            Expression::ArrayLiteral(items, comptime) => Expression::ArrayLiteral(
                items
                    .iter()
                    .map(|item| match item {
                        ArrayLiteralItem::Value(x, rep) => ArrayLiteralItem::Value(
                            Box::new(self.substitute_function_expr_inner(x, defs, expanding)),
                            rep.as_ref().map(|r| {
                                Box::new(self.substitute_function_expr_inner(r, defs, expanding))
                            }),
                        ),
                        ArrayLiteralItem::Defaul(x) => ArrayLiteralItem::Defaul(Box::new(
                            self.substitute_function_expr_inner(x, defs, expanding),
                        )),
                    })
                    .collect(),
                comptime.clone(),
            ),
            Expression::StructConstructor(ty, fields, comptime) => Expression::StructConstructor(
                ty.clone(),
                fields
                    .iter()
                    .map(|(name, x)| {
                        (
                            *name,
                            self.substitute_function_expr_inner(x, defs, expanding),
                        )
                    })
                    .collect(),
                comptime.clone(),
            ),
        }
    }

    pub(super) fn extract_function_target_expr(
        &self,
        body: &veryl_analyzer::ir::FunctionBody,
        target_id: VarId,
        defs: &HashMap<VarId, Expression>,
    ) -> Result<Expression, ParserError> {
        fn merge_branch_state(
            parser: &FfParser,
            cond: &Expression,
            mut then_state: HashMap<VarId, Expression>,
            else_state: HashMap<VarId, Expression>,
        ) -> HashMap<VarId, Expression> {
            let mut merged = HashMap::default();
            for (id, then_expr) in then_state.drain() {
                if let Some(else_expr) = else_state.get(&id) {
                    merged.insert(
                        id,
                        Expression::Ternary(
                            Box::new(FfParser::normalize_function_control_condition(cond.clone())),
                            Box::new(then_expr),
                            Box::new(else_expr.clone()),
                            Box::new(parser.function_state_merge_comptime(id)),
                        ),
                    );
                }
            }
            merged
        }

        fn build_state_from_statement(
            parser: &FfParser,
            stmt: &Statement,
            state: &HashMap<VarId, Expression>,
            substitute: &impl Fn(&Expression, &HashMap<VarId, Expression>) -> Expression,
        ) -> Result<HashMap<VarId, Expression>, ParserError> {
            match stmt {
                Statement::Assign(assign) => {
                    if assign.dst.len() != 1 {
                        return Err(ParserError::unsupported(
                            43,
                            LoweringPhase::FfLowering,
                            "function body assignment shape",
                            format!("{stmt}"),
                            Some(&assign.token),
                        ));
                    }

                    let dst = &assign.dst[0];
                    let is_whole_var =
                        dst.index.0.is_empty() && dst.select.0.is_empty() && dst.select.1.is_none();

                    let mut next = state.clone();
                    let rhs = substitute(&assign.expr, &next);
                    let rhs = parser.coerce_function_state_assignment(rhs, dst)?;

                    if is_whole_var {
                        next.insert(dst.id, rhs);
                    } else if is_static_access(&dst.index, &dst.select) {
                        let old_value = next.get(&dst.id).cloned().unwrap_or_else(|| {
                            Expression::Term(Box::new(Factor::Variable(
                                dst.id,
                                VarIndex::default(),
                                VarSelect::default(),
                                dst.comptime.clone(),
                            )))
                        });
                        let merged = build_partial_assign_expr(parser.module, dst, rhs, old_value)?;
                        next.insert(dst.id, merged);
                    } else {
                        return Err(ParserError::unsupported(
                            43,
                            LoweringPhase::FfLowering,
                            "function body non-whole assignment (dynamic index)",
                            format!("{stmt}"),
                            Some(&assign.token),
                        ));
                    }
                    Ok(next)
                }
                Statement::If(if_stmt) => {
                    let mut condition_state = state.clone();
                    let (cond, _) = parser.capture_nested_function_outputs_with_states(
                        &if_stmt.cond,
                        &mut condition_state,
                    )?;
                    let then_state = build_state_from_statements(
                        parser,
                        &if_stmt.true_side,
                        &condition_state,
                        substitute,
                    )?;
                    let else_state = build_state_from_statements(
                        parser,
                        &if_stmt.false_side,
                        &condition_state,
                        substitute,
                    )?;
                    let cond = substitute(&cond, &condition_state);
                    Ok(merge_branch_state(parser, &cond, then_state, else_state))
                }
                Statement::Case(case_stmt) => {
                    build_state_from_case(parser, case_stmt, 0, state, substitute)
                }
                Statement::Null => Ok(state.clone()),
                Statement::IfReset(ir) => Err(ParserError::unsupported(
                    43,
                    LoweringPhase::FfLowering,
                    "function body control flow",
                    format!("{stmt}"),
                    Some(&ir.token),
                )),
                Statement::SystemFunctionCall(call) => {
                    let (_, next_state, _) = parser.prepare_system_function_call(call, state)?;
                    Ok(next_state)
                }
                Statement::FunctionCall(call) => {
                    parser.apply_statement_function_call_to_state(call, state)
                }
                Statement::For(f) => Err(ParserError::unsupported(
                    43,
                    LoweringPhase::FfLowering,
                    "for loop in function body",
                    format!("{stmt}"),
                    Some(&f.token),
                )),
                Statement::TbMethodCall(_) | Statement::Break | Statement::Unsupported(_) => {
                    Err(ParserError::unsupported(
                        43,
                        LoweringPhase::FfLowering,
                        "function body control flow",
                        format!("{stmt}"),
                        None,
                    ))
                }
            }
        }

        fn build_state_from_case(
            parser: &FfParser,
            case_stmt: &CaseStatement,
            arm_index: usize,
            state: &HashMap<VarId, Expression>,
            substitute: &impl Fn(&Expression, &HashMap<VarId, Expression>) -> Expression,
        ) -> Result<HashMap<VarId, Expression>, ParserError> {
            let Some(arm) = case_stmt.arms.get(arm_index) else {
                return build_state_from_statements(parser, &case_stmt.default, state, substitute);
            };

            let then_state = build_state_from_statements(parser, &arm.body, state, substitute)?;
            let else_state =
                build_state_from_case(parser, case_stmt, arm_index + 1, state, substitute)?;
            let cond = substitute(
                &case_arm_condition_expr(&case_stmt.case_target, &arm.patterns),
                state,
            );
            Ok(merge_branch_state(parser, &cond, then_state, else_state))
        }

        fn build_state_from_statements(
            parser: &FfParser,
            statements: &[Statement],
            initial: &HashMap<VarId, Expression>,
            substitute: &impl Fn(&Expression, &HashMap<VarId, Expression>) -> Expression,
        ) -> Result<HashMap<VarId, Expression>, ParserError> {
            let mut state = initial.clone();
            for stmt in statements {
                state = build_state_from_statement(parser, stmt, &state, substitute)?;
            }
            Ok(state)
        }

        let state = build_state_from_statements(self, &body.statements, defs, &|expr, defs| {
            self.substitute_function_expr(expr, defs)
        })?;
        state.get(&target_id).cloned().ok_or_else(|| {
            ParserError::unsupported(
                43,
                LoweringPhase::FfLowering,
                "function return expression",
                format!("function target var id: {:?}", target_id),
                None,
            )
        })
    }

    pub(super) fn extract_function_return_expr(
        &self,
        body: &veryl_analyzer::ir::FunctionBody,
        ret_id: VarId,
    ) -> Result<Expression, ParserError> {
        fn resolve_return_expr(
            parser: &FfParser,
            statements: &[Statement],
            ret_id: VarId,
            defs: &HashMap<VarId, Expression>,
            substitute: &impl Fn(&Expression, &HashMap<VarId, Expression>) -> Expression,
        ) -> Result<Option<Expression>, ParserError> {
            if statements.is_empty() {
                return Ok(None);
            }

            let stmt = &statements[0];
            let rest = &statements[1..];

            match stmt {
                Statement::Assign(assign) => {
                    if assign.dst.len() != 1 {
                        return Err(ParserError::unsupported(
                            43,
                            LoweringPhase::FfLowering,
                            "function body assignment shape",
                            format!("{stmt}"),
                            Some(&assign.token),
                        ));
                    }

                    let dst = &assign.dst[0];
                    let is_whole_var =
                        dst.index.0.is_empty() && dst.select.0.is_empty() && dst.select.1.is_none();

                    let rhs = substitute(&assign.expr, defs);
                    let rhs = parser.coerce_function_state_assignment(rhs, dst)?;

                    if is_whole_var {
                        if dst.id == ret_id {
                            // Assignment to return variable corresponds to `return` and terminates
                            // this path.
                            return Ok(Some(rhs));
                        }

                        let mut next_defs = defs.clone();
                        next_defs.insert(dst.id, rhs);
                        resolve_return_expr(parser, rest, ret_id, &next_defs, substitute)
                    } else if is_static_access(&dst.index, &dst.select) {
                        let old_value = defs.get(&dst.id).cloned().unwrap_or_else(|| {
                            Expression::Term(Box::new(Factor::Variable(
                                dst.id,
                                VarIndex::default(),
                                VarSelect::default(),
                                dst.comptime.clone(),
                            )))
                        });
                        let merged = build_partial_assign_expr(parser.module, dst, rhs, old_value)?;

                        // Partial write to return var does NOT terminate the path —
                        // additional writes may fill in other bits.
                        let mut next_defs = defs.clone();
                        next_defs.insert(dst.id, merged);
                        resolve_return_expr(parser, rest, ret_id, &next_defs, substitute)
                    } else {
                        Err(ParserError::unsupported(
                            43,
                            LoweringPhase::FfLowering,
                            "function body non-whole assignment (dynamic index)",
                            format!("{stmt}"),
                            Some(&assign.token),
                        ))
                    }
                }
                Statement::If(if_stmt) => {
                    let mut condition_defs = defs.clone();
                    let (cond, _) = parser.capture_nested_function_outputs_with_states(
                        &if_stmt.cond,
                        &mut condition_defs,
                    )?;
                    let cond = substitute(&cond, &condition_defs);

                    let mut then_stmts = if_stmt.true_side.clone();
                    then_stmts.extend_from_slice(rest);
                    let then_expr = resolve_return_expr(
                        parser,
                        &then_stmts,
                        ret_id,
                        &condition_defs,
                        substitute,
                    )?;

                    let mut else_stmts = if_stmt.false_side.clone();
                    else_stmts.extend_from_slice(rest);
                    let else_expr = resolve_return_expr(
                        parser,
                        &else_stmts,
                        ret_id,
                        &condition_defs,
                        substitute,
                    )?;

                    match (then_expr, else_expr) {
                        (Some(then_expr), Some(else_expr)) => Ok(Some(Expression::Ternary(
                            Box::new(FfParser::normalize_function_control_condition(cond)),
                            Box::new(then_expr),
                            Box::new(else_expr),
                            Box::new(Comptime::create_unknown(TokenRange::default())),
                        ))),
                        _ => Ok(None),
                    }
                }
                Statement::Case(case_stmt) => {
                    resolve_return_expr_case(parser, case_stmt, rest, ret_id, defs, substitute)
                }
                Statement::Null => resolve_return_expr(parser, rest, ret_id, defs, substitute),
                Statement::IfReset(ir) => Err(ParserError::unsupported(
                    43,
                    LoweringPhase::FfLowering,
                    "function body control flow",
                    format!("{stmt}"),
                    Some(&ir.token),
                )),
                Statement::SystemFunctionCall(call) => {
                    let (_, next_defs, _) = parser.prepare_system_function_call(call, defs)?;
                    resolve_return_expr(parser, rest, ret_id, &next_defs, substitute)
                }
                Statement::FunctionCall(call) => {
                    let next_defs = parser.apply_statement_function_call_to_state(call, defs)?;
                    resolve_return_expr(parser, rest, ret_id, &next_defs, substitute)
                }
                Statement::For(f) => Err(ParserError::unsupported(
                    43,
                    LoweringPhase::FfLowering,
                    "for loop in function body",
                    format!("{stmt}"),
                    Some(&f.token),
                )),
                Statement::TbMethodCall(_) | Statement::Break | Statement::Unsupported(_) => {
                    Err(ParserError::unsupported(
                        43,
                        LoweringPhase::FfLowering,
                        "function body control flow",
                        format!("{stmt}"),
                        None,
                    ))
                }
            }
        }

        fn resolve_return_expr_case(
            parser: &FfParser,
            case_stmt: &CaseStatement,
            rest: &[Statement],
            ret_id: VarId,
            defs: &HashMap<VarId, Expression>,
            substitute: &impl Fn(&Expression, &HashMap<VarId, Expression>) -> Expression,
        ) -> Result<Option<Expression>, ParserError> {
            fn resolve_from_arm(
                parser: &FfParser,
                case_stmt: &CaseStatement,
                arm_index: usize,
                rest: &[Statement],
                ret_id: VarId,
                defs: &HashMap<VarId, Expression>,
                substitute: &impl Fn(&Expression, &HashMap<VarId, Expression>) -> Expression,
            ) -> Result<Option<Expression>, ParserError> {
                let Some(arm) = case_stmt.arms.get(arm_index) else {
                    let mut default_stmts = case_stmt.default.clone();
                    default_stmts.extend_from_slice(rest);
                    return resolve_return_expr(parser, &default_stmts, ret_id, defs, substitute);
                };

                let cond = substitute(
                    &case_arm_condition_expr(&case_stmt.case_target, &arm.patterns),
                    defs,
                );

                let mut then_stmts = arm.body.clone();
                then_stmts.extend_from_slice(rest);
                let then_expr = resolve_return_expr(parser, &then_stmts, ret_id, defs, substitute)?;
                let else_expr = resolve_from_arm(
                    parser,
                    case_stmt,
                    arm_index + 1,
                    rest,
                    ret_id,
                    defs,
                    substitute,
                )?;

                match (then_expr, else_expr) {
                    (Some(then_expr), Some(else_expr)) => Ok(Some(Expression::Ternary(
                        Box::new(FfParser::normalize_function_control_condition(cond)),
                        Box::new(then_expr),
                        Box::new(else_expr),
                        Box::new(Comptime::create_unknown(TokenRange::default())),
                    ))),
                    _ => Ok(None),
                }
            }

            resolve_from_arm(parser, case_stmt, 0, rest, ret_id, defs, substitute)
        }

        resolve_return_expr(
            self,
            &body.statements,
            ret_id,
            &HashMap::default(),
            &|expr, defs| self.substitute_function_expr(expr, defs),
        )?
        .ok_or_else(|| {
            ParserError::unsupported(
                43,
                LoweringPhase::FfLowering,
                "function return expression",
                format!("function call to id {:?}", ret_id),
                None,
            )
        })
    }

    pub(super) fn parse_function_call_expr<A>(
        &mut self,
        call: &veryl_analyzer::ir::FunctionCall,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let Some(function) = self.module.functions.get(&call.id) else {
            return Err(ParserError::unsupported(
                43,
                LoweringPhase::FfLowering,
                "function call",
                format!("unknown function id: {:?}", call.id),
                Some(&call.comptime.token),
            ));
        };

        let Some(function_body) = (if let Some(index) = &call.index {
            function.get_function(index)
        } else {
            function.get_function(&[])
        }) else {
            return Err(ParserError::unsupported(
                62,
                LoweringPhase::FfLowering,
                "function call specialization",
                format!("{call}"),
                Some(&call.comptime.token),
            ));
        };
        let ordered_arg_paths: Vec<_> = function
            .args
            .iter()
            .flat_map(|arg| arg.members.iter().map(|(path, _, _)| path.clone()))
            .collect();

        self.validate_function_call_bindings(call, &function_body)?;
        self.flush_captured_nonlocal_state_before_call(
            call, targets, domain, convert, sources, ir_builder,
        )?;

        let mut bindings: HashMap<VarId, Expression> = HashMap::default();
        for (arg_path, arg_id) in &function_body.arg_map {
            if let Some(arg_expr) = function_call_arg(&call.inputs, arg_path) {
                let arg_expr = self.coerce_function_input_expression(arg_expr.clone(), *arg_id);
                bindings.insert(*arg_id, arg_expr);
            }
        }
        self.function_expression_value_stack
            .push(HashMap::default());
        let materialized = match self.materialize_function_inputs(
            call,
            &function_body,
            &ordered_arg_paths,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
        ) {
            Ok(materialized) => materialized,
            Err(err) => {
                self.function_expression_value_stack.pop();
                return Err(err);
            }
        };
        let mut symbolic_bindings = bindings.clone();
        for arg_id in materialized.keys() {
            symbolic_bindings.remove(arg_id);
        }
        symbolic_bindings.retain(|var_id, _| self.module.variables[var_id].r#type.array.is_empty());

        self.function_arg_stack.push(bindings);
        self.function_arg_value_stack.push(materialized);
        self.function_array_view_stack.push(HashMap::default());
        self.function_array_view_enabled_stack.push(true);
        let result = (|| {
            let runtime_state = if self
                .statements_have_runtime_effect(&function_body.statements, &mut HashSet::default())
            {
                Some(self.emit_function_runtime_effects(
                    &function_body.statements,
                    function_body.ret,
                    &symbolic_bindings,
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )?)
            } else {
                None
            };

            let mut pending_outputs = Vec::new();
            for arg_path in &ordered_arg_paths {
                let Some(dsts) = function_call_arg(&call.outputs, arg_path) else {
                    continue;
                };
                if dsts.is_empty() {
                    continue;
                }
                let Some(arg_id) = function_body.arg_map.get(arg_path) else {
                    return Err(ParserError::unsupported(
                        61,
                        LoweringPhase::FfLowering,
                        "function call missing argument",
                        format!("{call}"),
                        Some(&call.comptime.token),
                    ));
                };

                let expr = if let Some(expr) =
                    runtime_state.as_ref().and_then(|state| state.get(arg_id))
                {
                    expr.clone()
                } else {
                    self.extract_function_target_expr(&function_body, *arg_id, &symbolic_bindings)?
                };
                self.parse_expression(&expr, targets, domain, convert, sources, ir_builder, None)?;

                let rhs_reg = self
                    .stack
                    .pop_back()
                    .expect("Function output expression evaluation failed");
                let rhs_reg = self.coerce_register_to_variable_type(
                    rhs_reg,
                    *arg_id,
                    expression_signed(&expr),
                    ir_builder,
                )?;
                let dsts = if let Some(state) = runtime_state.as_ref() {
                    dsts.iter()
                        .map(|dst| self.substitute_assignment_destination(dst, state))
                        .collect()
                } else {
                    dsts.clone()
                };
                pending_outputs.push((rhs_reg, dsts));
            }

            let Some(ret_id) = function_body.ret else {
                return Err(ParserError::illegal_context(
                    "void function call in expression",
                    format!("{call}"),
                    Some(&call.comptime.token),
                ));
            };
            let ret_expr =
                if let Some(expr) = runtime_state.as_ref().and_then(|state| state.get(&ret_id)) {
                    expr.clone()
                } else {
                    self.extract_function_return_expr(&function_body, ret_id)?
                };
            self.parse_expression(
                &ret_expr, targets, domain, convert, sources, ir_builder, None,
            )?;
            let ret_reg = self
                .stack
                .pop_back()
                .expect("Function return expression evaluation failed");
            let ret_reg = self.coerce_register_to_variable_type(
                ret_reg,
                ret_id,
                expression_signed(&ret_expr),
                ir_builder,
            )?;
            if let Some(state) = runtime_state.as_ref() {
                self.emit_nonlocal_function_state_writes(
                    state, targets, domain, convert, sources, ir_builder,
                )?;
            }
            for (rhs_reg, dsts) in pending_outputs {
                self.emit_multi_dst_assign(
                    rhs_reg, &dsts, targets, domain, convert, sources, ir_builder,
                )?;
            }
            self.stack.push_back(ret_reg);
            Ok(())
        })();
        self.function_expression_value_stack.pop();
        self.function_arg_value_stack.pop();
        self.function_array_view_enabled_stack.pop();
        let finished_views = self.function_array_view_stack.pop().unwrap();
        self.function_arg_stack.pop();
        self.restore_active_function_array_views(&finished_views, convert, ir_builder)?;
        result
    }

    pub(super) fn parse_function_call_statement<A>(
        &mut self,
        call: &veryl_analyzer::ir::FunctionCall,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let Some(function) = self.module.functions.get(&call.id) else {
            return Err(ParserError::unsupported(
                43,
                LoweringPhase::FfLowering,
                "function call",
                format!("unknown function id: {:?}", call.id),
                Some(&call.comptime.token),
            ));
        };

        let Some(function_body) = (if let Some(index) = &call.index {
            function.get_function(index)
        } else {
            function.get_function(&[])
        }) else {
            return Err(ParserError::unsupported(
                62,
                LoweringPhase::FfLowering,
                "function call specialization",
                format!("{call}"),
                Some(&call.comptime.token),
            ));
        };
        let ordered_arg_paths: Vec<_> = function
            .args
            .iter()
            .flat_map(|arg| arg.members.iter().map(|(path, _, _)| path.clone()))
            .collect();

        self.validate_function_call_bindings(call, &function_body)?;

        let has_runtime_effect =
            self.statements_have_runtime_effect(&function_body.statements, &mut HashSet::default());
        let has_effectful_input = call
            .inputs
            .values()
            .any(|expr| self.expression_needs_eager_evaluation(expr));
        if call.outputs.is_empty() && !has_runtime_effect && !has_effectful_input {
            // A pure statement-form function call has no observable result.
            return Ok(());
        }

        // Statement-form call ignores return value, if present.

        let mut bindings: HashMap<VarId, Expression> = HashMap::default();
        for (arg_path, arg_id) in &function_body.arg_map {
            if let Some(arg_expr) = function_call_arg(&call.inputs, arg_path) {
                let arg_expr = self.coerce_function_input_expression(arg_expr.clone(), *arg_id);
                bindings.insert(*arg_id, arg_expr);
            }
        }
        self.function_expression_value_stack
            .push(HashMap::default());
        let materialized = match self.materialize_function_inputs(
            call,
            &function_body,
            &ordered_arg_paths,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
        ) {
            Ok(materialized) => materialized,
            Err(err) => {
                self.function_expression_value_stack.pop();
                return Err(err);
            }
        };
        let mut symbolic_bindings = bindings.clone();
        for arg_id in materialized.keys() {
            symbolic_bindings.remove(arg_id);
        }
        symbolic_bindings.retain(|var_id, _| self.module.variables[var_id].r#type.array.is_empty());

        self.function_arg_stack.push(bindings);
        self.function_arg_value_stack.push(materialized);
        self.function_array_view_stack.push(HashMap::default());
        self.function_array_view_enabled_stack.push(true);
        let result = (|| {
            let runtime_state = if has_runtime_effect {
                Some(self.emit_function_runtime_effects(
                    &function_body.statements,
                    function_body.ret,
                    &symbolic_bindings,
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )?)
            } else {
                None
            };

            let mut pending_outputs = Vec::new();
            for arg_path in &ordered_arg_paths {
                let Some(dsts) = function_call_arg(&call.outputs, arg_path) else {
                    continue;
                };
                if dsts.is_empty() {
                    continue;
                }
                let Some(arg_id) = function_body.arg_map.get(arg_path) else {
                    return Err(ParserError::unsupported(
                        61,
                        LoweringPhase::FfLowering,
                        "function call missing argument",
                        format!("{call}"),
                        Some(&call.comptime.token),
                    ));
                };

                let expr = if let Some(expr) =
                    runtime_state.as_ref().and_then(|state| state.get(arg_id))
                {
                    expr.clone()
                } else {
                    self.extract_function_target_expr(&function_body, *arg_id, &symbolic_bindings)?
                };
                self.parse_expression(&expr, targets, domain, convert, sources, ir_builder, None)?;

                let rhs_reg = self
                    .stack
                    .pop_back()
                    .expect("Function output expression evaluation failed");
                let rhs_reg = self.coerce_register_to_variable_type(
                    rhs_reg,
                    *arg_id,
                    expression_signed(&expr),
                    ir_builder,
                )?;
                let dsts = if let Some(state) = runtime_state.as_ref() {
                    dsts.iter()
                        .map(|dst| self.substitute_assignment_destination(dst, state))
                        .collect()
                } else {
                    dsts.clone()
                };
                pending_outputs.push((rhs_reg, dsts));
            }

            if let Some(state) = runtime_state.as_ref() {
                self.emit_nonlocal_function_state_writes(
                    state, targets, domain, convert, sources, ir_builder,
                )?;
            }
            for (rhs_reg, dsts) in pending_outputs {
                self.emit_multi_dst_assign(
                    rhs_reg, &dsts, targets, domain, convert, sources, ir_builder,
                )?;
            }

            Ok(())
        })();
        self.function_expression_value_stack.pop();
        self.function_arg_value_stack.pop();
        self.function_array_view_enabled_stack.pop();
        let finished_views = self.function_array_view_stack.pop().unwrap();
        self.function_arg_stack.pop();
        self.restore_active_function_array_views(&finished_views, convert, ir_builder)?;
        result
    }
}
