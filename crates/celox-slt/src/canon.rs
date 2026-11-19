//! Canonical interning of SLT expressions and the shape measures of a
//! next-state DAG.
//!
//! [`SLTNodeArena::alloc`] interns nodes by their exact structure. Two
//! expressions that differ only in operand order, in how a slice or a
//! concatenation is built, or in an identity wrapper stay apart.
//! [`intern`] allocates a node in a normal form instead, so equal
//! expressions get one id by construction; [`Reinterner`] copies a DAG into
//! an arena through it. The normal form:
//!
//! - commutative operands are sorted (operands of equal width, or of a
//!   comparison, which extends both alike);
//! - `Ident` is its operand;
//! - a `Mux` whose arms are equal is that arm;
//! - a slice of a slice is one slice, a slice of an input a narrower input;
//! - a slice inside one part of a concatenation is a slice of that part;
//! - a full-width slice is its operand;
//! - nested concatenations are flattened; adjacent slices of one base,
//!   adjacent bits of one input and adjacent constants are merged;
//! - adjacent parts that compute the same bitwise operation over adjacent
//!   bits of the same operands are one wide operation, and adjacent `Mux`
//!   parts under one condition one wide `Mux` when one of its arms then
//!   joins.
//!
//! A rule never drops a node whose SIR lowering may give a signed register
//! or that the lowering extends as signed: the wrapper it would drop makes
//! the value unsigned for its users.
//!
//! [`Canonicalizer`] counts the canonical ids of a DAG built by plain
//! interning, the baseline [`intern`] is measured against.
//! [`same_op_bit_runs`] counts the 1-bit operations that a concatenation
//! lists over adjacent bits of the same operands, which one wide operation
//! would compute: the bit-blasting a next-state DAG must not keep.

use std::hash::Hash;

use celox_design::{BinaryOp, BitAccess, UnaryOp};

use num_bigint::BigUint;

use crate::lower::bound_signed;
use crate::{HashMap, HashSet, NodeId, SLTNode, SLTNodeArena, SLTNodeFactsError};

/// The canonical form of one node.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Key<A> {
    Input(A, usize, usize, bool, Vec<usize>),
    Constant(Vec<u64>, Vec<u64>, usize, bool),
    Binary(BinaryOp, usize, usize),
    Unary(UnaryOp, usize),
    Mux(usize, usize, usize),
    Slice(usize, usize, usize),
    Concat(Vec<(usize, usize)>),
    /// A node without a canonical form, such as a fold: itself.
    Opaque(usize),
}

/// Whether the operands of `op` over `lhs` and `rhs` may be swapped: a
/// comparison extends both alike; another operation extends its left
/// operand by its signedness and its right one with zeros, so only operands
/// of equal width commute.
fn commutes<A: Clone + Eq + Hash>(
    arena: &SLTNodeArena<A>,
    op: BinaryOp,
    lhs: NodeId,
    rhs: NodeId,
) -> bool {
    op.is_commutative()
        && (matches!(
            op,
            BinaryOp::Eq | BinaryOp::Ne | BinaryOp::EqCase | BinaryOp::NeCase
        ) || node_width(arena, lhs) == node_width(arena, rhs))
}

/// The canonical ids of the nodes of one arena.
pub struct Canonicalizer<'a, A: Clone + Eq + Hash> {
    arena: &'a SLTNodeArena<A>,
    keys: Vec<Key<A>>,
    /// A node of each canonical id, which tells how SIR lowering types it.
    representatives: Vec<NodeId>,
    table: HashMap<Key<A>, usize>,
    canonical: HashMap<NodeId, usize>,
}

impl<'a, A: Clone + Eq + Hash> Canonicalizer<'a, A> {
    pub fn new(arena: &'a SLTNodeArena<A>) -> Self {
        Self {
            arena,
            keys: Vec::new(),
            representatives: Vec::new(),
            table: HashMap::default(),
            canonical: HashMap::default(),
        }
    }

    /// The canonical id of `key`, which `node` computes.
    fn intern(&mut self, key: Key<A>, node: NodeId) -> usize {
        if let Some(&id) = self.table.get(&key) {
            return id;
        }
        let id = self.keys.len();
        self.keys.push(key.clone());
        self.representatives.push(node);
        self.table.insert(key, id);
        id
    }

    /// Whether the nodes of canonical id `id` are unsigned to their users,
    /// so a wrapper that makes them unsigned can be dropped (see
    /// [`unsigned`]).
    fn unsigned(&self, id: usize) -> bool {
        unsigned(self.arena, self.representatives[id])
    }

    fn width(&self, node: NodeId) -> usize {
        self.arena.node_width(node).unwrap_or(0)
    }

    /// The canonical id of `node`, computing those of its operands first.
    pub fn canonical(&mut self, node: NodeId) -> usize {
        if let Some(&id) = self.canonical.get(&node) {
            return id;
        }
        // Operands before users, without recursion.
        let mut stack = vec![(node, false)];
        while let Some((current, expanded)) = stack.pop() {
            if self.canonical.contains_key(&current) {
                continue;
            }
            if !expanded {
                stack.push((current, true));
                stack.extend(
                    operands(self.arena.get(current))
                        .into_iter()
                        .filter(|operand| !self.canonical.contains_key(operand))
                        .map(|operand| (operand, false)),
                );
                continue;
            }
            let id = self.canonical_of_ready(current);
            self.canonical.insert(current, id);
        }
        self.canonical[&node]
    }

