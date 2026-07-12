use std::{fmt, hash::Hash};

use num_bigint::{BigInt, BigUint};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use super::node_facts::{SLTNodeFactsError, verify_append, verify_raw_nodes};

use crate::{
    HashMap,
    ir::{BinaryOp, BitAccess, UnaryOp, VarAtomBase},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NodeId(pub usize);

/// Failure from a narrowly scoped mutation of a construction arena.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SLTNodeArenaEditError {
    RangeOutOfBounds {
        start: usize,
        end: usize,
        node_count: usize,
    },
    SiteIdOverflow {
        site_id: u32,
        offset: u32,
    },
    StorageUnavailable {
        effect_count: usize,
    },
    EffectCountOverflow,
    EditPlanMismatch,
}

impl fmt::Display for SLTNodeArenaEditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RangeOutOfBounds {
                start,
                end,
                node_count,
            } => write!(
                formatter,
                "SLT edit range {start}..{end} is outside arena length {node_count}"
            ),
            Self::SiteIdOverflow { site_id, offset } => write!(
                formatter,
                "ForFold runtime-event site {site_id} plus offset {offset} overflows u32"
            ),
            Self::StorageUnavailable { effect_count } => write!(
                formatter,
                "cannot reserve {effect_count} ForFold runtime-event edits"
            ),
            Self::EffectCountOverflow => {
                write!(
                    formatter,
                    "ForFold runtime-event effect count overflows usize"
                )
            }
            Self::EditPlanMismatch => {
                write!(
                    formatter,
                    "ForFold runtime-event edit plan no longer matches the arena"
                )
            }
        }
    }
}

impl std::error::Error for SLTNodeArenaEditError {}

#[derive(Debug, Clone, Serialize)]
#[serde(bound(serialize = "A: Serialize + std::hash::Hash + Eq + Clone"))]
pub struct SLTNodeArena<A: Hash + Eq + Clone> {
    nodes: Vec<SLTNode<A>>,
    #[serde(skip)]
    cache: crate::HashMap<SLTNode<A>, NodeId>,
    /// Locally derived construction widths in stable [`NodeId`] order.
    /// Full verification recomputes these independently from `nodes`.
    #[serde(skip)]
    widths: Vec<usize>,
}

