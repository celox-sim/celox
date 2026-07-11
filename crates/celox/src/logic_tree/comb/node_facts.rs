use std::{fmt, hash::Hash};

use crate::ir::BitAccess;

use super::node::{NodeId, SLTLoopBound, SLTNode, SLTNodeArena, SLTStepOp};
use super::node_rules;

/// Width facts for every node in an [`SLTNodeArena`].
///
/// Construction verifies the complete dependency graph before computing any
/// widths.  The implementation is iterative so malformed cycles and very deep
/// expression graphs cannot overflow the Rust call stack.
pub struct SLTNodeFacts<'arena, A: Hash + Eq + Clone> {
    arena: &'arena SLTNodeArena<A>,
    widths: Vec<usize>,
    lowerable: Vec<bool>,
}

impl<A: Hash + Eq + Clone> fmt::Debug for SLTNodeFacts<'_, A> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SLTNodeFacts")
            .field("node_count", &self.widths.len())
            .field("widths", &self.widths)
            .field("lowerable", &self.lowerable)
            .finish()
    }
}

impl<'arena, A> SLTNodeFacts<'arena, A>
where
    A: Hash + Eq + Clone,
{
    /// Verify `arena` and compute one width for every node.
    pub fn verify(arena: &'arena SLTNodeArena<A>) -> Result<Self, SLTNodeFactsError> {
        let node_count = arena.len();

        // An arena is a canonical append-only DAG: a node can only reference
        // operands that were already allocated.  Check every untrusted ID
        // without dereferencing it before building any fact table.
        for (node_index, node) in arena.iter().enumerate() {
            let owner = NodeId(node_index);
            try_for_each_child(node, |child| {
                if child.0 >= node_count {
                    return Err(SLTNodeFactsError::new(
                        "GRAPH.CHILD_EXISTS",
                        owner,
                        format!(
                            "node n{} references missing child n{}; arena contains {node_count} nodes",
                            owner.0, child.0
                        ),
                    ));
                }
                if child.0 >= owner.0 {
                    return Err(SLTNodeFactsError::new(
                        "GRAPH.CHILD_PRECEDES_OWNER",
                        owner,
                        format!(
                            "node n{} references child n{}, which does not precede its owner",
                            owner.0, child.0
                        ),
                    ));
                }
                Ok(())
            })?;
        }

        // Child facts are available by construction in NodeId order.  This
        // avoids reverse-edge storage, a Kahn worklist, and Option-sized fact
        // slots. Vec<bool> keeps the persistent lowerability fact packed.
        let allocation_node = NodeId(node_count.saturating_sub(1));
        let mut widths = Vec::new();
        widths.try_reserve_exact(node_count).map_err(|error| {
            SLTNodeFactsError::new(
                "FACTS.STORAGE_AVAILABLE",
                allocation_node,
                format!("cannot reserve widths for {node_count} nodes: {error}"),
            )
        })?;
        let mut lowerable = Vec::new();
        lowerable.try_reserve_exact(node_count).map_err(|error| {
            SLTNodeFactsError::new(
                "FACTS.STORAGE_AVAILABLE",
                allocation_node,
                format!("cannot reserve lowerability for {node_count} nodes: {error}"),
            )
        })?;
        for (node_index, node) in arena.iter().enumerate() {
            let node_id = NodeId(node_index);
            let width = compute_width(node_id, node, &widths)?;
            let mut node_lowerable = node_rules::direct_lowerable(
                width,
                matches!(node, SLTNode::Concat(parts) if parts.iter().any(|(_, width)| *width == 0)),
            );
            try_for_each_child(node, |child| {
                let Some(&child_lowerable) = lowerable.get(child.0) else {
                    return Err(SLTNodeFactsError::new(
                        "FACTS.CHILD_LOWERABILITY_AVAILABLE",
                        node_id,
                        format!(
                            "lowerability of child n{} was not available while evaluating n{}",
                            child.0, node_id.0
                        ),
                    ));
                };
                node_lowerable &= child_lowerable;
                Ok(())
            })?;
            widths.push(width);
            lowerable.push(node_lowerable);
        }

        Ok(Self {
            arena,
            widths,
            lowerable,
        })
    }

    /// Return the verified width of `node`, or `None` when the ID does not
    /// belong to the arena from which this table was built.
    pub fn width(&self, node: NodeId) -> Option<usize> {
        self.arena.get_checked(node)?;
        self.widths.get(node.0).copied()
    }

    /// Return a verified root width, diagnosing a root that does not belong to
    /// the arena instead of allowing a later unchecked lookup to panic.
    pub fn require_width(
        &self,
        node: NodeId,
        role: &'static str,
    ) -> Result<usize, SLTNodeFactsError> {
        self.width(node).ok_or_else(|| {
            SLTNodeFactsError::new(
                "ROOT.NODE_EXISTS",
                node,
                format!("{role} references missing root n{}", node.0),
            )
        })
    }

    /// Require a root and every node reachable from it to be lowerable to
    /// nonzero-width executable IR.
    pub fn require_lowerable(
        &self,
        node: NodeId,
        role: &'static str,
    ) -> Result<usize, SLTNodeFactsError> {
        let width = self.require_width(node, role)?;
        if !self.lowerable[node.0] {
            let blocker = self.lowerability_blocker(node);
            return Err(SLTNodeFactsError::new(
                "ROOT.LOWERABLE_NON_ZERO",
                blocker,
                format!(
                    "{role} root n{} reaches n{}, which has a zero executable width",
                    node.0, blocker.0
                ),
            ));
        }
        Ok(width)
    }

    /// Find the first direct zero-width cause on the first non-lowerable child
    /// path. This runs only for a rejected root and allocates no traversal
    /// storage; canonical child IDs strictly decrease at every step.
    fn lowerability_blocker(&self, mut node_id: NodeId) -> NodeId {
        loop {
            let Some(node) = self.arena.get_checked(node_id) else {
                return node_id;
            };
            let direct_blocker = self.widths.get(node_id.0).copied() == Some(0)
                || matches!(node, SLTNode::Concat(parts) if parts.iter().any(|(_, width)| *width == 0));
            if direct_blocker {
                return node_id;
            }

            let mut next = None;
            try_for_each_child(node, |child| {
                if next.is_none() && self.lowerable.get(child.0).copied() == Some(false) {
                    next = Some(child);
                }
                Ok::<(), std::convert::Infallible>(())
            })
            .unwrap_or_else(|never| match never {});
            let Some(child) = next else {
                // The table is private and built atomically, so this can only
                // describe an internal inconsistency. Keep the public failure
                // fallible and attribute it to the last verified node.
                return node_id;
            };
            node_id = child;
        }
    }

    /// Return all widths in `NodeId` order.
    #[cfg(test)]
    pub fn widths(&self) -> &[usize] {
        &self.widths
    }
}