    /// The canonical id of a node whose operands have theirs.
    fn canonical_of_ready(&mut self, node: NodeId) -> usize {
        // The rules are those of [`intern`], so the count is what canonical
        // interning reaches.
        let c = |this: &Self, operand: &NodeId| this.canonical[operand];
        let key = match self.arena.get(node) {
            SLTNode::Input {
                variable,
                signed,
                index,
                access,
            } => Key::Input(
                variable.clone(),
                access.lsb,
                access.msb,
                *signed,
                index.iter().map(|entry| c(self, &entry.node)).collect(),
            ),
            SLTNode::Constant(value, mask, width, signed) => {
                Key::Constant(value.to_u64_digits(), mask.to_u64_digits(), *width, *signed)
            }
            SLTNode::Unary(UnaryOp::Ident, operand) if !signed_register(self.arena, *operand) => {
                return c(self, operand);
            }
            SLTNode::Unary(op, operand) => Key::Unary(*op, c(self, operand)),
            SLTNode::Binary(lhs, op, rhs) => {
                let (mut a, mut b) = (c(self, lhs), c(self, rhs));
                if commutes(self.arena, *op, *lhs, *rhs) && a > b {
                    std::mem::swap(&mut a, &mut b);
                }
                Key::Binary(*op, a, b)
            }
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                let (then_id, else_id) = (c(self, then_expr), c(self, else_expr));
                if then_id == else_id && !signed_register(self.arena, *then_expr) {
                    return then_id;
                }
                Key::Mux(c(self, cond), then_id, else_id)
            }
            SLTNode::Slice { expr, access } => {
                let inner = c(self, expr);
                let (lsb, msb) = (access.lsb, access.msb);
                if lsb == 0 && msb + 1 == self.width(*expr) && self.unsigned(inner) {
                    return inner;
                }
                match self.keys[inner].clone() {
                    Key::Slice(base, base_lsb, _) => {
                        Key::Slice(base, base_lsb + lsb, base_lsb + msb)
                    }
                    // A slice of an input is the narrower input.
                    Key::Input(variable, base_lsb, _, _, index) => {
                        Key::Input(variable, base_lsb + lsb, base_lsb + msb, false, index)
                    }
                    Key::Concat(parts) => {
                        let mut offset = 0;
                        let mut hit = None;
                        for &(part, width) in parts.iter().rev() {
                            if lsb >= offset && msb < offset + width {
                                hit = Some((part, lsb - offset, msb - offset, width));
                                break;
                            }
                            offset += width;
                        }
                        match hit {
                            Some((part, 0, top, width))
                                if top + 1 == width && self.unsigned(part) =>
                            {
                                return part;
                            }
                            Some((part, low, high, _)) => Key::Slice(part, low, high),
                            None => Key::Slice(inner, lsb, msb),
                        }
                    }
                    _ => Key::Slice(inner, lsb, msb),
                }
            }
            SLTNode::Concat(parts) => {
                let mut flat: Vec<(usize, usize)> = Vec::new();
                for (part, width) in parts {
                    let id = c(self, part);
                    match self.keys[id].clone() {
                        Key::Concat(inner) => flat.extend(inner),
                        _ => flat.push((id, *width)),
                    }
                }
                let mut merged: Vec<(usize, usize)> = Vec::new();
                for (id, width) in flat {
                    if let Some(&(previous, previous_width)) = merged.last()
                        && let (
                            Key::Slice(high_base, high_lsb, high_msb),
                            Key::Slice(low_base, low_lsb, low_msb),
                        ) = (self.keys[previous].clone(), self.keys[id].clone())
                        && high_base == low_base
                        && low_msb + 1 == high_lsb
                    {
                        merged.pop();
                        let joined = self.intern(Key::Slice(high_base, low_lsb, high_msb), node);
                        merged.push((joined, previous_width + width));
                        continue;
                    }
                    merged.push((id, width));
                }
                match merged.as_slice() {
                    [(only, width)]
                        if self.unsigned(*only)
                            && self.arena.node_width(self.representatives[*only])
                                == Some(*width) =>
                    {
                        return *only;
                    }
                    _ => Key::Concat(merged),
                }
            }
            _ => Key::Opaque(node.0),
        };
        self.intern(key, node)
    }

    /// The distinct canonical ids of the nodes reachable from `roots`.
    ///
    /// The ids are those the canonical forms of the roots reach, not those
    /// of every node below them: a node a rule removes, such as an input a
    /// slice narrows, is no node of the canonical DAG.
    pub fn count(&mut self, roots: impl IntoIterator<Item = NodeId>) -> usize {
        let mut pending: Vec<usize> = roots.into_iter().map(|root| self.canonical(root)).collect();
        let mut seen: HashSet<usize> = HashSet::default();
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                continue;
            }
            match &self.keys[id] {
                Key::Input(_, _, _, _, index) => pending.extend(index.iter().copied()),
                Key::Binary(_, lhs, rhs) => pending.extend([*lhs, *rhs]),
                Key::Unary(_, operand) | Key::Slice(operand, _, _) => pending.push(*operand),
                Key::Mux(cond, then_id, else_id) => pending.extend([*cond, *then_id, *else_id]),
                Key::Concat(parts) => pending.extend(parts.iter().map(|(part, _)| *part)),
                Key::Constant(..) | Key::Opaque(_) => {}
            }
        }
        seen.len()
    }
}

