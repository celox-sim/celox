//! Increment and decrement operators (IEEE 1800-2023 11.4.2).
//!
//! An operator is executed as statements before the statement that holds
//! it: they read the operand into a hidden local, write the updated value
//! back with a blocking assignment, and the operator binds to the local,
//! the operand's value before (`x++`) or after (`++x`) the update. The
//! operators of an operand that `&&`, `||`, `->` or `?:` may skip (11.4.7,
//! 11.4.11) run only where it is evaluated: the operand that decides is
//! evaluated once, into a hidden local the expression then reads. A loop
//! condition runs its operators every time it is evaluated (see
//! [`BodyBuilder::loop_condition`]). Other contexts that skip or repeat an
//! operand keep rejecting it.
use super::*;
use celox_sv_syntax::{Declarations, DimensionKind, ExprId, ExprKind};

/// The operand of increment or decrement `id`, whether it is postfix, and
/// whether it increments.
fn update(native: &Declarations, id: ExprId) -> Option<(ExprId, bool, bool)> {
    match &native.expression(id).kind {
        ExprKind::Unary {
            operator,
            expression,
            postfix,
        } if matches!(operator.as_str(), "++" | "--") => {
            Some((*expression, *postfix, operator == "++"))
        }
        _ => None,
    }
}

fn dimension_operands(dimension: &DimensionKind, operands: &mut Vec<ExprId>) {
    match dimension {
        DimensionKind::Index(id) | DimensionKind::Queue(Some(id)) => operands.push(*id),
        DimensionKind::Range { left, right } => operands.extend([*left, *right]),
        DimensionKind::Indexed { base, width, .. } => operands.extend([*base, *width]),
        DimensionKind::Unsized | DimensionKind::Queue(None) | DimensionKind::Wildcard => {}
    }
}

/// Every operand of `kind`, in source order.
fn operands(kind: &ExprKind) -> Vec<ExprId> {
    let mut operands = Vec::new();
    match kind {
        ExprKind::Literal(_) | ExprKind::Name(_) | ExprKind::Type(_) => {}
        ExprKind::MinTypMax {
            minimum,
            typical,
            maximum,
        } => operands.extend([*minimum, *typical, *maximum]),
        ExprKind::Group(id) | ExprKind::Copy(id) => operands.push(*id),
        ExprKind::Unary { expression, .. } | ExprKind::Member { expression, .. } => {
            operands.push(*expression)
        }
        ExprKind::Binary { left, right, .. } | ExprKind::Assignment { left, right, .. } => {
            operands.extend([*left, *right])
        }
        ExprKind::Conditional {
            condition,
            then_expression,
            else_expression,
        } => operands.extend([*condition, *then_expression, *else_expression]),
        ExprKind::Tagged { value, .. } => operands.extend(*value),
        ExprKind::Call {
            expression,
            arguments,
        }
        | ExprKind::Parameterized {
            expression,
            arguments,
        } => {
            operands.push(*expression);
            operands.extend(arguments.iter().filter_map(|argument| argument.value));
        }
        ExprKind::ArrayMethodWith {
            expression,
            predicate,
        } => operands.extend([*expression, *predicate]),
        ExprKind::RandomizeWith {
            expression,
            constraints,
        } => {
            operands.push(*expression);
            operands.extend(constraints.iter().flatten());
        }
        ExprKind::Select {
            expression,
            dimension,
        }
        | ExprKind::StreamSelect {
            expression,
            dimension,
        } => {
            operands.push(*expression);
            dimension_operands(&dimension.kind, &mut operands);
        }
        ExprKind::Cast { target, expression } => operands.extend([*target, *expression]),
        ExprKind::Concatenation(items) => operands.extend(items),
        ExprKind::Replication { count, expressions } => {
            operands.push(*count);
            operands.extend(expressions);
        }
        ExprKind::Pattern { target, items } => {
            operands.extend(*target);
            for item in items {
                operands.extend(item.key);
                operands.push(item.value);
            }
        }
        ExprKind::Set(dimensions) => {
            for dimension in dimensions {
                dimension_operands(&dimension.kind, &mut operands);
            }
        }
        ExprKind::Streaming {
            slice, expressions, ..
        } => {
            operands.extend(*slice);
            operands.extend(expressions);
        }
    }
    operands
}