/// A structured failure produced while verifying an SLT node graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SLTNodeFactsError {
    pub invariant: &'static str,
    pub node: NodeId,
    pub message: String,
}

impl SLTNodeFactsError {
    pub(crate) fn new(invariant: &'static str, node: NodeId, message: impl Into<String>) -> Self {
        Self {
            invariant,
            node,
            message: message.into(),
        }
    }
}

impl fmt::Display for SLTNodeFactsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "SLT node facts verify [{}] at n{}: {}",
            self.invariant, self.node.0, self.message
        )
    }
}

impl std::error::Error for SLTNodeFactsError {}

fn compute_width<A>(
    node_id: NodeId,
    node: &SLTNode<A>,
    widths: &[usize],
) -> Result<usize, SLTNodeFactsError>
where
    A: Hash + Eq + Clone,
{
    let child_width = |child: NodeId| {
        widths.get(child.0).copied().ok_or_else(|| {
            SLTNodeFactsError::new(
                "FACTS.CHILD_WIDTH_AVAILABLE",
                node_id,
                format!(
                    "width of child n{} was not available while evaluating n{}",
                    child.0, node_id.0
                ),
            )
        })
    };

    match node {
        SLTNode::Input { access, .. } => checked_access_width(node_id, *access, "input"),
        SLTNode::Constant(value, mask, width, _) => node_rules::constant_width(value, mask, *width)
            .map_err(|error| rule_error(node_id, error)),
        SLTNode::Binary(lhs, op, rhs) => {
            let lhs_width = child_width(*lhs)?;
            let rhs_width = child_width(*rhs)?;
            node_rules::binary_width(*op, lhs_width, rhs_width)
                .map_err(|error| rule_error(node_id, error))
        }
        SLTNode::Unary(op, inner) => Ok(node_rules::unary_width(*op, child_width(*inner)?)),
        SLTNode::Mux {
            then_expr,
            else_expr,
            ..
        } => Ok(node_rules::mux_width(
            child_width(*then_expr)?,
            child_width(*else_expr)?,
        )),
        SLTNode::ForFold {
            loop_var: _,
            loop_width,
            loop_signed,
            start,
            end,
            inclusive,
            step_op,
            reverse,
            result,
            initials,
            updates,
            effects,
            continue_cond,
            ..
        } => {
            if *loop_width == 0 {
                return Err(SLTNodeFactsError::new(
                    "FOR_FOLD.LOOP_WIDTH_NON_ZERO",
                    node_id,
                    "ForFold loop width is zero",
                ));
            }
            if *reverse && *step_op != SLTStepOp::Add {
                return Err(SLTNodeFactsError::new(
                    "FOR_FOLD.REVERSE_STEP_IS_ADD",
                    node_id,
                    format!("reverse ForFold ignores unsupported {step_op:?} step semantics"),
                ));
            }
            if initials.len() != updates.len() {
                return Err(SLTNodeFactsError::new(
                    "FOR_FOLD.STATE_ARITY_MATCHES",
                    node_id,
                    format!(
                        "ForFold has {} initial states but {} updates",
                        initials.len(),
                        updates.len()
                    ),
                ));
            }

            let require_nonzero_child = |child: NodeId, role: &str| {
                let width = child_width(child)?;
                if width == 0 {
                    return Err(SLTNodeFactsError::new(
                        "FOR_FOLD.OPERAND_NON_ZERO",
                        node_id,
                        format!("{role} n{} has zero width", child.0),
                    ));
                }
                Ok(width)
            };

            let mut counter_width = *loop_width;
            for (role, bound) in [("start", start), ("end", end)] {
                let width = match bound {
                    SLTLoopBound::Const(value) => {
                        (usize::BITS as usize - value.leading_zeros() as usize).max(1)
                    }
                    SLTLoopBound::Expr(child) => require_nonzero_child(*child, role)?,
                };
                counter_width = counter_width.max(width);
            }
            if *inclusive && !*loop_signed && counter_width.checked_add(1).is_none() {
                return Err(SLTNodeFactsError::new(
                    "FOR_FOLD.INCLUSIVE_WIDTH_REPRESENTABLE",
                    node_id,
                    format!(
                        "inclusive unsigned ForFold cannot widen counter width {counter_width}"
                    ),
                ));
            }

            let mut target_accesses: crate::HashMap<A, Vec<(BitAccess, usize)>> =
                crate::HashMap::default();
            for (index, (initial, update)) in initials.iter().zip(updates).enumerate() {
                if initial.target != update.target {
                    return Err(SLTNodeFactsError::new(
                        "FOR_FOLD.POSITIONAL_TARGET_MATCHES",
                        node_id,
                        format!("initial and update target differ at state position {index}"),
                    ));
                }
                checked_access_width(node_id, update.target.access, "ForFold state target")?;
                require_nonzero_child(initial.expr, "ForFold initial state")?;
                require_nonzero_child(update.expr, "ForFold update state")?;
                target_accesses
                    .entry(update.target.id.clone())
                    .or_default()
                    .push((update.target.access, index));
            }
            for accesses in target_accesses.values_mut() {
                accesses.sort_unstable_by_key(|(access, _)| (access.lsb, access.msb));
                for pair in accesses.windows(2) {
                    let (previous, previous_index) = pair[0];
                    let (current, current_index) = pair[1];
                    if previous.msb >= current.lsb {
                        return Err(SLTNodeFactsError::new(
                            "FOR_FOLD.STATE_TARGETS_DISJOINT",
                            node_id,
                            format!(
                                "state targets at positions {previous_index} and {current_index} overlap"
                            ),
                        ));
                    }
                }
            }

            checked_access_width(node_id, result.access, "ForFold result")?;
            let result_count = updates
                .iter()
                .filter(|update| update.target == *result)
                .count();
            if result_count != 1 {
                return Err(SLTNodeFactsError::new(
                    "FOR_FOLD.RESULT_TARGET_UNIQUE",
                    node_id,
                    format!("ForFold result occurs {result_count} times in its update targets"),
                ));
            }

            for effect in effects {
                if let Some(guard) = effect.guard {
                    require_nonzero_child(guard, "ForFold effect guard")?;
                }
                for &arg in &effect.args {
                    require_nonzero_child(arg, "ForFold effect argument")?;
                }
            }
            require_nonzero_child(*continue_cond, "ForFold continue condition")?;
            checked_access_width(node_id, result.access, "ForFold result")
        }
        SLTNode::Concat(parts) => node_rules::concat_width(parts.iter().map(|(_, width)| *width))
            .map_err(|error| rule_error(node_id, error)),
        SLTNode::Slice { expr, access } => {
            let expression_width = child_width(*expr)?;
            node_rules::slice_width(*access, expression_width, format_args!("n{}", expr.0))
                .map_err(|error| rule_error(node_id, error))
        }
    }
}

