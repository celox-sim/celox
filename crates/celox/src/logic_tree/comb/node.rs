use std::{fmt, hash::Hash};

use num_bigint::BigUint;
use serde::{Deserialize, Deserializer, Serialize};

use crate::{
    HashMap,
    ir::{BinaryOp, BitAccess, UnaryOp, VarAtomBase},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NodeId(pub usize);

#[derive(Debug, Clone, Serialize)]
#[serde(bound(serialize = "A: Serialize + std::hash::Hash + Eq + Clone"))]
pub struct SLTNodeArena<A: Hash + Eq + Clone> {
    pub nodes: Vec<SLTNode<A>>,
    #[serde(skip)]
    pub cache: crate::HashMap<SLTNode<A>, NodeId>,
}

#[derive(Deserialize)]
#[serde(bound(deserialize = "A: Deserialize<'de> + std::hash::Hash + Eq + Clone"))]
struct SLTNodeArenaWire<A: Hash + Eq + Clone> {
    nodes: Vec<SLTNode<A>>,
}

impl<'de, A> Deserialize<'de> for SLTNodeArena<A>
where
    A: Deserialize<'de> + Hash + Eq + Clone,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = SLTNodeArenaWire::<A>::deserialize(deserializer)?;
        let mut arena = Self {
            nodes: wire.nodes,
            cache: crate::HashMap::default(),
        };
        arena.rebuild_cache();
        Ok(arena)
    }
}

impl<A: PartialEq + Hash + Eq + Clone> PartialEq for SLTNodeArena<A> {
    fn eq(&self, other: &Self) -> bool {
        self.nodes == other.nodes
    }
}

impl<A: Eq + Hash + Clone> Eq for SLTNodeArena<A> {}

impl<A: Hash + Eq + Clone> SLTNodeArena<A> {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            cache: crate::HashMap::default(),
        }
    }

    pub fn alloc(&mut self, node: SLTNode<A>) -> NodeId
    where
        A: Hash + Eq + Clone,
    {
        if let Some(id) = self.cache.get(&node) {
            return *id;
        }
        let id = NodeId(self.nodes.len());
        self.cache.insert(node.clone(), id);
        self.nodes.push(node);
        id
    }

    /// Rebuilds the derived interning cache from the persistent node list.
    ///
    /// If the list already contains duplicate nodes, the smallest (and therefore
    /// first) [`NodeId`] is retained as the canonical identity.
    pub fn rebuild_cache(&mut self) {
        self.cache.clear();
        for (idx, node) in self.nodes.iter().cloned().enumerate() {
            self.cache.entry(node).or_insert(NodeId(idx));
        }
    }

    pub fn get(&self, id: NodeId) -> &SLTNode<A> {
        &self.nodes[id.0]
    }

    pub fn display(&self, id: NodeId) -> NodeDisplay<'_, A> {
        NodeDisplay { arena: self, id }
    }
}

pub struct NodeDisplay<'a, A: Hash + Eq + Clone> {
    arena: &'a SLTNodeArena<A>,
    id: NodeId,
}

impl<'a, A: Hash + Eq + Clone + std::fmt::Display> std::fmt::Display for NodeDisplay<'a, A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "n{}: ", self.id.0)?;
        self.arena.get(self.id).fmt_expression(f, self.arena)
    }
}