/// Whether SIR lowering may give `node` a signed register: the operations
/// whose result register takes the signedness of their value.
fn signed_register<A: Clone + Eq + Hash>(arena: &SLTNodeArena<A>, node: NodeId) -> bool {
    matches!(
        arena.get(node),
        SLTNode::Unary(
            UnaryOp::ToTwoState | UnaryOp::ToPlanes | UnaryOp::Float { .. },
            _
        )
    )
}

/// Whether a wrapper that makes `node` unsigned (a slice, a concatenation,
/// a multiplexer) can be dropped: `node` is unsigned to its users anyway.
fn unsigned<A: Clone + Eq + Hash>(arena: &SLTNodeArena<A>, node: NodeId) -> bool {
    !signed_register(arena, node) && !bound_signed(node, arena)
}

fn node_width<A: Clone + Eq + Hash>(arena: &SLTNodeArena<A>, node: NodeId) -> usize {
    arena
        .node_width(node)
        .unwrap_or_else(|| panic!("SLT node id n{} is outside the arena", node.0))
}

/// The bitwise operations, which compute each bit from the same bits of
/// their operands.
fn bitwise_binary(op: &BinaryOp) -> bool {
    matches!(op, BinaryOp::And | BinaryOp::Or | BinaryOp::Xor)
}

fn bitwise_unary(op: &UnaryOp) -> bool {
    matches!(op, UnaryOp::BitNot | UnaryOp::ToTwoState)
}

/// Allocate `node`, whose operands are canonical nodes of `arena`, in its
/// normal form (see the [module documentation](self)): equal expressions
/// get one id.
pub fn intern<A: Clone + Eq + Hash>(
    arena: &mut SLTNodeArena<A>,
    node: SLTNode<A>,
) -> Result<NodeId, SLTNodeFactsError> {
    match node {
        SLTNode::Unary(UnaryOp::Ident, operand) if !signed_register(arena, operand) => Ok(operand),
        SLTNode::Binary(lhs, op, rhs) => {
            // A comparison extends both operands alike; other operations
            // extend their left operand by its signedness and their right
            // one with zeros, so only equal widths commute.
            let swap = lhs > rhs && commutes(arena, op, lhs, rhs);
            let (lhs, rhs) = if swap { (rhs, lhs) } else { (lhs, rhs) };
            arena.alloc(SLTNode::Binary(lhs, op, rhs))
        }
        SLTNode::Mux {
            then_expr,
            else_expr,
            ..
        } if then_expr == else_expr && !signed_register(arena, then_expr) => Ok(then_expr),
        SLTNode::Slice { expr, access } => slice(arena, expr, access),
        SLTNode::Concat(parts) => concat(arena, parts),
        node => arena.alloc(node),
    }
}

/// The canonical slice `access` of the canonical node `expr`.
fn slice<A: Clone + Eq + Hash>(
    arena: &mut SLTNodeArena<A>,
    expr: NodeId,
    access: BitAccess,
) -> Result<NodeId, SLTNodeFactsError> {
    if access.lsb == 0 && access.msb + 1 == node_width(arena, expr) && unsigned(arena, expr) {
        return Ok(expr);
    }
    let within = |outer: BitAccess| BitAccess::new(outer.lsb + access.lsb, outer.lsb + access.msb);
    match arena.get(expr).clone() {
        SLTNode::Slice {
            expr: base,
            access: outer,
        } => slice(arena, base, within(outer)),
        SLTNode::Input {
            variable,
            index,
            access: outer,
            ..
        } => arena.alloc(SLTNode::Input {
            variable,
            signed: false,
            index,
            access: within(outer),
        }),
        SLTNode::Constant(value, mask, _, _) => {
            let width = access.msb - access.lsb + 1;
            let bits = (BigUint::from(1u8) << width) - 1u8;
            arena.alloc(SLTNode::Constant(
                (value >> access.lsb) & &bits,
                (mask >> access.lsb) & bits,
                width,
                false,
            ))
        }
        SLTNode::Concat(parts)
            if parts
                .iter()
                .all(|&(part, width)| node_width(arena, part) == width) =>
        {
            let mut offset = 0;
            for &(part, width) in parts.iter().rev() {
                if access.lsb >= offset && access.msb < offset + width {
                    return slice(
                        arena,
                        part,
                        BitAccess::new(access.lsb - offset, access.msb - offset),
                    );
                }
                offset += width;
            }
            arena.alloc(SLTNode::Slice { expr, access })
        }
        _ => arena.alloc(SLTNode::Slice { expr, access }),
    }
}

