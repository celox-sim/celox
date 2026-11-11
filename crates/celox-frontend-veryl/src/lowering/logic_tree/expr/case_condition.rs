//! Case targets and the conditions of case arms.

use super::*;

pub(in crate::lowering::logic_tree) struct EvaluatedCaseTarget {
    pub(in crate::lowering::logic_tree) node: NodeId,
    pub(in crate::lowering::logic_tree) sources: HashSet<VarAtomBase<VarId>>,
    // Preserve the symbolic values at case entry. A context-determined target
    // must be computed in each label's comparison width, before truncation.
    pub(super) comparison_store: Option<SymbolicStore<VarId>>,
}

impl EvaluatedCaseTarget {
    pub(in crate::lowering::logic_tree) fn new(
        expression: &Expression,
        node: NodeId,
        sources: HashSet<VarAtomBase<VarId>>,
        store: &SymbolicStore<VarId>,
    ) -> Self {
        let comparison_store = if !matches!(expression, Expression::Term(_))
            && case_target_can_be_recomputed(expression)
        {
            Some(store.fork())
        } else {
            None
        };
        Self {
            node,
            sources,
            comparison_store,
        }
    }
}

// A call can update symbolic variables even without a display/assert runtime
// effect. Keep every call's single evaluation; recompute only operator trees
// whose leaves and index expressions are reads or literals.
pub(super) fn case_target_can_be_recomputed(expression: &Expression) -> bool {
    match expression {
        Expression::Term(factor) => match factor.as_ref() {
            Factor::Variable(_, index, select, _) => index
                .0
                .iter()
                .chain(select.0.iter())
                .chain(select.1.iter().map(|(_, end)| end))
                .all(case_target_can_be_recomputed),
            Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => true,
            _ => false,
        },
        Expression::Binary(lhs, _, rhs, _) => {
            case_target_can_be_recomputed(lhs) && case_target_can_be_recomputed(rhs)
        }
        Expression::Unary(_, inner, _) => case_target_can_be_recomputed(inner),
        Expression::Ternary(condition, yes, no, _) => {
            case_target_can_be_recomputed(condition)
                && case_target_can_be_recomputed(yes)
                && case_target_can_be_recomputed(no)
        }
        _ => false,
    }
}

pub(in crate::lowering::logic_tree) fn eval_case_target_effectful(
    module: &Module,
    store: &mut SymbolicStore<VarId>,
    target: &Expression,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<(EvaluatedCaseTarget, BoundaryMap<VarId>), ParserError> {
    let ((node, sources), boundaries) =
        eval_expression_effectful(module, store, target, arena, None)?;
    Ok((
        EvaluatedCaseTarget::new(target, node, sources, store),
        boundaries,
    ))
}

pub(in crate::lowering::logic_tree) fn short_circuit_rhs_guard(
    arena: &mut SLTNodeArena<VarId>,
    lhs: NodeId,
    is_and: bool,
) -> Result<NodeId, ParserError> {
    let not_lhs = arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, lhs))?;
    let shortcut_truth = if is_and {
        not_lhs
    } else {
        arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, not_lhs))?
    };
    let shortcut = arena.alloc(SLTNode::Unary(UnaryOp::ToTwoState, shortcut_truth))?;
    Ok(arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, shortcut))?)
}