fn checked_access_width(
    node: NodeId,
    access: BitAccess,
    role: &str,
) -> Result<usize, SLTNodeFactsError> {
    node_rules::access_width(access, role).map_err(|error| rule_error(node, error))
}

fn rule_error(node: NodeId, error: node_rules::NodeRuleError) -> SLTNodeFactsError {
    SLTNodeFactsError::new(error.invariant, node, error.message)
}

fn try_for_each_child<A, E>(
    node: &SLTNode<A>,
    mut visit: impl FnMut(NodeId) -> Result<(), E>,
) -> Result<(), E>
where
    A: Hash + Eq + Clone,
{
    match node {
        SLTNode::Input { index, .. } => {
            for entry in index {
                visit(entry.node)?;
            }
        }
        SLTNode::Constant(..) => {}
        SLTNode::Binary(lhs, _, rhs) => {
            visit(*lhs)?;
            visit(*rhs)?;
        }
        SLTNode::Unary(_, inner) => visit(*inner)?,
        SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } => {
            visit(*cond)?;
            visit(*then_expr)?;
            visit(*else_expr)?;
        }
        SLTNode::ForFold {
            start,
            end,
            initials,
            updates,
            effects,
            continue_cond,
            ..
        } => {
            if let SLTLoopBound::Expr(node) = start {
                visit(*node)?;
            }
            if let SLTLoopBound::Expr(node) = end {
                visit(*node)?;
            }
            for initial in initials {
                visit(initial.expr)?;
            }
            for update in updates {
                visit(update.expr)?;
            }
            for effect in effects {
                if let Some(guard) = effect.guard {
                    visit(guard)?;
                }
                for &arg in &effect.args {
                    visit(arg)?;
                }
            }
            visit(*continue_cond)?;
        }
        SLTNode::Concat(parts) => {
            for &(part, _) in parts {
                visit(part)?;
            }
        }
        SLTNode::Slice { expr, .. } => visit(*expr)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use num_bigint::BigUint;

    use crate::ir::{BinaryOp, UnaryOp, VarAtomBase};

    use super::*;
    use crate::logic_tree::comb::node::{SLTForEffect, SLTForUpdate, SLTStepOp};

    fn arena(nodes: Vec<SLTNode<u32>>) -> SLTNodeArena<u32> {
        SLTNodeArena::from_nodes_unchecked(nodes)
    }

    fn constant(width: usize) -> SLTNode<u32> {
        SLTNode::Constant(BigUint::from(0u8), BigUint::from(0u8), width, false)
    }

    fn valid_for_fold() -> SLTNode<u32> {
        let target = VarAtomBase::new(2, 0, 7);
        SLTNode::ForFold {
            loop_var: 1,
            loop_width: 8,
            loop_signed: false,
            start: SLTLoopBound::Const(0),
            end: SLTLoopBound::Const(1),
            inclusive: false,
            step: 1,
            step_op: SLTStepOp::Add,
            reverse: false,
            result: target,
            initials: vec![SLTForUpdate {
                target,
                expr: NodeId(0),
            }],
            updates: vec![SLTForUpdate {
                target,
                expr: NodeId(0),
            }],
            effects: Vec::new(),
            continue_cond: NodeId(1),
        }
    }

    fn verify_for_fold(node: SLTNode<u32>) -> Result<(), SLTNodeFactsError> {
        let arena = arena(vec![constant(8), constant(1), node]);
        SLTNodeFacts::verify(&arena).map(|_| ())
    }

    #[test]
    fn computes_declared_width_rules() {
        let arena = arena(vec![
            constant(0),                                          // n0
            constant(4),                                          // n1
            constant(9),                                          // n2
            SLTNode::Binary(NodeId(1), BinaryOp::Add, NodeId(2)), // n3 = 9
            SLTNode::Binary(NodeId(1), BinaryOp::Shl, NodeId(2)), // n4 = 4
            SLTNode::Binary(NodeId(1), BinaryOp::Eq, NodeId(2)),  // n5 = 1
            SLTNode::Unary(UnaryOp::LogicNot, NodeId(2)),         // n6 = 1
            SLTNode::Mux {
                cond: NodeId(0),
                then_expr: NodeId(1),
                else_expr: NodeId(2),
            }, // n7 = 9
            SLTNode::Concat(vec![(NodeId(1), 2), (NodeId(2), 7)]), // n8 = 9
            SLTNode::Slice {
                expr: NodeId(2),
                access: BitAccess { lsb: 2, msb: 5 },
            }, // n9 = 4
            SLTNode::Binary(NodeId(1), BinaryOp::EqWildcard, NodeId(1)), // n10 = 1
        ]);

        let facts = SLTNodeFacts::verify(&arena).expect("well-formed arena must verify");
        assert_eq!(facts.widths(), &[0, 4, 9, 9, 4, 1, 1, 9, 9, 4, 1]);
        assert_eq!(facts.width(NodeId(11)), None);
    }

    #[test]
    fn rejects_missing_child_before_graph_traversal() {
        let arena = arena(vec![SLTNode::Unary(UnaryOp::Ident, NodeId(7))]);
        let error = SLTNodeFacts::verify(&arena).expect_err("missing child must fail");
        assert_eq!(error.invariant, "GRAPH.CHILD_EXISTS");
        assert_eq!(error.node, NodeId(0));
        assert!(error.message.contains("n7"));
    }

    #[test]
    fn rejects_dependency_cycle_as_noncanonical_forward_edge() {
        let arena = arena(vec![
            SLTNode::Unary(UnaryOp::Ident, NodeId(1)),
            SLTNode::Unary(UnaryOp::Ident, NodeId(0)),
        ]);
        let error = SLTNodeFacts::verify(&arena).expect_err("cycle must fail");
        assert_eq!(error.invariant, "GRAPH.CHILD_PRECEDES_OWNER");
        assert_eq!(error.node, NodeId(0));
    }

    #[test]
    fn rejects_acyclic_forward_reference() {
        let arena = arena(vec![SLTNode::Unary(UnaryOp::Ident, NodeId(1)), constant(8)]);
        let error = SLTNodeFacts::verify(&arena).expect_err("forward edge must fail");
        assert_eq!(error.invariant, "GRAPH.CHILD_PRECEDES_OWNER");
        assert_eq!(error.node, NodeId(0));
        assert!(error.message.contains("child n1"));
    }

    #[test]
    fn rejects_self_reference() {
        let arena = arena(vec![SLTNode::Unary(UnaryOp::Ident, NodeId(0))]);
        let error = SLTNodeFacts::verify(&arena).expect_err("self edge must fail");
        assert_eq!(error.invariant, "GRAPH.CHILD_PRECEDES_OWNER");
        assert_eq!(error.node, NodeId(0));
    }

    #[test]
    fn rejects_malformed_and_overflowing_accesses() {
        let malformed = arena(vec![SLTNode::Input {
            variable: 1,
            signed: false,
            index: Vec::new(),
            access: BitAccess { lsb: 5, msb: 4 },
        }]);
        let error = SLTNodeFacts::verify(&malformed).expect_err("reversed access must fail");
        assert_eq!(error.invariant, "WIDTH.ACCESS_ORDERED");

        let overflowing = arena(vec![SLTNode::Input {
            variable: 1,
            signed: false,
            index: Vec::new(),
            access: BitAccess {
                lsb: 0,
                msb: usize::MAX,
            },
        }]);
        let error = SLTNodeFacts::verify(&overflowing).expect_err("overflowing access must fail");
        assert_eq!(error.invariant, "WIDTH.ACCESS_REPRESENTABLE");
    }

    #[test]
    fn rejects_slice_outside_child_width() {
        let arena = arena(vec![
            constant(4),
            SLTNode::Slice {
                expr: NodeId(0),
                access: BitAccess { lsb: 1, msb: 4 },
            },
        ]);
        let error = SLTNodeFacts::verify(&arena).expect_err("out-of-range slice must fail");
        assert_eq!(error.invariant, "WIDTH.SLICE_IN_BOUNDS");
    }

    #[test]
    fn rejects_concat_width_overflow() {
        let arena = arena(vec![
            constant(0),
            SLTNode::Concat(vec![(NodeId(0), usize::MAX), (NodeId(0), 1)]),
        ]);
        let error = SLTNodeFacts::verify(&arena).expect_err("concat overflow must fail");
        assert_eq!(error.invariant, "WIDTH.CONCAT_REPRESENTABLE");
    }

    #[test]
    fn rejects_mismatched_wildcard_operand_widths() {
        for op in [BinaryOp::EqWildcard, BinaryOp::NeWildcard] {
            let arena = arena(vec![
                constant(4),
                constant(8),
                SLTNode::Binary(NodeId(0), op, NodeId(1)),
            ]);
            let error = SLTNodeFacts::verify(&arena).expect_err("wildcard widths must agree");
            assert_eq!(error.invariant, "WIDTH.WILDCARD_OPERANDS_MATCH");
        }
    }

    #[test]
    fn rejects_constant_payload_and_mask_outside_declared_width() {
        let payload = arena(vec![SLTNode::Constant(
            BigUint::from(0x10u8),
            BigUint::from(0u8),
            4,
            false,
        )]);
        assert_eq!(
            SLTNodeFacts::verify(&payload)
                .expect_err("payload must fit")
                .invariant,
            "CONSTANT.VALUE_FITS_WIDTH"
        );

        let mask = arena(vec![SLTNode::Constant(
            BigUint::from(0u8),
            BigUint::from(0x10u8),
            4,
            false,
        )]);
        assert_eq!(
            SLTNodeFacts::verify(&mask)
                .expect_err("mask must fit")
                .invariant,
            "CONSTANT.MASK_FITS_WIDTH"
        );
    }

    #[test]
    fn validates_complete_for_fold_contract() {
        verify_for_fold(valid_for_fold()).expect("complete ForFold must verify");

        let mut node = valid_for_fold();
        let SLTNode::ForFold { loop_width, .. } = &mut node else {
            unreachable!()
        };
        *loop_width = 0;
        assert_eq!(
            verify_for_fold(node).unwrap_err().invariant,
            "FOR_FOLD.LOOP_WIDTH_NON_ZERO"
        );

        let mut node = valid_for_fold();
        let SLTNode::ForFold { updates, .. } = &mut node else {
            unreachable!()
        };
        updates.clear();
        assert_eq!(
            verify_for_fold(node).unwrap_err().invariant,
            "FOR_FOLD.STATE_ARITY_MATCHES"
        );

        let mut node = valid_for_fold();
        let SLTNode::ForFold { updates, .. } = &mut node else {
            unreachable!()
        };
        updates[0].target = VarAtomBase::new(3, 0, 7);
        assert_eq!(
            verify_for_fold(node).unwrap_err().invariant,
            "FOR_FOLD.POSITIONAL_TARGET_MATCHES"
        );

        let mut node = valid_for_fold();
        let SLTNode::ForFold { result, .. } = &mut node else {
            unreachable!()
        };
        *result = VarAtomBase::new(3, 0, 7);
        assert_eq!(
            verify_for_fold(node).unwrap_err().invariant,
            "FOR_FOLD.RESULT_TARGET_UNIQUE"
        );

        let mut node = valid_for_fold();
        let SLTNode::ForFold {
            reverse, step_op, ..
        } = &mut node
        else {
            unreachable!()
        };
        *reverse = true;
        *step_op = SLTStepOp::Mul;
        assert_eq!(
            verify_for_fold(node).unwrap_err().invariant,
            "FOR_FOLD.REVERSE_STEP_IS_ADD"
        );

        let mut node = valid_for_fold();
        let SLTNode::ForFold { continue_cond, .. } = &mut node else {
            unreachable!()
        };
        *continue_cond = NodeId(2);
        let zero_continue_arena = arena(vec![constant(8), constant(1), constant(0), node]);
        assert_eq!(
            SLTNodeFacts::verify(&zero_continue_arena)
                .unwrap_err()
                .invariant,
            "FOR_FOLD.OPERAND_NON_ZERO"
        );

        let mut node = valid_for_fold();
        let SLTNode::ForFold { effects, .. } = &mut node else {
            unreachable!()
        };
        effects.push(SLTForEffect {
            site_id: 0,
            guard: Some(NodeId(2)),
            emit_on_true: true,
            args: Vec::new(),
            fatal_error_code: None,
        });
        let zero_guard_arena = arena(vec![constant(8), constant(1), constant(0), node]);
        assert_eq!(
            SLTNodeFacts::verify(&zero_guard_arena)
                .unwrap_err()
                .invariant,
            "FOR_FOLD.OPERAND_NON_ZERO"
        );
    }

    #[test]
    fn rejects_overlapping_for_fold_state_targets() {
        let mut node = valid_for_fold();
        let SLTNode::ForFold {
            initials, updates, ..
        } = &mut node
        else {
            unreachable!()
        };
        let overlapping = VarAtomBase::new(2, 4, 11);
        initials.push(SLTForUpdate {
            target: overlapping,
            expr: NodeId(0),
        });
        updates.push(SLTForUpdate {
            target: overlapping,
            expr: NodeId(0),
        });
        assert_eq!(
            verify_for_fold(node).unwrap_err().invariant,
            "FOR_FOLD.STATE_TARGETS_DISJOINT"
        );
    }

    #[test]
    fn rejects_unsigned_inclusive_for_fold_width_overflow() {
        let target = VarAtomBase::new(2, 0, 0);
        let node = SLTNode::ForFold {
            loop_var: 1,
            loop_width: 1,
            loop_signed: false,
            start: SLTLoopBound::Expr(NodeId(0)),
            end: SLTLoopBound::Const(1),
            inclusive: true,
            step: 1,
            step_op: SLTStepOp::Add,
            reverse: false,
            result: target,
            initials: vec![SLTForUpdate {
                target,
                expr: NodeId(1),
            }],
            updates: vec![SLTForUpdate {
                target,
                expr: NodeId(1),
            }],
            effects: Vec::new(),
            continue_cond: NodeId(1),
        };
        let arena = arena(vec![constant(usize::MAX), constant(1), node]);
        assert_eq!(
            SLTNodeFacts::verify(&arena).unwrap_err().invariant,
            "FOR_FOLD.INCLUSIVE_WIDTH_REPRESENTABLE"
        );
    }

    #[test]
    fn checks_for_fold_result_access() {
        let arena = arena(vec![
            constant(1),
            SLTNode::ForFold {
                loop_var: 1,
                loop_width: 8,
                loop_signed: false,
                start: SLTLoopBound::Const(0),
                end: SLTLoopBound::Const(1),
                inclusive: false,
                step: 1,
                step_op: SLTStepOp::Add,
                reverse: false,
                result: VarAtomBase::new(2, 7, 3),
                initials: vec![SLTForUpdate {
                    target: VarAtomBase::new(2, 0, 0),
                    expr: NodeId(0),
                }],
                updates: vec![SLTForUpdate {
                    target: VarAtomBase::new(2, 0, 0),
                    expr: NodeId(0),
                }],
                effects: Vec::new(),
                continue_cond: NodeId(0),
            },
        ]);
        let error = SLTNodeFacts::verify(&arena).expect_err("malformed result must fail");
        assert_eq!(error.invariant, "WIDTH.ACCESS_ORDERED");
        assert_eq!(error.node, NodeId(1));
    }

    #[test]
    fn permits_zero_width_nodes_when_the_operation_defines_them() {
        let arena = arena(vec![constant(0), SLTNode::Concat(Vec::new())]);
        let facts = SLTNodeFacts::verify(&arena).expect("zero-width facts are representable");
        assert_eq!(facts.widths(), &[0, 0]);
    }

    #[test]
    fn reports_the_first_reachable_lowerability_blocker() {
        let arena = arena(vec![
            constant(0),
            SLTNode::Unary(UnaryOp::LogicNot, NodeId(0)),
        ]);
        let facts = SLTNodeFacts::verify(&arena).expect("zero-width facts are representable");
        let error = facts
            .require_lowerable(NodeId(1), "test result")
            .expect_err("a reachable zero-width node must reject the root");
        assert_eq!(error.invariant, "ROOT.LOWERABLE_NON_ZERO");
        assert_eq!(error.node, NodeId(0));
        assert!(error.message.contains("root n1 reaches n0"));
    }

    #[test]
    fn verifies_a_deep_chain_without_recursion() {
        const DEPTH: usize = 100_000;
        let mut nodes = Vec::with_capacity(DEPTH + 1);
        nodes.push(constant(17));
        for node in 1..=DEPTH {
            nodes.push(SLTNode::Unary(UnaryOp::Ident, NodeId(node - 1)));
        }
        let arena = arena(nodes);
        let facts = SLTNodeFacts::verify(&arena).expect("deep acyclic graph must verify");
        assert_eq!(facts.width(NodeId(DEPTH)), Some(17));
    }
}