/// The canonical concatenation of canonical `parts`, most significant first.
fn concat<A: Clone + Eq + Hash>(
    arena: &mut SLTNodeArena<A>,
    parts: Vec<(NodeId, usize)>,
) -> Result<NodeId, SLTNodeFactsError> {
    let exact = |arena: &SLTNodeArena<A>, part: NodeId, width: usize| {
        width > 0 && node_width(arena, part) == width
    };
    let mut flat = Vec::with_capacity(parts.len());
    for (part, width) in parts {
        match arena.get(part) {
            SLTNode::Concat(inner)
                if exact(arena, part, width)
                    && inner.iter().all(|&(inner_part, inner_width)| {
                        exact(arena, inner_part, inner_width)
                    }) =>
            {
                flat.extend(inner.iter().copied())
            }
            _ => flat.push((part, width)),
        }
    }
    let flat = join_runs(arena, flat)?;
    let mut merged: Vec<(NodeId, usize)> = Vec::with_capacity(flat.len());
    for (part, width) in flat {
        let mut current = (part, width);
        while let Some(&(high, high_width)) = merged.last() {
            let joined = if exact(arena, high, high_width) && exact(arena, current.0, current.1) {
                join(arena, high, high_width, current.0, current.1)?
            } else {
                None
            };
            let Some(joined) = joined else {
                break;
            };
            merged.pop();
            current = (joined, high_width + current.1);
        }
        merged.push(current);
    }
    match merged.as_slice() {
        [(only, width)] if exact(arena, *only, *width) && unsigned(arena, *only) => Ok(*only),
        _ => arena.alloc(SLTNode::Concat(merged)),
    }
}

/// How two adjacent concatenation parts join, read off the parts before
/// the arena changes.
enum Join<A> {
    Constant(BigUint, BigUint, BigUint, BigUint),
    Slice(NodeId, BitAccess),
    Input(A, Vec<crate::SLTIndex>, BitAccess),
    Binary(BinaryOp, [NodeId; 4]),
    Unary(UnaryOp, NodeId, NodeId),
}

/// One canonical node for the adjacent concatenation parts `high` and
/// `low`, when they are parts of one slice, one input or one constant, or
/// compute one bitwise operation over such parts: the wide operation
/// replaces the per-part ones.
fn join<A: Clone + Eq + Hash>(
    arena: &mut SLTNodeArena<A>,
    high: NodeId,
    high_width: usize,
    low: NodeId,
    low_width: usize,
) -> Result<Option<NodeId>, SLTNodeFactsError> {
    let both = |arena: &SLTNodeArena<A>, (a, b): (NodeId, NodeId)| {
        node_width(arena, a) == high_width && node_width(arena, b) == low_width
    };
    let plan = match (arena.get(high), arena.get(low)) {
        (
            SLTNode::Constant(high_value, high_mask, ..),
            SLTNode::Constant(low_value, low_mask, ..),
        ) => Join::Constant(
            high_value.clone(),
            high_mask.clone(),
            low_value.clone(),
            low_mask.clone(),
        ),
        (
            SLTNode::Slice {
                expr: high_base,
                access: high_access,
            },
            SLTNode::Slice {
                expr: low_base,
                access: low_access,
            },
        ) if high_base == low_base && low_access.msb + 1 == high_access.lsb => {
            Join::Slice(*high_base, BitAccess::new(low_access.lsb, high_access.msb))
        }
        (
            SLTNode::Input {
                variable: high_variable,
                index: high_index,
                access: high_access,
                ..
            },
            SLTNode::Input {
                variable: low_variable,
                index: low_index,
                access: low_access,
                ..
            },
        ) if high_variable == low_variable
            && high_index == low_index
            && low_access.msb + 1 == high_access.lsb =>
        {
            Join::Input(
                high_variable.clone(),
                high_index.clone(),
                BitAccess::new(low_access.lsb, high_access.msb),
            )
        }
        (SLTNode::Binary(a, high_op, b), SLTNode::Binary(c, low_op, d))
            if high_op == low_op
                && bitwise_binary(high_op)
                && both(arena, (*a, *c))
                && both(arena, (*b, *d)) =>
        {
            Join::Binary(*high_op, [*a, *b, *c, *d])
        }
        (SLTNode::Unary(high_op, a), SLTNode::Unary(low_op, c))
            if high_op == low_op && bitwise_unary(high_op) && both(arena, (*a, *c)) =>
        {
            Join::Unary(*high_op, *a, *c)
        }
        _ => return Ok(None),
    };
    let joined = match plan {
        Join::Constant(high_value, high_mask, low_value, low_mask) => {
            arena.alloc(SLTNode::Constant(
                (high_value << low_width) | low_value,
                (high_mask << low_width) | low_mask,
                high_width + low_width,
                false,
            ))?
        }
        Join::Slice(base, access) => slice(arena, base, access)?,
        Join::Input(variable, index, access) => arena.alloc(SLTNode::Input {
            variable,
            signed: false,
            index,
            access,
        })?,
        Join::Binary(op, [a, b, c, d]) => {
            let Some(lhs) = join(arena, a, high_width, c, low_width)? else {
                return Ok(None);
            };
            let Some(rhs) = join(arena, b, high_width, d, low_width)? else {
                return Ok(None);
            };
            intern(arena, SLTNode::Binary(lhs, op, rhs))?
        }
        Join::Unary(op, a, c) => {
            let Some(operand) = join(arena, a, high_width, c, low_width)? else {
                return Ok(None);
            };
            intern(arena, SLTNode::Unary(op, operand))?
        }
    };
    Ok(Some(joined))
}