#[derive(Serialize, Deserialize)]
#[serde(bound(
    serialize = "A: Serialize + std::hash::Hash + Eq + Clone",
    deserialize = "A: Deserialize<'de> + std::hash::Hash + Eq + Clone"
))]
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
        Self::from_raw_nodes(wire.nodes).map_err(D::Error::custom)
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
            widths: Vec::new(),
        }
    }

    pub fn alloc(&mut self, node: SLTNode<A>) -> Result<NodeId, SLTNodeFactsError>
    where
        A: Hash + Eq + Clone,
    {
        if let Some(id) = self.cache.get(&node) {
            return Ok(*id);
        }
        let width = verify_append(&node, &self.widths)?;
        let id = NodeId(self.nodes.len());
        self.cache.insert(node.clone(), id);
        self.nodes.push(node);
        self.widths.push(width);
        debug_assert_eq!(self.nodes.len(), self.widths.len());
        Ok(id)
    }

    /// Return a construction-time width computed once when `id` was interned.
    pub(super) fn width(&self, id: NodeId) -> Option<usize> {
        self.widths.get(id.0).copied()
    }

    pub(super) fn nodes(&self) -> &[SLTNode<A>] {
        &self.nodes
    }

    pub(super) fn cached_widths(&self) -> &[usize] {
        &self.widths
    }

    /// Return the number of nodes in stable [`NodeId`] order.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Return whether the arena contains no nodes.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Iterate over nodes in stable [`NodeId`] order.
    pub fn iter(&self) -> std::slice::Iter<'_, SLTNode<A>> {
        self.nodes.iter()
    }

    /// Look up a node without trusting that an externally supplied ID exists.
    pub fn get_checked(&self, id: NodeId) -> Option<&SLTNode<A>> {
        self.nodes.get(id.0)
    }

    /// Rewrite only the runtime-event identity carried by `ForFold` effects.
    ///
    /// The semantic interning cache is rebuilt internally whenever the rewrite
    /// changes a node, so callers cannot leave node storage and cache identity
    /// out of sync. `None` leaves an effect unchanged.
    pub(crate) fn remap_for_fold_effect_sites(
        &mut self,
        range: std::ops::Range<usize>,
        mut remap: impl FnMut(
            u32,
            Option<i64>,
        ) -> Result<Option<(u32, Option<i64>)>, SLTNodeArenaEditError>,
    ) -> Result<(), SLTNodeArenaEditError> {
        let node_count = self.nodes.len();
        let start = range.start;
        let end = range.end;
        let Some(nodes) = self.nodes.get(range.clone()) else {
            return Err(SLTNodeArenaEditError::RangeOutOfBounds {
                start,
                end,
                node_count,
            });
        };
        let effect_count = nodes.iter().try_fold(0usize, |count, node| {
            let effects = match node {
                SLTNode::ForFold { effects, .. } => effects.len(),
                _ => 0,
            };
            count.checked_add(effects)
        });
        let Some(effect_count) = effect_count else {
            return Err(SLTNodeArenaEditError::EffectCountOverflow);
        };
        let mut edits = Vec::new();
        edits
            .try_reserve_exact(effect_count)
            .map_err(|_| SLTNodeArenaEditError::StorageUnavailable { effect_count })?;
        for (node_index, node) in nodes.iter().enumerate() {
            let SLTNode::ForFold { effects, .. } = node else {
                continue;
            };
            for (effect_index, effect) in effects.iter().enumerate() {
                let Some((site_id, fatal_error_code)) =
                    remap(effect.site_id, effect.fatal_error_code)?
                else {
                    continue;
                };
                if effect.site_id != site_id || effect.fatal_error_code != fatal_error_code {
                    edits.push((node_index, effect_index, site_id, fatal_error_code));
                }
            }
        }
        if edits.is_empty() {
            return Ok(());
        }
        let Some(nodes) = self.nodes.get_mut(range) else {
            return Err(SLTNodeArenaEditError::RangeOutOfBounds {
                start,
                end,
                node_count,
            });
        };
        if edits.iter().any(|&(node_index, effect_index, _, _)| {
            !matches!(
                nodes.get(node_index),
                Some(SLTNode::ForFold { effects, .. }) if effect_index < effects.len()
            )
        }) {
            return Err(SLTNodeArenaEditError::EditPlanMismatch);
        }
        for (node_index, effect_index, site_id, fatal_error_code) in edits {
            if let Some(SLTNode::ForFold { effects, .. }) = nodes.get_mut(node_index)
                && let Some(effect) = effects.get_mut(effect_index)
            {
                effect.site_id = site_id;
                effect.fatal_error_code = fatal_error_code;
            }
        }
        self.rebuild_cache();
        Ok(())
    }

    /// Rebuild the derived interning cache from the persistent node list.
    ///
    /// If the list already contains duplicate nodes, the smallest (and therefore
    /// first) [`NodeId`] is retained as the canonical identity.
    fn rebuild_cache(&mut self) {
        self.cache.clear();
        for (idx, node) in self.nodes.iter().cloned().enumerate() {
            self.cache.entry(node).or_insert(NodeId(idx));
        }
        debug_assert_eq!(self.nodes.len(), self.widths.len());
    }

    pub fn get(&self, id: NodeId) -> &SLTNode<A> {
        &self.nodes[id.0]
    }

    pub fn display(&self, id: NodeId) -> NodeDisplay<'_, A> {
        NodeDisplay { arena: self, id }
    }

    fn from_raw_nodes(nodes: Vec<SLTNode<A>>) -> Result<Self, SLTNodeFactsError> {
        let widths = verify_raw_nodes(&nodes)?;
        let mut arena = Self {
            nodes,
            cache: crate::HashMap::default(),
            widths,
        };
        arena.rebuild_cache();
        Ok(arena)
    }

    /// Verify raw node storage for tests without exposing it to production
    /// construction paths.
    #[cfg(test)]
    pub(crate) fn try_from_nodes(nodes: Vec<SLTNode<A>>) -> Result<Self, SLTNodeFactsError> {
        Self::from_raw_nodes(nodes)
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
                    crate::ir::BinaryOp::DivU | crate::ir::BinaryOp::DivS => "/",
                    crate::ir::BinaryOp::RemU | crate::ir::BinaryOp::RemS => "%",
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
                    crate::ir::UnaryOp::PopCount => "popcount",
                    crate::ir::UnaryOp::CountLeadingZeros => "clz",
                    crate::ir::UnaryOp::CountTrailingZeros => "ctz",
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
            SLTNode::ForFoldGroup {
                loop_var,
                loop_width,
                loop_signed,
                start,
                step,
                trip_count,
                entry_guard,
                states,
            } => {
                write!(
                    f,
                    "fold_group {loop_var}:{loop_width}{} = {start} step {step} count {trip_count} if n{}:",
                    if *loop_signed { "s" } else { "u" },
                    entry_guard.0,
                )?;
                arena.get(*entry_guard).fmt_expression(f, arena)?;
                write!(f, " [")?;
                for (index, state) in states.iter().enumerate() {
                    if index > 0 {
                        write!(f, "; ")?;
                    }
                    write!(f, "{} = n{}:", state.target, state.initial.0,)?;
                    arena.get(state.initial).fmt_expression(f, arena)?;
                    write!(f, " -> n{}:", state.update.0)?;
                    arena.get(state.update).fmt_expression(f, arena)?;
                }
                write!(f, "]")
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

/// One loop-carried state in a grouped fixed-trip-count fold.
///
/// `initial` is evaluated before entering the loop and `update` is evaluated
/// once per iteration with all state targets bound to the previous iteration's
/// values.  A [`SLTNode::ForFoldGroup`] packs the final values in `states`
/// order, with the first state occupying the most-significant bits.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(bound(
    serialize = "A: Serialize + std::hash::Hash + Eq + Clone",
    deserialize = "A: Deserialize<'de> + std::hash::Hash + Eq + Clone"
))]
pub struct SLTForFoldGroupState<A: Hash + Eq + Clone> {
    pub target: VarAtomBase<A>,
    pub initial: NodeId,
    pub update: NodeId,
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
    /// A fixed-trip-count fold carrying multiple state values and returning
    /// their final values as one packed result.
    ForFoldGroup {
        loop_var: A,
        loop_width: usize,
        loop_signed: bool,
        start: BigInt,
        step: BigInt,
        trip_count: usize,
        entry_guard: NodeId,
        states: Vec<SLTForFoldGroupState<A>>,
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
    ForFoldGroup {
        loop_var: A,
        loop_width: usize,
        loop_signed: bool,
        /// Signed two's-complement, little-endian representation.
        start: Vec<u8>,
        /// Signed two's-complement, little-endian representation.
        step: Vec<u8>,
        trip_count: usize,
        entry_guard: NodeId,
        states: Vec<SLTForFoldGroupState<A>>,
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
            SLTNode::ForFoldGroup {
                loop_var,
                loop_width,
                loop_signed,
                start,
                step,
                trip_count,
                entry_guard,
                states,
            } => SLTNodeSerde::ForFoldGroup {
                loop_var,
                loop_width,
                loop_signed,
                start: start.to_signed_bytes_le(),
                step: step.to_signed_bytes_le(),
                trip_count,
                entry_guard,
                states,
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
            SLTNodeSerde::ForFoldGroup {
                loop_var,
                loop_width,
                loop_signed,
                start,
                step,
                trip_count,
                entry_guard,
                states,
            } => SLTNode::ForFoldGroup {
                loop_var,
                loop_width,
                loop_signed,
                start: BigInt::from_signed_bytes_le(&start),
                step: BigInt::from_signed_bytes_le(&step),
                trip_count,
                entry_guard,
                states,
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
    ) -> Result<NodeId, SLTNodeFactsError>
    where
        A: Hash + Eq + Clone,
        B: Hash + Eq + Clone,
        F: Fn(&A) -> B,
    {
        if let Some(mapped_id) = cache.get(&id) {
            return Ok(*mapped_id);
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
                    .map(|idx| {
                        Ok(SLTIndex {
                            node: arena.get(idx.node).map_addr(
                                idx.node,
                                arena,
                                target_arena,
                                cache,
                                f,
                            )?,
                            stride: idx.stride,
                        })
                    })
                    .collect::<Result<Vec<_>, SLTNodeFactsError>>()?;
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
                    .map_addr(*lhs, arena, target_arena, cache, f)?;
                let r = arena
                    .get(*rhs)
                    .map_addr(*rhs, arena, target_arena, cache, f)?;
                SLTNode::Binary(l, *op, r)
            }

            SLTNode::Unary(op, inner) => {
                let i = arena
                    .get(*inner)
                    .map_addr(*inner, arena, target_arena, cache, f)?;
                SLTNode::Unary(*op, i)
            }

            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                let c = arena
                    .get(*cond)
                    .map_addr(*cond, arena, target_arena, cache, f)?;
                let t =
                    arena
                        .get(*then_expr)
                        .map_addr(*then_expr, arena, target_arena, cache, f)?;
                let e =
                    arena
                        .get(*else_expr)
                        .map_addr(*else_expr, arena, target_arena, cache, f)?;
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
                let map_bound = |bound: &SLTLoopBound,
                                 cache: &mut HashMap<NodeId, NodeId>,
                                 target_arena: &mut SLTNodeArena<B>|
                 -> Result<SLTLoopBound, SLTNodeFactsError> {
                    match bound {
                        SLTLoopBound::Const(v) => Ok(SLTLoopBound::Const(*v)),
                        SLTLoopBound::Expr(node) => Ok(SLTLoopBound::Expr(
                            arena
                                .get(*node)
                                .map_addr(*node, arena, target_arena, cache, f)?,
                        )),
                    }
                };
                let mapped_initials = initials
                    .iter()
                    .map(|update| {
                        Ok(SLTForUpdate {
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
                            )?,
                        })
                    })
                    .collect::<Result<Vec<_>, SLTNodeFactsError>>()?;
                let mapped_updates = updates
                    .iter()
                    .map(|update| {
                        Ok(SLTForUpdate {
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
                            )?,
                        })
                    })
                    .collect::<Result<Vec<_>, SLTNodeFactsError>>()?;
                let mapped_effects = effects
                    .iter()
                    .map(|effect| {
                        let guard = effect
                            .guard
                            .map(|guard| {
                                arena
                                    .get(guard)
                                    .map_addr(guard, arena, target_arena, cache, f)
                            })
                            .transpose()?;
                        let args = effect
                            .args
                            .iter()
                            .map(|arg| {
                                arena
                                    .get(*arg)
                                    .map_addr(*arg, arena, target_arena, cache, f)
                            })
                            .collect::<Result<Vec<_>, SLTNodeFactsError>>()?;
                        Ok(SLTForEffect {
                            site_id: effect.site_id,
                            guard,
                            emit_on_true: effect.emit_on_true,
                            args,
                            fatal_error_code: effect.fatal_error_code,
                        })
                    })
                    .collect::<Result<Vec<_>, SLTNodeFactsError>>()?;
                SLTNode::ForFold {
                    loop_var: f(loop_var),
                    loop_width: *loop_width,
                    loop_signed: *loop_signed,
                    start: map_bound(start, cache, target_arena)?,
                    end: map_bound(end, cache, target_arena)?,
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
                    )?,
                }
            }

