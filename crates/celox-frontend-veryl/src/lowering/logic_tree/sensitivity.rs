//! Authored reads of an implicit combinational process, before symbolic
//! execution removes overwritten expressions or unreachable value projections.
use super::*;

pub(crate) fn implicit_reads(
    module: &Module,
    statements: &[Statement],
) -> Result<HashSet<VarAtomBase<VarId>>, ParserError> {
    let mut reads = Reads {
        module,
        atoms: HashSet::default(),
        active: HashSet::default(),
    };
    reads.statements(statements)?;
    reads.atoms.retain(|atom| {
        module.variables.get(&atom.id).is_some_and(|var| {
            var.affiliation != veryl_analyzer::symbol::Affiliation::Function
                && var.affiliation != veryl_analyzer::symbol::Affiliation::AlwaysComb
        })
    });
    Ok(reads.atoms)
}

struct Reads<'a> {
    module: &'a Module,
    atoms: HashSet<VarAtomBase<VarId>>,
    active: HashSet<VarId>,
}

impl Reads<'_> {
    fn selection(&mut self, index: &VarIndex, select: &VarSelect) -> Result<(), ParserError> {
        for expr in index
            .0
            .iter()
            .chain(select.0.iter())
            .chain(select.1.iter().map(|(_, expr)| expr))
        {
            self.expression(expr)?;
        }
        Ok(())
    }

    fn call(&mut self, call: &FunctionCall) -> Result<(), ParserError> {
        for input in call.inputs.values() {
            self.expression(input)?;
        }
        for dst in call.outputs.values().flatten() {
            self.selection(&dst.index, &dst.select)?;
        }
        if self.active.insert(call.id) {
            if let Some(body) =
                self.module.functions.get(&call.id).and_then(|function| {
                    function.get_function(call.index.as_deref().unwrap_or(&[]))
                })
            {
                self.statements(&body.statements)?;
            }
            self.active.remove(&call.id);
        }
        Ok(())
    }

    fn system(&mut self, kind: &SystemFunctionKind) -> Result<(), ParserError> {
        match kind {
            // Shape queries do not evaluate their operand.
            SystemFunctionKind::Bits(_)
            | SystemFunctionKind::Size(_, _)
            | SystemFunctionKind::Finish => {}
            SystemFunctionKind::Clog2(input)
            | SystemFunctionKind::Onehot(input)
            | SystemFunctionKind::Signed(input)
            | SystemFunctionKind::Unsigned(input)
            | SystemFunctionKind::Readmemh(input, _) => self.expression(&input.0)?,
            SystemFunctionKind::Display(args) | SystemFunctionKind::Write(args) => {
                for arg in args {
                    self.expression(&arg.0)?;
                }
            }
            SystemFunctionKind::Assert { cond, args, .. } => {
                self.expression(&cond.0)?;
                for arg in args {
                    self.expression(&arg.0)?;
                }
            }
        }
        Ok(())
    }

    fn expression(&mut self, expr: &Expression) -> Result<(), ParserError> {
        match expr {
            Expression::Term(factor) => match factor.as_ref() {
                Factor::Variable(id, index, select, _) => {
                    let access = eval_var_select(self.module, *id, index, select)?;
                    self.atoms
                        .insert(VarAtomBase::new(*id, access.lsb, access.msb));
                    self.selection(index, select)?;
                }
                Factor::FunctionCall(call) => self.call(call)?,
                Factor::SystemFunctionCall(call) => self.system(&call.kind)?,
                _ => {}
            },
            Expression::Unary(_, inner, _) => self.expression(inner)?,
            Expression::Binary(lhs, _, rhs, _) => {
                self.expression(lhs)?;
                self.expression(rhs)?;
            }
            Expression::Ternary(cond, yes, no, _) => {
                self.expression(cond)?;
                self.expression(yes)?;
                self.expression(no)?;
            }
            Expression::Concatenation(parts, _) => {
                for (part, _) in parts {
                    self.expression(part)?;
                }
            }
            Expression::ArrayLiteral(items, _) => {
                for item in items {
                    match item {
                        ArrayLiteralItem::Value(value, _) | ArrayLiteralItem::Defaul(value) => {
                            self.expression(value)?
                        }
                    }
                }
            }
            Expression::StructConstructor(_, fields, _) => {
                for (_, value) in fields {
                    self.expression(value)?;
                }
            }
        }
        Ok(())
    }

    fn statements(&mut self, statements: &[Statement]) -> Result<(), ParserError> {
        for statement in statements {
            match statement {
                Statement::Assign(assign) => {
                    self.expression(&assign.expr)?;
                    for dst in &assign.dst {
                        self.selection(&dst.index, &dst.select)?;
                    }
                }
                Statement::If(stmt) => {
                    self.expression(&stmt.cond)?;
                    self.statements(&stmt.true_side)?;
                    self.statements(&stmt.false_side)?;
                }
                Statement::IfReset(stmt) => {
                    self.statements(&stmt.true_side)?;
                    self.statements(&stmt.false_side)?;
                }
                Statement::Case(stmt) => {
                    self.expression(&stmt.case_target)?;
                    for arm in &stmt.arms {
                        for pattern in &arm.patterns {
                            match pattern {
                                CasePattern::Eq(expr) => self.expression(expr)?,
                                CasePattern::Range { lo, hi, .. } => {
                                    self.expression(lo)?;
                                    self.expression(hi)?;
                                }
                            }
                        }
                        self.statements(&arm.body)?;
                    }
                    self.statements(&stmt.default)?;
                }
                Statement::For(stmt) => {
                    let (start, end) = stmt.range.bounds();
                    for bound in [start, end] {
                        if let ForBound::Expression(expr) = bound {
                            self.expression(expr)?;
                        }
                    }
                    self.statements(&stmt.body)?;
                }
                Statement::FunctionCall(call) => self.call(call)?,
                Statement::SystemFunctionCall(call) => self.system(&call.kind)?,
                Statement::TbMethodCall(_)
                | Statement::Break
                | Statement::Unsupported(_)
                | Statement::Null => {}
            }
        }
        Ok(())
    }
}