/// The expressions of `roots` that hold an increment or decrement.
fn updating(native: &Declarations, roots: &[ExprId]) -> HashSet<ExprId> {
    let mut updating = HashSet::default();
    let mut jobs: Vec<(ExprId, bool)> = roots.iter().map(|id| (*id, false)).collect();
    let mut path = Vec::new();
    while let Some((id, exit)) = jobs.pop() {
        if exit {
            path.pop();
            continue;
        }
        if update(native, id).is_some() {
            updating.insert(id);
            updating.extend(path.iter().copied());
        }
        path.push(id);
        jobs.push((id, true));
        jobs.extend(
            operands(&native.expression(id).kind)
                .into_iter()
                .rev()
                .map(|id| (id, false)),
        );
    }
    updating
}

fn unsupported_context() -> AnalyzerError {
    unsupported("increment or decrement expression in this context")
}

impl BodyBuilder<'_, '_, '_> {
    /// Execute the increment and decrement operators of `roots`, evaluated
    /// in that order, and return the statements that do.
    pub(super) fn effects(&mut self, roots: &[ExprId]) -> Converted<Vec<Stmt>> {
        let native = self.tree.declarations();
        let updating = updating(native, roots);
        let mut stmts = Vec::new();
        if updating.is_empty() {
            return Ok(stmts);
        }
        for root in roots {
            self.effect(*root, &updating, &mut stmts)?;
        }
        Ok(stmts)
    }

    fn effect(
        &mut self,
        id: ExprId,
        updating: &HashSet<ExprId>,
        stmts: &mut Vec<Stmt>,
    ) -> Converted<()> {
        if !updating.contains(&id) {
            return Ok(());
        }
        let native = self.tree.declarations();
        if let Some((operand, postfix, increment)) = update(native, id) {
            self.effect(operand, updating, stmts)?;
            return self.update(id, operand, Some(postfix), increment, stmts);
        }
        match &native.expression(id).kind {
            ExprKind::Binary {
                left,
                operator,
                right,
            } if matches!(operator.as_str(), "&&" | "||" | "->") && updating.contains(right) => {
                self.effect(*left, updating, stmts)?;
                let decided = self.truth(*left, stmts)?;
                // `&&` and `->` evaluate the right operand unless the left
                // one is false, `||` unless it is true.
                let skip = if operator == "||" { "1'b1" } else { "1'b0" };
                let mut right_stmts = Vec::new();
                self.effect(*right, updating, &mut right_stmts)?;
                stmts.push(Self::unless(&decided, skip, right_stmts));
            }
            ExprKind::Conditional {
                condition,
                then_expression,
                else_expression,
            } if updating.contains(then_expression) || updating.contains(else_expression) => {
                self.effect(*condition, updating, stmts)?;
                let decided = self.truth(*condition, stmts)?;
                // An unknown condition evaluates both (11.4.11).
                for (arm, skip) in [(*then_expression, "1'b0"), (*else_expression, "1'b1")] {
                    let mut arm_stmts = Vec::new();
                    self.effect(arm, updating, &mut arm_stmts)?;
                    if !arm_stmts.is_empty() {
                        stmts.push(Self::unless(&decided, skip, arm_stmts));
                    }
                }
            }
            ExprKind::Group(_)
            | ExprKind::Unary { .. }
            | ExprKind::Binary { .. }
            | ExprKind::Conditional { .. }
            | ExprKind::Member { .. }
            | ExprKind::Select { .. }
            | ExprKind::Concatenation(_) => {
                for operand in operands(&native.expression(id).kind) {
                    self.effect(operand, updating, stmts)?;
                }
            }
            // The callee, then each actual.
            ExprKind::Call {
                expression,
                arguments,
            } if !matches!(
                native.expression(*expression).kind,
                ExprKind::ArrayMethodWith { .. } | ExprKind::RandomizeWith { .. }
            ) =>
            {
                self.effect(*expression, updating, stmts)?;
                // A query does not evaluate its first operand (20.6, 20.7).
                let unevaluated = match &native.expression(*expression).kind {
                    ExprKind::Name(name) => SystemFunction::from_name(&name.spelling)
                        .is_some_and(SystemFunction::has_unevaluated_operand),
                    _ => false,
                };
                for argument in arguments
                    .iter()
                    .skip(usize::from(unevaluated))
                    .filter_map(|argument| argument.value)
                {
                    self.effect(argument, updating, stmts)?;
                }
            }
            ExprKind::Cast { expression, .. } => self.effect(*expression, updating, stmts)?,
            ExprKind::Replication { expressions, .. } => {
                for operand in expressions {
                    self.effect(*operand, updating, stmts)?;
                }
            }
            _ => return Err(unsupported_context()),
        }
        Ok(())
    }

    /// `stmts`, run unless `decided` is `skip`.
    fn unless(decided: &str, skip: &str, stmts: Vec<Stmt>) -> Stmt {
        Stmt::If {
            condition: Expr::Binary {
                left: Box::new(Expr::Ident(decided.to_string())),
                op: BinaryOp::NeCase,
                right: Box::new(Expr::Literal(skip.into())),
            },
            then_body: stmts,
            else_body: Vec::new(),
        }
    }

    /// Declare a hidden local of type `ty` and return its source and
    /// unique names.
    pub(super) fn hidden(&mut self, ty: Type) -> (String, String) {
        // `%` cannot start or appear in an identifier, so the source name
        // shadows no declaration.
        let source = format!("update%{}", *self.state.counter);
        let unique = self.declare(&source, ty);
        (source, unique)
    }

    /// Evaluate the truth of `id` into a hidden 1-bit local, which `id`
    /// binds to from now on, and return its unique name.
    fn truth(&mut self, id: ExprId, stmts: &mut Vec<Stmt>) -> Converted<String> {
        let value = expression_binding::value(id, self.tree.declarations(), self.tree, &self.dims)?;
        let (source, unique) = self.hidden(Type::new(TypeKind::Logic));
        let truth = |value: Expr| Expr::Unary {
            op: UnaryOp::LogicNot,
            expr: Box::new(value),
        };
        let mut rhs = truth(truth(value));
        self.rename_expr(&mut rhs);
        stmts.push(Stmt::Assign {
            lhs: LValue::Ident(unique.clone()),
            rhs,
            nonblocking: false,
        });
        self.bind_effect(id, Expr::Ident(source));
        Ok(unique)
    }

    fn bind_effect(&mut self, id: ExprId, value: Expr) {
        Arc::make_mut(&mut self.dims.effect_values).insert(id, value);
    }

    /// Update `operand` of increment or decrement `id`. `postfix` is `None`
    /// for an operator statement, whose value is discarded; otherwise `id`
    /// binds to a hidden local that holds its value.
    pub(super) fn update(
        &mut self,
        id: ExprId,
        operand: ExprId,
        postfix: Option<bool>,
        increment: bool,
        stmts: &mut Vec<Stmt>,
    ) -> Converted<()> {
        let native = self.tree.declarations();
        self.reject_tagged_integral_target(operand)?;
        // A virtual interface is no number to step (IEEE 1800-2023 25.9).
        if let Some(ty) = virtual_interfaces::native_type(self.tree, operand, native, &self.dims) {
            return Err(virtual_interfaces::arithmetic(
                if increment { "++" } else { "--" },
                &ty,
            ));
        }
        if expression_binding::native_string(self.tree, operand, native, &self.dims) {
            return Err(unsupported("increment or decrement of a string"));
        }
        let current = expression_binding::value(operand, native, self.tree, &self.dims)?;
        // The operand is read and then written: a select of it must not
        // change in between. A member read through a virtual interface checks
        // for `null`, which changes nothing; its handle is pure.
        if inlining::effectful(&current)
            && virtual_interfaces::native_member(self.tree, operand, native, &self.dims).is_none()
        {
            return Err(unsupported(
                "increment or decrement of an operand selected with a side effect",
            ));
        }
        let step = |value: Expr| Expr::Binary {
            left: Box::new(value),
            op: if increment {
                BinaryOp::Add
            } else {
                BinaryOp::Sub
            },
            right: Box::new(Expr::Literal("1".into())),
        };
        let Some(postfix) = postfix else {
            self.bind_effect(id, step(current));
            stmts.push(self.assignment_value("=", false, (operand, id))?);
            return Ok(());
        };
        let ty = dimensions::bound_bits_type(&self.tree.state, &current, &self.dims)
            .ok_or_else(|| unsupported("increment or decrement of this operand"))?;
        // The local is of the operand's type, so a prefix result is the value
        // the update stores.
        let ty = if expression_binding::real_value(&current, &self.dims) {
            type_binding::builtin_type(if ty.width == 32 { "shortreal" } else { "real" }, None)
                .expect("a builtin type")
        } else {
            let mut r#type = Type::new(TypeKind::Logic);
            r#type.is_signed = ty.signed;
            if ty.width > 1 {
                r#type.packed_ranges = vec![PackedRange::new(
                    ConstExpr::Literal((ty.width - 1).to_string()),
                    ConstExpr::Literal("0".into()),
                )];
            }
            r#type
        };
        let (source, unique) = self.hidden(ty);
        let local = Expr::Ident(source);
        let read = |this: &mut Self, value: Expr, stmts: &mut Vec<Stmt>| {
            let mut rhs = value;
            this.rename_expr(&mut rhs);
            stmts.push(Stmt::Assign {
                lhs: LValue::Ident(unique.clone()),
                rhs,
                nonblocking: false,
            });
        };
        // The local holds the operand's value before or after the update. The
        // operand is selected once more, by the write, before the update.
        if postfix {
            read(self, current, stmts);
            self.bind_effect(id, step(local.clone()));
        } else {
            read(self, step(current), stmts);
            self.bind_effect(id, local.clone());
        }
        stmts.push(self.assignment_value("=", false, (operand, id))?);
        self.bind_effect(id, local);
        Ok(())
    }

    /// The statements of a loop whose condition `id` has increment or
    /// decrement operators, which run every time it is evaluated: they
    /// start each iteration, which ends the loop when the condition does not
    /// hold. `None` when the condition has no operator.
    pub(super) fn loop_condition(&mut self, id: ExprId) -> Converted<Option<Vec<Stmt>>> {
        let mut stmts = self.effects(&[id])?;
        if stmts.is_empty() {
            return Ok(None);
        }
        stmts.push(Stmt::If {
            condition: self.native_value(id)?,
            then_body: Vec::new(),
            else_body: vec![Stmt::Break],
        });
        Ok(Some(stmts))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operators_in_contexts_that_skip_or_repeat_operands_are_rejected() {
        for (body, diagnostic) in [
            (
                "int q[$], r[$], i; initial r = q.find with (item == i++);",
                "increment or decrement expression in this context",
            ),
            (
                "int i; initial wait (i++ > 0);",
                "increment or decrement expression",
            ),
            (
                "int i; initial begin static int t = i++; end",
                "increment or decrement in a static variable initializer",
            ),
            (
                "int a[4], i; initial case (i) i++: a[0] = 1; endcase",
                "increment or decrement expression",
            ),
            (
                "int a[4], n; function int f(); n++; return n; endfunction initial a[f()]++;",
                "increment or decrement of an operand selected with a side effect",
            ),
            (
                "int i, n; initial n = $bits(i++);",
                "increment or decrement expression",
            ),
            (
                "string s; initial s++;",
                "increment or decrement of a string",
            ),
        ] {
            let source = format!("module M; {body} endmodule");
            let error =
                crate::analyze_source(&source, Path::new("increment-context.sv")).expect_err(body);
            assert!(
                matches!(&error, AnalyzerError::Unsupported(construct) if construct.contains(diagnostic)),
                "{body}: {error}"
            );
        }
    }
}
