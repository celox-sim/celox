//! Multiclocked sequences (IEEE 1800-2023 16.13.1): singly clocked
//! subsequences, each a linear sequence of steps, joined by `##1` or `##0`
//! at a change of clock.
//!
//! A process for each clock counts the ticks of its clocking event, and the
//! monitor waits for any of them. In the Observed region it tells which
//! clocks ticked from the counts it has not handled yet, then decides each
//! subsequence whose clock ticked, in order: the attempts that
//! reached each tick of a subsequence are counted by age, all those of one
//! age being in step. An attempt that ends a subsequence goes on to the next
//! one at the nearest tick of its clock strictly after (`##1`) or possibly at
//! (`##0`) its end: deciding the subsequences in order lets a `##0` hand-off
//! start in the same time slot, and the time of a `##1` one keeps it from
//! that slot.
use super::*;
use celox_sv_syntax::{SequenceExpr, TimingControlId};

/// The decisions of a multiclocked sequence at an activation of its process.
pub(super) struct Multiclock {
    /// The statements that wait for a clock of the sequence to tick.
    pub wake: Vec<Stmt>,
    /// The statements of the Observed region that decide the subsequences.
    pub observed: Vec<Stmt>,
    /// The local that counts the attempts that pass at this activation.
    pub passes: String,
    /// The local that counts the attempts that fail at this activation.
    pub fails: String,
}

/// A singly clocked subsequence of a multiclocked one.
struct Segment {
    clock: TimingControlId,
    expr: SequenceExpr,
    /// The delay from the end of the subsequence before it, `##0` or `##1`.
    delay: u32,
}

/// Whether `expr` has a clocking event of a subsequence (16.13).
pub(super) fn clocked(expr: &SequenceExpr) -> bool {
    match expr {
        SequenceExpr::Clocked { .. } => true,
        SequenceExpr::Boolean(_) => false,
        SequenceExpr::Concat { left, right, .. } => {
            left.as_deref().is_some_and(clocked) || clocked(right)
        }
        SequenceExpr::Or(left, right)
        | SequenceExpr::And(left, right)
        | SequenceExpr::Intersect(left, right) => clocked(left) || clocked(right),
        SequenceExpr::Throughout { sequence, .. } => clocked(sequence),
    }
}

fn other_operator() -> AnalyzerError {
    unsupported(
        "multiclocked sequence with a clock change other than one joined by ##0 or ##1 (IEEE 1800-2023 16.13.1)",
    )
}

/// The singly clocked subsequences of `expr`, clocked by `clock` until a
/// clocking event of its own.
fn segments(expr: &SequenceExpr, clock: TimingControlId) -> Converted<Vec<Segment>> {
    match expr {
        SequenceExpr::Concat { left, delay, right }
            if matches!(right.as_ref(), SequenceExpr::Clocked { .. }) =>
        {
            let SequenceExpr::Clocked {
                clock: next,
                sequence,
            } = right.as_ref()
            else {
                unreachable!("a clocked subsequence");
            };
            if delay.min != delay.max || delay.min > 1 {
                return Err(unsupported(
                    "multiclocked sequence with a cycle delay other than ##0 or ##1 at a clock change (IEEE 1800-2023 16.13.1)",
                ));
            }
            // A subsequence before a clock change matches a nonempty word.
            let Some(left) = left else {
                return Err(unsupported(
                    "multiclocked sequence whose subsequence before a clock change is empty (IEEE 1800-2023 16.13.1)",
                ));
            };
            if clocked(left) {
                return Err(other_operator());
            }
            let mut found = vec![Segment {
                clock,
                expr: left.as_ref().clone(),
                delay: 0,
            }];
            let mut rest = segments(sequence, *next)?;
            rest[0].delay = delay.min;
            found.extend(rest);
            Ok(found)
        }
        SequenceExpr::Clocked {
            clock: next,
            sequence,
        } => segments(sequence, *next),
        expr if clocked(expr) => Err(other_operator()),
        expr => Ok(vec![Segment {
            clock,
            expr: expr.clone(),
            delay: 0,
        }]),
    }
}

fn int(value: u32) -> Expr {
    Expr::Literal(format!("32'sd{value}"))
}

fn add(left: Expr, right: Expr) -> Expr {
    Expr::Binary {
        left: Box::new(left),
        op: BinaryOp::Add,
        right: Box::new(right),
    }
}

