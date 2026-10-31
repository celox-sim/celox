//! Exact comparisons retain literal X/Z even when runtime storage has one plane.
use super::procedural::{ConstCache, node_is_signed, slt_const4};
use super::*;
use num_traits::Zero;

type Planes = (NodeId, NodeId);

pub(super) fn compare<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    left: NodeId,
    op: BinaryOp,
    right: NodeId,
) -> Result<NodeId, celox_slt::SLTNodeFactsError> {
    let width = celox_slt::get_width(left, arena).max(celox_slt::get_width(right, arena));
    let signed = node_is_signed(arena, left) && node_is_signed(arena, right);
    let left = coerce_node_width(arena, left, Some(width), signed)?;
    let right = coerce_node_width(arena, right, Some(width), signed)?;
    let mut builder = Builder {
        arena,
        values: HashMap::default(),
        constants: ConstCache::default(),
    };
    let l = builder.project(left)?;
    let r = builder.project(right)?;
    if builder.zero(l.1) && builder.zero(r.1) {
        return builder.node(SLTNode::Binary(l.0, op, r.0));
    }
    builder.exact(l, r, op == BinaryOp::NeCase)
}

pub(super) fn is_known<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    node: NodeId,
) -> Result<bool, celox_slt::SLTNodeFactsError> {
    let mut builder = Builder {
        arena,
        values: HashMap::default(),
        constants: ConstCache::default(),
    };
    let (_, mask) = builder.project(node)?;
    Ok(builder.zero(mask))
}

struct Builder<'a, A: std::hash::Hash + Eq + Clone> {
    arena: &'a mut SLTNodeArena<A>,
    values: HashMap<NodeId, Planes>,
    constants: ConstCache,
}