impl<A: Hash + Eq + Clone> SLTNode<A> {
    pub fn fmt_expression(
        &self,
        f: &mut std::fmt::Formatter<'_>,
        arena: &SLTNodeArena<A>,
    ) -> std::fmt::Result
    where
        A: std::fmt::Display,
    {
        match self {
            SLTNode::Input {
                variable,
                index,
                access,
                ..
            } => {
                write!(f, "{}", variable)?;
                for idx in index {
                    write!(f, "n{}", idx.node.0)?;
                    write!(f, "[(idx)")?;
                    arena.get(idx.node).fmt_expression(f, arena)?;
                    if idx.stride > 1 {
                        write!(f, " * {}", idx.stride)?;
                    }
                    write!(f, "]")?;
                }
                if index.is_empty() {
                    write!(f, "{}", access)?;
                } else {
                    // For array access, the 'access' field represents the bit-slice within the element.
                    // If it targets a multi-dimensional array, indices are processed recursively.
                    if access.lsb != 0 || access.msb != 0 {
                        // This depends on how SLTNode::Input is used for arrays.
                        // If it's a bit-slice of an array element, we show it.
                        // write!(f, "{}", access)?;
                    }
                }
                Ok(())
            }
            SLTNode::Constant(val, _mask, _width, _signed) => {
                write!(f, "{}", val)
            }
            SLTNode::Binary(lhs, op, rhs) => {
                write!(f, "(")?;
                write!(f, "n{}:", lhs.0)?;
                arena.get(*lhs).fmt_expression(f, arena)?;
                let op_str = match op {
                    crate::ir::BinaryOp::Add => "+",
                    crate::ir::BinaryOp::Sub => "-",
                    crate::ir::BinaryOp::Mul => "*",
                    crate::ir::BinaryOp::Div => "/",
                    crate::ir::BinaryOp::Rem => "%",
                    crate::ir::BinaryOp::And => "&",
                    crate::ir::BinaryOp::Or => "|",
                    crate::ir::BinaryOp::Xor => "^",
                    crate::ir::BinaryOp::Shl => "<<",
                    crate::ir::BinaryOp::Shr => ">>",
                    crate::ir::BinaryOp::Sar => ">>>",
                    crate::ir::BinaryOp::Eq => "==",
                    crate::ir::BinaryOp::Ne => "!=",
                    crate::ir::BinaryOp::LtU | crate::ir::BinaryOp::LtS => "<",
                    crate::ir::BinaryOp::LeU | crate::ir::BinaryOp::LeS => "<=",
                    crate::ir::BinaryOp::GtU | crate::ir::BinaryOp::GtS => ">",
                    crate::ir::BinaryOp::GeU | crate::ir::BinaryOp::GeS => ">=",
                    crate::ir::BinaryOp::LogicAnd => "&&",
                    crate::ir::BinaryOp::LogicOr => "||",
                    crate::ir::BinaryOp::EqWildcard => "==?",
                    crate::ir::BinaryOp::NeWildcard => "!=?",
                };
                write!(f, " {} ", op_str)?;
                write!(f, "n{}:", rhs.0)?;
                arena.get(*rhs).fmt_expression(f, arena)?;
                write!(f, ")")
            }
            SLTNode::Unary(op, inner) => {
                let op_str = match op {
                    crate::ir::UnaryOp::Ident => "",
                    crate::ir::UnaryOp::Minus => "-",
                    crate::ir::UnaryOp::BitNot => "~",
                    crate::ir::UnaryOp::LogicNot => "!",
                    crate::ir::UnaryOp::And => "&", // reduction
                    crate::ir::UnaryOp::Or => "|",
                    crate::ir::UnaryOp::Xor => "^",
                };
                write!(f, "{}(", op_str)?;
                write!(f, "n{}:", inner.0)?;
                arena.get(*inner).fmt_expression(f, arena)?;
                write!(f, ")")
            }
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                write!(f, "(")?;
                write!(f, "n{}:", cond.0)?;
                arena.get(*cond).fmt_expression(f, arena)?;
                write!(f, " ? ")?;
                write!(f, "n{}:", then_expr.0)?;
                arena.get(*then_expr).fmt_expression(f, arena)?;
                write!(f, " : ")?;
                write!(f, "n{}:", else_expr.0)?;
                arena.get(*else_expr).fmt_expression(f, arena)?;
                write!(f, ")")
            }
            SLTNode::ForFold {
                loop_var,
                start,
                end,
                inclusive,
                step,
                step_op,
                reverse,
                result,
                initials,
                updates,
                ..
            } => {
                let fmt_bound =
                    |f: &mut std::fmt::Formatter<'_>, bound: &SLTLoopBound| -> std::fmt::Result {
                        match bound {
                            SLTLoopBound::Const(v) => write!(f, "{v}"),
                            SLTLoopBound::Expr(node) => {
                                write!(f, "n{}:", node.0)?;
                                arena.get(*node).fmt_expression(f, arena)
                            }
                        }
                    };
                write!(f, "for {loop_var} in ")?;
                if *reverse {
                    write!(f, "rev ")?;
                }
                fmt_bound(f, start)?;
                if *inclusive {
                    write!(f, "..=")?;
                } else {
                    write!(f, "..")?;
                }
                fmt_bound(f, end)?;
                if *step != 1 || *step_op != SLTStepOp::Add {
                    write!(f, " step ")?;
                    match step_op {
                        SLTStepOp::Add => write!(f, "+=")?,
                        SLTStepOp::Mul => write!(f, "*=")?,
                        SLTStepOp::Shl => write!(f, "<<=")?,
                    }
                    write!(f, " {step}")?;
                }
                write!(f, " => {result} init[")?;
                for (i, init) in initials.iter().enumerate() {
                    if i > 0 {
                        write!(f, "; ")?;
                    }
                    write!(f, "{} = n{}:", init.target, init.expr.0)?;
                    arena.get(init.expr).fmt_expression(f, arena)?;
                }
                write!(f, "] {{ ")?;
                for (i, update) in updates.iter().enumerate() {
                    if i > 0 {
                        write!(f, "; ")?;
                    }
                    write!(f, "{} = n{}:", update.target, update.expr.0)?;
                    arena.get(update.expr).fmt_expression(f, arena)?;
                }
                write!(f, " }}")
            }
            SLTNode::Concat(parts) => {
                write!(f, "{{")?;
                for (i, (part, w)) in parts.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "n{}@{w}:", part.0)?;
                    arena.get(*part).fmt_expression(f, arena)?;
                }
                write!(f, "}}")
            }
            SLTNode::Slice { expr, access } => {
                write!(f, "n{}:", expr.0)?;
                arena.get(*expr).fmt_expression(f, arena)?;
                write!(f, "{}", access)
            }
        }
    }
}

