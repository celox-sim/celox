//! The current path: what the branches taken on it decide (conditions with
//! a known value, complementary conditions, two-state values), and the
//! condition under which the current statement runs.

use super::*;

impl Comb<'_, '_> {
    /// The value a condition is known to have on the current path, from an
    /// enclosing branch on the same or the complementary two-state condition.
    pub(super) fn known(&self, truth: NodeId) -> Option<bool> {
        self.facts.iter().rev().find_map(|&(fact, value, _)| {
            if fact == truth {
                Some(value)
            } else if self.complementary(fact, truth) {
                Some(!value)
            } else {
                None
            }
        })
    }

    /// The boolean a truth node tests, without the truth wrappers.
    pub(super) fn truth_core(&self, node: NodeId) -> NodeId {
        let mut node = node;
        loop {
            match self.arena.get(node) {
                SLTNode::Unary(UnaryOp::ToTwoState | UnaryOp::Ident, inner) => node = *inner,
                SLTNode::Unary(UnaryOp::Or, inner) if self.width(*inner) == 1 => node = *inner,
                _ => return node,
            }
        }
    }

    /// Whether two truth nodes are complements for every two-state input.
    pub(super) fn complementary(&self, left: NodeId, right: NodeId) -> bool {
        let left = self.truth_core(left);
        let right = self.truth_core(right);
        if !self.two_state(left) || !self.two_state(right) {
            return false;
        }
        let negates = |a: NodeId, b: NodeId| match self.arena.get(b) {
            SLTNode::Unary(UnaryOp::LogicNot, inner) => self.truth_core(*inner) == a,
            SLTNode::Unary(UnaryOp::BitNot, inner) => {
                self.width(*inner) == 1 && self.truth_core(*inner) == a
            }
            _ => false,
        };
        if negates(left, right) || negates(right, left) {
            return true;
        }
        match (self.arena.get(left), self.arena.get(right)) {
            (SLTNode::Binary(a, op_a, b), SLTNode::Binary(c, op_b, d)) => {
                a == c
                    && b == d
                    && matches!(
                        (op_a, op_b),
                        (BinaryOp::Eq, BinaryOp::Ne)
                            | (BinaryOp::Ne, BinaryOp::Eq)
                            | (BinaryOp::EqCase, BinaryOp::NeCase)
                            | (BinaryOp::NeCase, BinaryOp::EqCase)
                    )
            }
            _ => false,
        }
    }

    /// Whether a value never holds unknown bits.
    pub(super) fn two_state(&self, node: NodeId) -> bool {
        if !self.m.four_state {
            return true;
        }
        match self.arena.get(node) {
            SLTNode::Input {
                variable, index, ..
            } => {
                index.is_empty()
                    && self
                        .m
                        .variables
                        .get(variable)
                        .is_some_and(|var| !var.is_4state)
            }
            SLTNode::Constant(_, mask, _, _) => mask.is_zero(),
            SLTNode::Unary(UnaryOp::ToTwoState, _) => true,
            SLTNode::Unary(_, inner)
            | SLTNode::Slice { expr: inner, .. }
            | SLTNode::Capture { expr: inner, .. } => self.two_state(*inner),
            SLTNode::Binary(left, op, right) => {
                !matches!(
                    op,
                    BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS
                ) && self.two_state(*left)
                    && self.two_state(*right)
            }
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => self.two_state(*cond) && self.two_state(*then_expr) && self.two_state(*else_expr),
            SLTNode::Concat(parts) => parts.iter().all(|(part, _)| self.two_state(*part)),
            _ => false,
        }
    }

    /// The condition under which the current statement runs, if it does not
    /// run unconditionally.
    pub(super) fn path_condition(&mut self) -> Result<Option<Value>, sv::AnalyzerError> {
        let mut terms: Vec<Value> = Vec::new();
        for (truth, value, sources) in self.facts.clone() {
            let node = if value {
                truth
            } else {
                slt_not(self.arena, &mut self.consts, truth)?
            };
            terms.push((node, sources));
        }
        terms.extend(self.guards.iter().cloned());
        let mut result: Option<Value> = None;
        for (node, sources) in terms {
            if slt_bool(self.arena, &mut self.consts, node) == Some(true) {
                continue;
            }
            result = Some(match result {
                None => (node, sources),
                Some((other, mut other_sources)) => {
                    other_sources.extend(sources);
                    (
                        slt_and(self.arena, &mut self.consts, other, node)?,
                        other_sources,
                    )
                }
            });
        }
        Ok(result)
    }
}