pub(in crate::lowering::logic_tree) fn eval_case_comparison(
    module: &Module,
    store: &mut ExpressionStore<'_>,
    target_expr: &Expression,
    target: &EvaluatedCaseTarget,
    other: &Expression,
    target_is_lhs: bool,
    op: Op,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    let target_width = get_expr_width(target_expr)
        .or_else(|| target_expr.comptime().r#type.total_width())
        .unwrap_or(1);
    let other_width = get_expr_width(other)
        .or_else(|| other.comptime().r#type.total_width())
        .unwrap_or(1);
    let target_signed = expression_signed(target_expr);
    let other_signed = expression_signed(other);
    let (lhs_width, rhs_width, lhs_signed, rhs_signed) = if target_is_lhs {
        (target_width, other_width, target_signed, other_signed)
    } else {
        (other_width, target_width, other_signed, target_signed)
    };
    let semantics = binary_semantics(
        op,
        lhs_width,
        rhs_width,
        lhs_signed,
        rhs_signed,
        Some(ValueContext {
            width: 1,
            signed: false,
        }),
    );

    let target_context = if target_is_lhs {
        semantics.lhs_context
    } else {
        semantics.rhs_context
    };
    let context_changes = target_context.is_some_and(|context| {
        context.width != get_width(target.node, arena) || context.signed != target_signed
    });
    let (target_node, target_boundaries) =
        if context_changes && let Some(snapshot) = &target.comparison_store {
            let ((node, _), boundaries) = eval_expression_in_context(
                module,
                &mut ExpressionStore::ReadOnly(snapshot),
                &crate::case::unfold_context_determined_constants(target_expr),
                arena,
                target_context,
            )?;
            (node, boundaries)
        } else {
            (
                coerce_node_width(
                    arena,
                    target.node,
                    target_context.map(|context| context.width),
                    target_context
                        .map(|context| context.signed)
                        .unwrap_or(target_signed),
                )?,
                BoundaryMap::default(),
            )
        };
    let other_context = if target_is_lhs {
        semantics.rhs_context
    } else {
        semantics.lhs_context
    };
    let ((other_node, other_sources), boundaries) =
        eval_expression_in_context(module, store, other, arena, other_context)?;
    let (lhs, rhs) = if target_is_lhs {
        (target_node, other_node)
    } else {
        (other_node, target_node)
    };
    let node = arena.alloc(SLTNode::Binary(
        lhs,
        resolve_binary_op(op, semantics.lhs_signed, semantics.rhs_signed),
        rhs,
    ))?;
    let node = coerce_node_width(
        arena,
        node,
        Some(semantics.result_width),
        semantics.result_signed,
    )?;
    let mut sources = target.sources.clone();
    sources.extend(other_sources);
    Ok((
        (node, sources),
        merge_boundaries(target_boundaries, boundaries),
    ))
}

pub(super) fn merge_short_circuit_case_condition(
    module: &Module,
    store: &mut SymbolicStore<VarId>,
    lhs: ((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>),
    eval_rhs: impl FnOnce(
        &mut SymbolicStore<VarId>,
        &mut SLTNodeArena<VarId>,
    ) -> Result<
        ((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>),
        ParserError,
    >,
    is_and: bool,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    let ((lhs_node, mut sources), lhs_boundaries) = lhs;
    let base_store = store.fork();
    let mut rhs_store = base_store.fork();
    let ((rhs_node, rhs_sources), rhs_boundaries) = eval_rhs(&mut rhs_store, arena)?;
    let execute_rhs = short_circuit_rhs_guard(arena, lhs_node, is_and)?;
    *store = super::super::merge_symbolic_versions(
        module,
        &rhs_store,
        &base_store,
        execute_rhs,
        &sources,
        arena,
    )?;
    sources.extend(rhs_sources);
    let op = if is_and {
        BinaryOp::LogicAnd
    } else {
        BinaryOp::LogicOr
    };
    Ok((
        (
            arena.alloc(SLTNode::Binary(lhs_node, op, rhs_node))?,
            sources,
        ),
        merge_boundaries(lhs_boundaries, rhs_boundaries),
    ))
}

pub(in crate::lowering::logic_tree) fn eval_case_arm_condition_effectful(
    module: &Module,
    store: &mut SymbolicStore<VarId>,
    target_expr: &Expression,
    target: &EvaluatedCaseTarget,
    patterns: &[CasePattern],
    arena: &mut SLTNodeArena<VarId>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    fn eval_pattern(
        module: &Module,
        store: &mut SymbolicStore<VarId>,
        target_expr: &Expression,
        target: &EvaluatedCaseTarget,
        pattern: &CasePattern,
        arena: &mut SLTNodeArena<VarId>,
    ) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
        match pattern {
            CasePattern::Eq(value) => {
                let mut store = ExpressionStore::Effectful(store);
                eval_case_comparison(
                    module,
                    &mut store,
                    target_expr,
                    target,
                    value,
                    true,
                    Op::EqWildcard,
                    arena,
                )
            }
            CasePattern::Range { lo, hi, inclusive } => {
                let lhs = {
                    let mut expression_store = ExpressionStore::Effectful(store);
                    eval_case_comparison(
                        module,
                        &mut expression_store,
                        target_expr,
                        target,
                        lo,
                        false,
                        Op::LessEq,
                        arena,
                    )?
                };
                merge_short_circuit_case_condition(
                    module,
                    store,
                    lhs,
                    |rhs_store, arena| {
                        let mut expression_store = ExpressionStore::Effectful(rhs_store);
                        eval_case_comparison(
                            module,
                            &mut expression_store,
                            target_expr,
                            target,
                            hi,
                            true,
                            if *inclusive { Op::LessEq } else { Op::Less },
                            arena,
                        )
                    },
                    true,
                    arena,
                )
            }
        }
    }

    let mut patterns = patterns.iter();
    let first = patterns
        .next()
        .expect("CaseArm must have at least one pattern");
    let mut condition = eval_pattern(module, store, target_expr, target, first, arena)?;
    for pattern in patterns {
        condition = merge_short_circuit_case_condition(
            module,
            store,
            condition,
            |rhs_store, arena| eval_pattern(module, rhs_store, target_expr, target, pattern, arena),
            false,
            arena,
        )?;
    }
    Ok(condition)
}