impl<A: Hash + Eq + Clone> Default for SLTNodeArena<A> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SLTIndex {
    pub node: NodeId,
    pub stride: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SLTLoopBound {
    Const(usize),
    Expr(NodeId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SLTStepOp {
    Add,
    Mul,
    Shl,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(bound(
    serialize = "A: Serialize + std::hash::Hash + Eq + Clone",
    deserialize = "A: Deserialize<'de> + std::hash::Hash + Eq + Clone"
))]
pub struct SLTForUpdate<A: Hash + Eq + Clone> {
    pub target: VarAtomBase<A>,
    pub expr: NodeId,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SLTForEffect {
    pub site_id: u32,
    pub guard: Option<NodeId>,
    pub emit_on_true: bool,
    pub args: Vec<NodeId>,
    pub fatal_error_code: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    into = "SLTNodeSerde<A>",
    from = "SLTNodeSerde<A>",
    bound(
        serialize = "A: Serialize + Clone",
        deserialize = "A: Deserialize<'de>"
    )
)]
pub enum SLTNode<A: Hash + Eq + Clone> {
    Input {
        variable: A,
        signed: bool,
        index: Vec<SLTIndex>,
        access: BitAccess,
    },
    Constant(BigUint, BigUint, usize, bool),
    Binary(NodeId, BinaryOp, NodeId),
    Unary(UnaryOp, NodeId),
    Mux {
        cond: NodeId,
        then_expr: NodeId,
        else_expr: NodeId,
    },
    ForFold {
        loop_var: A,
        loop_width: usize,
        loop_signed: bool,
        start: SLTLoopBound,
        end: SLTLoopBound,
        inclusive: bool,
        step: usize,
        step_op: SLTStepOp,
        reverse: bool,
        result: VarAtomBase<A>,
        initials: Vec<SLTForUpdate<A>>,
        updates: Vec<SLTForUpdate<A>>,
        effects: Vec<SLTForEffect>,
        continue_cond: NodeId,
    },
    // Concat/Slice are primarily used for RHS expression evaluation.
    // On the LHS (assignments), bit manipulation is handled implicitly by RangeStore atomization.
    Concat(Vec<(NodeId, usize)>),
    Slice {
        expr: NodeId,
        access: BitAccess,
    },
}

/// Serde-friendly mirror of [`SLTNode`] that replaces [`BigUint`] with `Vec<u8>` (little-endian).
#[derive(Serialize, Deserialize)]
#[serde(bound(serialize = "A: Serialize", deserialize = "A: Deserialize<'de>"))]
enum SLTNodeSerde<A: Hash + Eq + Clone> {
    Input {
        variable: A,
        signed: bool,
        index: Vec<SLTIndex>,
        access: BitAccess,
    },
    Constant {
        payload: Vec<u8>,
        mask: Vec<u8>,
        width: usize,
        signed: bool,
    },
    Binary(NodeId, BinaryOp, NodeId),
    Unary(UnaryOp, NodeId),
    Mux {
        cond: NodeId,
        then_expr: NodeId,
        else_expr: NodeId,
    },
    ForFold {
        loop_var: A,
        loop_width: usize,
        loop_signed: bool,
        start: SLTLoopBound,
        end: SLTLoopBound,
        inclusive: bool,
        step: usize,
        step_op: SLTStepOp,
        reverse: bool,
        result: VarAtomBase<A>,
        initials: Vec<SLTForUpdate<A>>,
        updates: Vec<SLTForUpdate<A>>,
        effects: Vec<SLTForEffect>,
        continue_cond: NodeId,
    },
    Concat(Vec<(NodeId, usize)>),
    Slice {
        expr: NodeId,
        access: BitAccess,
    },
}

impl<A: Hash + Eq + Clone> From<SLTNode<A>> for SLTNodeSerde<A> {
    fn from(node: SLTNode<A>) -> Self {
        match node {
            SLTNode::Input {
                variable,
                signed,
                index,
                access,
            } => SLTNodeSerde::Input {
                variable,
                signed,
                index,
                access,
            },
            SLTNode::Constant(payload, mask, width, signed) => SLTNodeSerde::Constant {
                payload: payload.to_bytes_le(),
                mask: mask.to_bytes_le(),
                width,
                signed,
            },
            SLTNode::Binary(a, op, b) => SLTNodeSerde::Binary(a, op, b),
            SLTNode::Unary(op, a) => SLTNodeSerde::Unary(op, a),
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => SLTNodeSerde::Mux {
                cond,
                then_expr,
                else_expr,
            },
            SLTNode::ForFold {
                loop_var,
                loop_width,
                loop_signed,
                start,
                end,
                inclusive,
                step,
                step_op,
                reverse,
                result,
                initials,
                updates,
                effects,
                continue_cond,
            } => SLTNodeSerde::ForFold {
                loop_var,
                loop_width,
                loop_signed,
                start,
                end,
                inclusive,
                step,
                step_op,
                reverse,
                result,
                initials,
                updates,
                effects,
                continue_cond,
            },
            SLTNode::Concat(parts) => SLTNodeSerde::Concat(parts),
            SLTNode::Slice { expr, access } => SLTNodeSerde::Slice { expr, access },
        }
    }
}

impl<A: Hash + Eq + Clone> From<SLTNodeSerde<A>> for SLTNode<A> {
    fn from(node: SLTNodeSerde<A>) -> Self {
        match node {
            SLTNodeSerde::Input {
                variable,
                signed,
                index,
                access,
            } => SLTNode::Input {
                variable,
                signed,
                index,
                access,
            },
            SLTNodeSerde::Constant {
                payload,
                mask,
                width,
                signed,
            } => SLTNode::Constant(
                BigUint::from_bytes_le(&payload),
                BigUint::from_bytes_le(&mask),
                width,
                signed,
            ),
            SLTNodeSerde::Binary(a, op, b) => SLTNode::Binary(a, op, b),
            SLTNodeSerde::Unary(op, a) => SLTNode::Unary(op, a),
            SLTNodeSerde::Mux {
                cond,
                then_expr,
                else_expr,
            } => SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            },
            SLTNodeSerde::ForFold {
                loop_var,
                loop_width,
                loop_signed,
                start,
                end,
                inclusive,
                step,
                step_op,
                reverse,
                result,
                initials,
                updates,
                effects,
                continue_cond,
            } => SLTNode::ForFold {
                loop_var,
                loop_width,
                loop_signed,
                start,
                end,
                inclusive,
                step,
                step_op,
                reverse,
                result,
                initials,
                updates,
                effects,
                continue_cond,
            },
            SLTNodeSerde::Concat(parts) => SLTNode::Concat(parts),
            SLTNodeSerde::Slice { expr, access } => SLTNode::Slice { expr, access },
        }
    }
}
impl<A: fmt::Debug + fmt::Display + Hash + Eq + Clone> SLTNode<A> {
    /// Maps the address type A to B recursively throughout the tree.
    pub fn map_addr<B, F>(
        &self,
        id: NodeId,
        arena: &SLTNodeArena<A>,
        target_arena: &mut SLTNodeArena<B>,
        cache: &mut HashMap<NodeId, NodeId>,
        f: &F,
    ) -> NodeId
    where
        A: Hash + Eq + Clone,
        B: Hash + Eq + Clone,
        F: Fn(&A) -> B,
    {
        if let Some(mapped_id) = cache.get(&id) {
            return *mapped_id;
        }

        let new_node = match self {
            // Leaf: Transform address A to B
            SLTNode::Input {
                variable: addr,
                signed,
                index,
                access,
            } => {
                let mapped_index = index
                    .iter()
                    .map(|idx| SLTIndex {
                        node: arena
                            .get(idx.node)
                            .map_addr(idx.node, arena, target_arena, cache, f),
                        stride: idx.stride,
                    })
                    .collect();
                SLTNode::Input {
                    variable: f(addr),
                    signed: *signed,
                    index: mapped_index,
                    access: *access,
                }
            }

            // Leaf: Constants remain unchanged
            SLTNode::Constant(val, mask, width, signed) => {
                SLTNode::Constant(val.clone(), mask.clone(), *width, *signed)
            }

            // Recursive cases
            SLTNode::Binary(lhs, op, rhs) => {
                let l = arena
                    .get(*lhs)
                    .map_addr(*lhs, arena, target_arena, cache, f);
                let r = arena
                    .get(*rhs)
                    .map_addr(*rhs, arena, target_arena, cache, f);
                SLTNode::Binary(l, *op, r)
            }

            SLTNode::Unary(op, inner) => {
                let i = arena
                    .get(*inner)
                    .map_addr(*inner, arena, target_arena, cache, f);
                SLTNode::Unary(*op, i)
            }

            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                let c = arena
                    .get(*cond)
                    .map_addr(*cond, arena, target_arena, cache, f);
                let t = arena
                    .get(*then_expr)
                    .map_addr(*then_expr, arena, target_arena, cache, f);
                let e = arena
                    .get(*else_expr)
                    .map_addr(*else_expr, arena, target_arena, cache, f);
                SLTNode::Mux {
                    cond: c,
                    then_expr: t,
                    else_expr: e,
                }
            }

            SLTNode::ForFold {
                loop_var,
                loop_width,
                loop_signed,
                start,
                end,
                inclusive,
                step,
                step_op,
                reverse,
                result,
                initials,
                updates,
                effects,
                continue_cond,
            } => {
                let map_bound =
                    |bound: &SLTLoopBound,
                     cache: &mut HashMap<NodeId, NodeId>,
                     target_arena: &mut SLTNodeArena<B>|
                     -> SLTLoopBound {
                        match bound {
                            SLTLoopBound::Const(v) => SLTLoopBound::Const(*v),
                            SLTLoopBound::Expr(node) => SLTLoopBound::Expr(
                                arena
                                    .get(*node)
                                    .map_addr(*node, arena, target_arena, cache, f),
                            ),
                        }
                    };
                let mapped_initials = initials
                    .iter()
                    .map(|update| SLTForUpdate {
                        target: VarAtomBase::new(
                            f(&update.target.id),
                            update.target.access.lsb,
                            update.target.access.msb,
                        ),
                        expr: arena.get(update.expr).map_addr(
                            update.expr,
                            arena,
                            target_arena,
                            cache,
                            f,
                        ),
                    })
                    .collect();
                let mapped_updates = updates
                    .iter()
                    .map(|update| SLTForUpdate {
                        target: VarAtomBase::new(
                            f(&update.target.id),
                            update.target.access.lsb,
                            update.target.access.msb,
                        ),
                        expr: arena.get(update.expr).map_addr(
                            update.expr,
                            arena,
                            target_arena,
                            cache,
                            f,
                        ),
                    })
                    .collect();
                let mapped_effects = effects
                    .iter()
                    .map(|effect| SLTForEffect {
                        site_id: effect.site_id,
                        guard: effect.guard.map(|guard| {
                            arena
                                .get(guard)
                                .map_addr(guard, arena, target_arena, cache, f)
                        }),
                        emit_on_true: effect.emit_on_true,
                        args: effect
                            .args
                            .iter()
                            .map(|arg| {
                                arena
                                    .get(*arg)
                                    .map_addr(*arg, arena, target_arena, cache, f)
                            })
                            .collect(),
                        fatal_error_code: effect.fatal_error_code,
                    })
                    .collect();
                SLTNode::ForFold {
                    loop_var: f(loop_var),
                    loop_width: *loop_width,
                    loop_signed: *loop_signed,
                    start: map_bound(start, cache, target_arena),
                    end: map_bound(end, cache, target_arena),
                    inclusive: *inclusive,
                    step: *step,
                    step_op: *step_op,
                    reverse: *reverse,
                    result: VarAtomBase::new(f(&result.id), result.access.lsb, result.access.msb),
                    initials: mapped_initials,
                    updates: mapped_updates,
                    effects: mapped_effects,
                    continue_cond: arena.get(*continue_cond).map_addr(
                        *continue_cond,
                        arena,
                        target_arena,
                        cache,
                        f,
                    ),
                }
            }

            SLTNode::Concat(parts) => {
                let mapped_parts = parts
                    .iter()
                    .map(|(node, width)| {
                        (
                            arena
                                .get(*node)
                                .map_addr(*node, arena, target_arena, cache, f),
                            *width,
                        )
                    })
                    .collect();
                SLTNode::Concat(mapped_parts)
            }

            SLTNode::Slice { expr, access } => {
                let e = arena
                    .get(*expr)
                    .map_addr(*expr, arena, target_arena, cache, f);
                SLTNode::Slice {
                    expr: e,
                    access: *access,
                }
            }
        };
        let new_id = target_arena.alloc(new_node);
        cache.insert(id, new_id);
        new_id
    }
}

#[cfg(test)]
mod tests {
    use super::{NodeId, SLTNode, SLTNodeArena};
    use num_bigint::BigUint;