fn assign(name: &str, value: Expr) -> Stmt {
    Stmt::Assign {
        lhs: LValue::Ident(name.to_owned()),
        rhs: value,
        nonblocking: false,
    }
}

fn not_equal(left: Expr, right: Expr) -> Expr {
    Expr::Binary {
        left: Box::new(left),
        op: BinaryOp::Ne,
        right: Box::new(right),
    }
}

impl BodyBuilder<'_, '_, '_> {
    /// The monitor of `sequence`, a multiclocked sequence led by `leading`.
    pub(super) fn multiclock_monitor(
        &mut self,
        leading: TimingControlId,
        sequence: &celox_sv_syntax::Sequence,
    ) -> Converted<Multiclock> {
        if sequence.antecedent.is_some() {
            return Err(unsupported("implication of multiclocked sequences"));
        }
        if !sequence.locals.is_empty() {
            return Err(unsupported("local variable of a multiclocked sequence"));
        }
        let segments = segments(&sequence.consequent, leading)?;
        // Each subsequence a linear sequence of steps at their offsets.
        let mut linear = Vec::new();
        for segment in &segments {
            let mut paths = assertions::sequence_paths(&segment.expr)?;
            if paths.len() > 1 {
                return Err(unsupported(
                    "multiclocked sequence whose subsequence has more than one way to match",
                ));
            }
            let path = paths.pop().expect("a sequence has a way to match");
            let mut at = 0u32;
            let mut steps = Vec::new();
            for step in path {
                if !step.items.is_empty() {
                    return Err(unsupported("match item in a multiclocked sequence"));
                }
                at = at
                    .checked_add(step.delay)
                    .filter(|at| *at <= 1024)
                    .ok_or_else(|| {
                        unsupported("multiclocked subsequence longer than 1024 ticks")
                    })?;
                steps.push((at, step.expression));
            }
            linear.push(steps);
        }
        // A process for each distinct clock counts the occurrences of its
        // clocking event, waiting for it as an event control does: the
        // edges of 4-state values and an `iff` judged when the event occurs
        // (IEEE 1800-2023 9.4.2). In the Observed region the monitor tells
        // which clocks ticked from the counts it has not handled yet.
        let mut observed = Vec::new();
        let mut clocks: Vec<(TimingControlId, String)> = Vec::new();
        let mut wakes = Vec::new();
        let mut pending: Option<Expr> = None;
        let mut bit = Type::new(TypeKind::Bit);
        bit.is_signed = false;
        for segment in &segments {
            if clocks.iter().any(|(clock, _)| *clock == segment.clock) {
                continue;
            }
            let wait = self.native_timing_control(segment.clock)?;
            let Stmt::WaitEvent(items) = &wait else {
                return Err(unsupported(
                    "multiclocked sequence with a clocking event other than value changes",
                ));
            };
            wakes.extend(items.iter().cloned());
            let tick = self.declare("tick@multiclock", int_type());
            let handled = self.declare("handled@multiclock", int_type());
            let ticked = self.declare("ticked@multiclock", bit.clone());
            self.disable_watchers.push(vec![Stmt::Loop {
                kind: crate::procedural::LoopKind::Forever,
                init: Vec::new(),
                condition: None,
                step: Vec::new(),
                body: vec![wait, assign(&tick, add(Expr::Ident(tick.clone()), int(1)))],
            }]);
            let unhandled = not_equal(Expr::Ident(tick.clone()), Expr::Ident(handled.clone()));
            observed.push(assign(&ticked, unhandled.clone()));
            observed.push(assign(&handled, Expr::Ident(tick.clone())));
            pending = Some(match pending {
                None => unhandled,
                Some(pending) => Expr::Binary {
                    left: Box::new(pending),
                    op: BinaryOp::LogicOr,
                    right: Box::new(unhandled),
                },
            });
            clocks.push((segment.clock, ticked));
        }
        // The monitor waits for a clock to tick, unless one ticked since it
        // last decided.
        let wake = vec![Stmt::If {
            condition: Expr::Unary {
                op: UnaryOp::LogicNot,
                expr: Box::new(pending.expect("a sequence has a clock")),
            },
            then_body: vec![Stmt::WaitEvent(wakes)],
            else_body: Vec::new(),
        }];
        let passes = self.declare("passes@multiclock", int_type());
        let fails = self.declare("fails@multiclock", int_type());
        observed.push(assign(&passes, int(0)));
        observed.push(assign(&fails, int(0)));
        // The attempts handed to each subsequence after the first: those it
        // may start, and those of `##1` handed at the time `time` holds.
        let mut ready = vec![String::new()];
        let mut held = vec![String::new()];
        let mut held_at = vec![String::new()];
        for _ in 1..segments.len() {
            ready.push(self.declare("ready@multiclock", int_type()));
            held.push(self.declare("held@multiclock", int_type()));
            held_at.push(self.declare("held_at@multiclock", classes::unsigned_type(64)));
        }
        let now = Expr::Call {
            callee: Callee::System(SystemFunction::Time),
            // In ticks of the global precision, which only compare here.
            args: vec![Some(Expr::Literal("64'd1".into()))],
            arg_order: Vec::new(),
        };
        // `##1` hand-offs of an earlier time may start now.
        let release = |index: usize| Stmt::If {
            condition: not_equal(Expr::Ident(held_at[index].clone()), now.clone()),
            then_body: vec![
                assign(
                    &ready[index],
                    add(
                        Expr::Ident(ready[index].clone()),
                        Expr::Ident(held[index].clone()),
                    ),
                ),
                assign(&held[index], int(0)),
            ],
            else_body: Vec::new(),
        };
        for (index, (segment, steps)) in segments.iter().zip(&linear).enumerate() {
            let length = steps.last().map_or(0, |(at, _)| *at);
            let counts = (0..=length)
                .map(|_| self.declare("count@multiclock", int_type()))
                .collect::<Vec<_>>();
            let mut tick = Vec::new();
            // The attempts that start at this tick: one of each tick of the
            // first clock, those handed over to a later subsequence.
            let start = if index == 0 {
                int(1)
            } else {
                tick.push(release(index));
                Expr::Ident(ready[index].clone())
            };
            for age in (1..=length as usize).rev() {
                tick.push(assign(&counts[age], Expr::Ident(counts[age - 1].clone())));
            }
            tick.push(assign(&counts[0], start));
            if index > 0 {
                tick.push(assign(&ready[index], int(0)));
            }
            // Each age with steps, the oldest attempts first: a miss fails
            // them, a match of the last step ends the subsequence.
            for age in (0..=length).rev() {
                let conditions = steps
                    .iter()
                    .filter(|(at, _)| *at == age)
                    .map(|(_, expression)| {
                        let condition = self.native_value(*expression)?;
                        assertions::concurrent_operand(&condition)?;
                        Ok(procedural_truth_condition(assertions::sampled(condition)))
                    })
                    .collect::<Converted<Vec<_>>>()?;
                let Some(matched) = conditions.into_iter().reduce(|left, right| Expr::Binary {
                    left: Box::new(left),
                    op: BinaryOp::LogicAnd,
                    right: Box::new(right),
                }) else {
                    continue;
                };
                let count = Expr::Ident(counts[age as usize].clone());
                let ended = if age < length {
                    Vec::new()
                } else if index + 1 == segments.len() {
                    vec![assign(
                        &passes,
                        add(Expr::Ident(passes.clone()), count.clone()),
                    )]
                } else if segments[index + 1].delay == 0 {
                    vec![assign(
                        &ready[index + 1],
                        add(Expr::Ident(ready[index + 1].clone()), count.clone()),
                    )]
                } else {
                    vec![
                        release(index + 1),
                        assign(&held_at[index + 1], now.clone()),
                        assign(
                            &held[index + 1],
                            add(Expr::Ident(held[index + 1].clone()), count.clone()),
                        ),
                    ]
                };
                let mut on_match = ended;
                if age == length {
                    on_match.push(assign(&counts[age as usize], int(0)));
                }
                tick.push(Stmt::If {
                    condition: not_equal(count.clone(), int(0)),
                    then_body: vec![Stmt::If {
                        condition: matched,
                        then_body: on_match,
                        else_body: vec![
                            assign(&fails, add(Expr::Ident(fails.clone()), count)),
                            assign(&counts[age as usize], int(0)),
                        ],
                    }],
                    else_body: Vec::new(),
                });
            }
            let ticked = clocks
                .iter()
                .find(|(clock, _)| *clock == segment.clock)
                .map(|(_, ticked)| ticked.clone())
                .expect("each clock is known");
            observed.push(Stmt::If {
                condition: Expr::Ident(ticked),
                then_body: tick,
                else_body: Vec::new(),
            });
        }
        Ok(Multiclock {
            wake,
            observed,
            passes,
            fails,
        })
    }
}
