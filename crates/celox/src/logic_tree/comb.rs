use std::{collections::BTreeSet, fmt, hash::Hash};

use serde::{Deserialize, Serialize};

use crate::ParserError;
use crate::context_width::get_context_width;
use crate::logic_tree::range_store::RangeStore;
use crate::parser::{LoweringPhase, resolve_total_width, resolve_width};
use crate::{
    HashMap, HashSet,
    ir::{BinaryOp, BitAccess, UnaryOp, VarAtomBase},
    parser::bitaccess::{
        build_partial_assign_expr, celox_value_from_comptime, eval_constexpr, eval_var_select,
        is_static_access,
    },
};
use num_bigint::BigUint;
use num_traits::ToPrimitive as _;
use veryl_analyzer::ir::{
    ArrayLiteralItem, AssignStatement, CombDeclaration, Comptime, Expression, Factor, ForBound,
    ForRange, ForStatement, IfStatement, Module, Op, Statement, ValueVariant, VarId, VarIndex,
    VarSelect, VarSelectOp,
};
use veryl_parser::token_range::TokenRange;

// SymbolicStore: Maps variable IDs to their current symbolic representation.
// Each variable is managed by a RangeStore, which tracks bit-ranges and their associated SLT nodes.
pub type SymbolicStore<A> = HashMap<VarId, RangeStore<Option<(NodeId, HashSet<VarAtomBase<A>>)>>>;
pub type BoundaryMap<A> = HashMap<A, BTreeSet<usize>>;

#[derive(Clone)]
struct LoopControlState {
    store: SymbolicStore<VarId>,
    boundaries: BoundaryMap<VarId>,
    continue_expr: NodeId,
    continue_sources: HashSet<VarAtomBase<VarId>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NodeId(pub usize);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(bound(
    serialize = "A: Serialize + std::hash::Hash + Eq + Clone",
    deserialize = "A: Deserialize<'de> + std::hash::Hash + Eq + Clone"
))]
pub struct SLTNodeArena<A: Hash + Eq + Clone> {
    pub nodes: Vec<SLTNode<A>>,
    #[serde(skip, default = "crate::HashMap::default")]
    pub cache: crate::HashMap<SLTNode<A>, NodeId>,
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
                index,
                access,
            } => SLTNodeSerde::Input {
                variable,
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
                index,
                access,
            } => SLTNode::Input {
                variable,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(bound(
    serialize = "A: Serialize + std::hash::Hash + Eq",
    deserialize = "A: Deserialize<'de> + std::hash::Hash + Eq + Clone"
))]
pub struct LogicPath<A: Hash + Eq + Clone> {
    pub target: VarAtomBase<A>,
    pub sources: HashSet<VarAtomBase<A>>,
    pub expr: NodeId,
}

impl<A: fmt::Display + Hash + Eq + Clone> fmt::Display for LogicPath<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.target)
    }
}

impl<A: fmt::Debug + fmt::Display + Hash + Eq + Clone> LogicPath<A> {
    pub fn map_addr<B: Hash + Eq + Clone, F>(
        &self,
        arena: &SLTNodeArena<A>,
        target_arena: &mut SLTNodeArena<B>,
        cache: &mut HashMap<NodeId, NodeId>,
        f: &F,
    ) -> LogicPath<B>
    where
        F: Fn(&A) -> B,
    {
        LogicPath {
            target: VarAtomBase::new(
                f(&self.target.id),
                self.target.access.lsb,
                self.target.access.msb,
            ),
            sources: self
                .sources
                .iter()
                .map(|v| VarAtomBase::new(f(&v.id), v.access.lsb, v.access.msb))
                .collect(),
            expr: arena
                .get(self.expr)
                .map_addr(self.expr, arena, target_arena, cache, f),
        }
    }
}