    fn constant(value: u8) -> SLTNode<u32> {
        SLTNode::Constant(BigUint::from(value), BigUint::from(0u8), 8, false)
    }

    #[test]
    fn rebuild_cache_uses_first_duplicate_node_id() {
        let duplicate = constant(7);
        let mut arena = SLTNodeArena {
            nodes: vec![constant(1), duplicate.clone(), duplicate.clone()],
            cache: crate::HashMap::default(),
        };
        arena.cache.insert(duplicate.clone(), NodeId(2));

        arena.rebuild_cache();

        assert_eq!(arena.cache.get(&duplicate), Some(&NodeId(1)));
        let node_count = arena.nodes.len();
        assert_eq!(arena.alloc(duplicate), NodeId(1));
        assert_eq!(arena.nodes.len(), node_count);
    }

    #[test]
    fn json_roundtrip_rebuilds_cache_with_minimum_node_id() {
        let duplicate = constant(9);
        let arena = SLTNodeArena {
            nodes: vec![constant(2), duplicate.clone(), duplicate.clone()],
            cache: crate::HashMap::default(),
        };

        let json = serde_json::to_string(&arena).unwrap();
        let mut decoded: SLTNodeArena<u32> = serde_json::from_str(&json).unwrap();
        let node_count = decoded.nodes.len();

        assert_eq!(decoded.alloc(duplicate), NodeId(1));
        assert_eq!(decoded.nodes.len(), node_count);
    }
}

/// Display implementation for SLTNode - provides human-readable tree structure
///
/// This implementation formats the Signal Logic Tree (SLT) as a hierarchical ASCII tree,
/// making it easy to visualize the expression structure. Each node type displays relevant
/// information:
///
/// - **Input**: Shows variable ID, dynamic indices (if any), and bit range [lsb:msb]
/// - **Constant**: Displays the value in hexadecimal and width in bits
/// - **Binary**: Shows the operation and recursively formats both operands with indentation
/// - **Unary**: Shows the operation and recursively formats the inner expression
/// - **Mux**: Displays condition, then-branch, and else-branch with clear labels
/// - **Concat**: Lists concatenated parts with their widths
/// - **Slice**: Shows the bit range extraction with the inner expression
///
/// # Example
///
/// A binary expression `a + b` would display as:
/// ```text
/// Binary(Add)
///   Const(0x1, 32bits)
///   Const(0x2, 32bits)
/// ```
///
/// A more complex expression `(a + b) * (c - d)` would show:
/// ```text
/// Binary(Mul)
///   Binary(Add)
///     Const(0x1, 32bits)
///     Const(0x2, 32bits)
///   Binary(Sub)
///     Const(0x3, 32bits)
///     Const(0x4, 32bits)
/// ```
impl<A: fmt::Debug + fmt::Display + Hash + Eq + Clone> SLTNode<A> {
    pub fn fmt_display(&self, f: &mut fmt::Formatter<'_>, arena: &SLTNodeArena<A>) -> fmt::Result {
        self.fmt_recursive(f, 0, arena)
    }
}

impl<A: fmt::Debug + fmt::Display + Hash + Eq + Clone> SLTNode<A> {
    fn fmt_recursive(
        &self,
        f: &mut fmt::Formatter<'_>,
        depth: usize,
        arena: &SLTNodeArena<A>,
    ) -> fmt::Result {
        let indent = "  ".repeat(depth);
        let child_indent = "  ".repeat(depth + 1);
        match self {
            SLTNode::Input {
                variable,
                index,
                access,
                ..
            } => {
                write!(f, "{}Input({:?}", indent, variable)?;
                if !index.is_empty() {
                    write!(f, "[")?;
                    for (i, idx) in index.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "n{}:...", idx.node.0)?;
                        if idx.stride > 1 {
                            write!(f, "*{}", idx.stride)?;
                        }
                    }
                    write!(f, "]")?;
                }
                write!(f, "[{}:{}]", access.lsb, access.msb)?;
                write!(f, ")")
            }
            SLTNode::Constant(val, _mask, width, _signed) => {
                write!(f, "{}Const({:#x}, {}bits)", indent, val, width)
            }
            SLTNode::Binary(lhs, op, rhs) => {
                let op_str = format!("{:?}", op); // Just use Debug for simplicity
                writeln!(f, "{}Binary({})", indent, op_str)?;
                arena.get(*lhs).fmt_recursive(f, depth + 1, arena)?;
                writeln!(f)?; // Insert empty line between left and right expressions
                arena.get(*rhs).fmt_recursive(f, depth + 1, arena)
            }
            SLTNode::Unary(op, inner) => {
                writeln!(f, "{}Unary({:?})", indent, op)?;
                arena.get(*inner).fmt_recursive(f, depth + 1, arena)
            }
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                writeln!(f, "{}Mux", indent)?;
                writeln!(f, "{}cond:", child_indent)?;
                arena.get(*cond).fmt_recursive(f, depth + 2, arena)?;
                writeln!(f, "\n{}then:", child_indent)?;
                arena.get(*then_expr).fmt_recursive(f, depth + 2, arena)?;
                writeln!(f, "\n{}else:", child_indent)?;
                arena.get(*else_expr).fmt_recursive(f, depth + 2, arena)
            }
            SLTNode::ForFold {
                loop_var,
                loop_width,
                loop_signed,
                start,
                end,
                inclusive,
                step,
                step_op,
                reverse,
                result,
                initials,
                updates,
                effects,
                continue_cond,
            } => {
                writeln!(
                    f,
                    "{}ForFold(loop_var={}, width={}, signed={}, inclusive={}, step={}, step_op={:?}, reverse={}, result={})",
                    indent,
                    loop_var,
                    loop_width,
                    loop_signed,
                    inclusive,
                    step,
                    step_op,
                    reverse,
                    result
                )?;
                writeln!(f, "{}start: {:?}", child_indent, start)?;
                writeln!(f, "{}end: {:?}", child_indent, end)?;
                for init in initials {
                    writeln!(f, "{}init {}:", child_indent, init.target)?;
                    arena.get(init.expr).fmt_recursive(f, depth + 2, arena)?;
                    writeln!(f)?;
                }
                for update in updates {
                    writeln!(f, "{}update {}:", child_indent, update.target)?;
                    arena.get(update.expr).fmt_recursive(f, depth + 2, arena)?;
                    writeln!(f)?;
                }
                for effect in effects {
                    writeln!(f, "{}effect site={}:", child_indent, effect.site_id)?;
                    if let Some(guard) = effect.guard {
                        writeln!(f, "{}guard:", child_indent)?;
                        arena.get(guard).fmt_recursive(f, depth + 2, arena)?;
                        writeln!(f)?;
                    }
                    for arg in &effect.args {
                        writeln!(f, "{}arg:", child_indent)?;
                        arena.get(*arg).fmt_recursive(f, depth + 2, arena)?;
                        writeln!(f)?;
                    }
                }
                writeln!(f, "{}continue:", child_indent)?;
                arena
                    .get(*continue_cond)
                    .fmt_recursive(f, depth + 2, arena)?;
                Ok(())
            }
            SLTNode::Concat(parts) => {
                writeln!(f, "{}Concat", indent)?;
                for (i, (part, width)) in parts.iter().enumerate() {
                    if i > 0 {
                        writeln!(f)?;
                    }
                    writeln!(f, "{}[{}bits]:", child_indent, width)?;
                    arena.get(*part).fmt_recursive(f, depth + 2, arena)?;
                }
                Ok(())
            }
            SLTNode::Slice { expr, access } => {
                writeln!(f, "{}Slice[{}:{}]", indent, access.lsb, access.msb)?;
                arena.get(*expr).fmt_recursive(f, depth + 1, arena)
            }
        }
    }
}