            SLTNode::ForFoldGroup {
                loop_var,
                loop_width,
                loop_signed,
                start,
                step,
                trip_count,
                entry_guard,
                states,
            } => {
                let mapped_states = states
                    .iter()
                    .map(|state| {
                        Ok(SLTForFoldGroupState {
                            target: VarAtomBase::new(
                                f(&state.target.id),
                                state.target.access.lsb,
                                state.target.access.msb,
                            ),
                            initial: arena.get(state.initial).map_addr(
                                state.initial,
                                arena,
                                target_arena,
                                cache,
                                f,
                            )?,
                            update: arena.get(state.update).map_addr(
                                state.update,
                                arena,
                                target_arena,
                                cache,
                                f,
                            )?,
                        })
                    })
                    .collect::<Result<Vec<_>, SLTNodeFactsError>>()?;
                SLTNode::ForFoldGroup {
                    loop_var: f(loop_var),
                    loop_width: *loop_width,
                    loop_signed: *loop_signed,
                    start: start.clone(),
                    step: step.clone(),
                    trip_count: *trip_count,
                    entry_guard: arena.get(*entry_guard).map_addr(
                        *entry_guard,
                        arena,
                        target_arena,
                        cache,
                        f,
                    )?,
                    states: mapped_states,
                }
            }