/// `parts` with each run of adjacent constants made one constant, and each
/// run of adjacent multiplexers under one condition one multiplexer of the
/// concatenated arms when an arm then joins. A run is joined at once: joining
/// it pair by pair would rebuild a growing node for every part.
fn join_runs<A: Clone + Eq + Hash>(
    arena: &mut SLTNodeArena<A>,
    parts: Vec<(NodeId, usize)>,
) -> Result<Vec<(NodeId, usize)>, SLTNodeFactsError> {
    let exact = |arena: &SLTNodeArena<A>, part: NodeId, width: usize| {
        width > 0 && node_width(arena, part) == width
    };
    let constant = |arena: &SLTNodeArena<A>, (part, width): (NodeId, usize)| {
        exact(arena, part, width) && matches!(arena.get(part), SLTNode::Constant(..))
    };
    // A multiplexer part: its condition and arms of the part's width.
    let mux = |arena: &SLTNodeArena<A>, (part, width): (NodeId, usize)| match arena.get(part) {
        SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } if exact(arena, part, width)
            && node_width(arena, *then_expr) == width
            && node_width(arena, *else_expr) == width =>
        {
            Some((*cond, *then_expr, *else_expr))
        }
        _ => None,
    };
    let mut joined = Vec::with_capacity(parts.len());
    let mut at = 0;
    while at < parts.len() {
        let mut end = at + 1;
        if constant(arena, parts[at]) {
            while end < parts.len() && constant(arena, parts[end]) {
                end += 1;
            }
            if end - at > 1 {
                let mut value = BigUint::default();
                let mut mask = BigUint::default();
                let mut offset = 0;
                // The least significant part first, so each shift is of the
                // part alone.
                for &(part, width) in parts[at..end].iter().rev() {
                    let SLTNode::Constant(part_value, part_mask, ..) = arena.get(part) else {
                        unreachable!("a constant run");
                    };
                    value |= part_value << offset;
                    mask |= part_mask << offset;
                    offset += width;
                }
                joined.push((
                    arena.alloc(SLTNode::Constant(value, mask, offset, false))?,
                    offset,
                ));
                at = end;
                continue;
            }
        } else if let Some((cond, _, _)) = mux(arena, parts[at]) {
            while end < parts.len()
                && mux(arena, parts[end]).is_some_and(|(other, _, _)| other == cond)
            {
                end += 1;
            }
            if end - at > 1 {
                let run = &parts[at..end];
                let arms = |pick: fn((NodeId, NodeId, NodeId)) -> NodeId| {
                    run.iter()
                        .map(|&(part, width)| {
                            (pick(mux(arena, (part, width)).expect("a mux run")), width)
                        })
                        .collect::<Vec<_>>()
                };
                let then_parts = arms(|(_, then_expr, _)| then_expr);
                let else_parts = arms(|(_, _, else_expr)| else_expr);
                let then_expr = concat(arena, then_parts)?;
                let else_expr = concat(arena, else_parts)?;
                // Joined only when an arm is fewer nodes than the run.
                let fewer = |arena: &SLTNodeArena<A>, arm: NodeId| match arena.get(arm) {
                    SLTNode::Concat(arm_parts) => arm_parts.len() < run.len(),
                    _ => true,
                };
                if fewer(arena, then_expr) || fewer(arena, else_expr) {
                    let width = run.iter().map(|(_, width)| width).sum();
                    joined.push((
                        intern(
                            arena,
                            SLTNode::Mux {
                                cond,
                                then_expr,
                                else_expr,
                            },
                        )?,
                        width,
                    ));
                    at = end;
                    continue;
                }
            }
        }
        joined.extend_from_slice(&parts[at..end]);
        at = end;
    }
    Ok(joined)
}

/// Why a node cannot be copied through [`intern`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReinternError {
    /// A node without a normal form, such as a loop fold.
    Unsupported(NodeId),
    Facts(SLTNodeFactsError),
}

impl std::fmt::Display for ReinternError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(node) => write!(f, "node n{} has no canonical form", node.0),
            Self::Facts(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for ReinternError {}

impl From<SLTNodeFactsError> for ReinternError {
    fn from(error: SLTNodeFactsError) -> Self {
        Self::Facts(error)
    }
}

/// Copies nodes of one arena into another through [`intern`], each node
/// once.
pub struct Reinterner<'a, A: Clone + Eq + Hash> {
    source: &'a SLTNodeArena<A>,
    copies: HashMap<NodeId, NodeId>,
}

impl<'a, A: Clone + Eq + Hash> Reinterner<'a, A> {
    pub fn new(source: &'a SLTNodeArena<A>) -> Self {
        Self {
            source,
            copies: HashMap::default(),
        }
    }

    /// The canonical copy of `root` in `target`, its operands first.
    pub fn copy(
        &mut self,
        target: &mut SLTNodeArena<A>,
        root: NodeId,
    ) -> Result<NodeId, ReinternError> {
        let mut stack = vec![(root, false)];
        while let Some((node, expanded)) = stack.pop() {
            if self.copies.contains_key(&node) {
                continue;
            }
            let source = self.source.get(node);
            if !expanded {
                stack.push((node, true));
                stack.extend(
                    operands(source)
                        .into_iter()
                        .filter(|operand| !self.copies.contains_key(operand))
                        .map(|operand| (operand, false)),
                );
                continue;
            }
            let copy = |operand: &NodeId| self.copies[operand];
            let rebuilt = match source {
                SLTNode::Input {
                    variable,
                    signed,
                    index,
                    access,
                } => SLTNode::Input {
                    variable: variable.clone(),
                    signed: *signed,
                    index: index
                        .iter()
                        .map(|entry| {
                            let mut entry = entry.clone();
                            entry.node = copy(&entry.node);
                            entry
                        })
                        .collect(),
                    access: *access,
                },
                SLTNode::Constant(..) => source.clone(),
                SLTNode::Binary(lhs, op, rhs) => SLTNode::Binary(copy(lhs), *op, copy(rhs)),
                SLTNode::Unary(op, operand) => SLTNode::Unary(*op, copy(operand)),
                SLTNode::Mux {
                    cond,
                    then_expr,
                    else_expr,
                } => SLTNode::Mux {
                    cond: copy(cond),
                    then_expr: copy(then_expr),
                    else_expr: copy(else_expr),
                },
                SLTNode::Slice { expr, access } => SLTNode::Slice {
                    expr: copy(expr),
                    access: *access,
                },
                SLTNode::Concat(parts) => SLTNode::Concat(
                    parts
                        .iter()
                        .map(|(part, width)| (copy(part), *width))
                        .collect(),
                ),
                _ => return Err(ReinternError::Unsupported(node)),
            };
            let id = intern(target, rebuilt)?;
            self.copies.insert(node, id);
        }
        Ok(self.copies[&root])
    }
}