impl<A: std::hash::Hash + Eq + Clone> Builder<'_, A> {
    fn node(&mut self, node: SLTNode<A>) -> Result<NodeId, celox_slt::SLTNodeFactsError> {
        self.arena.alloc(node)
    }
    fn constant(
        &mut self,
        value: BigUint,
        width: usize,
    ) -> Result<NodeId, celox_slt::SLTNodeFactsError> {
        self.node(SLTNode::Constant(value, BigUint::default(), width, false))
    }
    fn zeros(&mut self, width: usize) -> Result<NodeId, celox_slt::SLTNodeFactsError> {
        self.constant(BigUint::default(), width)
    }
    fn ones(&mut self, width: usize) -> Result<NodeId, celox_slt::SLTNodeFactsError> {
        self.constant((BigUint::from(1u8) << width) - 1u8, width)
    }
    fn binary(
        &mut self,
        a: NodeId,
        op: BinaryOp,
        b: NodeId,
    ) -> Result<NodeId, celox_slt::SLTNodeFactsError> {
        self.node(SLTNode::Binary(a, op, b))
    }
    fn unary(&mut self, op: UnaryOp, a: NodeId) -> Result<NodeId, celox_slt::SLTNodeFactsError> {
        self.node(SLTNode::Unary(op, a))
    }
    fn mux(
        &mut self,
        cond: NodeId,
        yes: NodeId,
        no: NodeId,
    ) -> Result<NodeId, celox_slt::SLTNodeFactsError> {
        self.node(SLTNode::Mux {
            cond,
            then_expr: yes,
            else_expr: no,
        })
    }
    fn zero(&mut self, node: NodeId) -> bool {
        slt_const4(self.arena, &mut self.constants, node)
            .is_some_and(|(v, m, _)| v.is_zero() && m.is_zero())
    }
    fn any(&mut self, node: NodeId) -> Result<NodeId, celox_slt::SLTNodeFactsError> {
        self.unary(UnaryOp::Or, node)
    }
    fn expanded(
        &mut self,
        flag: NodeId,
        width: usize,
    ) -> Result<NodeId, celox_slt::SLTNodeFactsError> {
        let one = self.ones(width)?;
        let zero = self.zeros(width)?;
        self.mux(flag, one, zero)
    }
    fn exact(
        &mut self,
        l: Planes,
        r: Planes,
        negate: bool,
    ) -> Result<NodeId, celox_slt::SLTNodeFactsError> {
        let payload = self.binary(l.0, BinaryOp::Eq, r.0)?;
        let mask = self.binary(l.1, BinaryOp::Eq, r.1)?;
        let equal = self.binary(payload, BinaryOp::And, mask)?;
        if negate {
            self.unary(UnaryOp::LogicNot, equal)
        } else {
            Ok(equal)
        }
    }
    fn truth(&mut self, p: Planes) -> Result<Planes, celox_slt::SLTNodeFactsError> {
        let not_mask = self.unary(UnaryOp::BitNot, p.1)?;
        let known = self.binary(p.0, BinaryOp::And, not_mask)?;
        let known_one = self.any(known)?;
        let unknown = self.any(p.1)?;
        let no_one = self.unary(UnaryOp::LogicNot, known_one)?;
        let unknown = self.binary(no_one, BinaryOp::And, unknown)?;
        let value = self.binary(known_one, BinaryOp::Or, unknown)?;
        Ok((value, unknown))
    }
    fn arithmetic(
        &mut self,
        value: NodeId,
        mask: NodeId,
        width: usize,
    ) -> Result<Planes, celox_slt::SLTNodeFactsError> {
        let mask = self.expanded(mask, width)?;
        let value = self.binary(value, BinaryOp::Or, mask)?;
        Ok((value, mask))
    }
    fn project(&mut self, root: NodeId) -> Result<Planes, celox_slt::SLTNodeFactsError> {
        // Append-only node IDs let this worklist retain sharing without cloning
        // the arena or using the native call stack for deeply nested operands.
        let mut work = vec![(root, false)];
        while let Some((id, expanded)) = work.pop() {
            if self.values.contains_key(&id) {
                continue;
            }
            let node = self.arena.get(id).clone();
            let width = celox_slt::get_width(id, self.arena);
            if let Some((value, mask, _)) = slt_const4(self.arena, &mut self.constants, id) {
                let signed = node_is_signed(self.arena, id);
                let value =
                    self.node(SLTNode::Constant(value, BigUint::default(), width, signed))?;
                let mask = self.constant(mask, width)?;
                self.values.insert(id, (value, mask));
                continue;
            }
            let children = match &node {
                SLTNode::Unary(_, a)
                | SLTNode::Slice { expr: a, .. }
                | SLTNode::Capture { expr: a, .. } => vec![*a],
                SLTNode::Binary(a, _, b) => vec![*a, *b],
                SLTNode::Concat(parts) => parts.iter().map(|p| p.0).collect(),
                SLTNode::Mux {
                    cond,
                    then_expr,
                    else_expr,
                } => vec![*cond, *then_expr, *else_expr],
                // Calls/folds and inputs cross a runtime storage boundary.
                // Their result is two-state; explicit literal operands remain
                // visible in the structural and arithmetic nodes above.
                _ => Vec::new(),
            };
            if !expanded && !children.is_empty() {
                work.push((id, true));
                for child in children.into_iter().rev() {
                    if !self.values.contains_key(&child) {
                        work.push((child, false));
                    }
                }
                continue;
            }
            let result = match node {
                SLTNode::Binary(a, op, b) => {
                    let l = self.values[&a];
                    let r = self.values[&b];
                    self.binary_planes(id, l, op, r, width)?
                }
                SLTNode::Unary(op, inner) => {
                    let p = self.values[&inner];
                    if self.zero(p.1) {
                        (self.unary(op, p.0)?, self.zeros(width)?)
                    } else {
                        match op {
                            UnaryOp::Ident => p,
                            UnaryOp::ToTwoState => {
                                let not = self.unary(UnaryOp::BitNot, p.1)?;
                                (self.binary(p.0, BinaryOp::And, not)?, self.zeros(width)?)
                            }
                            UnaryOp::BitNot => {
                                let not = self.unary(op, p.0)?;
                                (self.binary(not, BinaryOp::Or, p.1)?, p.1)
                            }
                            UnaryOp::LogicNot => {
                                let truth = self.truth(p)?;
                                let not = self.unary(UnaryOp::LogicNot, truth.0)?;
                                (self.binary(not, BinaryOp::Or, truth.1)?, truth.1)
                            }
                            UnaryOp::Or => self.truth(p)?,
                            UnaryOp::And => {
                                let candidate = self.binary(p.0, BinaryOp::Or, p.1)?;
                                let value = self.unary(op, candidate)?;
                                let unknown = self.any(p.1)?;
                                (value, self.binary(value, BinaryOp::And, unknown)?)
                            }
                            UnaryOp::Xor | UnaryOp::Minus => {
                                let value = self.unary(op, p.0)?;
                                let unknown = self.any(p.1)?;
                                self.arithmetic(value, unknown, width)?
                            }
                            UnaryOp::PopCount => {
                                let not = self.unary(UnaryOp::BitNot, p.1)?;
                                let known = self.binary(p.0, BinaryOp::And, not)?;
                                (self.unary(op, known)?, self.zeros(width)?)
                            }
                            _ => {
                                let value = self.unary(op, p.0)?;
                                let unknown = self.any(p.1)?;
                                self.arithmetic(value, unknown, width)?
                            }
                        }
                    }
                }
                SLTNode::Slice { expr, access } => {
                    let p = self.values[&expr];
                    (
                        self.node(SLTNode::Slice { expr: p.0, access })?,
                        self.node(SLTNode::Slice { expr: p.1, access })?,
                    )
                }
                SLTNode::Concat(parts) => {
                    let payload = parts.iter().map(|(p, w)| (self.values[p].0, *w)).collect();
                    let mask = parts.iter().map(|(p, w)| (self.values[p].1, *w)).collect();
                    (
                        self.node(SLTNode::Concat(payload))?,
                        self.node(SLTNode::Concat(mask))?,
                    )
                }
                SLTNode::Mux {
                    cond,
                    then_expr,
                    else_expr,
                } => {
                    let c = self.truth(self.values[&cond])?;
                    let t = self.values[&then_expr];
                    let e = self.values[&else_expr];
                    let value = self.mux(c.0, t.0, e.0)?;
                    let mask = self.mux(c.0, t.1, e.1)?;
                    if self.zero(c.1) {
                        (value, mask)
                    } else {
                        // An unknown condition merges equal bits and makes
                        // only differing bits X, retaining equal X/Z bits.
                        let dv = self.binary(t.0, BinaryOp::Xor, e.0)?;
                        let dm = self.binary(t.1, BinaryOp::Xor, e.1)?;
                        let diff = self.binary(dv, BinaryOp::Or, dm)?;
                        let merged_value = self.binary(t.0, BinaryOp::Or, diff)?;
                        let merged_mask = self.binary(t.1, BinaryOp::Or, diff)?;
                        (
                            self.mux(c.1, merged_value, value)?,
                            self.mux(c.1, merged_mask, mask)?,
                        )
                    }
                }
                SLTNode::Capture { expr, key } => {
                    let p = self.values[&expr];
                    (
                        self.node(SLTNode::Capture { expr: p.0, key })?,
                        self.node(SLTNode::Capture { expr: p.1, key })?,
                    )
                }
                _ => (id, self.zeros(width)?),
            };
            self.values.insert(id, result);
        }
        Ok(self.values[&root])
    }
    fn binary_planes(
        &mut self,
        original: NodeId,
        l: Planes,
        op: BinaryOp,
        r: Planes,
        width: usize,
    ) -> Result<Planes, celox_slt::SLTNodeFactsError> {
        if self.zero(l.1)
            && self.zero(r.1)
            && !matches!(
                op,
                BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS
            )
        {
            return Ok((self.binary(l.0, op, r.0)?, self.zeros(width)?));
        }
        match op {
            BinaryOp::EqCase | BinaryOp::NeCase => {
                Ok((self.exact(l, r, op == BinaryOp::NeCase)?, self.zeros(1)?))
            }
            BinaryOp::And | BinaryOp::Or | BinaryOp::Xor => {
                let masks = self.binary(l.1, BinaryOp::Or, r.1)?;
                let value = self.binary(l.0, op, r.0)?;
                let mask = if op == BinaryOp::Xor {
                    masks
                } else {
                    let (a, b) = if op == BinaryOp::And {
                        (
                            self.unary(UnaryOp::BitNot, l.0)?,
                            self.unary(UnaryOp::BitNot, r.0)?,
                        )
                    } else {
                        (l.0, r.0)
                    };
                    let lm = self.unary(UnaryOp::BitNot, l.1)?;
                    let rm = self.unary(UnaryOp::BitNot, r.1)?;
                    let a = self.binary(a, BinaryOp::And, lm)?;
                    let b = self.binary(b, BinaryOp::And, rm)?;
                    let known = self.binary(a, BinaryOp::Or, b)?;
                    let not = self.unary(UnaryOp::BitNot, known)?;
                    self.binary(masks, BinaryOp::And, not)?
                };
                Ok((self.binary(value, BinaryOp::Or, mask)?, mask))
            }
            BinaryOp::LogicAnd | BinaryOp::LogicOr => {
                let a = self.truth(l)?;
                let b = self.truth(r)?;
                self.binary_planes(
                    original,
                    a,
                    if op == BinaryOp::LogicAnd {
                        BinaryOp::And
                    } else {
                        BinaryOp::Or
                    },
                    b,
                    1,
                )
            }
            BinaryOp::Eq | BinaryOp::Ne | BinaryOp::EqWildcard | BinaryOp::NeWildcard => {
                let masks = self.binary(l.1, BinaryOp::Or, r.1)?;
                let not = self.unary(UnaryOp::BitNot, masks)?;
                let diff = self.binary(l.0, BinaryOp::Xor, r.0)?;
                let diff = self.binary(diff, BinaryOp::And, not)?;
                let mismatch = self.any(diff)?;
                let ambiguous = if matches!(op, BinaryOp::EqWildcard | BinaryOp::NeWildcard) {
                    let care = self.unary(UnaryOp::BitNot, r.1)?;
                    let lm = self.binary(l.1, BinaryOp::And, care)?;
                    self.any(lm)?
                } else {
                    self.any(masks)?
                };
                let no_diff = self.unary(UnaryOp::LogicNot, mismatch)?;
                let mask = self.binary(no_diff, BinaryOp::And, ambiguous)?;
                let value = if matches!(op, BinaryOp::Ne | BinaryOp::NeWildcard) {
                    self.binary(mismatch, BinaryOp::Or, mask)?
                } else {
                    no_diff
                };
                Ok((value, mask))
            }
            BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => {
                let value = self.binary(l.0, op, r.0)?;
                let mask = self.binary(l.1, op, r.0)?;
                let unknown = self.any(r.1)?;
                let all = self.expanded(unknown, width)?;
                Ok((
                    self.binary(value, BinaryOp::Or, all)?,
                    self.binary(mask, BinaryOp::Or, all)?,
                ))
            }
            BinaryOp::Float { .. } => Ok((original, self.zeros(width)?)),
            _ => {
                let lm = self.any(l.1)?;
                let rm = self.any(r.1)?;
                let mut unknown = self.binary(lm, BinaryOp::Or, rm)?;
                if matches!(
                    op,
                    BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS
                ) {
                    let zero = self.zeros(celox_slt::get_width(r.0, self.arena))?;
                    let zero = self.binary(r.0, BinaryOp::Eq, zero)?;
                    unknown = self.binary(unknown, BinaryOp::Or, zero)?;
                }
                let value = self.binary(l.0, op, r.0)?;
                self.arithmetic(value, unknown, width)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_known_operands_reuse_the_original_comparison() {
        let mut arena = SLTNodeArena::<SourceVarId>::new();
        let mut value = arena
            .alloc(SLTNode::Input {
                variable: SourceVarId::default(),
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(0, 31),
            })
            .unwrap();
        for i in 0..512u32 {
            let constant = arena
                .alloc(SLTNode::Constant(i.into(), BigUint::default(), 32, false))
                .unwrap();
            value = arena
                .alloc(SLTNode::Binary(value, BinaryOp::Xor, constant))
                .unwrap();
        }
        let zero = arena
            .alloc(SLTNode::Constant(
                BigUint::default(),
                BigUint::default(),
                32,
                false,
            ))
            .unwrap();
        let expected = arena
            .alloc(SLTNode::Binary(value, BinaryOp::EqCase, zero))
            .unwrap();
        let before = arena.len();
        assert_eq!(
            compare(&mut arena, value, BinaryOp::EqCase, zero).unwrap(),
            expected
        );
        assert!(
            arena.len() <= before + 1,
            "known operands allocated shadow work for every node"
        );
    }
}
