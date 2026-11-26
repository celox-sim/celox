//! Until properties of Boolean operands (IEEE 1800-2023 16.12.12), as the
//! property of a module assertion or the consequent of an implication with
//! a Boolean antecedent.
//!
//! An attempt holds while its left operand holds at each tick, until a tick
//! where its right operand holds: the attempt then passes, the left operand
//! holding at that tick too in an overlapping form. It fails at the first
//! tick before that where the left operand does not hold. Every attempt that
//! has not passed or failed sees the same ticks from then on, so one count
//! of the pending attempts decides them all: at each tick they pass, fail or
//! stay pending together, and the pass or fail action runs once for each.
//! A strong form also requires the right operand to hold at some tick: the
//! attempts still pending when the simulation ends fail then (F.5.3.1,
//! neutral satisfaction on the finite trace), in a final procedure; a weak
//! one still pending neither passes nor fails.
use super::*;

/// An until property's decisions at each tick of its clock (see
/// [`multiclock::Multiclock`]), and the attempts of a strong form still
/// pending, which fail when the simulation ends.
pub(super) struct Until {
    pub decisions: multiclock::Multiclock,
    pub unresolved: Option<Unresolved>,
}

/// The attempts of a strong until property still pending: their count, and
/// with `disable iff`, whether they are disabled, the condition true since
/// the last tick or now (16.12): those neither pass nor fail.
pub(super) struct Unresolved {
    pub count: Expr,
    pub disabled: Option<Expr>,
}

impl BodyBuilder<'_, '_, '_> {
    /// The monitor of `condition until release` of form `until`, with
    /// Boolean `antecedent` and `disable iff (disable)`.
    pub(super) fn until_monitor(
        &mut self,
        antecedent: Option<celox_sv_syntax::ExprId>,
        condition: celox_sv_syntax::ExprId,
        until: celox_sv_syntax::UntilSpec,
        disable: Option<celox_sv_syntax::ExprId>,
    ) -> Converted<Until> {
        if self.controls_assertions() {
            return Err(unsupported(
                "until property in a design that calls an assertion control task",
            ));
        }
        // Each operand over sampled values; X and Z are false (16.12.7).
        let truth = |this: &mut Self, id| -> Converted<Expr> {
            let value = this.native_value(id)?;
            this.concurrent_operand(&value)?;
            Ok(procedural_truth_condition(assertions::sampled(value)))
        };
        let antecedent = antecedent.map(|id| truth(self, id)).transpose()?;
        let hold = truth(self, condition)?;
        let release = truth(self, until.release)?;
        // The disable condition over current values, true at this tick or
        // at any time since the previous one (16.12).
        let disable = disable
            .map(|id| self.native_value(id).map(procedural_truth_condition))
            .transpose()?;
        let seen = match &disable {
            Some(disable) => {
                self.concurrent_operand(disable)?;
                assertions::concurrent_disable(disable)?;
                Some(self.disable_history(disable, 1).seen)
            }
            None => None,
        };
        let pending = self.declare("pending@until", int_type());
        let next = until.next.then(|| self.declare("next@until", int_type()));
        let active = self.declare("active@until", int_type());
        let passes = self.declare("passes@until", int_type());
        let fails = self.declare("fails@until", int_type());

        let ident = |name: &String| Expr::Ident(name.clone());
        let assign = |name: &String, rhs: Expr| Stmt::Assign {
            lhs: LValue::Ident(name.clone()),
            rhs,
            nonblocking: false,
        };
        let literal = |value: &str| Expr::Literal(value.into());
        let add = |name: &String, value: Expr| {
            assign(
                name,
                Expr::Binary {
                    left: Box::new(ident(name)),
                    op: BinaryOp::Add,
                    right: Box::new(value),
                },
            )
        };
        let when = |condition: Expr, then_body: Vec<Stmt>, else_body: Vec<Stmt>| Stmt::If {
            condition,
            then_body,
            else_body,
        };
        let not = |expr: Expr| Expr::Unary {
            op: UnaryOp::LogicNot,
            expr: Box::new(expr),
        };

        let mut observed = vec![assign(&passes, literal("0")), assign(&fails, literal("0"))];
        // A pending attempt is disabled when the condition was true since
        // the previous tick or is now, one that starts when it is now.
        if let (Some(disable), Some(seen)) = (&disable, &seen) {
            let mut cancel = vec![assign(&pending, literal("0"))];
            cancel.extend(next.iter().map(|next| assign(next, literal("0"))));
            observed.push(when(
                Expr::Binary {
                    left: Box::new(ident(seen)),
                    op: BinaryOp::LogicOr,
                    right: Box::new(disable.clone()),
                },
                cancel,
                Vec::new(),
            ));
            observed.push(assign(seen, literal("1'b0")));
        }
        // The attempts the operands decide at this tick: those pending, and
        // those of `|=>` whose antecedent matched at the previous tick.
        observed.push(assign(&active, ident(&pending)));
        if let Some(next) = &next {
            observed.push(add(&active, ident(next)));
            observed.push(assign(next, literal("0")));
        }
        observed.push(assign(&pending, literal("0")));
        // The attempt that starts at this tick: a missed antecedent
        // succeeds vacuously (16.12.7).
        let start = match (&antecedent, &next) {
            (None, _) => vec![add(&active, literal("1"))],
            (Some(antecedent), None) => vec![when(
                antecedent.clone(),
                vec![add(&active, literal("1"))],
                vec![add(&passes, literal("1"))],
            )],
            (Some(antecedent), Some(next)) => vec![when(
                antecedent.clone(),
                vec![assign(next, literal("1"))],
                vec![add(&passes, literal("1"))],
            )],
        };
        match &disable {
            Some(disable) => observed.push(when(disable.clone(), Vec::new(), start)),
            None => observed.extend(start),
        }
        let released = if until.overlapping {
            Expr::Binary {
                left: Box::new(hold.clone()),
                op: BinaryOp::LogicAnd,
                right: Box::new(release),
            }
        } else {
            release
        };
        observed.push(when(
            released,
            vec![add(&passes, ident(&active))],
            vec![when(
                not(hold),
                vec![add(&fails, ident(&active))],
                vec![assign(&pending, ident(&active))],
            )],
        ));
        let unresolved = until.strong.then(|| Unresolved {
            count: match &next {
                Some(next) => Expr::Binary {
                    left: Box::new(ident(&pending)),
                    op: BinaryOp::Add,
                    right: Box::new(ident(next)),
                },
                None => ident(&pending),
            },
            disabled: disable.zip(seen).map(|(disable, seen)| Expr::Binary {
                left: Box::new(Expr::Ident(seen)),
                op: BinaryOp::LogicOr,
                right: Box::new(disable),
            }),
        });
        Ok(Until {
            decisions: multiclock::Multiclock {
                wake: Vec::new(),
                observed,
                passes,
                fails,
            },
            unresolved,
        })
    }
}