/// The operands of a node that the canonical form and the DAG measures
/// follow.
fn operands<A: Clone + Eq + Hash>(node: &SLTNode<A>) -> Vec<NodeId> {
    match node {
        SLTNode::Input { index, .. } => index.iter().map(|entry| entry.node).collect(),
        SLTNode::Binary(lhs, _, rhs) => vec![*lhs, *rhs],
        SLTNode::Unary(_, operand)
        | SLTNode::Slice { expr: operand, .. }
        | SLTNode::Capture { expr: operand, .. } => vec![*operand],
        SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } => vec![*cond, *then_expr, *else_expr],
        SLTNode::Concat(parts) => parts.iter().map(|(part, _)| *part).collect(),
        _ => Vec::new(),
    }
}

/// The nodes reachable from `roots`.
pub fn reachable<A: Clone + Eq + Hash>(
    arena: &SLTNodeArena<A>,
    roots: impl IntoIterator<Item = NodeId>,
) -> Vec<NodeId> {
    let mut seen = HashSet::default();
    let mut order = Vec::new();
    let mut stack: Vec<NodeId> = roots.into_iter().collect();
    while let Some(node) = stack.pop() {
        if !seen.insert(node) {
            continue;
        }
        order.push(node);
        stack.extend(operands(arena.get(node)));
    }
    order
}

