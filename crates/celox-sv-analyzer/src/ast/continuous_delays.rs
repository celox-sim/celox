//! Inertial continuous drivers, using cancellable process waits (10.3.3).
use super::*;
use crate::procedural::{EventEdge, EventItemBase, LoopKind};

pub(super) fn ticks(
    delay: &celox_sv_syntax::AssignmentDelay,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<u64> {
    let [id] = delay.values.as_slice() else {
        return Err(unsupported(
            "delayed continuous assignment with rise/fall/turn-off delays",
        ));
    };
    let native = tree.declarations();
    let mut id = expression_binding::ungroup(*id, native);
    if let celox_sv_syntax::ExprKind::MinTypMax { typical, .. } = native.expression(id).kind {
        id = typical;
    }
    if let celox_sv_syntax::ExprKind::Literal(text) = &native.expression(id).kind
        && let Some(ticks) = timescales::delay_literal(id, text, tree)?
    {
        return Ok(ticks);
    }
    let ticks = constant_binding::bind_in(id, native, tree, session, dimensions)?
        .and_then(|expr| eval_ast_const_expr(&tree.functions, &expr, &dimensions.const_env))
        .and_then(|value| u64::try_from(value).ok())
        .and_then(|value| value.checked_mul(timescales::time_divisor(id, tree).ok()?));
    ticks.ok_or_else(|| {
        unsupported("delayed continuous assignment with nonconstant, negative or overflowing delay")
    })
}

pub(super) fn lower(
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
    signals: &mut Vec<Signal>,
    processes: &mut [CombProcess],
    initial: &mut Vec<InitialProcess>,
) -> Converted<()> {
    for (index, process) in processes.iter_mut().enumerate() {
        let Some(ticks) = process.delay.take() else {
            continue;
        };
        // An explicit zero delay has ordinary continuous-assignment semantics.
        if ticks == 0 {
            continue;
        }
        let [
            Stmt::Assign {
                lhs,
                rhs,
                nonblocking: false,
            },
        ] = process.body.as_slice()
        else {
            return Err(unsupported(
                "delayed continuous assignment to an aggregate or concatenation",
            ));
        };
        if inlining::effectful(rhs) {
            return Err(unsupported("delayed continuous assignment with calls"));
        }
        let name = match lhs {
            LValue::Ident(name) | LValue::Select { name, .. } => name,
        };
        let shape = dimensions
            .get(name)
            .ok_or_else(|| unsupported("delayed continuous assignment target"))?;
        if shape.string
            || shape.real
            || shape.class.is_some()
            || shape.chandle
            || shape.container.is_some()
            || !shape.unpacked.is_empty()
            || shape.unpacked_struct.is_some()
        {
            return Err(unsupported(
                "nonintegral or unpacked delayed continuous assignment",
            ));
        }
        if let LValue::Select { msb, lsb, .. } = lhs
            && [msb, lsb].iter().any(|expr| {
                eval_ast_const_expr(&tree.functions, expr, &dimensions.const_env).is_none()
            })
        {
            return Err(unsupported("dynamic delayed continuous assignment target"));
        }
        let ty = lvalue_expr_type(&tree.functions, lhs, dimensions)
            .ok_or_else(|| unsupported("delayed continuous assignment width"))?;
        let mut storage = Type::new(if shape.is_2state {
            TypeKind::Bit
        } else {
            TypeKind::Logic
        });
        storage.is_signed = ty.signed;
        storage.packed_ranges.push(PackedRange::new(
            ConstExpr::Literal((ty.width - 1).to_string()),
            ConstExpr::Literal("0".into()),
        ));
        let mut serial = index;
        let (driven, sample) = loop {
            let driven = format!("%continuous_delay{serial}_driven");
            let sample = format!("%continuous_delay{serial}_sample");
            if [driven.as_str(), sample.as_str()].iter().all(|name| {
                dimensions.get(name).is_none()
                    && !signals.iter().any(|signal| signal.name() == *name)
            }) {
                break (driven, sample);
            }
            serial += 1;
        };
        // Module storage is shared by the parent and its fork children and
        // survives checkpoints; automatic locals would give each child a copy.
        signals.push(Signal::new(driven.clone(), storage.clone()));
        signals.push(Signal::new(sample.clone(), storage));
        // Freeze the value after assignment conversion: widening uses the
        // RHS signedness before adopting the destination's signedness.
        let value = coerce_procedural_assignment_rhs(tree, rhs.clone(), lhs, dimensions);
        let different = |left, right| Expr::Binary {
            left: Box::new(left),
            op: BinaryOp::NeCase,
            right: Box::new(right),
        };
        // Wait only for a change of the evaluated, sized RHS. Changes to inputs
        // that leave it unchanged must not move an already scheduled update.
        let changed = different(value.clone(), Expr::Ident(sample.clone()));
        let body = vec![
            Stmt::Assign {
                lhs: LValue::Ident(sample.clone()),
                rhs: value.clone(),
                nonblocking: false,
            },
            Stmt::If {
                condition: different(Expr::Ident(sample.clone()), Expr::Ident(driven.clone())),
                then_body: vec![
                    Stmt::Fork {
                        join: celox_design::ForkJoin::Any,
                        branches: vec![
                            vec![
                                Stmt::Delay {
                                    amount: Expr::Literal(ticks.to_string()),
                                    ticks_per_unit: 1,
                                    ticks_per_precision: 1,
                                },
                                Stmt::Assign {
                                    lhs: LValue::Ident(driven.clone()),
                                    rhs: Expr::Ident(sample.clone()),
                                    nonblocking: false,
                                },
                            ],
                            vec![Stmt::Wait(changed)],
                        ],
                    },
                    Stmt::DisableFork,
                ],
                else_body: vec![Stmt::WaitEvent(vec![EventItemBase {
                    edge: EventEdge::Any,
                    expr: value,
                    iff: None,
                }])],
            },
        ];
        initial.push(InitialProcess {
            creation_order: process.creation_order,
            site: process.process_site.clone().map(|mut site| {
                site.construct = "delayed continuous assignment";
                site
            }),
            condition: process.condition.clone(),
            body: vec![Stmt::Loop {
                kind: LoopKind::Forever,
                init: Vec::new(),
                condition: None,
                step: Vec::new(),
                body,
            }],
            kind: celox_design::ProcessKind::Regular,
            validation_body: Vec::new(),
            synthetic: true,
            repeats: true,
        });
        let assignment = Assignment::new(lhs.clone(), Expr::Ident(driven));
        process.body = vec![Stmt::Assign {
            lhs: assignment.lhs.clone(),
            rhs: assignment.rhs.clone(),
            nonblocking: false,
        }];
        process.assignments = vec![assignment];
    }
    Ok(())
}