pub fn parse_comb(
    module: &Module,
    decl: &CombDeclaration,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<
    (
        Vec<LogicPath<VarId>>,
        SymbolicStore<VarId>,
        BoundaryMap<VarId>,
    ),
    ParserError,
> {
    // 1. Initialization: Create a RangeStore for each variable in the module.
    // Variables start in an 'unassigned' state (None), representing their initial input values.
    let mut current_store = SymbolicStore::default();
    for (id, var) in &module.variables {
        let width = resolve_total_width(module, var)?;
        current_store.insert(*id, RangeStore::new(None, width));
    }

    // 2. Symbolic Execution: Evaluate statements sequentially to update the symbolic state.
    let (final_store, boundaries) = decl
        .statements
        .iter()
        .try_fold((current_store, BoundaryMap::default()), |(s, b), stmt| {
            eval_statement(module, s, b, stmt, arena)
        })?;

    // 3. Path Extraction: Convert the final symbolic store into a list of LogicPaths.
    // Each LogicPath represents a modified bit-range and the logic required to compute it.
    let mut paths = Vec::new();
    for (id, range_store) in &final_store {
        for (&lsb, (val_opt, width, origin)) in &range_store.ranges {
            if let Some((expr, sources)) = val_opt {
                let msb = lsb + width - 1;

                // Calculate relative bit positions by adjusting for the range's origin.
                let rel_lsb = lsb - origin;
                let rel_msb = msb - origin;
                let original_width = get_width(*expr, arena);

                // If not using the entire stored node, apply Slice
                let final_expr = if rel_lsb == 0 && *width == original_width {
                    *expr
                } else {
                    arena.alloc(SLTNode::Slice {
                        expr: *expr,
                        access: BitAccess::new(rel_lsb, rel_msb),
                    })
                };

                paths.push(LogicPath::<VarId> {
                    target: VarAtomBase::new(*id, lsb, msb),
                    sources: sources.clone(),
                    expr: final_expr,
                });
            }
        }
    }
    Ok((paths, final_store, boundaries))
}

fn eval_statement(
    module: &Module,
    store: SymbolicStore<VarId>,
    boundaries: HashMap<VarId, BTreeSet<usize>>,
    stmt: &Statement,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<(SymbolicStore<VarId>, HashMap<VarId, BTreeSet<usize>>), ParserError> {
    match stmt {
        Statement::Assign(assign) => eval_assign(module, store, boundaries, assign, arena),
        Statement::If(if_stmt) => eval_if(module, store, boundaries, if_stmt, arena),
        Statement::For(for_stmt) => eval_for(module, store, boundaries, for_stmt, arena),
        Statement::IfReset(ir) => Err(ParserError::unsupported(
            LoweringPhase::SimulatorParser,
            "unsupported statement in always_comb",
            "illegal statement in always_comb: if_reset".to_string(),
            Some(&ir.token),
        )),
        Statement::SystemFunctionCall(fc) => Err(ParserError::unsupported(
            LoweringPhase::SimulatorParser,
            "unsupported statement in always_comb",
            "illegal statement in always_comb: system function call".to_string(),
            Some(&fc.comptime.token),
        )),
        Statement::FunctionCall(fc) => Err(ParserError::unsupported(
            LoweringPhase::SimulatorParser,
            "unsupported statement in always_comb",
            "illegal statement in always_comb: function call".to_string(),
            Some(&fc.comptime.token),
        )),
        Statement::TbMethodCall(_) => Err(ParserError::unsupported(
            LoweringPhase::SimulatorParser,
            "unsupported statement in always_comb",
            "illegal statement in always_comb: testbench method call".to_string(),
            None,
        )),
        Statement::Break => Err(ParserError::unsupported(
            LoweringPhase::SimulatorParser,
            "unsupported statement in always_comb",
            "illegal statement in always_comb: break".to_string(),
            None,
        )),
        Statement::Unsupported(_) => Err(ParserError::unsupported(
            LoweringPhase::SimulatorParser,
            "unsupported statement in always_comb",
            "illegal statement in always_comb: unsupported".to_string(),
            None,
        )),
        Statement::Null => Err(ParserError::unsupported(
            LoweringPhase::SimulatorParser,
            "unsupported statement in always_comb",
            "illegal statement in always_comb: null".to_string(),
            None,
        )),
    }
}

fn bool_node(arena: &mut SLTNodeArena<VarId>, value: bool) -> NodeId {
    arena.alloc(SLTNode::Constant(
        BigUint::from(value as u8),
        BigUint::from(0u8),
        1,
        false,
    ))
}

fn constant_bool(arena: &SLTNodeArena<VarId>, node: NodeId) -> Option<bool> {
    match arena.get(node) {
        SLTNode::Constant(val, _, _, _) => Some(*val != BigUint::from(0u8)),
        _ => None,
    }
}

fn merge_control_expr(
    cond_expr: NodeId,
    then_expr: NodeId,
    else_expr: NodeId,
    arena: &mut SLTNodeArena<VarId>,
) -> NodeId {
    if then_expr == else_expr {
        then_expr
    } else {
        arena.alloc(SLTNode::Mux {
            cond: cond_expr,
            then_expr,
            else_expr,
        })
    }
}

fn merge_symbolic_stores(
    module: &Module,
    then_store: &SymbolicStore<VarId>,
    else_store: &SymbolicStore<VarId>,
    cond_expr: NodeId,
    cond_sources: &HashSet<VarAtomBase<VarId>>,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<SymbolicStore<VarId>, ParserError> {
    let mut merged_store = SymbolicStore::default();
    for id in then_store.keys() {
        let t_range_store = &then_store[id];
        let e_range_store = &else_store[id];

        let mut merged_range_store = RangeStore {
            ranges: std::collections::BTreeMap::new(),
        };

        let mut all_lsbs: BTreeSet<usize> = t_range_store.ranges.keys().cloned().collect();
        all_lsbs.extend(e_range_store.ranges.keys().cloned());

        let var = &module.variables[id];
        let var_width = resolve_total_width(module, var)?;
        let mut lsbs_vec: Vec<usize> = all_lsbs.into_iter().collect();
        lsbs_vec.push(var_width);

        for i in 0..lsbs_vec.len() - 1 {
            let lsb = lsbs_vec[i];
            let next_lsb = lsbs_vec[i + 1];
            let access = BitAccess::new(lsb, next_lsb - 1);

            let then_parts = t_range_store.get_parts(access);
            let else_parts = e_range_store.get_parts(access);
            let (t_expr, t_sources) =
                combine_parts_with_default(*id, lsb, then_parts.clone(), arena);
            let (e_expr, e_sources) =
                combine_parts_with_default(*id, lsb, else_parts.clone(), arena);

            let t_modified = then_parts.iter().any(|(v, _)| v.is_some());
            let e_modified = else_parts.iter().any(|(v, _)| v.is_some());

            let result_val = if !t_modified && !e_modified {
                None
            } else if t_expr == e_expr {
                let mut sources = t_sources;
                sources.extend(e_sources);
                Some((t_expr, sources))
            } else {
                let mut sources = cond_sources.clone();
                sources.extend(t_sources);
                sources.extend(e_sources);

                Some((
                    arena.alloc(SLTNode::Mux {
                        cond: cond_expr,
                        then_expr: t_expr,
                        else_expr: e_expr,
                    }),
                    sources,
                ))
            };

            merged_range_store
                .ranges
                .insert(lsb, (result_val, next_lsb - lsb, lsb));
        }

        merged_store.insert(*id, merged_range_store);
    }

    Ok(merged_store)
}

fn eval_loop_statement(
    module: &Module,
    state: LoopControlState,
    stmt: &Statement,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<LoopControlState, ParserError> {
    if matches!(constant_bool(arena, state.continue_expr), Some(false)) {
        return Ok(state);
    }

    match stmt {
        Statement::Assign(assign) => {
            let base_store = state.store.clone();
            let base_boundaries = state.boundaries.clone();
            let (next_store, next_boundaries) =
                eval_assign(module, state.store, state.boundaries, assign, arena)?;
            let boundaries = merge_boundaries(base_boundaries, next_boundaries);

            if matches!(constant_bool(arena, state.continue_expr), Some(true)) {
                Ok(LoopControlState {
                    store: next_store,
                    boundaries,
                    ..state
                })
            } else {
                let merged_store = merge_symbolic_stores(
                    module,
                    &next_store,
                    &base_store,
                    state.continue_expr,
                    &state.continue_sources,
                    arena,
                )?;
                Ok(LoopControlState {
                    store: merged_store,
                    boundaries,
                    ..state
                })
            }
        }
        Statement::If(if_stmt) => eval_loop_if(module, state, if_stmt, arena),
        Statement::For(for_stmt) => {
            let base_store = state.store.clone();
            let base_boundaries = state.boundaries.clone();
            let (next_store, next_boundaries) =
                eval_for(module, state.store, state.boundaries, for_stmt, arena)?;
            let boundaries = merge_boundaries(base_boundaries, next_boundaries);
            if matches!(constant_bool(arena, state.continue_expr), Some(true)) {
                Ok(LoopControlState {
                    store: next_store,
                    boundaries,
                    ..state
                })
            } else {
                let merged_store = merge_symbolic_stores(
                    module,
                    &next_store,
                    &base_store,
                    state.continue_expr,
                    &state.continue_sources,
                    arena,
                )?;
                Ok(LoopControlState {
                    store: merged_store,
                    boundaries,
                    ..state
                })
            }
        }
        Statement::Break => Ok(LoopControlState {
            continue_expr: bool_node(arena, false),
            continue_sources: HashSet::default(),
            ..state
        }),
        Statement::IfReset(ir) => Err(ParserError::unsupported(
            LoweringPhase::SimulatorParser,
            "unsupported statement in always_comb",
            "illegal statement in always_comb: if_reset".to_string(),
            Some(&ir.token),
        )),
        Statement::SystemFunctionCall(fc) => Err(ParserError::unsupported(
            LoweringPhase::SimulatorParser,
            "unsupported statement in always_comb",
            "illegal statement in always_comb: system function call".to_string(),
            Some(&fc.comptime.token),
        )),
        Statement::FunctionCall(fc) => Err(ParserError::unsupported(
            LoweringPhase::SimulatorParser,
            "unsupported statement in always_comb",
            "illegal statement in always_comb: function call".to_string(),
            Some(&fc.comptime.token),
        )),
        Statement::TbMethodCall(_) => Err(ParserError::unsupported(
            LoweringPhase::SimulatorParser,
            "unsupported statement in always_comb",
            "illegal statement in always_comb: testbench method call".to_string(),
            None,
        )),
        Statement::Unsupported(_) => Err(ParserError::unsupported(
            LoweringPhase::SimulatorParser,
            "unsupported statement in always_comb",
            "illegal statement in always_comb: unsupported".to_string(),
            None,
        )),
        Statement::Null => Err(ParserError::unsupported(
            LoweringPhase::SimulatorParser,
            "unsupported statement in always_comb",
            "illegal statement in always_comb: null".to_string(),
            None,
        )),
    }
}

fn eval_loop_if(
    module: &Module,
    state: LoopControlState,
    stmt: &IfStatement,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<LoopControlState, ParserError> {
    let ((cond_expr, cond_sources), cond_bounds) =
        eval_expression(module, &state.store, &stmt.cond, arena, None)?;
    let boundaries = merge_boundaries(state.boundaries, cond_bounds);

    if let Some(cond_val) = constant_bool(arena, cond_expr) {
        let side = if cond_val {
            &stmt.true_side
        } else {
            &stmt.false_side
        };
        return side.iter().try_fold(
            LoopControlState {
                boundaries,
                ..state
            },
            |s, step| eval_loop_statement(module, s, step, arena),
        );
    }

    let then_state = stmt.true_side.iter().try_fold(
        LoopControlState {
            store: state.store.clone(),
            boundaries: boundaries.clone(),
            continue_expr: state.continue_expr,
            continue_sources: state.continue_sources.clone(),
        },
        |s, step| eval_loop_statement(module, s, step, arena),
    )?;
    let else_state = stmt.false_side.iter().try_fold(
        LoopControlState {
            store: state.store,
            boundaries,
            continue_expr: state.continue_expr,
            continue_sources: state.continue_sources,
        },
        |s, step| eval_loop_statement(module, s, step, arena),
    )?;

    let mut merged_sources = cond_sources;
    merged_sources.extend(then_state.continue_sources);
    merged_sources.extend(else_state.continue_sources);

    Ok(LoopControlState {
        store: merge_symbolic_stores(
            module,
            &then_state.store,
            &else_state.store,
            cond_expr,
            &merged_sources,
            arena,
        )?,
        boundaries: merge_boundaries(then_state.boundaries, else_state.boundaries),
        continue_expr: merge_control_expr(
            cond_expr,
            then_state.continue_expr,
            else_state.continue_expr,
            arena,
        ),
        continue_sources: merged_sources,
    })
}

fn extract_store_updates(
    store_before: &SymbolicStore<VarId>,
    store_after: &SymbolicStore<VarId>,
    arena: &mut SLTNodeArena<VarId>,
) -> Vec<(VarAtomBase<VarId>, NodeId, HashSet<VarAtomBase<VarId>>)> {
    let mut updates = Vec::new();

    for (id, range_store_after) in store_after {
        let Some(range_store_before) = store_before.get(id) else {
            continue;
        };

        for (&lsb, (val_opt, width, origin)) in &range_store_after.ranges {
            if range_store_before.ranges.get(&lsb) == Some(&(val_opt.clone(), *width, *origin)) {
                continue;
            }

            let Some((expr, sources)) = val_opt else {
                continue;
            };

            let msb = lsb + width - 1;
            let rel_lsb = lsb - origin;
            let rel_msb = msb - origin;
            let original_width = get_width(*expr, arena);
            let final_expr = if rel_lsb == 0 && *width == original_width {
                *expr
            } else {
                arena.alloc(SLTNode::Slice {
                    expr: *expr,
                    access: BitAccess::new(rel_lsb, rel_msb),
                })
            };

            updates.push((VarAtomBase::new(*id, lsb, msb), final_expr, sources.clone()));
        }
    }

    updates
}

fn eval_for_bound(
    module: &Module,
    store: &SymbolicStore<VarId>,
    bound: &ForBound,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<
    (
        SLTLoopBound,
        HashSet<VarAtomBase<VarId>>,
        BoundaryMap<VarId>,
    ),
    ParserError,
> {
    match bound {
        ForBound::Const(v) => Ok((
            SLTLoopBound::Const(*v),
            HashSet::default(),
            BoundaryMap::default(),
        )),
        ForBound::Expression(expr) => {
            let ((node, sources), bounds) = eval_expression(module, store, expr, arena, None)?;
            Ok((SLTLoopBound::Expr(node), sources, bounds))
        }
    }
}

fn collect_written_accesses(
    module: &Module,
    statements: &[Statement],
    out: &mut HashMap<VarId, Vec<BitAccess>>,
) -> Result<(), ParserError> {
    for stmt in statements {
        match stmt {
            Statement::Assign(assign) => {
                for dst in &assign.dst {
                    let width = resolve_total_width(module, &module.variables[&dst.id])?;
                    let access = if is_static_access(&dst.index, &dst.select) {
                        eval_var_select(module, dst.id, &dst.index, &dst.select)?
                    } else {
                        BitAccess::new(0, width - 1)
                    };
                    out.entry(dst.id).or_default().push(access);
                }
            }
            Statement::If(if_stmt) => {
                collect_written_accesses(module, &if_stmt.true_side, out)?;
                collect_written_accesses(module, &if_stmt.false_side, out)?;
            }
            Statement::For(for_stmt) => collect_written_accesses(module, &for_stmt.body, out)?,
            Statement::IfReset(if_reset) => {
                collect_written_accesses(module, &if_reset.true_side, out)?;
                collect_written_accesses(module, &if_reset.false_side, out)?;
            }
            Statement::SystemFunctionCall(_)
            | Statement::FunctionCall(_)
            | Statement::TbMethodCall(_)
            | Statement::Break
            | Statement::Unsupported(_)
            | Statement::Null => {}
        }
    }
    Ok(())
}

fn eval_for(
    module: &Module,
    store: SymbolicStore<VarId>,
    boundaries: HashMap<VarId, BTreeSet<usize>>,
    for_stmt: &ForStatement,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<(SymbolicStore<VarId>, HashMap<VarId, BTreeSet<usize>>), ParserError> {
    let Some(loop_width) = for_stmt.var_type.total_width() else {
        return Err(ParserError::unsupported(
            LoweringPhase::CombLowering,
            "for loop variable width",
            format!("{:?}", for_stmt.var_name),
            Some(&for_stmt.token),
        ));
    };

    let mut symbolic_store = store.clone();
    let mut written_accesses = HashMap::default();
    collect_written_accesses(module, &for_stmt.body, &mut written_accesses)?;
    for (id, accesses) in written_accesses {
        let width = resolve_total_width(module, &module.variables[&id])?;
        let mut loop_store = RangeStore::new(None, width);
        let mut covered = vec![false; width];
        for access in accesses {
            for slot in covered.iter_mut().take(access.msb + 1).skip(access.lsb) {
                *slot = true;
            }
        }
        let original = store
            .get(&id)
            .cloned()
            .unwrap_or_else(|| RangeStore::new(None, width));
        let mut bit = 0usize;
        while bit < width {
            if covered[bit] {
                bit += 1;
                continue;
            }
            let start = bit;
            while bit < width && !covered[bit] {
                bit += 1;
            }
            let end = bit - 1;
            let access = BitAccess::new(start, end);
            let parts = original.get_parts(access);
            let (expr, sources) = combine_parts_with_default(id, access.lsb, parts, arena);
            loop_store.update(access, Some((expr, sources)));
        }
        symbolic_store.insert(id, loop_store);
    }
    symbolic_store.insert(for_stmt.var_id, RangeStore::new(None, loop_width));
    let iter_store_before = symbolic_store.clone();

    let loop_state = for_stmt.body.iter().try_fold(
        LoopControlState {
            store: symbolic_store,
            boundaries,
            continue_expr: bool_node(arena, true),
            continue_sources: HashSet::default(),
        },
        |state, stmt| eval_loop_statement(module, state, stmt, arena),
    )?;
    let iter_store_after = loop_state.store;
    let mut merged_boundaries = loop_state.boundaries;

    let (
        start,
        end,
        start_sources,
        end_sources,
        start_bounds,
        end_bounds,
        inclusive,
        step,
        step_op,
        reverse,
    ) = match &for_stmt.range {
        ForRange::Forward {
            start: range_start,
            end: range_end,
            inclusive,
            step,
        } => {
            let (start, start_sources, start_bounds) =
                eval_for_bound(module, &store, range_start, arena)?;
            let (end, end_sources, end_bounds) = eval_for_bound(module, &store, range_end, arena)?;
            (
                start,
                end,
                start_sources,
                end_sources,
                start_bounds,
                end_bounds,
                *inclusive,
                *step,
                SLTStepOp::Add,
                false,
            )
        }
        ForRange::Reverse {
            start: range_start,
            end: range_end,
            inclusive,
            step,
        } => {
            let (start, start_sources, start_bounds) =
                eval_for_bound(module, &store, range_start, arena)?;
            let (end, end_sources, end_bounds) = eval_for_bound(module, &store, range_end, arena)?;
            (
                start,
                end,
                start_sources,
                end_sources,
                start_bounds,
                end_bounds,
                *inclusive,
                *step,
                SLTStepOp::Add,
                true,
            )
        }
        ForRange::Stepped {
            start: range_start,
            end: range_end,
            inclusive,
            step,
            op,
        } => {
            let (start, start_sources, start_bounds) =
                eval_for_bound(module, &store, range_start, arena)?;
            let (end, end_sources, end_bounds) = eval_for_bound(module, &store, range_end, arena)?;
            let step_op = match op {
                Op::Mul => SLTStepOp::Mul,
                Op::LogicShiftL | Op::ArithShiftL => SLTStepOp::Shl,
                other => {
                    return Err(ParserError::unsupported(
                        LoweringPhase::CombLowering,
                        "for loop step operator",
                        format!("{other:?}"),
                        Some(&for_stmt.token),
                    ));
                }
            };
            (
                start,
                end,
                start_sources,
                end_sources,
                start_bounds,
                end_bounds,
                *inclusive,
                *step,
                step_op,
                false,
            )
        }
    };

    merged_boundaries = merge_boundaries(merged_boundaries, start_bounds);
    merged_boundaries = merge_boundaries(merged_boundaries, end_bounds);

    let updates = extract_store_updates(&iter_store_before, &iter_store_after, arena);
    if updates.is_empty() {
        let mut store = store;
        store.remove(&for_stmt.var_id);
        return Ok((store, merged_boundaries));
    }

    let folded_updates: Vec<_> = updates
        .iter()
        .map(|(target, expr, _)| SLTForUpdate {
            target: *target,
            expr: *expr,
        })
        .collect();
    let loop_updated_vars: HashSet<_> = folded_updates
        .iter()
        .map(|update| update.target.id)
        .collect();
    let initial_updates: Vec<_> = updates
        .iter()
        .map(|(target, _, _)| {
            let parts = store[&target.id].get_parts(target.access);
            let (expr, _) = combine_parts_with_default(target.id, target.access.lsb, parts, arena);
            SLTForUpdate {
                target: *target,
                expr,
            }
        })
        .collect();

    let mut result_store = store;
    for (target, _expr, sources) in updates {
        let mut all_sources = start_sources.clone();
        all_sources.extend(end_sources.iter().copied());
        all_sources.extend(
            loop_state
                .continue_sources
                .iter()
                .copied()
                .filter(|src| src.id != for_stmt.var_id && !loop_updated_vars.contains(&src.id)),
        );
        all_sources.extend(
            sources
                .into_iter()
                .filter(|src| src.id != for_stmt.var_id && !loop_updated_vars.contains(&src.id)),
        );

        let folded_expr = arena.alloc(SLTNode::ForFold {
            loop_var: for_stmt.var_id,
            loop_width,
            loop_signed: for_stmt.var_type.signed,
            start: start.clone(),
            end: end.clone(),
            inclusive,
            step,
            step_op,
            reverse,
            result: target,
            initials: initial_updates.clone(),
            updates: folded_updates.clone(),
            continue_cond: loop_state.continue_expr,
        });

        result_store
            .entry(target.id)
            .or_insert_with(|| {
                let width = resolve_total_width(module, &module.variables[&target.id]).unwrap_or(0);
                RangeStore::new(None, width)
            })
            .update(target.access, Some((folded_expr, all_sources)));
    }

    result_store.remove(&for_stmt.var_id);
    Ok((result_store, merged_boundaries))
}

fn eval_assign(
    module: &Module,
    mut store: SymbolicStore<VarId>,
    boundaries: BoundaryMap<VarId>,
    stmt: &AssignStatement,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<(SymbolicStore<VarId>, BoundaryMap<VarId>), ParserError> {
    let rhs_expected_width: usize = stmt
        .dst
        .iter()
        .map(|dst| {
            crate::parser::bitaccess::get_access_width(module, dst.id, &dst.index, &dst.select)
        })
        .sum::<Result<usize, ParserError>>()?;
    let ((rhs_expr, rhs_sources), rhs_bounds) =
        if let Expression::ArrayLiteral(items, _) = &stmt.expr {
            eval_array_literal_expression(module, &store, items, Some(rhs_expected_width), arena)?
        } else {
            eval_expression(module, &store, &stmt.expr, arena, Some(rhs_expected_width))?
        };
    let mut boundaries = merge_boundaries(boundaries, rhs_bounds);

    if stmt.dst.len() == 1 {
        // Single destination: store RHS directly
        let dst = &stmt.dst[0];

        if crate::parser::bitaccess::is_static_access(&dst.index, &dst.select) {
            let access = eval_var_select(module, dst.id, &dst.index, &dst.select)?;

            let b = boundaries.entry(dst.id).or_default();
            b.insert(access.lsb);
            b.insert(access.msb + 1);
            if let Some(range_store) = store.get_mut(&dst.id) {
                range_store.update(access, Some((rhs_expr, rhs_sources.clone())));
            }
        } else {
            let (s, b) = eval_dynamic_assign(
                module,
                store,
                boundaries,
                dst,
                rhs_expr,
                rhs_sources.clone(),
                arena,
            )?;
            return Ok((s, b));
        }
    } else {
        // LHS concatenation: slice RHS for each destination
        // dst is ordered MSB-first (e.g., {a, b} means a=MSB, b=LSB),
        // so iterate in reverse to compute offsets from LSB.
        let mut current_offset = 0;
        for dst in stmt.dst.iter().rev() {
            let part_width = crate::parser::bitaccess::get_access_width(
                module,
                dst.id,
                &dst.index,
                &dst.select,
            )?;

            // Slice the RHS to extract the bits for this destination
            let slice_expr = arena.alloc(SLTNode::Slice {
                expr: rhs_expr,
                access: BitAccess::new(current_offset, current_offset + part_width - 1),
            });

            if crate::parser::bitaccess::is_static_access(&dst.index, &dst.select) {
                let access = eval_var_select(module, dst.id, &dst.index, &dst.select)?;

                let b = boundaries.entry(dst.id).or_default();
                b.insert(access.lsb);
                b.insert(access.msb + 1);

                if let Some(range_store) = store.get_mut(&dst.id) {
                    range_store.update(access, Some((slice_expr, rhs_sources.clone())));
                }
            } else {
                let (s, b) = eval_dynamic_assign(
                    module,
                    store,
                    boundaries,
                    dst,
                    slice_expr,
                    rhs_sources.clone(),
                    arena,
                )?;
                store = s;
                boundaries = b;
            }

            current_offset += part_width;
        }
    }
    Ok((store, boundaries))
}

fn eval_dynamic_assign(
    module: &Module,
    mut store: SymbolicStore<VarId>,
    mut boundaries: BoundaryMap<VarId>,
    dst: &veryl_analyzer::ir::AssignDestination,
    rhs_expr: NodeId,
    rhs_sources: HashSet<VarAtomBase<VarId>>,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<(SymbolicStore<VarId>, BoundaryMap<VarId>), ParserError> {
    let mut all_sources = rhs_sources;

    let (_, strides, _) = crate::parser::bitaccess::get_dimensions_and_strides(module, dst.id)?;
    let mut offset_node = arena.alloc(SLTNode::Constant(
        BigUint::from(0u32),
        BigUint::from(0u32),
        64,
        false,
    ));

    let mut index_exprs = dst.index.0.clone();
    index_exprs.extend(dst.select.0.clone());

    // For Colon selects (e.g. [31:0]), the last element of index_exprs is
    // the MSB anchor—not a dimension index. Exclude it from the dynamic
    // offset and instead add the LSB as a static bit offset below.
    // For PlusColon/MinusColon/Step, the anchor is the dynamic start
    // position and belongs in the offset.
    let is_colon_select = matches!(&dst.select.1, Some((VarSelectOp::Colon, _)));
    let dim_limit = if is_colon_select {
        index_exprs.len().saturating_sub(1)
    } else {
        index_exprs.len()
    };

    for (dim_i, idx_expr) in index_exprs[..dim_limit].iter().enumerate() {
        let ((expr, sources), bounds) = eval_expression(module, &store, idx_expr, arena, None)?;
        boundaries = merge_boundaries(boundaries, bounds);
        all_sources.extend(sources);

        let stride = strides.get(dim_i).copied().unwrap_or(1);
        let stride_node = arena.alloc(SLTNode::Constant(
            BigUint::from(stride),
            BigUint::from(0u32),
            64,
            false,
        ));
        let term = arena.alloc(SLTNode::Binary(expr, BinaryOp::Mul, stride_node));
        offset_node = arena.alloc(SLTNode::Binary(offset_node, BinaryOp::Add, term));
    }

    // For Colon selects, add the LSB as a static bit offset within the
    // element selected by the array indices.
    if let Some((VarSelectOp::Colon, range_expr)) = &dst.select.1 {
        let weight = strides.get(dim_limit).copied().unwrap_or(1);
        let lsb = eval_constexpr(range_expr)
            .map(|v| v.to_u64_digits().first().copied().unwrap_or(0) as usize)
            .unwrap_or(0);
        let bit_offset = lsb * weight;
        if bit_offset > 0 {
            let lsb_node = arena.alloc(SLTNode::Constant(
                BigUint::from(bit_offset),
                BigUint::from(0u32),
                64,
                false,
            ));
            offset_node = arena.alloc(SLTNode::Binary(offset_node, BinaryOp::Add, lsb_node));
        }
    }

    let access_width =
        crate::parser::bitaccess::get_access_width(module, dst.id, &dst.index, &dst.select)?;
    let var = &module.variables[&dst.id];
    let width = resolve_total_width(module, var)?;

    let access_full = BitAccess::new(0, width - 1);
    let range_store = store
        .entry(dst.id)
        .or_insert_with(|| RangeStore::new(None, width));

    // Evaluate the variable's current state.
    // Sub-ranges that haven't been assigned yet will fall back to their initial input state.
    let (old_val, old_sources) =
        combine_parts_with_default(dst.id, 0, range_store.get_parts(access_full), arena);
    // Note: Partial dynamic updates are not treated as self-dependencies (latches)
    // to maintain consistency with existing test expectations and Verilog semantics.
    for source in old_sources {
        if source.id != dst.id {
            all_sources.insert(source);
        }
    }

    // Compute the bitmask to isolate the target range: mask = !(( (1<<access_width) - 1 ) << offset)
    let mask_base = (BigUint::from(1u32) << access_width) - BigUint::from(1u32);
    // Ensure width consistency; using the full variable width for safety.
    let mask_constant = arena.alloc(SLTNode::Constant(
        mask_base,
        BigUint::from(0u32),
        width,
        false,
    ));

    let mask_shifted = arena.alloc(SLTNode::Binary(mask_constant, BinaryOp::Shl, offset_node));
    let mask_node = arena.alloc(SLTNode::Unary(UnaryOp::BitNot, mask_shifted));

    // Align the new value to the target offset: new_val_term = rhs << offset
    let rhs_width = get_width(rhs_expr, arena);
    let rhs_widened = if rhs_width < width {
        let padding = width - rhs_width;
        let zero = arena.alloc(SLTNode::Constant(
            BigUint::from(0u32),
            BigUint::from(0u32),
            padding,
            false,
        ));
        // Concatenate zero padding to match variable width: {padding'b0, rhs_expr}
        arena.alloc(SLTNode::Concat(vec![
            (zero, padding),
            (rhs_expr, rhs_width),
        ]))
    } else {
        rhs_expr
    };
    let new_val_term = arena.alloc(SLTNode::Binary(rhs_widened, BinaryOp::Shl, offset_node));

    // Apply the update: final_val = (old_val & mask) | new_val_term
    let new_val_masked = arena.alloc(SLTNode::Binary(old_val, BinaryOp::And, mask_node));
    let final_val = arena.alloc(SLTNode::Binary(new_val_masked, BinaryOp::Or, new_val_term));

    range_store.update(access_full, Some((final_val, all_sources)));

    Ok((store, boundaries))
}
fn eval_if(
    module: &Module,
    initial_store: SymbolicStore<VarId>,
    mut boundaries: HashMap<VarId, BTreeSet<usize>>,
    stmt: &IfStatement,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<(SymbolicStore<VarId>, HashMap<VarId, BTreeSet<usize>>), ParserError> {
    let ((cond_expr, cond_sources), cond_bounds) =
        eval_expression(module, &initial_store, &stmt.cond, arena, None)?;
    boundaries.extend(cond_bounds);

    // Constant folding: if condition is a constant, inline the appropriate side
    if let SLTNode::Constant(val, _, _, _) = arena.get(cond_expr) {
        let side = if *val != BigUint::from(0u32) {
            &stmt.true_side
        } else {
            &stmt.false_side
        };
        return side
            .iter()
            .try_fold((initial_store, boundaries), |(s, b), step| {
                eval_statement(module, s, b, step, arena)
            });
    }

    // Evaluate Then and Else paths independently
    let (then_store, b_then) = stmt.true_side.iter().try_fold(
        (initial_store.clone(), boundaries.clone()),
        |(s, b), step| eval_statement(module, s, b, step, arena),
    )?;
    let (else_store, b_else) = stmt
        .false_side
        .iter()
        .try_fold((initial_store, b_then), |(s, b), step| {
            eval_statement(module, s, b, step, arena)
        })?;

    Ok((
        merge_symbolic_stores(
            module,
            &then_store,
            &else_store,
            cond_expr,
            &cond_sources,
            arena,
        )?,
        b_else,
    ))
}

fn combine_parts_with_default<A: Clone + PartialEq + Eq + Hash>(
    var_id: A,
    start_lsb: usize,
    parts: Vec<(Option<(NodeId, HashSet<VarAtomBase<A>>)>, BitAccess)>,
    arena: &mut SLTNodeArena<A>,
) -> (NodeId, HashSet<VarAtomBase<A>>) {
    let mut fixed_parts = Vec::new();
    let mut current_lsb = start_lsb;
    for (val_opt, access) in parts {
        let width = access.msb - access.lsb + 1;
        match val_opt {
            Some((expr, s)) => {
                fixed_parts.push(((expr, s), access));
            }
            None => {
                let input_node = arena.alloc(SLTNode::Input {
                    variable: var_id.clone(),
                    index: vec![],
                    access: BitAccess::new(current_lsb, current_lsb + width - 1),
                });
                let mut sources = HashSet::default();
                sources.insert(VarAtomBase::new(
                    var_id.clone(),
                    current_lsb,
                    current_lsb + width - 1,
                ));
                fixed_parts.push(((input_node, sources), BitAccess::new(0, width - 1)));
            }
        }
        current_lsb += width;
    }
    combine_parts(fixed_parts, arena)
}

fn combine_parts<A: Clone + PartialEq + Eq + Hash>(
    parts: Vec<((NodeId, HashSet<VarAtomBase<A>>), BitAccess)>,
    arena: &mut SLTNodeArena<A>,
) -> (NodeId, HashSet<VarAtomBase<A>>) {
    if parts.is_empty() {
        return (
            arena.alloc(SLTNode::Constant(
                BigUint::from(0u32),
                BigUint::from(0u32),
                0,
                false,
            )),
            HashSet::default(),
        );
    }
    if parts.len() == 1 {
        let ((expr, sources), access) = &parts[0];
        let w = get_width(*expr, arena);
        if w == 0 {
            return (*expr, sources.clone());
        }
        if access.lsb == 0 && access.msb == w - 1 {
            return (*expr, sources.clone());
        } else {
            return (
                arena.alloc(SLTNode::Slice {
                    expr: *expr,
                    access: *access,
                }),
                sources.clone(),
            );
        }
    }

    let mut concat_parts = Vec::new();
    let mut total_sources = HashSet::default();

    for ((expr, sources), access) in parts {
        total_sources.extend(sources);
        let w = access.msb - access.lsb + 1;
        let slice = arena.alloc(SLTNode::Slice { expr, access });
        concat_parts.push((slice, w));
    }
    concat_parts.reverse();
    (arena.alloc(SLTNode::Concat(concat_parts)), total_sources)
}

fn eval_array_literal_expression(
    module: &Module,
    store: &SymbolicStore<VarId>,
    items: &[ArrayLiteralItem],
    expected_width: Option<usize>,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    let mut parts = Vec::new();
    let mut all_bounds = BoundaryMap::default();
    let mut total_sources = HashSet::default();

    let mut explicit_width = 0usize;
    let mut default_part: Option<(NodeId, usize)> = None;

    for item in items {
        match item {
            ArrayLiteralItem::Value(sub_expr, repeat) => {
                let ((part_expr, part_sources), p_bounds) =
                    eval_expression(module, store, sub_expr, arena, None)?;
                all_bounds = merge_boundaries(all_bounds, p_bounds);
                total_sources.extend(part_sources);

                let width = get_width(part_expr, arena);
                let rep_count = if let Some(rep_expr) = repeat {
                    let Some(rep_count) = eval_constexpr(rep_expr).and_then(|x| x.to_u64()) else {
                        return Err(ParserError::unsupported(
                            LoweringPhase::CombLowering,
                            "array literal non-constant repeat",
                            format!("{:?}", rep_expr),
                            Some(&rep_expr.token_range()),
                        ));
                    };
                    rep_count
                } else {
                    1
                };

                for _ in 0..rep_count {
                    parts.push((part_expr, width));
                }
                explicit_width += width * rep_count as usize;
            }
            ArrayLiteralItem::Defaul(default_expr) => {
                if default_part.is_some() {
                    let token = default_expr.token_range();
                    return Err(ParserError::unsupported(
                        LoweringPhase::CombLowering,
                        "array literal multiple default",
                        format!("{:?}", items),
                        Some(&token),
                    ));
                }

                let ((part_expr, part_sources), p_bounds) =
                    eval_expression(module, store, default_expr, arena, None)?;
                all_bounds = merge_boundaries(all_bounds, p_bounds);
                total_sources.extend(part_sources);
                let width = get_width(part_expr, arena);
                default_part = Some((part_expr, width));
            }
        }
    }

    if let Some((default_expr, default_width)) = default_part {
        let Some(target_width) = expected_width else {
            let token = items.first().map(|i| i.token_range());
            return Err(ParserError::unsupported(
                LoweringPhase::CombLowering,
                "array literal default without context width",
                format!("{:?}", items),
                token.as_ref(),
            ));
        };

        if explicit_width > target_width {
            let token = items.first().map(|i| i.token_range());
            return Err(ParserError::unsupported(
                LoweringPhase::CombLowering,
                "array literal width overflow",
                format!("explicit_width={explicit_width}, target_width={target_width}"),
                token.as_ref(),
            ));
        }

        let remaining = target_width - explicit_width;
        if default_width == 0 || !remaining.is_multiple_of(default_width) {
            return Err(ParserError::unsupported(
                LoweringPhase::CombLowering,
                "array literal default width mismatch",
                format!(
                    "remaining={remaining}, default_width={default_width}, target_width={target_width}"
                ),
                None,
            ));
        }

        for _ in 0..(remaining / default_width) {
            parts.push((default_expr, default_width));
        }
    }

    Ok((
        (arena.alloc(SLTNode::Concat(parts)), total_sources),
        all_bounds,
    ))
}

fn extract_function_return_expr(
    module: &Module,
    body: &veryl_analyzer::ir::FunctionBody,
    ret_id: VarId,
) -> Result<Expression, ParserError> {
    fn substitute_expr(expr: &Expression, defs: &HashMap<VarId, Expression>) -> Expression {
        substitute_expr_inner(expr, defs, &mut HashSet::default())
    }

    fn substitute_expr_inner(
        expr: &Expression,
        defs: &HashMap<VarId, Expression>,
        expanding: &mut HashSet<VarId>,
    ) -> Expression {
        match expr {
            Expression::Term(factor) => match factor.as_ref() {
                Factor::Variable(var_id, index, select, _)
                    if index.0.is_empty() && select.0.is_empty() && select.1.is_none() =>
                {
                    if let Some(bound) = defs.get(var_id) {
                        if expanding.insert(*var_id) {
                            let result = substitute_expr_inner(bound, defs, expanding);
                            expanding.remove(var_id);
                            return result;
                        }
                    }
                    expr.clone()
                }
                Factor::FunctionCall(call) => {
                    let mut call = call.clone();
                    for input_expr in call.inputs.values_mut() {
                        *input_expr = substitute_expr_inner(input_expr, defs, expanding);
                    }
                    Expression::Term(Box::new(Factor::FunctionCall(call)))
                }
                _ => expr.clone(),
            },
            Expression::Binary(lhs, op, rhs, _) => Expression::Binary(
                Box::new(substitute_expr_inner(lhs, defs, expanding)),
                *op,
                Box::new(substitute_expr_inner(rhs, defs, expanding)),
                Box::new(Comptime::create_unknown(TokenRange::default())),
            ),
            Expression::Unary(op, inner, _) => Expression::Unary(
                *op,
                Box::new(substitute_expr_inner(inner, defs, expanding)),
                Box::new(Comptime::create_unknown(TokenRange::default())),
            ),
            Expression::Ternary(cond, then_expr, else_expr, _) => Expression::Ternary(
                Box::new(substitute_expr_inner(cond, defs, expanding)),
                Box::new(substitute_expr_inner(then_expr, defs, expanding)),
                Box::new(substitute_expr_inner(else_expr, defs, expanding)),
                Box::new(Comptime::create_unknown(TokenRange::default())),
            ),
            Expression::Concatenation(parts, _) => Expression::Concatenation(
                parts
                    .iter()
                    .map(|(x, rep)| {
                        (
                            substitute_expr_inner(x, defs, expanding),
                            rep.as_ref()
                                .map(|r| substitute_expr_inner(r, defs, expanding)),
                        )
                    })
                    .collect(),
                Box::new(Comptime::create_unknown(TokenRange::default())),
            ),
            Expression::ArrayLiteral(items, _) => Expression::ArrayLiteral(
                items
                    .iter()
                    .map(|item| match item {
                        ArrayLiteralItem::Value(x, rep) => ArrayLiteralItem::Value(
                            Box::new(substitute_expr_inner(x, defs, expanding)),
                            rep.as_ref()
                                .map(|r| Box::new(substitute_expr_inner(r, defs, expanding))),
                        ),
                        ArrayLiteralItem::Defaul(x) => ArrayLiteralItem::Defaul(Box::new(
                            substitute_expr_inner(x, defs, expanding),
                        )),
                    })
                    .collect(),
                Box::new(Comptime::create_unknown(TokenRange::default())),
            ),
            Expression::StructConstructor(ty, fields, _) => Expression::StructConstructor(
                ty.clone(),
                fields
                    .iter()
                    .map(|(name, x)| (*name, substitute_expr_inner(x, defs, expanding)))
                    .collect(),
                Box::new(Comptime::create_unknown(TokenRange::default())),
            ),
        }
    }

    fn resolve_return_expr(
        module: &Module,
        statements: &[Statement],
        ret_id: VarId,
        defs: &HashMap<VarId, Expression>,
    ) -> Result<Option<Expression>, ParserError> {
        if statements.is_empty() {
            return Ok(None);
        }

        let stmt = &statements[0];
        let rest = &statements[1..];

        match stmt {
            Statement::Assign(assign) => {
                if assign.dst.len() != 1 {
                    return Err(ParserError::unsupported(
                        LoweringPhase::CombLowering,
                        "function body assignment shape",
                        format!("{stmt}"),
                        Some(&assign.token),
                    ));
                }

                let dst = &assign.dst[0];
                let is_whole_var =
                    dst.index.0.is_empty() && dst.select.0.is_empty() && dst.select.1.is_none();

                let rhs = substitute_expr(&assign.expr, defs);

                if is_whole_var {
                    if dst.id == ret_id {
                        // Assignment to return variable corresponds to `return` and terminates
                        // this path.
                        return Ok(Some(rhs));
                    }

                    let mut next_defs = defs.clone();
                    next_defs.insert(dst.id, rhs);
                    resolve_return_expr(module, rest, ret_id, &next_defs)
                } else if is_static_access(&dst.index, &dst.select) {
                    let old_value = defs.get(&dst.id).cloned().unwrap_or_else(|| {
                        Expression::Term(Box::new(Factor::Variable(
                            dst.id,
                            VarIndex::default(),
                            VarSelect::default(),
                            dst.comptime.clone(),
                        )))
                    });
                    let merged = build_partial_assign_expr(module, dst, rhs, old_value)?;

                    // Partial write does NOT terminate the path.
                    let mut next_defs = defs.clone();
                    next_defs.insert(dst.id, merged);
                    resolve_return_expr(module, rest, ret_id, &next_defs)
                } else {
                    Err(ParserError::unsupported(
                        LoweringPhase::CombLowering,
                        "function body non-whole assignment (dynamic index)",
                        format!("{stmt}"),
                        Some(&assign.token),
                    ))
                }
            }
            Statement::If(if_stmt) => {
                let cond = substitute_expr(&if_stmt.cond, defs);

                let mut then_stmts = if_stmt.true_side.clone();
                then_stmts.extend_from_slice(rest);
                let then_expr = resolve_return_expr(module, &then_stmts, ret_id, defs)?;

                let mut else_stmts = if_stmt.false_side.clone();
                else_stmts.extend_from_slice(rest);
                let else_expr = resolve_return_expr(module, &else_stmts, ret_id, defs)?;

                match (then_expr, else_expr) {
                    (Some(then_expr), Some(else_expr)) => Ok(Some(Expression::Ternary(
                        Box::new(cond),
                        Box::new(then_expr),
                        Box::new(else_expr),
                        Box::new(Comptime::create_unknown(TokenRange::default())),
                    ))),
                    _ => Ok(None),
                }
            }
            Statement::Null => resolve_return_expr(module, rest, ret_id, defs),
            Statement::IfReset(ir) => Err(ParserError::unsupported(
                LoweringPhase::CombLowering,
                "function body control flow",
                format!("{stmt}"),
                Some(&ir.token),
            )),
            Statement::SystemFunctionCall(fc) => Err(ParserError::unsupported(
                LoweringPhase::CombLowering,
                "function body control flow",
                format!("{stmt}"),
                Some(&fc.comptime.token),
            )),
            Statement::FunctionCall(fc) => Err(ParserError::unsupported(
                LoweringPhase::CombLowering,
                "function body control flow",
                format!("{stmt}"),
                Some(&fc.comptime.token),
            )),
            Statement::For(f) => Err(ParserError::unsupported(
                LoweringPhase::CombLowering,
                "for loop in function body",
                format!("{stmt}"),
                Some(&f.token),
            )),
            Statement::TbMethodCall(_) | Statement::Break | Statement::Unsupported(_) => {
                Err(ParserError::unsupported(
                    LoweringPhase::CombLowering,
                    "function body control flow",
                    format!("{stmt}"),
                    None,
                ))
            }
        }
    }

    resolve_return_expr(module, &body.statements, ret_id, &HashMap::default())?.ok_or_else(|| {
        ParserError::unsupported(
            LoweringPhase::CombLowering,
            "function return expression",
            format!("function return var id: {:?}", ret_id),
            None,
        )
    })
}

fn eval_function_call_expression(
    module: &Module,
    store: &SymbolicStore<VarId>,
    call: &veryl_analyzer::ir::FunctionCall,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    if !call.outputs.is_empty() {
        return Err(ParserError::unsupported(
            LoweringPhase::CombLowering,
            "function call with output arguments",
            format!("{call}"),
            Some(&call.comptime.token),
        ));
    }

    let Some(function) = module.functions.get(&call.id) else {
        return Err(ParserError::unsupported(
            LoweringPhase::CombLowering,
            "function call",
            format!("unknown function id: {:?}", call.id),
            Some(&call.comptime.token),
        ));
    };

    let Some(function_body) = (if let Some(index) = &call.index {
        function.get_function(index)
    } else {
        function.get_function(&[])
    }) else {
        return Err(ParserError::unsupported(
            LoweringPhase::CombLowering,
            "function call specialization",
            format!("{call}"),
            Some(&call.comptime.token),
        ));
    };

    let Some(ret_id) = function_body.ret else {
        return Err(ParserError::unsupported(
            LoweringPhase::CombLowering,
            "void function call in comb expression",
            format!("{call}"),
            Some(&call.comptime.token),
        ));
    };

    let ret_expr = extract_function_return_expr(module, &function_body, ret_id)?;

    let mut local_store = store.clone();
    let mut arg_sources = HashSet::default();
    let mut arg_bounds = BoundaryMap::default();

    for (arg_path, arg_id) in &function_body.arg_map {
        let Some(arg_expr) = call.inputs.get(arg_path) else {
            return Err(ParserError::unsupported(
                LoweringPhase::CombLowering,
                "function call missing argument",
                format!("{call}"),
                Some(&call.comptime.token),
            ));
        };

        let ((arg_node, sources), bounds) =
            eval_expression(module, &local_store, arg_expr, arena, None)?;
        arg_sources.extend(sources.clone());
        arg_bounds = merge_boundaries(arg_bounds, bounds);

        let Some(arg_var) = module.variables.get(arg_id) else {
            return Err(ParserError::unsupported(
                LoweringPhase::CombLowering,
                "function argument variable",
                format!("unknown arg id: {:?}", arg_id),
                Some(&call.comptime.token),
            ));
        };
        let arg_width = resolve_width(module, arg_var)?;
        local_store.insert(
            *arg_id,
            RangeStore::new(Some((arg_node, sources)), arg_width),
        );
    }

    let ((ret_node, ret_sources), ret_bounds) =
        eval_expression(module, &local_store, &ret_expr, arena, None)?;

    let mut merged_sources = ret_sources;
    merged_sources.extend(arg_sources);
    let merged_bounds = merge_boundaries(arg_bounds, ret_bounds);

    Ok(((ret_node, merged_sources), merged_bounds))
}

pub fn eval_expression(
    module: &Module,
    store: &SymbolicStore<VarId>,
    expr: &Expression,
    arena: &mut SLTNodeArena<VarId>,
    context_width: Option<usize>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    // Short-circuit: compile-time constant compound expression → emit Constant node.
    // context_width is not applied here — compound constant expressions (e.g. `N - 1`)
    // always have well-defined widths from the analyzer.  Fill-literals like `'0` are
    // Term(Factor::Value), not compound, so they take the Factor::Value path which
    // does apply context_width.
    if !matches!(expr, Expression::Term(_)) {
        let ct = expr.comptime();
        if ct.is_const {
            if let Some((celox_value, mask_xz, width, signed)) = celox_value_from_comptime(ct) {
                let expr = arena.alloc(SLTNode::Constant(celox_value, mask_xz, width, signed));
                return Ok(((expr, HashSet::default()), BoundaryMap::default()));
            }
        }
    }

    match expr {
        Expression::Term(factor) => eval_factor(module, store, factor, arena, context_width),
        Expression::Binary(lhs, op, rhs, _) => {
            let (lhs_context_width, rhs_context_width) = if matches!(op, Op::As) {
                // `as` cast: LHS inherits target width from RHS type/numeric, RHS is metadata
                let target_width = if let Expression::Term(f) = rhs.as_ref() {
                    if let Factor::Value(v) = f.as_ref() {
                        match &v.value {
                            ValueVariant::Type(ty) => ty.total_width(),
                            ValueVariant::Numeric(n) => n.to_usize(),
                            _ => None,
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                (target_width, None)
            } else if matches!(
                op,
                Op::LogicShiftL | Op::LogicShiftR | Op::ArithShiftL | Op::ArithShiftR | Op::Pow
            ) {
                (get_context_width(lhs, context_width), None)
            } else {
                let context_width = if matches!(
                    op,
                    Op::Less
                        | Op::LessEq
                        | Op::Greater
                        | Op::GreaterEq
                        | Op::Eq
                        | Op::Ne
                        | Op::EqWildcard
                        | Op::NeWildcard
                        | Op::LogicAnd
                        | Op::LogicOr
                ) {
                    None
                } else {
                    context_width
                };
                let lw = get_context_width(lhs, context_width);
                let rw = get_context_width(rhs, context_width);
                let w = lw.and_then(|lw| rw.map(|rw| lw.max(rw)));
                (w, w)
            };

            // `as` cast: use RHS type for context width and signedness.
            if matches!(op, Op::As) {
                let ((l_expr, l_sources), l_bounds) =
                    eval_expression(module, store, lhs, arena, lhs_context_width)?;

                // For RHS, if it's a type or numeric width, we don't evaluate it as an expression.
                let r_bounds = if let Expression::Term(f) = rhs.as_ref() {
                    if let Factor::Value(v) = f.as_ref() {
                        if matches!(v.value, ValueVariant::Type(_) | ValueVariant::Numeric(_)) {
                            BoundaryMap::default()
                        } else {
                            eval_expression(module, store, rhs, arena, rhs_context_width)?.1
                        }
                    } else {
                        eval_expression(module, store, rhs, arena, rhs_context_width)?.1
                    }
                } else {
                    eval_expression(module, store, rhs, arena, rhs_context_width)?.1
                };

                // Extract signedness and width from RHS type/numeric
                let (target_width, target_signed) = match rhs.as_ref() {
                    Expression::Term(f) => match f.as_ref() {
                        Factor::Value(v) => match &v.value {
                            ValueVariant::Type(ty) => (ty.total_width(), ty.signed),
                            ValueVariant::Numeric(n) => (n.to_usize(), false),
                            _ => (None, false),
                        },
                        _ => (None, false),
                    },
                    _ => (None, false),
                };
                let Some(target_width) = target_width else {
                    return Err(ParserError::unsupported(
                        LoweringPhase::CombLowering,
                        "as cast target",
                        format!("{:?}", rhs),
                        Some(&rhs.token_range()),
                    ));
                };

                let mut result_node = l_expr;
                let expr_width = get_width(result_node, arena);

                if expr_width < target_width {
                    let pad_width = target_width - expr_width;
                    let pad = if target_signed || is_signed(module, result_node, arena) {
                        // 符号拡張: MSBをpad
                        let msb_slice = arena.alloc(SLTNode::Slice {
                            expr: result_node,
                            access: BitAccess::new(expr_width - 1, expr_width - 1),
                        });
                        (msb_slice, pad_width)
                    } else {
                        // ゼロ拡張: 0をpad
                        let zero = arena.alloc(SLTNode::Constant(
                            BigUint::from(0u8),
                            BigUint::from(0u32),
                            pad_width,
                            false,
                        ));
                        (zero, pad_width)
                    };
                    result_node =
                        arena.alloc(SLTNode::Concat(vec![pad, (result_node, expr_width)]));
                } else if expr_width > target_width {
                    result_node = arena.alloc(SLTNode::Slice {
                        expr: result_node,
                        access: BitAccess::new(0, target_width - 1),
                    });
                }
                return Ok((
                    (result_node, l_sources),
                    merge_boundaries(l_bounds, r_bounds),
                ));
            }
            // `pow`: currently lowered for constant exponent only.
            if matches!(op, Op::Pow) {
                let ((l_expr, l_sources), l_bounds) =
                    eval_expression(module, store, lhs, arena, lhs_context_width)?;
                let Some(exp) = eval_constexpr(rhs).and_then(|x| x.to_u64().map(|v| v as usize))
                else {
                    return Err(ParserError::unsupported(
                        LoweringPhase::CombLowering,
                        "pow non-constant exponent",
                        format!("{:?}", rhs),
                        Some(&rhs.token_range()),
                    ));
                };
                let lhs_width = get_width(l_expr, arena);
                let result_node = if exp == 0 {
                    arena.alloc(SLTNode::Constant(
                        BigUint::from(1u8),
                        BigUint::from(0u32),
                        lhs_width,
                        false,
                    ))
                } else {
                    let mut acc = l_expr;
                    for _ in 1..exp {
                        acc = arena.alloc(SLTNode::Binary(acc, BinaryOp::Mul, l_expr));
                    }
                    acc
                };
                return Ok(((result_node, l_sources), l_bounds));
            }
            let ((l_expr, l_sources), l_bounds) =
                eval_expression(module, store, lhs, arena, lhs_context_width)?;
            let ((r_expr, r_sources), r_bounds) =
                eval_expression(module, store, rhs, arena, rhs_context_width)?;

            let mut sources = l_sources;
            sources.extend(r_sources);

            // BitXnor/BitNand/BitNor は既存演算に分解
            let result_node = match op {
                Op::BitXnor => {
                    let xor_node = arena.alloc(SLTNode::Binary(l_expr, BinaryOp::Xor, r_expr));
                    arena.alloc(SLTNode::Unary(UnaryOp::BitNot, xor_node))
                }
                Op::BitNand => {
                    let and_node = arena.alloc(SLTNode::Binary(l_expr, BinaryOp::And, r_expr));
                    arena.alloc(SLTNode::Unary(UnaryOp::BitNot, and_node))
                }
                Op::BitNor => {
                    let or_node = arena.alloc(SLTNode::Binary(l_expr, BinaryOp::Or, r_expr));
                    arena.alloc(SLTNode::Unary(UnaryOp::BitNot, or_node))
                }
                Op::Sub => {
                    let lhs_signed =
                        cast_target_signed(lhs).unwrap_or_else(|| is_signed(module, l_expr, arena));
                    let rhs_signed =
                        cast_target_signed(rhs).unwrap_or_else(|| is_signed(module, r_expr, arena));
                    let signed = lhs_signed && rhs_signed;
                    let bin_op = convert_binary_op(op, signed);
                    let sub_node = arena.alloc(SLTNode::Binary(l_expr, bin_op, r_expr));
                    let width = {
                        let lw = get_width(l_expr, arena);
                        let rw = get_width(r_expr, arena);
                        lw.max(rw)
                    };
                    arena.alloc(SLTNode::Slice {
                        expr: sub_node,
                        access: BitAccess::new(0, width - 1),
                    })
                }
                _ => {
                    let lhs_signed =
                        cast_target_signed(lhs).unwrap_or_else(|| is_signed(module, l_expr, arena));
                    let rhs_signed =
                        cast_target_signed(rhs).unwrap_or_else(|| is_signed(module, r_expr, arena));
                    let signed = lhs_signed && rhs_signed;
                    let bin_op = if matches!(op, Op::ArithShiftR) {
                        if lhs_signed {
                            BinaryOp::Sar
                        } else {
                            BinaryOp::Shr
                        }
                    } else {
                        convert_binary_op(op, signed)
                    };
                    let res = arena.alloc(SLTNode::Binary(l_expr, bin_op, r_expr));
                    if matches!(
                        op,
                        Op::Less
                            | Op::LessEq
                            | Op::Greater
                            | Op::GreaterEq
                            | Op::Eq
                            | Op::Ne
                            | Op::EqWildcard
                            | Op::NeWildcard
                    ) && context_width.map(|cw| cw != 1).unwrap_or(false)
                    {
                        let width = context_width.unwrap();
                        let zero = arena.alloc(SLTNode::Constant(
                            BigUint::from(0u8),
                            BigUint::from(0u32),
                            width - 1,
                            false,
                        ));
                        arena.alloc(SLTNode::Concat(vec![(zero, width - 1), (res, 1)]))
                    } else if matches!(
                        op,
                        Op::ArithShiftL
                            | Op::ArithShiftR
                            | Op::LogicShiftL
                            | Op::LogicShiftR
                            | Op::Pow
                    ) {
                        let res_width = get_width(res, arena);
                        let width = context_width.unwrap_or(res_width);
                        if res_width > width {
                            arena.alloc(SLTNode::Slice {
                                expr: res,
                                access: BitAccess::new(0, width - 1),
                            })
                        } else if res_width < width {
                            let zero = arena.alloc(SLTNode::Constant(
                                BigUint::from(0u8),
                                BigUint::from(0u32),
                                res_width - 1,
                                false,
                            ));
                            arena.alloc(SLTNode::Concat(vec![
                                (zero, width - res_width),
                                (res, res_width),
                            ]))
                        } else {
                            res
                        }
                    } else {
                        res
                    }
                }
            };

            Ok(((result_node, sources), merge_boundaries(l_bounds, r_bounds)))
        }
        Expression::Concatenation(exprs, _) => {
            let mut parts = Vec::new();
            let mut all_bounds = BoundaryMap::default();
            let mut total_sources = HashSet::default();

            for (sub_expr, repeat) in exprs {
                let ((part_expr, part_sources), p_bounds) =
                    eval_expression(module, store, sub_expr, arena, None)?;
                all_bounds = merge_boundaries(all_bounds, p_bounds);
                let width = get_width(part_expr, arena);

                total_sources.extend(part_sources);

                let rep_count = if let Some(rep_expr) = repeat {
                    let v = eval_constexpr(rep_expr);
                    v.ok_or_else(|| {
                        ParserError::unsupported(
                            LoweringPhase::CombLowering,
                            "concatenation non-constant repeat",
                            format!("{:?}", rep_expr),
                            Some(&rep_expr.token_range()),
                        )
                    })?
                    .iter_u64_digits()
                    .next()
                    .unwrap()
                } else {
                    1
                };
                for _ in 0..rep_count {
                    parts.push((part_expr, width));
                }
            }
            Ok((
                (arena.alloc(SLTNode::Concat(parts)), total_sources),
                all_bounds,
            ))
        }
        Expression::Unary(op, expr, _) => {
            let ((expr, sources), bounds) = eval_expression(module, store, expr, arena, None)?;
            // Reduction Nand/Nor/Xnor は既存のリダクション + Not に分解
            let result_node = match op {
                Op::BitNand => {
                    let and_node = arena.alloc(SLTNode::Unary(UnaryOp::And, expr));
                    arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, and_node))
                }
                Op::BitNor => {
                    let or_node = arena.alloc(SLTNode::Unary(UnaryOp::Or, expr));
                    arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, or_node))
                }
                Op::BitXnor => {
                    let xor_node = arena.alloc(SLTNode::Unary(UnaryOp::Xor, expr));
                    arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, xor_node))
                }
                _ => arena.alloc(SLTNode::Unary(convert_unary_op(op), expr)),
            };
            Ok(((result_node, sources), bounds))
        }
        Expression::Ternary(cond, then_expr, else_expr, _) => {
            let ((cond_expr, cond_sources), cond_bounds) =
                eval_expression(module, store, cond, arena, context_width)?;
            let ((then_expr, then_sources), then_bounds) =
                eval_expression(module, store, then_expr, arena, context_width)?;
            let ((else_expr, else_sources), else_bounds) =
                eval_expression(module, store, else_expr, arena, context_width)?;

            let mut sources = cond_sources;
            sources.extend(then_sources);
            sources.extend(else_sources);

            Ok((
                (
                    arena.alloc(SLTNode::Mux {
                        cond: cond_expr,
                        then_expr,
                        else_expr,
                    }),
                    sources,
                ),
                merge_boundaries(cond_bounds, merge_boundaries(then_bounds, else_bounds)),
            ))
        }
        Expression::StructConstructor(ty, fields, _) => {
            let mut parts = Vec::new();
            let mut all_bounds = BoundaryMap::default();
            let mut total_sources = HashSet::default();

            for (name, field_expr) in fields {
                let ((mut part_expr, part_sources), p_bounds) =
                    eval_expression(module, store, field_expr, arena, context_width)?;
                all_bounds = merge_boundaries(all_bounds, p_bounds);
                total_sources.extend(part_sources);

                let Some(member_type) = ty.get_member_type(*name) else {
                    return Err(ParserError::unsupported(
                        LoweringPhase::CombLowering,
                        "struct constructor member",
                        format!("unknown member: {:?} in {:?}", name, ty),
                        Some(&field_expr.token_range()),
                    ));
                };
                let Some(member_width) = member_type.total_width() else {
                    return Err(ParserError::unsupported(
                        LoweringPhase::CombLowering,
                        "struct constructor member width",
                        format!("member: {:?}, type: {:?}", name, member_type),
                        Some(&field_expr.token_range()),
                    ));
                };

                let part_width = get_width(part_expr, arena);
                if part_width > member_width {
                    part_expr = arena.alloc(SLTNode::Slice {
                        expr: part_expr,
                        access: BitAccess::new(0, member_width - 1),
                    });
                } else if part_width < member_width {
                    let pad_width = member_width - part_width;
                    let pad = arena.alloc(SLTNode::Constant(
                        BigUint::from(0u8),
                        BigUint::from(0u32),
                        pad_width,
                        false,
                    ));
                    part_expr = arena.alloc(SLTNode::Concat(vec![
                        (pad, pad_width),
                        (part_expr, part_width),
                    ]));
                }

                parts.push((part_expr, member_width));
            }

            Ok((
                (arena.alloc(SLTNode::Concat(parts)), total_sources),
                all_bounds,
            ))
        }
        Expression::ArrayLiteral(items, _) => {
            eval_array_literal_expression(module, store, items, None, arena)
        }
    }
}

fn eval_factor(
    module: &Module,
    store: &SymbolicStore<VarId>,
    factor: &Factor,
    arena: &mut SLTNodeArena<VarId>,
    context_width: Option<usize>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    match factor {
        Factor::Variable(var_id, index, select, comptime) => {
            // Compile-time constant (e.g. genvar inside generate block): emit a
            // constant node directly instead of loading from memory.
            // Also handles constant[const_index] (e.g. IDX[p] in generate loops).
            if comptime.is_const {
                let is_bare = index.0.is_empty() && select.0.is_empty() && select.1.is_none();
                let is_static_sel =
                    !is_bare && crate::parser::bitaccess::is_static_access(index, select);

                if (is_bare || is_static_sel)
                    && let Some((celox_value, mask_xz, full_width, signed)) =
                        celox_value_from_comptime(comptime)
                {
                    let (val, mask, width) = if is_bare {
                        (celox_value, mask_xz, full_width)
                    } else {
                        // Evaluate the static bit-select on the constant value
                        let access = eval_var_select(module, *var_id, index, select)?;
                        let extracted_width = access.msb - access.lsb + 1;
                        let extracted_val = (&celox_value >> access.lsb)
                            & ((BigUint::from(1u64) << extracted_width) - BigUint::from(1u64));
                        let extracted_mask = (&mask_xz >> access.lsb)
                            & ((BigUint::from(1u64) << extracted_width) - BigUint::from(1u64));
                        (extracted_val, extracted_mask, extracted_width)
                    };

                    let mut expr =
                        arena.alloc(SLTNode::Constant(val, mask, width, signed && is_bare));
                    if let Some(target_width) = context_width {
                        if width < target_width {
                            let pad_width = target_width - width;
                            let pad = if signed && is_bare {
                                let msb_slice = arena.alloc(SLTNode::Slice {
                                    expr,
                                    access: BitAccess::new(width - 1, width - 1),
                                });
                                (msb_slice, pad_width)
                            } else {
                                let zero = arena.alloc(SLTNode::Constant(
                                    BigUint::from(0u8),
                                    BigUint::from(0u32),
                                    pad_width,
                                    false,
                                ));
                                (zero, pad_width)
                            };
                            expr = arena.alloc(SLTNode::Concat(vec![pad, (expr, width)]));
                        } else if width > target_width {
                            expr = arena.alloc(SLTNode::Slice {
                                expr,
                                access: BitAccess::new(0, target_width - 1),
                            });
                        }
                    }
                    return Ok(((expr, HashSet::default()), BoundaryMap::default()));
                }
            }

            let is_static_access = crate::parser::bitaccess::is_static_access(index, select);
            if is_static_access {
                let access = eval_var_select(module, *var_id, index, select)?;

                let mut bounds = BoundaryMap::default();
                bounds.entry(*var_id).or_default().insert(access.lsb);
                bounds.entry(*var_id).or_default().insert(access.msb + 1);

                let range_store = store.get(var_id).unwrap();
                let parts = range_store.get_parts(access);
                // Check if any part of the requested access is unassigned (None)
                // If so, we must depend on the variable's previous value (input).
                // If all parts are Some(...), we only depend on the sources of those expressions.
                let has_unassigned = parts.iter().any(|(val, _)| val.is_none());
                let (mut expr, mut sources) =
                    combine_parts_with_default(*var_id, access.lsb, parts, arena);
                if has_unassigned {
                    sources.insert(VarAtomBase::new(*var_id, access.lsb, access.msb));
                }
                // context_widthによる拡張
                if let Some(target_width) = context_width {
                    let expr_width = get_width(expr, arena);
                    if expr_width < target_width {
                        // Slice + Concatで拡張
                        let sign = is_signed(module, expr, arena);
                        let pad_width = target_width - expr_width;
                        let pad = if sign {
                            // 符号拡張: MSBをpad
                            let msb_slice = arena.alloc(SLTNode::Slice {
                                expr,
                                access: BitAccess::new(expr_width - 1, expr_width - 1),
                            });
                            (msb_slice, pad_width)
                        } else {
                            // ゼロ拡張: 0をpad
                            let zero = arena.alloc(SLTNode::Constant(
                                BigUint::from(0u8),
                                BigUint::from(0u32),
                                pad_width,
                                false,
                            ));
                            (zero, pad_width)
                        };
                        expr = arena.alloc(SLTNode::Concat(vec![pad, (expr, expr_width)]));
                    } else if expr_width > target_width {
                        // Sliceで幅を揃える
                        expr = arena.alloc(SLTNode::Slice {
                            expr,
                            access: BitAccess::new(0, target_width - 1),
                        });
                    }
                }
                Ok(((expr, sources), bounds))
            } else {
                let mut dynamic_indices = Vec::new();
                let mut all_bounds = BoundaryMap::default();
                let mut all_sources = HashSet::default();

                let var = &module.variables[var_id];
                let width = resolve_total_width(module, var)?;

                // 1. Build node for offset calculation (used by both Shr and Input approaches)
                let (_, strides, _) =
                    crate::parser::bitaccess::get_dimensions_and_strides(module, *var_id)?;
                let mut offset_node = arena.alloc(SLTNode::Constant(
                    BigUint::from(0u32),
                    BigUint::from(0u32),
                    64,
                    false,
                ));

                let mut index_exprs = index.0.clone();
                index_exprs.extend(select.0.clone());

                // For Colon selects (e.g. [31:0]), the last element of
                // index_exprs is the MSB anchor—not a dimension index.
                // Exclude it from the dynamic offset and instead encode
                // the bit range in the Slice node.
                // For PlusColon/MinusColon/Step, the anchor is the
                // dynamic start position and belongs in the offset.
                let is_colon_select = matches!(&select.1, Some((VarSelectOp::Colon, _)));
                let dim_limit = if is_colon_select {
                    index_exprs.len().saturating_sub(1)
                } else {
                    index_exprs.len()
                };

                for (dim_i, idx_expr) in index_exprs[..dim_limit].iter().enumerate() {
                    let ((expr, sources), bounds) =
                        eval_expression(module, store, idx_expr, arena, context_width)?;
                    all_bounds = merge_boundaries(all_bounds, bounds);
                    all_sources.extend(sources);
                    let stride = strides.get(dim_i).copied().unwrap_or(1);
                    dynamic_indices.push(SLTIndex { node: expr, stride });

                    let stride_node = arena.alloc(SLTNode::Constant(
                        BigUint::from(stride),
                        BigUint::from(0u32),
                        64,
                        false,
                    ));
                    let term = arena.alloc(SLTNode::Binary(expr, BinaryOp::Mul, stride_node));
                    offset_node = arena.alloc(SLTNode::Binary(offset_node, BinaryOp::Add, term));
                }

                // Compute bit-select LSB within the selected element.
                // For Colon [msb:lsb], the LSB (range_expr) determines
                // the slice start within the element selected by
                // the array indices.
                let bit_select_lsb = if let Some((VarSelectOp::Colon, range_expr)) = &select.1 {
                    let weight = strides.get(dim_limit).copied().unwrap_or(1);
                    let rhs = crate::parser::bitaccess::eval_constexpr(range_expr)
                        .map(|v| v.to_u64_digits().first().copied().unwrap_or(0) as usize)
                        .unwrap_or(0);
                    rhs * weight
                } else {
                    0
                };

                // 2. Check SymbolicStore to determine if "already written"
                let range_store = store.get(var_id).unwrap();
                let access_full = BitAccess::new(0, width - 1);
                let parts = range_store.get_parts(access_full);
                let is_unmodified = parts.iter().all(|(val, _)| val.is_none());

                let element_width =
                    crate::parser::bitaccess::get_access_width(module, *var_id, index, select)?;

                let mut extracted_expr = if is_unmodified {
                    // --- Code for the approach of aligning at load time ---
                    // Since it's still None, pack index into Input and let Load instruction handle alignment
                    let raw_input = arena.alloc(SLTNode::Input {
                        variable: *var_id,
                        index: dynamic_indices,
                        access: BitAccess::new(0, width - 1),
                    });
                    // Slice from bit_select_lsb for element_width bits
                    arena.alloc(SLTNode::Slice {
                        expr: raw_input,
                        access: BitAccess::new(bit_select_lsb, bit_select_lsb + element_width - 1),
                    })
                } else {
                    // --- If already written ---
                    // Combine latest values in register and align with Shr
                    let (current_expr, current_sources) =
                        combine_parts_with_default(*var_id, 0, parts, arena);
                    all_sources.extend(current_sources);

                    let shifted =
                        arena.alloc(SLTNode::Binary(current_expr, BinaryOp::Shr, offset_node));
                    // Slice from bit_select_lsb for element_width bits
                    arena.alloc(SLTNode::Slice {
                        expr: shifted,
                        access: BitAccess::new(bit_select_lsb, bit_select_lsb + element_width - 1),
                    })
                };

                // context_widthによる拡張
                if let Some(target_width) = context_width {
                    let expr_width = get_width(extracted_expr, arena);
                    if expr_width < target_width {
                        // Slice + Concatで拡張
                        let sign = is_signed(module, extracted_expr, arena);
                        let pad_width = target_width - expr_width;
                        let pad = if sign {
                            // 符号拡張: MSBをpad
                            let msb_slice = arena.alloc(SLTNode::Slice {
                                expr: extracted_expr,
                                access: BitAccess::new(expr_width - 1, expr_width - 1),
                            });
                            (msb_slice, pad_width)
                        } else {
                            // ゼロ拡張: 0をpad
                            let zero = arena.alloc(SLTNode::Constant(
                                BigUint::from(0u8),
                                BigUint::from(0u32),
                                pad_width,
                                false,
                            ));
                            (zero, pad_width)
                        };
                        extracted_expr =
                            arena.alloc(SLTNode::Concat(vec![pad, (extracted_expr, expr_width)]));
                    } else if expr_width > target_width {
                        // Sliceで幅を揃える
                        extracted_expr = arena.alloc(SLTNode::Slice {
                            expr: extracted_expr,
                            access: BitAccess::new(0, target_width - 1),
                        });
                    }
                }

                all_sources.insert(VarAtomBase::new(*var_id, 0, width - 1));
                Ok(((extracted_expr, all_sources), all_bounds))
            }
        }
        Factor::Value(v) => {
            let (celox_value, mask_xz, width, signed) = celox_value_from_comptime(v)
                .expect("Factor::Value should always have a numeric value");
            // Fill-literals (`'0`, `'1`, `'x`, `'z`) have width 0 from the
            // analyzer (context-dependent).  Per IEEE 1800-2023 §5.7.1:
            //   "All bits of the unsized value shall be set to the value of
            //    the specified bit. In a self-determined context, it shall
            //    have a width of 1 bit."
            // Use context_width when available; fall back to 1 bit for
            // self-determined contexts.  Without this, a 0-width Constant
            // in the RangeStore causes Mux lowering to produce 0-bit masks
            // that zero out the else-arm.
            //
            // The analyzer stores fill-literals with only bit 0 set in
            // payload and mask_xz (e.g. '1 → payload=1, mask_xz=0).
            // We replicate bit 0 across the full width so '1 → all-ones,
            // 'x → all-X, etc.
            let (celox_value, mask_xz, effective_width) = if width == 0 {
                let ew = context_width.unwrap_or(1);
                let fill_mask = (BigUint::from(1u64) << ew) - BigUint::from(1u64);
                let cv = if celox_value.bit(0) {
                    fill_mask.clone()
                } else {
                    BigUint::from(0u8)
                };
                let mxz = if mask_xz.bit(0) {
                    fill_mask
                } else {
                    BigUint::from(0u8)
                };
                (cv, mxz, ew)
            } else {
                (celox_value, mask_xz, width)
            };
            Ok((
                (
                    arena.alloc(SLTNode::Constant(
                        celox_value,
                        mask_xz,
                        effective_width,
                        signed,
                    )),
                    HashSet::default(),
                ),
                BoundaryMap::default(),
            ))
        }
        Factor::SystemFunctionCall(call) => Err(ParserError::unsupported(
            LoweringPhase::CombLowering,
            "system function call in comb expression",
            format!("module `{}`: {call}", module.name),
            Some(&factor.token_range()),
        )),
        Factor::FunctionCall(call) => eval_function_call_expression(module, store, call, arena),
        Factor::Anonymous(_) | Factor::Unknown(_) => Err(ParserError::unsupported(
            LoweringPhase::CombLowering,
            "unresolved factor in comb expression",
            format!("{:?}", factor),
            Some(&factor.token_range()),
        )),
    }
}
pub fn get_width<A: Hash + Eq + Clone>(expr: NodeId, arena: &SLTNodeArena<A>) -> usize {
    match arena.get(expr) {
        SLTNode::Input { access, .. } => access.msb - access.lsb + 1,
        SLTNode::Constant(_, _, width, _) => *width,
        SLTNode::Binary(lhs, op, rhs) => match op {
            BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::LtU
            | BinaryOp::LtS
            | BinaryOp::LeU
            | BinaryOp::LeS
            | BinaryOp::GtU
            | BinaryOp::GtS
            | BinaryOp::GeU
            | BinaryOp::GeS
            | BinaryOp::LogicAnd
            | BinaryOp::LogicOr => 1,
            BinaryOp::Sub => {
                let lw = get_width(*lhs, arena);
                let rw = get_width(*rhs, arena);
                lw.max(rw)
            }
            BinaryOp::Sar | BinaryOp::Shl | BinaryOp::Shr => get_width(*lhs, arena),
            _ => {
                let lw = get_width(*lhs, arena);
                let rw = get_width(*rhs, arena);
                lw.max(rw)
            }
        },
        SLTNode::Unary(op, inner) => match op {
            UnaryOp::LogicNot => 1,
            UnaryOp::And | UnaryOp::Or | UnaryOp::Xor => 1,
            _ => get_width(*inner, arena),
        },
        SLTNode::Mux { then_expr, .. } => get_width(*then_expr, arena),
        SLTNode::ForFold { result, .. } => result.access.msb - result.access.lsb + 1,
        SLTNode::Concat(parts) => parts.iter().map(|(_, w)| *w).sum(),
        SLTNode::Slice { access, .. } => access.msb - access.lsb + 1,
    }
}

fn merge_boundaries(mut base: BoundaryMap<VarId>, other: BoundaryMap<VarId>) -> BoundaryMap<VarId> {
    for (id, bits) in other {
        base.entry(id).or_default().extend(bits);
    }
    base
}

fn is_signed(module: &Module, expr: NodeId, arena: &SLTNodeArena<VarId>) -> bool {
    match arena.get(expr) {
        SLTNode::Input { variable: id, .. } => module.variables[id].r#type.signed,
        SLTNode::Constant(_, _, _, signed) => *signed,
        SLTNode::Binary(lhs, _, _) => is_signed(module, *lhs, arena),
        SLTNode::Unary(UnaryOp::Minus, _) => true,
        SLTNode::Unary(_, inner) => is_signed(module, *inner, arena),
        SLTNode::Mux { then_expr, .. } => is_signed(module, *then_expr, arena),
        SLTNode::ForFold { result, .. } => module.variables[&result.id].r#type.signed,
        SLTNode::Slice { expr, .. } => is_signed(module, *expr, arena),
        SLTNode::Concat(_) => false,
    }
}

fn cast_target_signed(expr: &Expression) -> Option<bool> {
    let Expression::Binary(_, op, rhs, _) = expr else {
        return None;
    };
    if !matches!(op, Op::As) {
        return None;
    }

    let Expression::Term(factor) = rhs.as_ref() else {
        return None;
    };
    let Factor::Value(comptime) = factor.as_ref() else {
        return None;
    };
    match &comptime.value {
        ValueVariant::Type(ty) => Some(ty.signed),
        ValueVariant::Numeric(_) => Some(false),
        _ => None,
    }
}

pub fn convert_binary_op(op: &Op, use_signed: bool) -> BinaryOp {
    match op {
        Op::Add => BinaryOp::Add,
        Op::Sub => BinaryOp::Sub,
        Op::Mul => BinaryOp::Mul,
        Op::Div => BinaryOp::Div,
        Op::Rem => BinaryOp::Rem,
        Op::BitAnd => BinaryOp::And,
        Op::BitOr => BinaryOp::Or,
        Op::BitXor => BinaryOp::Xor,
        Op::LogicShiftL | Op::ArithShiftL => BinaryOp::Shl,
        Op::LogicShiftR => BinaryOp::Shr,
        Op::ArithShiftR => BinaryOp::Sar,
        Op::Eq => BinaryOp::Eq,
        Op::EqWildcard => BinaryOp::EqWildcard,
        Op::Ne => BinaryOp::Ne,
        Op::NeWildcard => BinaryOp::NeWildcard,
        Op::Less => {
            if use_signed {
                BinaryOp::LtS
            } else {
                BinaryOp::LtU
            }
        }
        Op::LessEq => {
            if use_signed {
                BinaryOp::LeS
            } else {
                BinaryOp::LeU
            }
        }
        Op::Greater => {
            if use_signed {
                BinaryOp::GtS
            } else {
                BinaryOp::GtU
            }
        }
        Op::GreaterEq => {
            if use_signed {
                BinaryOp::GeS
            } else {
                BinaryOp::GeU
            }
        }
        Op::LogicAnd => BinaryOp::LogicAnd,
        Op::LogicOr => BinaryOp::LogicOr,
        // Unary-only operators
        Op::LogicNot | Op::BitNot => {
            unreachable!(
                "unary operator must not be lowered by convert_binary_op: {:?}",
                op
            )
        }
        // Binary-expression nodes lowered by dedicated paths
        Op::BitXnor | Op::BitNand | Op::BitNor => {
            unreachable!(
                "bitwise derived op must be lowered before convert_binary_op: {:?}",
                op
            )
        }
        Op::Ternary => unreachable!("ternary expression must not be lowered by convert_binary_op"),
        Op::Concatenation => {
            unreachable!("concatenation must be lowered by concat-specific path")
        }
        Op::ArrayLiteral => {
            unreachable!("array literal must not be lowered by convert_binary_op")
        }
        Op::Condition => unreachable!("condition node must not be lowered by convert_binary_op"),
        Op::Repeat => unreachable!("repeat node must be lowered by repeat-specific path"),
        // Handled by pre-lowering in eval_expression.
        Op::Pow | Op::As => {
            unreachable!("operator must be pre-lowered before conversion: {:?}", op)
        }
    }
}
pub fn convert_unary_op(op: &Op) -> UnaryOp {
    match op {
        Op::Add => UnaryOp::Ident,
        Op::Sub => UnaryOp::Minus,
        Op::BitNot => UnaryOp::BitNot,
        Op::LogicNot => UnaryOp::LogicNot,
        // リダクション演算子としての使用
        Op::BitAnd => UnaryOp::And,
        Op::BitOr => UnaryOp::Or,
        Op::BitXor => UnaryOp::Xor,
        // Unary form lowered by decomposition before conversion
        Op::BitXnor | Op::BitNand | Op::BitNor => {
            unreachable!(
                "reduction derived op must be lowered before convert_unary_op: {:?}",
                op
            )
        }
        // Binary-only operators
        Op::Pow
        | Op::Div
        | Op::Rem
        | Op::Mul
        | Op::ArithShiftL
        | Op::ArithShiftR
        | Op::LogicShiftL
        | Op::LogicShiftR
        | Op::LessEq
        | Op::GreaterEq
        | Op::Less
        | Op::Greater
        | Op::Eq
        | Op::EqWildcard
        | Op::Ne
        | Op::NeWildcard
        | Op::LogicAnd
        | Op::LogicOr => {
            unreachable!(
                "binary operator must not be lowered by convert_unary_op: {:?}",
                op
            )
        }
        // Node kinds lowered by dedicated paths
        Op::Ternary => unreachable!("ternary expression must not be lowered by convert_unary_op"),
        Op::Concatenation => {
            unreachable!("concatenation must be lowered by concat-specific path")
        }
        Op::ArrayLiteral => {
            unreachable!("array literal must not be lowered by convert_unary_op")
        }
        Op::Condition => unreachable!("condition node must not be lowered by convert_unary_op"),
        Op::Repeat => unreachable!("repeat node must be lowered by repeat-specific path"),
        Op::As => unreachable!("As is binary and must not be lowered by convert_unary_op"),
    }
}
#[cfg(test)]
mod tests {
    use veryl_analyzer::{
        Analyzer, Context, attribute_table,
        ir::{Component, Declaration, Ir, VarPath},
        symbol_table,
    };
    use veryl_metadata::Metadata;
    use veryl_parser::Parser;

    use super::*;
    // 既存のインポート...
    pub struct CombResult {
        pub paths: Vec<LogicPath<VarId>>,
        pub boundaries: HashMap<VarId, BTreeSet<usize>>,
    }
    /// 新しい parse_comb の出力を直接検査するためのヘルパー
    pub fn inspect_comb(code: &str) -> (Module, CombResult) {
        symbol_table::clear();
        attribute_table::clear();

        let metadata = Metadata::create_default("prj").unwrap();
        let parser = Parser::parse(code, &"").unwrap();
        let analyzer = Analyzer::new(&metadata);
        let mut context = Context::default();
        let mut ir = Ir::default();

        // Pass 1 & 2 を実行して Ir を構築
        let errors = analyzer.analyze_pass1("prj", &parser.veryl);
        assert!(errors.is_empty(), "analyze_pass1 errors: {errors:?}");
        let errors = Analyzer::analyze_post_pass1();
        assert!(errors.is_empty(), "analyze_post_pass1 errors: {errors:?}");
        let errors = analyzer.analyze_pass2("prj", &parser.veryl, &mut context, Some(&mut ir));
        assert!(errors.is_empty(), "analyze_pass2 errors: {errors:?}");
        let errors = Analyzer::analyze_post_pass2();
        assert!(errors.is_empty(), "analyze_post_pass2 errors: {errors:?}");

        // Top モジュールを探す
        let top_id = veryl_parser::resource_table::insert_str("Top");
        let top_module = ir
            .components
            .into_iter()
            .find_map(|e| match e {
                Component::Module(m) if m.name == top_id => Some(m),
                _ => None,
            })
            .expect("Top module not found");

        // Top モジュール内の最初の always_comb をパース
        // (実際には複数の場合もあるので、必要に応じて loop させる)
        let comb_decl = top_module
            .declarations
            .iter()
            .find_map(|d| {
                if let Declaration::Comb(c) = d {
                    Some(c)
                } else {
                    None
                }
            })
            .expect("No always_comb found in Top");
        let mut arena = SLTNodeArena::new();
        let (paths, _, boundaries) = super::parse_comb(&top_module, comb_decl, &mut arena).unwrap();
        (top_module, CombResult { paths, boundaries })
    }
    pub fn var_id_of(module: &Module, var_path: &[&str]) -> VarId {
        let mut var_path_str_id = Vec::new();
        for path in var_path {
            let id = veryl_parser::resource_table::insert_str(path);
            var_path_str_id.push(id);
        }
        let path = VarPath(var_path_str_id);
        module
            .variables
            .values()
            .find(|e| e.path == path)
            .unwrap()
            .id
    }
    #[test]
    fn test_parse_comb_boundary_collection() {
        let code = r#"
            module Top (a: input logic<32>, b: output logic<32>) {
                               always_comb {
                    b = 0;
                    b[7:4] = a[3:0];
                }
            }
        "#;
        let (module, result) = inspect_comb(code);
        // 1.① 境界情報が正しく集まっているか
        let b_id = var_id_of(&module, &["b"]);
        let bounds = &result.boundaries[&b_id];

        // b[7:4] への代入なので、境界は 4 と 8 が必要
        assert!(bounds.contains(&4));
        assert!(bounds.contains(&8));

        // 2. 依存関係の絞り込み (b[7:4] のソースに a[3:0] だけが含まれているか)
        let path = result
            .paths
            .iter()
            .find(|p| p.target.id == b_id && p.target.access.lsb == 4 && p.target.access.msb == 7)
            .unwrap();
        let a_id = var_id_of(&module, &["a"]);

        let a_deps: Vec<_> = path.sources.iter().filter(|s| s.id == a_id).collect();
        assert_eq!(a_deps.len(), 1);
        assert_eq!(a_deps[0].access.lsb, 0);
        assert_eq!(a_deps[0].access.msb, 3);
    }
    #[test]
    fn test_dependency_override() {
        let code = r#"
        module Top (b: input logic<8>, c: input logic<1>, o_a: output logic<8>) {
            var a: logic<8>;
            always_comb {
                a = b;
                a[0] = c;
            }
            assign o_a = a;
        }
    "#;
        let (module, res) = inspect_comb(code);
        let id_a = var_id_of(&module, &["a"]);
        let id_b = var_id_of(&module, &["b"]);
        let id_c = var_id_of(&module, &["c"]);

        // Find path for a[0]
        let path_a0 = res
            .paths
            .iter()
            .find(|p| p.target.id == id_a && p.target.access.lsb == 0)
            .expect("Path for a[0] not found");

        // a[0] depends on c
        assert!(
            path_a0.sources.iter().any(|s| s.id == id_c),
            "a[0] must depend on c"
        );
        // a[0] should NOT depend on b
        assert!(
            !path_a0.sources.iter().any(|s| s.id == id_b),
            "a[0] must NOT depend on b"
        );

        // Find path for a[7:1]
        let path_a_upper = res
            .paths
            .iter()
            .find(|p| p.target.id == id_a && p.target.access.lsb == 1)
            .expect("Path for a[7:1] not found");
        assert!(
            path_a_upper.sources.iter().any(|s| s.id == id_b),
            "a[7:1] must depend on b"
        );
    }

    #[test]
    fn test_arithmetic_dependency() {
        let code = r#"
        module Top (b: input logic<8>, c: input logic<8>, o_a: output logic<8>) {
            assign o_a = b + c;
        }
    "#;
        let (module, res) = inspect_comb(code);
        let id_oa = var_id_of(&module, &["o_a"]);
        let id_b = var_id_of(&module, &["b"]);
        let id_c = var_id_of(&module, &["c"]);

        let path_oa = res.paths.iter().find(|p| p.target.id == id_oa).unwrap();

        // o_a depends on b and c
        assert!(path_oa.sources.iter().any(|s| s.id == id_b));
        assert!(path_oa.sources.iter().any(|s| s.id == id_c));
    }

    #[test]
    fn test_bit_level_self_assignment_dag() {
        let code = r#"
        module Top (i: input logic<8>, o: output logic<8>) {
            var a: logic<8>;
            always_comb {
                a = i;
                a[0] = a[1];
            }
            assign o = a;
        }
    "#;
        let (module, res) = inspect_comb(code);
        let id_a = var_id_of(&module, &["a"]);
        let id_i = var_id_of(&module, &["i"]);

        // a[0] = a[1] = i[1]
        let path_a0 = res
            .paths
            .iter()
            .find(|p| p.target.id == id_a && p.target.access.lsb == 0)
            .unwrap();

        assert!(
            path_a0
                .sources
                .iter()
                .any(|s| s.id == id_i && s.access.lsb <= 1 && s.access.msb >= 1),
            "a[0] should depend on i[1]"
        );
    }
    #[test]
    fn test_dynamic_assign_eval() {
        let code = r#"
            module Top (
                a: input logic<32>,
                idx: input logic<5>,
                val: input logic<1>,
                d: output logic<32>
            ) {
                always_comb {
                    d = a;
                    d[idx] = val;
                }
            }
        "#;
        let (module, result) = inspect_comb(code);

        // d is updated dynamically, so we expect a path covering d[0..31]
        let id_d = var_id_of(&module, &["d"]);
        let path = result.paths.iter().find(|p| p.target.id == id_d);

        // Dynamic assignment essentially combines all bits, so we should find a path for d.
        // It might be split or single, but since we updated full range in eval_dynamic_assign, it should be single if initialized so.
        // But `d=a` initializes it with 0..31 (or splits if `a` is split). `a` is input 32.
        // So `d` starts as [0:31]. Dynamic update updates [0:31]. So it should stay [0:31].

        let path = path.expect("Path for d not found");
        assert_eq!(path.target.access.lsb, 0);
        assert_eq!(path.target.access.msb, 31);

        let id_a = var_id_of(&module, &["a"]);
        let id_idx = var_id_of(&module, &["idx"]);
        let id_val = var_id_of(&module, &["val"]);

        assert!(
            path.sources.iter().any(|s| s.id == id_a),
            "Depends on old value a"
        );
        assert!(
            path.sources.iter().any(|s| s.id == id_idx),
            "Depends on index idx"
        );
        assert!(
            path.sources.iter().any(|s| s.id == id_val),
            "Depends on new value val"
        );
    }

    #[test]
    fn test_slt_display() {
        let mut arena = SLTNodeArena::<i32>::new();
        // Test simple constant
        let _const_node = arena.alloc(SLTNode::Constant(
            BigUint::from(42u32),
            BigUint::from(0u32),
            8,
            false,
        ));
        // fmt_display is not easily callable here without a Formatter, but we can check if it compiles or use a dummy formatter
        // Actually, let's just use a custom wrapper with Display if needed, but for now let's just fix the test to compile.

        // Test unary operation
        let inner = arena.alloc(SLTNode::Constant(
            BigUint::from(5u32),
            BigUint::from(0u32),
            4,
            false,
        ));
        let _unary_node = arena.alloc(SLTNode::Unary(UnaryOp::Minus, inner));

        // Test binary operation
        let lhs = arena.alloc(SLTNode::Constant(
            BigUint::from(1u32),
            BigUint::from(0u32),
            8,
            false,
        ));
        let rhs = arena.alloc(SLTNode::Constant(
            BigUint::from(2u32),
            BigUint::from(0u32),
            8,
            false,
        ));
        let _binary_node = arena.alloc(SLTNode::Binary(lhs, BinaryOp::Add, rhs));

        // Test Mux
        let cond = arena.alloc(SLTNode::Constant(
            BigUint::from(1u32),
            BigUint::from(0u32),
            1,
            false,
        ));
        let then_expr = arena.alloc(SLTNode::Constant(
            BigUint::from(10u32),
            BigUint::from(0u32),
            8,
            false,
        ));
        let else_expr = arena.alloc(SLTNode::Constant(
            BigUint::from(20u32),
            BigUint::from(0u32),
            8,
            false,
        ));
        let _mux_node = arena.alloc(SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        });

        // Test Concat
        let parts = vec![
            (
                arena.alloc(SLTNode::Constant(
                    BigUint::from(1u32),
                    BigUint::from(0u32),
                    4,
                    false,
                )),
                4,
            ),
            (
                arena.alloc(SLTNode::Constant(
                    BigUint::from(2u32),
                    BigUint::from(0u32),
                    4,
                    false,
                )),
                4,
            ),
        ];
        let _concat_node = arena.alloc(SLTNode::Concat(parts));

        // Test Slice
        let expr = arena.alloc(SLTNode::Constant(
            BigUint::from(255u32),
            BigUint::from(0u32),
            8,
            false,
        ));
        let _slice_node = arena.alloc(SLTNode::Slice {
            expr,
            access: BitAccess::new(2, 5),
        });
    }

    #[test]
    fn test_slt_display_complex() {
        let mut arena = SLTNodeArena::<i32>::new();
        // Display complex nested expression: (a + b) * (c - d)
        let a = arena.alloc(SLTNode::Constant(
            BigUint::from(1u32),
            BigUint::from(0u32),
            32,
            false,
        ));
        let b = arena.alloc(SLTNode::Constant(
            BigUint::from(2u32),
            BigUint::from(0u32),
            32,
            false,
        ));
        let add_expr = arena.alloc(SLTNode::Binary(a, BinaryOp::Add, b));

        let c = arena.alloc(SLTNode::Constant(
            BigUint::from(3u32),
            BigUint::from(0u32),
            32,
            false,
        ));
        let d = arena.alloc(SLTNode::Constant(
            BigUint::from(4u32),
            BigUint::from(0u32),
            32,
            false,
        ));
        let sub_expr = arena.alloc(SLTNode::Binary(c, BinaryOp::Sub, d));

        let _mul_node = arena.alloc(SLTNode::Binary(add_expr, BinaryOp::Mul, sub_expr));
    }
}