/// The 1-bit operations that concatenations reachable from `roots` list
/// over adjacent bits of the same operands, in runs of at least two: one
/// wide operation over the operands' slices would compute each run. The
/// operations counted are the bitwise ones and multiplexers under one
/// condition; [`intern`] joins each such run.
pub fn same_op_bit_runs<A: Clone + Eq + Hash>(
    arena: &SLTNodeArena<A>,
    roots: impl IntoIterator<Item = NodeId>,
) -> usize {
    // A 1-bit operand: its base and bit, or a constant, or an operand that
    // every part of a run shares (a multiplexer's condition).
    #[derive(PartialEq, Eq)]
    enum Bit<A> {
        Of(A, usize),
        Node(NodeId, usize),
        Constant,
        Shared(NodeId),
    }
    let bit_of = |node: NodeId| -> Option<Bit<A>> {
        match arena.get(node) {
            SLTNode::Input {
                variable,
                access,
                index,
                ..
            } if access.lsb == access.msb && index.is_empty() => {
                Some(Bit::Of(variable.clone(), access.lsb))
            }
            SLTNode::Slice { expr, access } if access.lsb == access.msb => {
                Some(Bit::Node(*expr, access.lsb))
            }
            SLTNode::Constant(..) => Some(Bit::Constant),
            _ => None,
        }
    };
    let below = |high: &Bit<A>, low: &Bit<A>| match (high, low) {
        (Bit::Of(a, i), Bit::Of(b, j)) => a == b && *j + 1 == *i,
        (Bit::Node(a, i), Bit::Node(b, j)) => a == b && *j + 1 == *i,
        (Bit::Constant, Bit::Constant) => true,
        (Bit::Shared(a), Bit::Shared(b)) => a == b,
        _ => false,
    };
    // The operation and operands of a 1-bit node.
    let signature = |node: NodeId| -> Option<(String, Vec<Bit<A>>)> {
        let bits = |nodes: &[NodeId]| nodes.iter().map(|&n| bit_of(n)).collect::<Option<Vec<_>>>();
        match arena.get(node) {
            SLTNode::Binary(lhs, op, rhs) if bitwise_binary(op) => {
                Some((format!("{op:?}"), bits(&[*lhs, *rhs])?))
            }
            SLTNode::Unary(op, operand) if bitwise_unary(op) => {
                Some((format!("{op:?}"), bits(&[*operand])?))
            }
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                let mut operands = bits(&[*then_expr, *else_expr])?;
                operands.push(Bit::Shared(*cond));
                Some(("Mux".into(), operands))
            }
            _ => None,
        }
    };
    let mut total = 0;
    for node in reachable(arena, roots) {
        let SLTNode::Concat(parts) = arena.get(node) else {
            continue;
        };
        let mut run = 1;
        let mut previous: Option<(String, Vec<Bit<A>>)> = None;
        // Parts are listed most significant first.
        for &(part, width) in parts {
            let current = (width == 1).then(|| signature(part)).flatten();
            let adjacent = match (&previous, &current) {
                (Some((high_op, high)), Some((low_op, low))) => {
                    high_op == low_op
                        && high.len() == low.len()
                        && high.iter().zip(low).all(|(h, l)| below(h, l))
                }
                _ => false,
            };
            if adjacent {
                run += 1;
            } else {
                if run >= 2 {
                    total += run;
                }
                run = 1;
            }
            previous = current;
        }
        if run >= 2 {
            total += run;
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use num_bigint::BigUint;

    fn input(arena: &mut SLTNodeArena<u32>, variable: u32, lsb: usize, msb: usize) -> NodeId {
        arena
            .alloc(SLTNode::Input {
                variable,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(lsb, msb),
            })
            .unwrap()
    }

    /// Operand order, identity wrappers and slices of concatenations do
    /// not separate equal expressions.
    #[test]
    fn equal_expressions_share_a_canonical_id() {
        let mut arena = SLTNodeArena::new();
        let a = input(&mut arena, 1, 0, 7);
        let b = input(&mut arena, 2, 0, 7);
        let ab = arena.alloc(SLTNode::Binary(a, BinaryOp::And, b)).unwrap();
        let ba = arena.alloc(SLTNode::Binary(b, BinaryOp::And, a)).unwrap();
        let ident = arena.alloc(SLTNode::Unary(UnaryOp::Ident, ba)).unwrap();
        let concat = arena.alloc(SLTNode::Concat(vec![(a, 8), (ab, 8)])).unwrap();
        let low = arena
            .alloc(SLTNode::Slice {
                expr: concat,
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        let mut canonical = Canonicalizer::new(&arena);
        assert_eq!(canonical.canonical(ab), canonical.canonical(ident));
        assert_eq!(canonical.canonical(low), canonical.canonical(ab));
        assert!(canonical.count([ab, ident, low]) < reachable(&arena, [ab, ident, low]).len());
    }

    /// A concatenation of the bitwise ANDs of adjacent bits is one run.
    #[test]
    fn bitwise_runs_are_counted() {
        let mut arena = SLTNodeArena::new();
        let mut parts = Vec::new();
        for bit in (0..4).rev() {
            let a = input(&mut arena, 1, bit, bit);
            let b = input(&mut arena, 2, bit, bit);
            parts.push((
                arena.alloc(SLTNode::Binary(a, BinaryOp::And, b)).unwrap(),
                1,
            ));
        }
        let zero = arena
            .alloc(SLTNode::Constant(
                BigUint::from(0u8),
                BigUint::from(0u8),
                1,
                false,
            ))
            .unwrap();
        parts.push((zero, 1));
        let concat = arena.alloc(SLTNode::Concat(parts)).unwrap();
        assert_eq!(same_op_bit_runs(&arena, [concat]), 4);
    }

    /// The interned forms of equal expressions are one node.
    #[test]
    fn interning_shares_equal_expressions() {
        let mut arena = SLTNodeArena::new();
        let a = input(&mut arena, 1, 0, 7);
        let b = input(&mut arena, 2, 0, 7);
        let ab = intern(&mut arena, SLTNode::Binary(a, BinaryOp::And, b)).unwrap();
        let ba = intern(&mut arena, SLTNode::Binary(b, BinaryOp::And, a)).unwrap();
        assert_eq!(ab, ba);
        let ident = intern(&mut arena, SLTNode::Unary(UnaryOp::Ident, ab)).unwrap();
        assert_eq!(ident, ab);
        let concat = intern(&mut arena, SLTNode::Concat(vec![(a, 8), (ab, 8)])).unwrap();
        let low = intern(
            &mut arena,
            SLTNode::Slice {
                expr: concat,
                access: BitAccess::new(0, 7),
            },
        )
        .unwrap();
        assert_eq!(low, ab);
        // A slice of an input is the narrower input; adjacent ones join.
        let high = intern(
            &mut arena,
            SLTNode::Slice {
                expr: a,
                access: BitAccess::new(4, 7),
            },
        )
        .unwrap();
        assert_eq!(high, input(&mut arena, 1, 4, 7));
        let low = input(&mut arena, 1, 0, 3);
        assert_eq!(
            intern(&mut arena, SLTNode::Concat(vec![(high, 4), (low, 4)])).unwrap(),
            a
        );
    }

    /// A wrapper that makes a signed register unsigned stays.
    #[test]
    fn signed_wrappers_stay() {
        let mut arena = SLTNodeArena::new();
        let a = input(&mut arena, 1, 0, 7);
        let two_state = intern(&mut arena, SLTNode::Unary(UnaryOp::ToTwoState, a)).unwrap();
        let ident = intern(&mut arena, SLTNode::Unary(UnaryOp::Ident, two_state)).unwrap();
        assert_ne!(ident, two_state);
        // Operands of different widths do not commute.
        let b = input(&mut arena, 2, 0, 3);
        let ab = intern(&mut arena, SLTNode::Binary(a, BinaryOp::Add, b)).unwrap();
        let ba = intern(&mut arena, SLTNode::Binary(b, BinaryOp::Add, a)).unwrap();
        assert_ne!(ab, ba);
    }

    /// Bitwise operations and multiplexers over adjacent bits join into one
    /// wide operation.
    #[test]
    fn interning_joins_bit_runs() {
        let mut arena = SLTNodeArena::new();
        let cond = input(&mut arena, 3, 0, 0);
        let mut ands = Vec::new();
        let mut muxes = Vec::new();
        for bit in (0..4).rev() {
            let a = input(&mut arena, 1, bit, bit);
            let b = input(&mut arena, 2, bit, bit);
            let and = intern(&mut arena, SLTNode::Binary(a, BinaryOp::And, b)).unwrap();
            ands.push((and, 1));
            let mux = intern(
                &mut arena,
                SLTNode::Mux {
                    cond,
                    then_expr: and,
                    else_expr: a,
                },
            )
            .unwrap();
            muxes.push((mux, 1));
        }
        let and = intern(&mut arena, SLTNode::Concat(ands)).unwrap();
        let a = input(&mut arena, 1, 0, 3);
        let b = input(&mut arena, 2, 0, 3);
        assert_eq!(
            arena.get(and),
            &SLTNode::Binary(a, BinaryOp::And, b),
            "one wide AND"
        );
        let mux = intern(&mut arena, SLTNode::Concat(muxes)).unwrap();
        assert_eq!(
            arena.get(mux),
            &SLTNode::Mux {
                cond,
                then_expr: and,
                else_expr: a,
            }
        );
        assert_eq!(same_op_bit_runs(&arena, [and, mux]), 0);
    }

    /// A copy through the canonical form keeps no run and no duplicate.
    #[test]
    fn reinterned_copies_are_canonical() {
        let mut source = SLTNodeArena::new();
        let mut parts = Vec::new();
        for bit in (0..4).rev() {
            let a = input(&mut source, 1, bit, bit);
            let b = input(&mut source, 2, bit, bit);
            parts.push((
                source.alloc(SLTNode::Binary(b, BinaryOp::Or, a)).unwrap(),
                1,
            ));
        }
        let concat = source.alloc(SLTNode::Concat(parts)).unwrap();
        assert_eq!(same_op_bit_runs(&source, [concat]), 4);
        let mut target = SLTNodeArena::new();
        let copy = Reinterner::new(&source).copy(&mut target, concat).unwrap();
        assert_eq!(same_op_bit_runs(&target, [copy]), 0);
        assert_eq!(reachable(&target, [copy]).len(), 3);
    }

    fn constant(arena: &mut SLTNodeArena<u32>, value: u64, width: usize, signed: bool) -> NodeId {
        arena
            .alloc(SLTNode::Constant(
                BigUint::from(value),
                BigUint::default(),
                width,
                signed,
            ))
            .unwrap()
    }

    /// Constants of one bit pattern but another signedness compare
    /// differently with a wider operand: their canonical ids differ.
    #[test]
    fn canonical_counts_keep_literal_signedness() {
        let mut arena = SLTNodeArena::new();
        let wide = constant(&mut arena, 0xffff, 16, true);
        let signed = constant(&mut arena, 0xff, 8, true);
        let unsigned = constant(&mut arena, 0xff, 8, false);
        let signed_eq = arena
            .alloc(SLTNode::Binary(signed, BinaryOp::Eq, wide))
            .unwrap();
        let unsigned_eq = arena
            .alloc(SLTNode::Binary(unsigned, BinaryOp::Eq, wide))
            .unwrap();
        let mut canonical = Canonicalizer::new(&arena);
        assert_ne!(
            canonical.canonical(signed_eq),
            canonical.canonical(unsigned_eq)
        );
    }

    /// Operands of different widths extend differently on either side, so
    /// they keep their order; operands of equal width commute.
    #[test]
    fn canonical_counts_keep_unequal_operand_order() {
        let mut arena = SLTNodeArena::new();
        let narrow = input(&mut arena, 1, 0, 3);
        let wide = input(&mut arena, 2, 0, 7);
        let other = input(&mut arena, 3, 0, 7);
        let narrow_wide = arena
            .alloc(SLTNode::Binary(narrow, BinaryOp::Add, wide))
            .unwrap();
        let wide_narrow = arena
            .alloc(SLTNode::Binary(wide, BinaryOp::Add, narrow))
            .unwrap();
        let wide_other = arena
            .alloc(SLTNode::Binary(wide, BinaryOp::Add, other))
            .unwrap();
        let other_wide = arena
            .alloc(SLTNode::Binary(other, BinaryOp::Add, wide))
            .unwrap();
        let mut canonical = Canonicalizer::new(&arena);
        assert_ne!(
            canonical.canonical(narrow_wide),
            canonical.canonical(wide_narrow)
        );
        assert_eq!(
            canonical.canonical(wide_other),
            canonical.canonical(other_wide)
        );
    }

    /// A slice of an input counts as the narrower input, as [`intern`]
    /// makes it, and the count over a DAG equals the nodes interning it
    /// reaches.
    #[test]
    fn canonical_counts_match_interning() {
        let mut arena = SLTNodeArena::new();
        let whole = input(&mut arena, 1, 0, 7);
        let high = arena
            .alloc(SLTNode::Slice {
                expr: whole,
                access: BitAccess::new(4, 7),
            })
            .unwrap();
        let narrow = input(&mut arena, 1, 4, 7);
        let and = arena
            .alloc(SLTNode::Binary(high, BinaryOp::And, narrow))
            .unwrap();
        let mut canonical = Canonicalizer::new(&arena);
        assert_eq!(canonical.canonical(high), canonical.canonical(narrow));
        let count = canonical.count([and]);
        let mut target = SLTNodeArena::new();
        let copy = Reinterner::new(&arena).copy(&mut target, and).unwrap();
        assert_eq!(count, reachable(&target, [copy]).len());
    }
}