            SLTNode::Concat(parts) => {
                let mapped_parts = parts
                    .iter()
                    .map(|(node, width)| {
                        Ok((
                            arena
                                .get(*node)
                                .map_addr(*node, arena, target_arena, cache, f)?,
                            *width,
                        ))
                    })
                    .collect::<Result<Vec<_>, SLTNodeFactsError>>()?;
                SLTNode::Concat(mapped_parts)
            }

            SLTNode::Slice { expr, access } => {
                let e = arena
                    .get(*expr)
                    .map_addr(*expr, arena, target_arena, cache, f)?;
                SLTNode::Slice {
                    expr: e,
                    access: *access,
                }
            }
        };
        let new_id = target_arena.alloc(new_node)?;
        cache.insert(id, new_id);
        Ok(new_id)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        NodeId, SLTForEffect, SLTForFoldGroupState, SLTIndex, SLTLoopBound, SLTNode, SLTNodeArena,
        SLTNodeArenaEditError, SLTNodeArenaWire, SLTStepOp,
    };
    use crate::{
        ir::{BinaryOp, BitAccess, UnaryOp, VarAtomBase},
        logic_tree::comb::{expr::get_width, node_facts::SLTNodeFacts},
    };
    use num_bigint::{BigInt, BigUint};

    fn constant(value: u8) -> SLTNode<u32> {
        SLTNode::Constant(BigUint::from(value), BigUint::from(0u8), 8, false)
    }

    #[test]
    fn rebuild_cache_uses_first_duplicate_node_id() {
        let duplicate = constant(7);
        let mut arena =
            SLTNodeArena::try_from_nodes(vec![constant(1), duplicate.clone(), duplicate.clone()])
                .unwrap();
        arena.cache.insert(duplicate.clone(), NodeId(2));

        arena.rebuild_cache();

        assert_eq!(arena.cache.get(&duplicate), Some(&NodeId(1)));
        let node_count = arena.len();
        assert_eq!(arena.alloc(duplicate).unwrap(), NodeId(1));
        assert_eq!(arena.len(), node_count);
    }

    #[test]
    fn json_roundtrip_rebuilds_cache_with_minimum_node_id() {
        let duplicate = constant(9);
        let arena =
            SLTNodeArena::try_from_nodes(vec![constant(2), duplicate.clone(), duplicate.clone()])
                .unwrap();

        let json = serde_json::to_string(&arena).unwrap();
        let mut decoded: SLTNodeArena<u32> = serde_json::from_str(&json).unwrap();
        let node_count = decoded.len();

        assert_eq!(decoded.alloc(duplicate).unwrap(), NodeId(1));
        assert_eq!(decoded.len(), node_count);
        assert_eq!(decoded.width(NodeId(1)), Some(8));
        assert_eq!(decoded.widths.len(), decoded.nodes.len());
    }

    fn arena_with_for_fold_group() -> (SLTNodeArena<u32>, NodeId) {
        let mut arena = SLTNodeArena::new();
        let entry_guard = arena
            .alloc(SLTNode::Constant(
                BigUint::from(1u8),
                BigUint::from(0u8),
                1,
                false,
            ))
            .unwrap();
        let initial_wide = arena.alloc(constant(3)).unwrap();
        let update_wide = arena.alloc(constant(4)).unwrap();
        let initial_narrow = arena
            .alloc(SLTNode::Constant(
                BigUint::from(1u8),
                BigUint::from(0u8),
                4,
                false,
            ))
            .unwrap();
        let update_narrow = arena
            .alloc(SLTNode::Constant(
                BigUint::from(2u8),
                BigUint::from(0u8),
                4,
                false,
            ))
            .unwrap();
        let group = arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var: 7,
                loop_width: 8,
                loop_signed: true,
                start: BigInt::from(-4),
                step: BigInt::from(2),
                trip_count: 3,
                entry_guard,
                states: vec![
                    SLTForFoldGroupState {
                        target: VarAtomBase::new(8, 0, 7),
                        initial: initial_wide,
                        update: update_wide,
                    },
                    SLTForFoldGroupState {
                        target: VarAtomBase::new(9, 4, 7),
                        initial: initial_narrow,
                        update: update_narrow,
                    },
                ],
            })
            .unwrap();
        (arena, group)
    }

    #[test]
    fn for_fold_group_json_roundtrip_preserves_signed_iteration_and_width() {
        let (arena, group) = arena_with_for_fold_group();
        let facts = SLTNodeFacts::verify(&arena).expect("valid ForFoldGroup must verify");
        assert_eq!(facts.width(group), Some(12));

        let json = serde_json::to_string(&arena).unwrap();
        let decoded: SLTNodeArena<u32> = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded, arena);
        assert_eq!(decoded.width(group), Some(12));
        assert_eq!(
            SLTNodeFacts::verify(&decoded).unwrap().width(group),
            Some(12)
        );
    }

    #[test]
    fn for_fold_group_map_addr_maps_loop_and_state_targets_and_children() {
        let (arena, group) = arena_with_for_fold_group();
        let mut mapped_arena = SLTNodeArena::<u64>::new();
        let mut cache = crate::HashMap::default();

        let mapped_group = arena
            .get(group)
            .map_addr(group, &arena, &mut mapped_arena, &mut cache, &|address| {
                u64::from(*address) + 100
            })
            .unwrap();

        let SLTNode::ForFoldGroup {
            loop_var,
            start,
            step,
            entry_guard,
            states,
            ..
        } = mapped_arena.get(mapped_group)
        else {
            panic!("mapped node must remain ForFoldGroup");
        };
        assert_eq!(*loop_var, 107);
        assert_eq!(*start, BigInt::from(-4));
        assert_eq!(*step, BigInt::from(2));
        assert_eq!(states[0].target, VarAtomBase::new(108, 0, 7));
        assert_eq!(states[1].target, VarAtomBase::new(109, 4, 7));
        assert!(entry_guard.0 < mapped_group.0);
        assert!(
            states.iter().all(|state| {
                state.initial.0 < mapped_group.0 && state.update.0 < mapped_group.0
            })
        );
        assert_eq!(
            SLTNodeFacts::verify(&mapped_arena)
                .unwrap()
                .width(mapped_group),
            Some(12)
        );
    }

    #[test]
    fn default_allocation_and_clone_preserve_width_cache() {
        let mut arena = SLTNodeArena::<u32>::default();
        assert!(arena.is_empty());
        assert!(arena.widths.is_empty());

        let narrow = arena
            .alloc(SLTNode::Constant(
                BigUint::from(3u8),
                BigUint::from(0u8),
                2,
                false,
            ))
            .unwrap();
        let wide = arena.alloc(constant(7)).unwrap();
        let sum = arena
            .alloc(SLTNode::Binary(narrow, BinaryOp::Add, wide))
            .unwrap();
        assert_eq!(get_width(sum, &arena), 8);
        assert_eq!(arena.widths.len(), arena.nodes.len());

        let facts = SLTNodeFacts::verify(&arena).expect("allocated arena must verify");
        for index in 0..arena.len() {
            let id = NodeId(index);
            assert_eq!(arena.width(id), facts.width(id));
        }

        let mut cloned = arena.clone();
        assert_eq!(cloned.widths, arena.widths);
        assert_eq!(get_width(sum, &cloned), 8);
        let node_count = cloned.len();
        assert_eq!(cloned.alloc(arena.get(sum).clone()).unwrap(), sum);
        assert_eq!(cloned.len(), node_count);
    }

    #[test]
    fn get_width_does_not_rewalk_a_shared_mux_dag() {
        const DEPTH: usize = 64;

        let mut arena = SLTNodeArena::<u32>::new();
        let cond = arena
            .alloc(SLTNode::Constant(
                BigUint::from(1u8),
                BigUint::from(0u8),
                1,
                false,
            ))
            .unwrap();
        let mut value = arena.alloc(constant(0)).unwrap();
        for _ in 0..DEPTH {
            let then_expr = arena.alloc(SLTNode::Unary(UnaryOp::Ident, value)).unwrap();
            let else_expr = arena.alloc(SLTNode::Unary(UnaryOp::BitNot, value)).unwrap();
            value = arena
                .alloc(SLTNode::Mux {
                    cond,
                    then_expr,
                    else_expr,
                })
                .unwrap();
        }

        // Recursively walking both arms revisits the shared predecessor twice
        // per level. A construction-time fact lookup is independent of that
        // exponential number of graph paths.
        assert_eq!(get_width(value, &arena), 8);
        assert_eq!(arena.width(value), Some(8));
        assert_eq!(arena.widths.len(), arena.nodes.len());
    }

    #[test]
    fn failed_append_does_not_mutate_arena_or_caches() {
        let mut arena = SLTNodeArena::<u32>::new();
        let valid = arena.alloc(constant(1)).unwrap();
        let nodes_before = arena.nodes.clone();
        let widths_before = arena.widths.clone();
        let cache_before = arena.cache.clone();

        let error = arena
            .alloc(SLTNode::Binary(valid, BinaryOp::Add, NodeId(99)))
            .unwrap_err();

        assert_eq!(error.invariant, "GRAPH.CHILD_EXISTS");
        assert_eq!(arena.nodes, nodes_before);
        assert_eq!(arena.widths, widths_before);
        assert_eq!(arena.cache, cache_before);

        let error = arena
            .alloc(SLTNode::Concat(vec![(valid, usize::MAX), (valid, 1)]))
            .unwrap_err();
        assert_eq!(error.invariant, "WIDTH.CONCAT_REPRESENTABLE");
        assert_eq!(arena.nodes, nodes_before);
        assert_eq!(arena.widths, widths_before);
        assert_eq!(arena.cache, cache_before);
    }

    #[test]
    fn append_derives_width_but_defers_semantic_rules_to_full_verifier() {
        let mut arena = SLTNodeArena::<u32>::new();
        let oversized = arena
            .alloc(SLTNode::Constant(
                BigUint::from(0x10u8),
                BigUint::from(0u8),
                4,
                false,
            ))
            .expect("declared width is locally derivable");

        assert_eq!(arena.width(oversized), Some(4));
        assert_eq!(
            SLTNodeFacts::verify(&arena).unwrap_err().invariant,
            "CONSTANT.VALUE_FITS_WIDTH"
        );
    }

    #[test]
    fn full_verifier_rejects_a_divergent_construction_width_cache() {
        let mut arena = SLTNodeArena::<u32>::new();
        arena.alloc(constant(1)).unwrap();
        arena.widths[0] = 7;

        assert_eq!(
            SLTNodeFacts::verify(&arena).unwrap_err().invariant,
            "FACTS.CACHED_WIDTH_MATCHES"
        );
    }

    #[test]
    fn zero_width_coercion_is_a_structured_error() {
        let mut arena = SLTNodeArena::<u32>::new();
        let zero = arena
            .alloc(SLTNode::Constant(
                BigUint::from(0u8),
                BigUint::from(0u8),
                0,
                false,
            ))
            .unwrap();
        let nonzero = arena.alloc(constant(1)).unwrap();

        assert_eq!(
            crate::logic_tree::coerce_node_width(&mut arena, zero, Some(8), true)
                .unwrap_err()
                .invariant,
            "WIDTH.COERCE_SOURCE_NON_ZERO"
        );
        assert_eq!(
            crate::logic_tree::coerce_node_width(&mut arena, nonzero, Some(0), false)
                .unwrap_err()
                .invariant,
            "WIDTH.COERCE_TARGET_NON_ZERO"
        );
    }

    #[test]
    fn json_deserialization_rejects_noncanonical_graphs() {
        let wire = SLTNodeArenaWire {
            nodes: vec![
                SLTNode::Binary(NodeId(1), BinaryOp::Add, NodeId(1)),
                constant(1),
            ],
        };
        let json = serde_json::to_string(&wire).unwrap();

        let error = serde_json::from_str::<SLTNodeArena<u32>>(&json).unwrap_err();

        assert!(error.to_string().contains("GRAPH.CHILD_PRECEDES_OWNER"));
    }

    #[test]
    fn json_deserialization_checks_children_not_used_to_derive_width() {
        let cases = [
            vec![
                constant(1),
                SLTNode::Mux {
                    cond: NodeId(99),
                    then_expr: NodeId(0),
                    else_expr: NodeId(0),
                },
            ],
            vec![SLTNode::Input {
                variable: 1,
                signed: false,
                index: vec![SLTIndex {
                    node: NodeId(99),
                    stride: 1,
                }],
                access: BitAccess::new(0, 0),
            }],
        ];

        for nodes in cases {
            let json = serde_json::to_string(&SLTNodeArenaWire { nodes }).unwrap();
            let error = serde_json::from_str::<SLTNodeArena<u32>>(&json).unwrap_err();
            assert!(error.to_string().contains("GRAPH.CHILD_EXISTS"));
        }
    }

    #[test]
    fn for_fold_effect_remap_updates_only_the_requested_range() {
        let mut arena = SLTNodeArena::new();
        let condition = arena.alloc(constant(1)).unwrap();
        let first_fold = arena
            .alloc(SLTNode::ForFold {
                loop_var: 1,
                loop_width: 8,
                loop_signed: false,
                start: SLTLoopBound::Const(0),
                end: SLTLoopBound::Const(1),
                inclusive: false,
                step: 1,
                step_op: SLTStepOp::Add,
                reverse: false,
                result: VarAtomBase::new(2, 0, 7),
                initials: Vec::new(),
                updates: Vec::new(),
                effects: vec![SLTForEffect {
                    site_id: 3,
                    guard: None,
                    emit_on_true: true,
                    args: Vec::new(),
                    fatal_error_code: Some(3),
                }],
                continue_cond: condition,
            })
            .unwrap();
        let second_fold = arena
            .alloc(SLTNode::ForFold {
                loop_var: 1,
                loop_width: 8,
                loop_signed: false,
                start: SLTLoopBound::Const(0),
                end: SLTLoopBound::Const(1),
                inclusive: false,
                step: 1,
                step_op: SLTStepOp::Add,
                reverse: false,
                result: VarAtomBase::new(2, 0, 7),
                initials: Vec::new(),
                updates: Vec::new(),
                effects: vec![SLTForEffect {
                    site_id: 4,
                    guard: None,
                    emit_on_true: true,
                    args: Vec::new(),
                    fatal_error_code: None,
                }],
                continue_cond: condition,
            })
            .unwrap();

        arena
            .remap_for_fold_effect_sites(first_fold.0..second_fold.0, |site, fatal| {
                Ok(Some((site + 10, fatal.map(|_| 99))))
            })
            .expect("valid remap range must succeed");

        let SLTNode::ForFold { effects, .. } = arena.get(first_fold) else {
            panic!("expected first ForFold");
        };
        assert_eq!(effects[0].site_id, 13);
        assert_eq!(effects[0].fatal_error_code, Some(99));
        let remapped_first = arena.get(first_fold).clone();
        assert_eq!(arena.alloc(remapped_first).unwrap(), first_fold);
        let SLTNode::ForFold { effects, .. } = arena.get(second_fold) else {
            panic!("expected second ForFold");
        };
        assert_eq!(effects[0].site_id, 4);
        assert_eq!(effects[0].fatal_error_code, None);

        let error = arena
            .remap_for_fold_effect_sites(first_fold.0..second_fold.0 + 1, |site, fatal| {
                if site == 4 {
                    Err(SLTNodeArenaEditError::SiteIdOverflow {
                        site_id: site,
                        offset: u32::MAX,
                    })
                } else {
                    Ok(Some((site + 1, fatal)))
                }
            })
            .expect_err("failed remap must be reported");
        assert!(matches!(
            error,
            SLTNodeArenaEditError::SiteIdOverflow { .. }
        ));
        let SLTNode::ForFold { effects, .. } = arena.get(first_fold) else {
            panic!("expected first ForFold");
        };
        assert_eq!(effects[0].site_id, 13, "failed remap must be atomic");

        let error = arena
            .remap_for_fold_effect_sites(0..arena.len() + 1, |site, fatal| Ok(Some((site, fatal))))
            .expect_err("out-of-range remap must fail");
        assert!(matches!(
            error,
            SLTNodeArenaEditError::RangeOutOfBounds { .. }
        ));
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
            SLTNode::ForFoldGroup {
                loop_var,
                loop_width,
                loop_signed,
                start,
                step,
                trip_count,
                entry_guard,
                states,
            } => {
                writeln!(
                    f,
                    "{}ForFoldGroup(loop_var={}, width={}, signed={}, start={}, step={}, trip_count={})",
                    indent, loop_var, loop_width, loop_signed, start, step, trip_count,
                )?;
                writeln!(f, "{}entry guard:", child_indent)?;
                arena.get(*entry_guard).fmt_recursive(f, depth + 2, arena)?;
                writeln!(f)?;
                for state in states {
                    writeln!(f, "{}state {} initial:", child_indent, state.target)?;
                    arena
                        .get(state.initial)
                        .fmt_recursive(f, depth + 2, arena)?;
                    writeln!(f)?;
                    writeln!(f, "{}state {} update:", child_indent, state.target)?;
                    arena.get(state.update).fmt_recursive(f, depth + 2, arena)?;
                    writeln!(f)?;
                }
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
