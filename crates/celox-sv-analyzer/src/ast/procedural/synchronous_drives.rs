//! Synchronous drives of clocking block outputs (IEEE 1800-2023 14.16).
//!
//! `cb.x <= e` evaluates `e` now and updates the clocking signal `x` in the
//! Re-NBA region of the time slot of a clocking event of `cb`, plus the
//! output skew of `x`. A drive executed in a time slot in which the
//! clocking event occurred matures at that event; any other drive matures
//! at the next one, as if it had executed then: a detached process waits for
//! the clocking block to record that event and drives the saved value.
use super::*;
use crate::procedural::{EventEdge, EventItemBase};
use celox_sv_syntax::{ExprId, ExprKind, TimingControlId, TokenRange};

impl BodyBuilder<'_, '_, '_> {
    /// The statements of the synchronous drive `left <= right`, or `None`
    /// if `left` is no clockvar.
    pub(super) fn synchronous_drive(
        &mut self,
        left: ExprId,
        right: ExprId,
        timing: Option<TokenRange>,
        control: Option<TimingControlId>,
    ) -> Converted<Option<Vec<Stmt>>> {
        let native = self.syntax().declarations();
        let mut root = expression_binding::ungroup(left, native);
        while let ExprKind::Select { expression, .. } = &native.expression(root).kind {
            root = expression_binding::ungroup(*expression, native);
        }
        let Some((block, var, members)) =
            clocking::clockvar_path(root, native, &self.tree, self.session, &self.dims)
        else {
            return Ok(None);
        };
        let invalid = |text: String| AnalyzerError::InvalidClocking(text);
        if !members.is_empty() {
            return Err(invalid(format!(
                "a synchronous drive drives a clockvar or a select of one, not a member of `{block}.{var}` (IEEE 1800-2023 14.16)"
            )));
        }
        if timing.is_some() {
            return Err(match control {
                None => unsupported("cycle delay in a synchronous drive (IEEE 1800-2023 14.16)"),
                Some(_) => invalid(format!(
                    "intra-assignment delay in the synchronous drive of `{block}.{var}` (IEEE 1800-2023 14.16)"
                )),
            });
        }
        let declaration = clocking::declared(&self.tree, &block)?
            .ok_or_else(|| unsupported(format!("clocking block `{block}` of another scope")))?;
        let output = declaration
            .vars
            .iter()
            .find(|clockvar| clockvar.name == var)
            .and_then(|clockvar| clockvar.output)
            .ok_or_else(|| {
                invalid(format!(
                    "`{block}.{var}` is no output clockvar of clocking block `{block}`, which a synchronous drive writes (IEEE 1800-2023 14.3)"
                ))
            })?;
        // The drive's target is the signal the clocking block names, which
        // the names of the procedure's own scopes would take for theirs.
        if self.names.lookup(&var).is_some() {
            return Err(unsupported(format!(
                "synchronous drive of `{block}.{var}` in a scope whose variable `{var}` hides the clocking signal"
            )));
        }
        // The skew is a constant of the clocking block's scope, which the
        // procedure's own declarations would rebind for it here.
        if let clocking::OutputSkew::Delay(value) = output {
            let tokens = native.expression(value).tokens;
            if let Some(name) = native.tokens[tokens.start..tokens.end]
                .iter()
                .filter(|token| token.kind == celox_sv_syntax::TokenKind::Identifier)
                .map(|token| crate::normalize_identifier(&token.text))
                .find(|name| self.names.binds(name))
            {
                return Err(unsupported(format!(
                    "synchronous drive of `{block}.{var}` in a scope whose declaration `{name}` hides a name of its output skew"
                )));
            }
        }
        let ticks = clocking::skew_ticks(output, &self.tree, self.session, &self.dims)?;
        let mut output = self.effects(&[left, right])?;
        self.prepare_pattern_values(left)?;
        self.prepare_pattern_values(right)?;
        output.append(&mut self.pattern_prelude);
        // The target names the clocking signal.
        self.session.clockvar_drive = Some(root);
        let assignment = self.assignment_value("=", true, (left, right));
        self.session.clockvar_drive = None;
        let Ok([Stmt::Assign { lhs, rhs, .. }]) = <[Stmt; 1]>::try_from(assignment?) else {
            return Err(unsupported(
                "synchronous drive of an aggregate clocking signal",
            ));
        };
        // A drive that matures later writes the place it names now; only
        // constant selects are known to name the same place then.
        if let LValue::Select { msb, lsb, .. } = &lhs
            && [msb, lsb].iter().any(|bound| {
                eval_ast_const_expr(&self.tree.functions, bound, &self.dims.const_env).is_none()
            })
        {
            return Err(unsupported(
                "synchronous drive of a select with a run-time index (IEEE 1800-2023 14.16)",
            ));
        }
        let ty = lvalue_expr_type(&self.tree.functions, &lhs, &self.dims)
            .ok_or_else(|| unsupported("synchronous drive width"))?;
        let two_state = self.dims.get(&var).is_some_and(|shape| shape.is_2state);
        let mut storage = Type::new(if two_state {
            TypeKind::Bit
        } else {
            TypeKind::Logic
        });
        storage.is_signed = ty.signed;
        storage.packed_ranges.push(PackedRange::new(
            ConstExpr::Literal((ty.width - 1).to_string()),
            ConstExpr::Literal("0".into()),
        ));
        // The value is evaluated now, whenever the drive matures (14.16).
        // Each execution declares its own value and turn: the process of a
        // drive that matures later keeps those of its execution, as a fork
        // keeps the locals of the scope it forks from (see `Stmt::Local`).
        let (_, saved) = self.hidden(storage);
        output.push(Stmt::Local {
            name: saved.clone(),
            init: None,
        });
        output.push(Stmt::Assign {
            lhs: LValue::Ident(saved.clone()),
            rhs,
            nonblocking: false,
        });
        let drive = Stmt::DelayedNonblocking {
            control: Box::new(Stmt::Delay {
                amount: Expr::Literal(format!("64'd{ticks}")),
                ticks_per_unit: 1,
                ticks_per_precision: 1,
                precision_steps_per_tick: 1,
            }),
            assignment: Box::new(Stmt::Assign {
                lhs,
                rhs: Expr::Ident(saved),
                nonblocking: true,
            }),
            re_nba: true,
        };
        let wait = |name: String| {
            Stmt::WaitEvent(vec![EventItemBase {
                edge: EventEdge::Any,
                expr: Expr::Ident(name),
                iff: None,
            }])
        };
        let increment = |name: String| Stmt::Assign {
            lhs: LValue::Ident(name.clone()),
            rhs: Expr::Binary {
                left: Box::new(Expr::Ident(name)),
                op: BinaryOp::Add,
                right: Box::new(Expr::Literal("64'd1".into())),
            },
            nonblocking: false,
        };
        // The drives executed before an event all mature at it, in the
        // order they were executed (14.16.2): each takes a turn now, and
        // drives when the drives of the turns before it have.
        let mut count = Type::new(TypeKind::Bit);
        count.packed_ranges.push(PackedRange::new(
            ConstExpr::Literal("63".into()),
            ConstExpr::Literal("0".into()),
        ));
        let (_, turn) = self.hidden(count);
        let (issued, matured) = (
            clocking::drives_issued(&block),
            clocking::drives_matured(&block),
        );
        output.push(Stmt::If {
            condition: clocking::occurred(&block),
            then_body: vec![drive.clone()],
            else_body: vec![
                Stmt::Local {
                    name: turn.clone(),
                    init: Some(Expr::Ident(issued.clone())),
                },
                increment(issued),
                // A process of the analysis, no child of the process:
                // `disable fork` and `wait fork` do not reach the drive.
                Stmt::Fork {
                    join: celox_design::ForkJoin::None,
                    unseeded: true,
                    branches: vec![vec![
                        wait(clocking::marker(&block)),
                        Stmt::Loop {
                            kind: LoopKind::While,
                            init: Vec::new(),
                            condition: Some(Expr::Binary {
                                left: Box::new(Expr::Ident(matured.clone())),
                                op: BinaryOp::Ne,
                                right: Box::new(Expr::Ident(turn)),
                            }),
                            step: Vec::new(),
                            body: vec![wait(matured.clone())],
                        },
                        drive,
                        increment(matured),
                    ]],
                },
            ],
        });
        Ok(Some(output))
    }
}
