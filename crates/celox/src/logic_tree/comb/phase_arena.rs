//! Phase-typed symbolic-logic-tree storage for the verifier-first pipeline.
//!
//! This is intentionally not an adapter around the legacy `NodeId` arena.
//! The only construction operation interns an ordinary completed node.  Gated
//! mux identity must later be coordinated by an aggregate provenance builder,
//! so this low-level foundation exposes no gated operation.

#![expect(
    dead_code,
    reason = "verifier-first foundation is intentionally not connected to legacy lowering"
)]

use std::{cmp::Ordering, fmt, marker::PhantomData};

use num_bigint::BigUint;

use crate::ir::{BinaryOp, BitAccess, UnaryOp};

use super::{
    node::SLTStepOp,
    node_rules::{self, NodeRuleError},
};

mod sealed {
    pub trait Sealed {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseKind {
    Source,
    DraftOccurrence,
    Occurrence,
}

/// A closed marker trait for an SLT node-ID namespace.
pub trait SLTPhase: sealed::Sealed + Copy + Eq + Ord + fmt::Debug + 'static {
    const KIND: PhaseKind;
}

macro_rules! phase_marker {
    ($name:ident, $kind:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name;
        impl sealed::Sealed for $name {}
        impl SLTPhase for $name {
            const KIND: PhaseKind = PhaseKind::$kind;
        }
    };
}

phase_marker!(SourcePhase, Source);
phase_marker!(DraftOccurrencePhase, DraftOccurrence);
phase_marker!(OccurrencePhase, Occurrence);

/// A checked index into one owning arena in phase `P`.
#[derive(PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PhaseNodeId<P: SLTPhase> {
    index: usize,
    phase: PhantomData<fn() -> P>,
}

impl<P: SLTPhase> PhaseNodeId<P> {
    fn new(index: usize) -> Self {
        Self {
            index,
            phase: PhantomData,
        }
    }

    pub fn index(self) -> usize {
        self.index
    }
}

impl<P: SLTPhase> Copy for PhaseNodeId<P> {}

impl<P: SLTPhase> Clone for PhaseNodeId<P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<P: SLTPhase> fmt::Debug for PhaseNodeId<P> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "pn{}", self.index)
    }
}

/// A checked input-table ID in phase `P`.
#[derive(PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PhaseInputId<P: SLTPhase> {
    index: u32,
    phase: PhantomData<fn() -> P>,
}

impl<P: SLTPhase> PhaseInputId<P> {
    fn new(index: u32) -> Self {
        Self {
            index,
            phase: PhantomData,
        }
    }

    pub fn index(self) -> u32 {
        self.index
    }
}

impl<P: SLTPhase> Copy for PhaseInputId<P> {}

impl<P: SLTPhase> Clone for PhaseInputId<P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<P: SLTPhase> fmt::Debug for PhaseInputId<P> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "pi{}", self.index)
    }
}

pub type SourceInputId = PhaseInputId<SourcePhase>;
pub type DraftOccurrenceInputId = PhaseInputId<DraftOccurrencePhase>;
pub type OccurrenceInputId = PhaseInputId<OccurrencePhase>;

/// A checked runtime-event site ID isolated by the owning SLT phase.
#[derive(PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PhaseRuntimeEventSiteId<P: SLTPhase> {
    index: u32,
    phase: PhantomData<fn() -> P>,
}

impl<P: SLTPhase> PhaseRuntimeEventSiteId<P> {
    fn new(index: u32) -> Self {
        Self {
            index,
            phase: PhantomData,
        }
    }

    pub fn index(self) -> u32 {
        self.index
    }
}

impl<P: SLTPhase> Copy for PhaseRuntimeEventSiteId<P> {}

impl<P: SLTPhase> Clone for PhaseRuntimeEventSiteId<P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<P: SLTPhase> fmt::Debug for PhaseRuntimeEventSiteId<P> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ps{}", self.index)
    }
}

pub type SourceRuntimeEventSiteId = PhaseRuntimeEventSiteId<SourcePhase>;
pub type DraftOccurrenceRuntimeEventSiteId = PhaseRuntimeEventSiteId<DraftOccurrencePhase>;
pub type OccurrenceRuntimeEventSiteId = PhaseRuntimeEventSiteId<OccurrencePhase>;

/// Whether an input's declared element domain is two-state or four-state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InputElementDomain {
    Bit,
    Logic,
}

/// One row independently derived from verified source/flattened type data.
#[derive(Debug, PartialEq, Eq)]
pub struct InputSemanticFact {
    width: usize,
    signed: bool,
    domain: InputElementDomain,
    dynamic_index_strides: Vec<usize>,
}

impl InputSemanticFact {
    fn try_new(
        width: usize,
        signed: bool,
        domain: InputElementDomain,
        dynamic_index_strides: Vec<usize>,
    ) -> Result<Self, InputSemanticFactError> {
        if width == 0 {
            return Err(InputSemanticFactError::new(
                "INPUT.WIDTH_NON_ZERO",
                "input semantic width is zero",
            ));
        }
        if let Some((ordinal, _)) = dynamic_index_strides
            .iter()
            .enumerate()
            .find(|(_, stride)| **stride == 0)
        {
            return Err(InputSemanticFactError::new(
                "INPUT.INDEX_STRIDE_NON_ZERO",
                format!("dynamic index stride {ordinal} is zero"),
            ));
        }
        Ok(Self {
            width,
            signed,
            domain,
            dynamic_index_strides,
        })
    }
}

/// Phase-bound semantic input facts. It has no serialization boundary.
#[derive(Debug, PartialEq, Eq)]
pub struct InputSemanticFacts<P: SLTPhase> {
    rows: Vec<InputSemanticFact>,
    phase: PhantomData<fn() -> P>,
}

impl<P: SLTPhase> InputSemanticFacts<P> {
    fn try_from_verified_rows(rows: Vec<InputSemanticFact>) -> Result<Self, PhaseArenaError<P>> {
        if rows.len() > u32::MAX as usize {
            return Err(PhaseArenaError::new(
                "INPUT.ID_REPRESENTABLE",
                None,
                format!("{} input rows do not fit a checked u32 ID", rows.len()),
            ));
        }
        Ok(Self {
            rows,
            phase: PhantomData,
        })
    }

    pub fn id_at(&self, index: usize) -> Option<PhaseInputId<P>> {
        self.rows.get(index)?;
        let index = u32::try_from(index).ok()?;
        Some(PhaseInputId::new(index))
    }

    fn get(&self, input: PhaseInputId<P>) -> Option<&InputSemanticFact> {
        self.rows.get(input.index as usize)
    }
}

/// A dynamic input index and its declaration-derived stride.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhaseSLTIndex<P: SLTPhase> {
    pub node: PhaseNodeId<P>,
    pub stride: usize,
}

/// A bit range on one phase input.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhaseInputAtom<P: SLTPhase> {
    pub input: PhaseInputId<P>,
    pub access: BitAccess,
}

/// The exact coercion applied to a completed value use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PhaseCoercion {
    pub target_width: usize,
    /// Declared semantic target signedness. Structural replay checks the
    /// role-specific rule, but this is not proof of typed-HIR agreement until
    /// the owning aggregate verifier matches the expected value relation.
    pub target_signed: bool,
    pub kind: PhaseCoercionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PhaseCoercionKind {
    Identity,
    ZeroExtend,
    SignExtend,
    Truncate,
}

/// A completed operand together with its explicit width coercion.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhaseValueUse<P: SLTPhase> {
    pub value: PhaseNodeId<P>,
    pub coercion: PhaseCoercion,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhaseTypedConstant {
    pub payload: BigUint,
    pub width: usize,
    pub signed: bool,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PhaseSLTLoopBound<P: SLTPhase> {
    Const {
        value: PhaseTypedConstant,
        coercion: PhaseCoercion,
    },
    Expr(PhaseValueUse<P>),
}

/// One canonical ForFold state row. Initial/update order remains parallel.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhaseForFoldState<P: SLTPhase> {
    pub target: PhaseInputAtom<P>,
    pub initial: PhaseValueUse<P>,
    pub update: PhaseValueUse<P>,
}

/// A source-ordered ForFold effect row.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhaseForFoldEffect<P: SLTPhase> {
    pub site_id: PhaseRuntimeEventSiteId<P>,
    pub guard: Option<PhaseNodeId<P>>,
    pub emit_on_true: bool,
    pub args: Vec<PhaseNodeId<P>>,
    pub fatal_error_code: Option<i64>,
}

/// One concat operand and its explicit coercion to the declared part width.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhaseConcatPart<P: SLTPhase> {
    pub value: PhaseValueUse<P>,
}

/// One fallibly allocated, uniquely owned out-of-line node payload.
///
/// `Vec<T>` is used instead of `Box<T>` because stable Rust has no fallible
/// `Box::try_new`. The storage is private, always has exactly one element, and
/// is never cloned into an interning key.
#[derive(Debug, PartialEq, Eq)]
pub struct PhaseOwnedPayload<T> {
    storage: Vec<T>,
}

impl<T> PhaseOwnedPayload<T> {
    fn try_new<P: SLTPhase>(value: T, role: &'static str) -> Result<Self, PhaseArenaError<P>> {
        let mut storage = Vec::new();
        storage.try_reserve_exact(1).map_err(|error| {
            PhaseArenaError::new(
                "NODE.PAYLOAD_STORAGE_AVAILABLE",
                None,
                format!("cannot reserve out-of-line {role} payload: {error}"),
            )
        })?;
        storage.push(value);
        Ok(Self { storage })
    }

    fn get(&self) -> &T {
        // `storage` is private and `try_new` is its only constructor.
        &self.storage[0]
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhaseInputNode<P: SLTPhase> {
    pub input: PhaseInputId<P>,
    pub index: Vec<PhaseSLTIndex<P>>,
    pub access: BitAccess,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhaseConstantNode {
    pub payload: BigUint,
    pub mask: BigUint,
    pub width: usize,
    pub signed: bool,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhaseMuxNode<P: SLTPhase> {
    pub cond: PhaseNodeId<P>,
    pub then_value: PhaseValueUse<P>,
    pub else_value: PhaseValueUse<P>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct PhaseForFoldNode<P: SLTPhase> {
    pub loop_input: PhaseInputId<P>,
    pub loop_width: usize,
    pub loop_signed: bool,
    pub start: PhaseSLTLoopBound<P>,
    pub end: PhaseSLTLoopBound<P>,
    pub inclusive: bool,
    pub step: PhaseTypedConstant,
    pub step_coercion: PhaseCoercion,
    pub step_op: SLTStepOp,
    pub reverse: bool,
    pub states: Vec<PhaseForFoldState<P>>,
    pub result_state: usize,
    pub effects: Vec<PhaseForFoldEffect<P>>,
    pub continue_cond: PhaseNodeId<P>,
}

/// A phase-local symbolic node. It is intentionally not serializable.
#[derive(Debug, PartialEq, Eq)]
pub enum PhaseSLTNode<P: SLTPhase> {
    Input(PhaseOwnedPayload<PhaseInputNode<P>>),
    Constant(PhaseOwnedPayload<PhaseConstantNode>),
    Binary {
        lhs: PhaseNodeId<P>,
        op: BinaryOp,
        rhs: PhaseNodeId<P>,
    },
    Unary {
        op: UnaryOp,
        inner: PhaseNodeId<P>,
    },
    Mux(PhaseOwnedPayload<PhaseMuxNode<P>>),
    ForFold(PhaseOwnedPayload<PhaseForFoldNode<P>>),
    Concat(Vec<PhaseConcatPart<P>>),
    Slice {
        expr: PhaseNodeId<P>,
        access: BitAccess,
    },
}

impl<P: SLTPhase> PhaseSLTNode<P> {
    pub fn try_input(
        input: PhaseInputId<P>,
        index: Vec<PhaseSLTIndex<P>>,
        access: BitAccess,
    ) -> Result<Self, PhaseArenaError<P>> {
        Ok(Self::Input(PhaseOwnedPayload::try_new::<P>(
            PhaseInputNode {
                input,
                index,
                access,
            },
            "input",
        )?))
    }

    pub fn try_constant(
        payload: BigUint,
        mask: BigUint,
        width: usize,
        signed: bool,
    ) -> Result<Self, PhaseArenaError<P>> {
        Ok(Self::Constant(PhaseOwnedPayload::try_new::<P>(
            PhaseConstantNode {
                payload,
                mask,
                width,
                signed,
            },
            "constant",
        )?))
    }

    pub fn try_mux(
        cond: PhaseNodeId<P>,
        then_value: PhaseValueUse<P>,
        else_value: PhaseValueUse<P>,
    ) -> Result<Self, PhaseArenaError<P>> {
        Ok(Self::Mux(PhaseOwnedPayload::try_new::<P>(
            PhaseMuxNode {
                cond,
                then_value,
                else_value,
            },
            "mux",
        )?))
    }

    pub fn try_for_fold(payload: PhaseForFoldNode<P>) -> Result<Self, PhaseArenaError<P>> {
        Ok(Self::ForFold(PhaseOwnedPayload::try_new::<P>(
            payload, "ForFold",
        )?))
    }
}

impl<P: SLTPhase> Ord for PhaseSLTNode<P> {
    fn cmp(&self, other: &Self) -> Ordering {
        let tag_order = phase_node_tag(self).cmp(&phase_node_tag(other));
        if tag_order != Ordering::Equal {
            return tag_order;
        }
        match (self, other) {
            (Self::Input(lhs), Self::Input(rhs)) => lhs.get().cmp(rhs.get()),
            (Self::Constant(lhs), Self::Constant(rhs)) => lhs.get().cmp(rhs.get()),
            (
                Self::Binary {
                    lhs: lhs_lhs,
                    op: lhs_op,
                    rhs: lhs_rhs,
                },
                Self::Binary {
                    lhs: rhs_lhs,
                    op: rhs_op,
                    rhs: rhs_rhs,
                },
            ) => (lhs_lhs, binary_op_tag(*lhs_op), lhs_rhs).cmp(&(
                rhs_lhs,
                binary_op_tag(*rhs_op),
                rhs_rhs,
            )),
            (
                Self::Unary {
                    op: lhs_op,
                    inner: lhs_inner,
                },
                Self::Unary {
                    op: rhs_op,
                    inner: rhs_inner,
                },
            ) => (unary_op_tag(*lhs_op), lhs_inner).cmp(&(unary_op_tag(*rhs_op), rhs_inner)),
            (Self::Mux(lhs), Self::Mux(rhs)) => lhs.get().cmp(rhs.get()),
            (Self::ForFold(lhs), Self::ForFold(rhs)) => {
                let lhs = lhs.get();
                let rhs = rhs.get();
                (
                    &lhs.loop_input,
                    &lhs.loop_width,
                    &lhs.loop_signed,
                    &lhs.start,
                    &lhs.end,
                    &lhs.inclusive,
                    &lhs.step,
                    &lhs.step_coercion,
                )
                    .cmp(&(
                        &rhs.loop_input,
                        &rhs.loop_width,
                        &rhs.loop_signed,
                        &rhs.start,
                        &rhs.end,
                        &rhs.inclusive,
                        &rhs.step,
                        &rhs.step_coercion,
                    ))
                    .then_with(|| {
                        (
                            step_op_tag(lhs.step_op),
                            &lhs.reverse,
                            &lhs.states,
                            &lhs.result_state,
                            &lhs.effects,
                            &lhs.continue_cond,
                        )
                            .cmp(&(
                                step_op_tag(rhs.step_op),
                                &rhs.reverse,
                                &rhs.states,
                                &rhs.result_state,
                                &rhs.effects,
                                &rhs.continue_cond,
                            ))
                    })
            }
            (Self::Concat(lhs), Self::Concat(rhs)) => lhs.cmp(rhs),
            (
                Self::Slice {
                    expr: lhs_expr,
                    access: lhs_access,
                },
                Self::Slice {
                    expr: rhs_expr,
                    access: rhs_access,
                },
            ) => (lhs_expr, lhs_access).cmp(&(rhs_expr, rhs_access)),
            _ => Ordering::Equal,
        }
    }
}

impl<P: SLTPhase> PartialOrd for PhaseSLTNode<P> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn phase_node_tag<P: SLTPhase>(node: &PhaseSLTNode<P>) -> u8 {
    match node {
        PhaseSLTNode::Input(_) => 0,
        PhaseSLTNode::Constant(_) => 1,
        PhaseSLTNode::Binary { .. } => 2,
        PhaseSLTNode::Unary { .. } => 3,
        PhaseSLTNode::Mux(_) => 4,
        PhaseSLTNode::ForFold(_) => 5,
        PhaseSLTNode::Concat(_) => 6,
        PhaseSLTNode::Slice { .. } => 7,
    }
}

fn binary_op_tag(op: BinaryOp) -> u8 {
    match op {
        BinaryOp::Add => 0,
        BinaryOp::Sub => 1,
        BinaryOp::Mul => 2,
        BinaryOp::Div => 3,
        BinaryOp::Rem => 4,
        BinaryOp::And => 5,
        BinaryOp::Or => 6,
        BinaryOp::Xor => 7,
        BinaryOp::Shl => 8,
        BinaryOp::Shr => 9,
        BinaryOp::Sar => 10,
        BinaryOp::Eq => 11,
        BinaryOp::Ne => 12,
        BinaryOp::LtU => 13,
        BinaryOp::LtS => 14,
        BinaryOp::LeU => 15,
        BinaryOp::LeS => 16,
        BinaryOp::GtU => 17,
        BinaryOp::GtS => 18,
        BinaryOp::GeU => 19,
        BinaryOp::GeS => 20,
        BinaryOp::LogicAnd => 21,
        BinaryOp::LogicOr => 22,
        BinaryOp::EqWildcard => 23,
        BinaryOp::NeWildcard => 24,
    }
}

fn unary_op_tag(op: UnaryOp) -> u8 {
    match op {
        UnaryOp::Ident => 0,
        UnaryOp::Minus => 1,
        UnaryOp::BitNot => 2,
        UnaryOp::LogicNot => 3,
        UnaryOp::And => 4,
        UnaryOp::Or => 5,
        UnaryOp::Xor => 6,
    }
}

fn step_op_tag(op: SLTStepOp) -> u8 {
    match op {
        SLTStepOp::Add => 0,
        SLTStepOp::Mul => 1,
        SLTStepOp::Shl => 2,
    }
}

/// Whether ordinary interning reused or inserted the canonical node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InternOutcome<P: SLTPhase> {
    Existing(PhaseNodeId<P>),
    Inserted(PhaseNodeId<P>),
}

impl<P: SLTPhase> InternOutcome<P> {
    pub fn id(self) -> PhaseNodeId<P> {
        match self {
            Self::Existing(id) | Self::Inserted(id) => id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InputSemanticFactError {
    pub invariant: &'static str,
    pub message: String,
}

impl InputSemanticFactError {
    fn new(invariant: &'static str, message: impl Into<String>) -> Self {
        Self {
            invariant,
            message: message.into(),
        }
    }
}

impl fmt::Display for InputSemanticFactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "input semantic fact [{}]: {}",
            self.invariant, self.message
        )
    }
}

impl std::error::Error for InputSemanticFactError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseArenaOwner {
    Typed(usize),
    Raw(usize),
}

/// A structured construction or replay failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhaseArenaError<P: SLTPhase> {
    pub phase: PhaseKind,
    pub invariant: &'static str,
    pub owner: Option<PhaseArenaOwner>,
    pub message: String,
    marker: PhantomData<fn() -> P>,
}

impl<P: SLTPhase> PhaseArenaError<P> {
    fn new(invariant: &'static str, node_index: Option<usize>, message: impl Into<String>) -> Self {
        Self {
            phase: P::KIND,
            invariant,
            owner: node_index.map(PhaseArenaOwner::Typed),
            message: message.into(),
            marker: PhantomData,
        }
    }

    fn raw(invariant: &'static str, node_index: usize, message: impl Into<String>) -> Self {
        Self {
            phase: P::KIND,
            invariant,
            owner: Some(PhaseArenaOwner::Raw(node_index)),
            message: message.into(),
            marker: PhantomData,
        }
    }
}

impl<P: SLTPhase> fmt::Display for PhaseArenaError<P> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.owner {
            Some(PhaseArenaOwner::Typed(node)) => write!(
                formatter,
                "{:?} phase SLT verify [{}] at pn{node}: {}",
                self.phase, self.invariant, self.message
            ),
            Some(PhaseArenaOwner::Raw(node)) => write!(
                formatter,
                "{:?} phase SLT replay [{}] at raw node {node}: {}",
                self.phase, self.invariant, self.message
            ),
            None => write!(
                formatter,
                "{:?} phase SLT verify [{}]: {}",
                self.phase, self.invariant, self.message
            ),
        }
    }
}

impl<P: SLTPhase> std::error::Error for PhaseArenaError<P> {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NodeFactsRow {
    width: usize,
    signed: bool,
    zero_mask: bool,
    lowerable: bool,
}

/// Retained compact facts for a fully replayed phase arena.
#[derive(Debug)]
pub struct PhaseSLTNodeFacts<P: SLTPhase> {
    widths: Vec<usize>,
    signed: Vec<bool>,
    zero_mask: Vec<bool>,
    lowerable: Vec<bool>,
    phase: PhantomData<fn() -> P>,
}

impl<P: SLTPhase> PhaseSLTNodeFacts<P> {
    pub fn width(&self, node: PhaseNodeId<P>) -> Option<usize> {
        self.widths.get(node.index).copied()
    }

    pub fn signed(&self, node: PhaseNodeId<P>) -> Option<bool> {
        self.signed.get(node.index).copied()
    }

    pub fn has_zero_mask(&self, node: PhaseNodeId<P>) -> Option<bool> {
        self.zero_mask.get(node.index).copied()
    }

    pub fn is_lowerable(&self, node: PhaseNodeId<P>) -> Option<bool> {
        self.lowerable.get(node.index).copied()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AvlLink {
    left: usize,
    right: usize,
    parent: usize,
    height: u16,
}

impl AvlLink {
    const NONE: usize = usize::MAX;

    const fn leaf(parent: Option<usize>) -> Self {
        Self {
            left: Self::NONE,
            right: Self::NONE,
            parent: match parent {
                Some(parent) => parent,
                None => Self::NONE,
            },
            height: 1,
        }
    }

    fn decode(index: usize) -> Option<usize> {
        (index != Self::NONE).then_some(index)
    }

    fn encode(index: Option<usize>) -> usize {
        index.unwrap_or(Self::NONE)
    }

    fn left(self) -> Option<usize> {
        Self::decode(self.left)
    }

    fn right(self) -> Option<usize> {
        Self::decode(self.right)
    }

    fn parent(self) -> Option<usize> {
        Self::decode(self.parent)
    }

    fn set_left(&mut self, index: Option<usize>) {
        self.left = Self::encode(index);
    }

    fn set_right(&mut self, index: Option<usize>) {
        self.right = Self::encode(index);
    }

    fn set_parent(&mut self, index: Option<usize>) {
        self.parent = Self::encode(index);
    }
}

/// Mutable ordinary-node construction storage for one phase.
pub struct MutableSLTNodeArena<P: SLTPhase> {
    inputs: InputSemanticFacts<P>,
    nodes: Vec<PhaseSLTNode<P>>,
    widths: Vec<usize>,
    signed: Vec<bool>,
    zero_mask: Vec<bool>,
    lowerable: Vec<bool>,
    ordinary_links: Vec<AvlLink>,
    ordinary_root: Option<usize>,
    #[cfg(test)]
    max_nodes: Option<usize>,
}

impl<P: SLTPhase> MutableSLTNodeArena<P> {
    pub fn new(inputs: InputSemanticFacts<P>) -> Self {
        Self {
            inputs,
            nodes: Vec::new(),
            widths: Vec::new(),
            signed: Vec::new(),
            zero_mask: Vec::new(),
            lowerable: Vec::new(),
            ordinary_links: Vec::new(),
            ordinary_root: None,
            #[cfg(test)]
            max_nodes: None,
        }
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn node(&self, id: PhaseNodeId<P>) -> Option<&PhaseSLTNode<P>> {
        self.nodes.get(id.index)
    }

    pub fn width(&self, id: PhaseNodeId<P>) -> Option<usize> {
        self.widths.get(id.index).copied()
    }

    /// Intern one ordinary completed node. No gated identity can enter here.
    pub fn try_intern_ordinary(
        &mut self,
        node: PhaseSLTNode<P>,
    ) -> Result<InternOutcome<P>, PhaseArenaError<P>> {
        let prospective = self.nodes.len();
        if prospective == AvlLink::NONE {
            return Err(PhaseArenaError::new(
                "INTERN.INDEX_REPRESENTABLE",
                None,
                "ordinary AVL sentinel exhausted the internal usize index namespace",
            ));
        }
        let facts = verify_node(
            prospective,
            &node,
            &self.inputs,
            FactSlices::from_arena(self),
        )?;

        if let Some(existing) =
            avl_find(self.ordinary_root, &node, &self.nodes, &self.ordinary_links)
        {
            return Ok(InternOutcome::Existing(PhaseNodeId::new(existing)));
        }

        #[cfg(test)]
        if self.max_nodes.is_some_and(|limit| prospective >= limit) {
            return Err(PhaseArenaError::new(
                "ARENA.STORAGE_AVAILABLE",
                Some(prospective),
                "test capacity policy rejected the node before commit",
            ));
        }

        reserve_one(&mut self.nodes, prospective, "nodes")?;
        reserve_one(&mut self.widths, prospective, "width facts")?;
        reserve_one(&mut self.signed, prospective, "signedness facts")?;
        reserve_one(&mut self.zero_mask, prospective, "zero-mask facts")?;
        reserve_one(&mut self.lowerable, prospective, "lowerability facts")?;
        reserve_one(&mut self.ordinary_links, prospective, "ordinary AVL links")?;

        self.nodes.push(node);
        self.widths.push(facts.width);
        self.signed.push(facts.signed);
        self.zero_mask.push(facts.zero_mask);
        self.lowerable.push(facts.lowerable);
        self.ordinary_links.push(AvlLink::leaf(None));
        self.ordinary_root = Some(avl_insert_iterative(
            self.ordinary_root,
            prospective,
            &self.nodes,
            &mut self.ordinary_links,
        ));
        Ok(InternOutcome::Inserted(PhaseNodeId::new(prospective)))
    }

    #[cfg(test)]
    fn set_max_nodes_for_test(&mut self, max_nodes: usize) {
        self.max_nodes = Some(max_nodes);
    }
}

fn reserve_one<P: SLTPhase, T>(
    storage: &mut Vec<T>,
    node_index: usize,
    role: &'static str,
) -> Result<(), PhaseArenaError<P>> {
    storage.try_reserve(1).map_err(|error| {
        PhaseArenaError::new(
            "ARENA.STORAGE_AVAILABLE",
            Some(node_index),
            format!("cannot reserve {role}: {error}"),
        )
    })
}

#[derive(Clone, Copy)]
struct FactSlices<'a> {
    widths: &'a [usize],
    signed: &'a [bool],
    zero_mask: &'a [bool],
    lowerable: &'a [bool],
}

impl<'a> FactSlices<'a> {
    fn from_arena<P: SLTPhase>(arena: &'a MutableSLTNodeArena<P>) -> Self {
        Self {
            widths: &arena.widths,
            signed: &arena.signed,
            zero_mask: &arena.zero_mask,
            lowerable: &arena.lowerable,
        }
    }

    fn child<P: SLTPhase>(
        self,
        owner: usize,
        child: usize,
    ) -> Result<NodeFactsRow, PhaseArenaError<P>> {
        if child >= owner {
            return Err(PhaseArenaError::new(
                if child >= self.widths.len() {
                    "GRAPH.CHILD_EXISTS"
                } else {
                    "GRAPH.CHILD_PRECEDES_OWNER"
                },
                Some(owner),
                format!("child pn{child} does not precede completed owner pn{owner}"),
            ));
        }
        let Some(width) = self.widths.get(child).copied() else {
            return Err(PhaseArenaError::new(
                "GRAPH.CHILD_EXISTS",
                Some(owner),
                format!("owner pn{owner} references missing child pn{child}"),
            ));
        };
        let Some(signed) = self.signed.get(child).copied() else {
            return Err(PhaseArenaError::new(
                "FACTS.CHILD_AVAILABLE",
                Some(owner),
                format!("signedness for child pn{child} is unavailable"),
            ));
        };
        let Some(zero_mask) = self.zero_mask.get(child).copied() else {
            return Err(PhaseArenaError::new(
                "FACTS.CHILD_AVAILABLE",
                Some(owner),
                format!("zero-mask fact for child pn{child} is unavailable"),
            ));
        };
        let Some(lowerable) = self.lowerable.get(child).copied() else {
            return Err(PhaseArenaError::new(
                "FACTS.CHILD_AVAILABLE",
                Some(owner),
                format!("lowerability for child pn{child} is unavailable"),
            ));
        };
        Ok(NodeFactsRow {
            width,
            signed,
            zero_mask,
            lowerable,
        })
    }
}

fn verify_node<P: SLTPhase>(
    owner: usize,
    node: &PhaseSLTNode<P>,
    inputs: &InputSemanticFacts<P>,
    facts: FactSlices<'_>,
) -> Result<NodeFactsRow, PhaseArenaError<P>> {
    // Validate every edge before any child payload is read.
    try_for_each_child(node, |child| facts.child(owner, child.index).map(|_| ()))?;

    let mut all_children_lowerable = true;
    let mut all_children_zero_mask = true;
    try_for_each_child(node, |child| {
        let child = facts.child(owner, child.index)?;
        all_children_lowerable &= child.lowerable;
        all_children_zero_mask &= child.zero_mask;
        Ok(())
    })?;

    let (width, signed, zero_mask) = match node {
        PhaseSLTNode::Input(payload) => {
            let PhaseInputNode {
                input,
                index,
                access,
            } = payload.get();
            let Some(input_fact) = inputs.get(*input) else {
                return Err(PhaseArenaError::new(
                    "INPUT.ID_EXISTS",
                    Some(owner),
                    format!("input pi{} does not exist", input.index),
                ));
            };
            let width = map_rule(owner, node_rules::access_width(*access, "input"))?;
            if access.msb >= input_fact.width {
                return Err(PhaseArenaError::new(
                    "INPUT.ACCESS_IN_BOUNDS",
                    Some(owner),
                    format!(
                        "input access [{}:{}] exceeds declared width {}",
                        access.msb, access.lsb, input_fact.width
                    ),
                ));
            }
            if index.len() != input_fact.dynamic_index_strides.len() {
                return Err(PhaseArenaError::new(
                    "INPUT.INDEX_GEOMETRY_MATCHES",
                    Some(owner),
                    format!(
                        "input supplies {} dynamic indices but declaration requires {}",
                        index.len(),
                        input_fact.dynamic_index_strides.len()
                    ),
                ));
            }
            for (ordinal, (actual, expected)) in index
                .iter()
                .zip(input_fact.dynamic_index_strides.iter())
                .enumerate()
            {
                if actual.stride != *expected {
                    return Err(PhaseArenaError::new(
                        "INPUT.INDEX_GEOMETRY_MATCHES",
                        Some(owner),
                        format!(
                            "dynamic index {ordinal} has stride {}, expected {expected}",
                            actual.stride
                        ),
                    ));
                }
                let child = facts.child(owner, actual.node.index)?;
                require_nonzero(owner, child.width, "input dynamic index", actual.node.index)?;
            }
            (
                width,
                selected_signed(input_fact.signed, input_fact.width, *access),
                input_fact.domain == InputElementDomain::Bit && all_children_zero_mask,
            )
        }
        PhaseSLTNode::Constant(node) => {
            let node = node.get();
            (
                map_rule(
                    owner,
                    node_rules::constant_width(&node.payload, &node.mask, node.width),
                )?,
                node.signed,
                node.mask == BigUint::from(0u8),
            )
        }
        PhaseSLTNode::Binary { lhs, op, rhs } => {
            let lhs_fact = facts.child(owner, lhs.index)?;
            let rhs_fact = facts.child(owner, rhs.index)?;
            let width = map_rule(
                owner,
                node_rules::binary_width(*op, lhs_fact.width, rhs_fact.width),
            )?;
            (
                width,
                node_rules::binary_signed(*op, lhs_fact.signed, rhs_fact.signed),
                all_children_zero_mask,
            )
        }
        PhaseSLTNode::Unary { op, inner } => {
            let inner = facts.child(owner, inner.index)?;
            (
                node_rules::unary_width(*op, inner.width),
                node_rules::unary_signed(*op, inner.signed),
                all_children_zero_mask,
            )
        }
        PhaseSLTNode::Mux(payload) => {
            let PhaseMuxNode {
                cond,
                then_value,
                else_value,
            } = payload.get();
            let condition = facts.child(owner, cond.index)?;
            require_nonzero(owner, condition.width, "mux condition", cond.index)?;
            let then_source = facts.child(owner, then_value.value.index)?;
            let else_source = facts.child(owner, else_value.value.index)?;
            let expected_signed = node_rules::mux_signed(then_source.signed, else_source.signed);
            if then_value.coercion.target_signed != expected_signed
                || else_value.coercion.target_signed != expected_signed
            {
                return Err(PhaseArenaError::new(
                    "COERCION.MUX_SIGNEDNESS_DERIVED",
                    Some(owner),
                    format!(
                        "mux raw arms derive signed={expected_signed}, declared targets are {}/{}",
                        then_value.coercion.target_signed, else_value.coercion.target_signed
                    ),
                ));
            }
            let then_type = verify_value_use_with_basis(
                owner,
                then_value,
                facts,
                node_rules::CoercionBasis::TargetSigned,
            )?;
            let else_type = verify_value_use_with_basis(
                owner,
                else_value,
                facts,
                node_rules::CoercionBasis::TargetSigned,
            )?;
            let raw_arm_width = node_rules::mux_width(then_source.width, else_source.width);
            if then_type != else_type || then_type.0 < raw_arm_width {
                return Err(PhaseArenaError::new(
                    "COERCION.MUX_ARMS_MATCH",
                    Some(owner),
                    format!(
                        "mux raw width is {raw_arm_width}; arm coercions produce {then_type:?} and {else_type:?}"
                    ),
                ));
            }
            (
                node_rules::mux_width(then_type.0, else_type.0),
                node_rules::mux_signed(then_type.1, else_type.1),
                all_children_zero_mask,
            )
        }
        PhaseSLTNode::ForFold(payload) => {
            let PhaseForFoldNode {
                loop_input,
                loop_width,
                loop_signed,
                start,
                end,
                inclusive: _,
                step,
                step_coercion,
                step_op: _,
                reverse: _,
                states,
                result_state,
                effects,
                continue_cond,
            } = payload.get();
            if *loop_width == 0 {
                return Err(PhaseArenaError::new(
                    "FOR_FOLD.LOOP_WIDTH_NON_ZERO",
                    Some(owner),
                    "ForFold loop width is zero",
                ));
            }
            let Some(loop_fact) = inputs.get(*loop_input) else {
                return Err(PhaseArenaError::new(
                    "INPUT.ID_EXISTS",
                    Some(owner),
                    format!("ForFold loop input pi{} does not exist", loop_input.index),
                ));
            };
            if loop_fact.width != *loop_width || loop_fact.signed != *loop_signed {
                return Err(PhaseArenaError::new(
                    "FOR_FOLD.LOOP_INPUT_TYPE_MATCHES",
                    Some(owner),
                    format!(
                        "ForFold loop declares width/sign {loop_width}/{loop_signed} but input has {}/{}",
                        loop_fact.width, loop_fact.signed
                    ),
                ));
            }
            let (start_source, start_coercion) = bound_source_type(owner, "start", start, facts)?;
            let (end_source, end_coercion) = bound_source_type(owner, "end", end, facts)?;
            let step_source = typed_constant_type(owner, "step", step)?;
            // Step 1 retains and structurally validates the independently
            // typed operands. The source aggregate's transition-semantics row
            // derives compare width and operator-specific step-math width;
            // this arena must not bless a partial max-width formula as proof.
            for (source, coercion) in [
                (start_source, start_coercion),
                (end_source, end_coercion),
                (step_source, step_coercion),
            ] {
                verify_coercion(
                    owner,
                    source,
                    *coercion,
                    node_rules::CoercionBasis::SourceAndTargetSigned,
                )?;
            }
            let Some(result) = states.get(*result_state) else {
                return Err(PhaseArenaError::new(
                    "FOR_FOLD.RESULT_STATE_EXISTS",
                    Some(owner),
                    format!(
                        "result state {result_state} is outside {} canonical state rows",
                        states.len()
                    ),
                ));
            };
            let mut previous: Option<&PhaseInputAtom<P>> = None;
            for (ordinal, state) in states.iter().enumerate() {
                let Some(target_input) = inputs.get(state.target.input) else {
                    return Err(PhaseArenaError::new(
                        "INPUT.ID_EXISTS",
                        Some(owner),
                        format!(
                            "ForFold state {ordinal} target pi{} does not exist",
                            state.target.input.index
                        ),
                    ));
                };
                let target_width = map_rule(
                    owner,
                    node_rules::access_width(state.target.access, "ForFold state target"),
                )?;
                if state.target.access.msb >= target_input.width {
                    return Err(PhaseArenaError::new(
                        "FOR_FOLD.STATE_TARGET_IN_BOUNDS",
                        Some(owner),
                        format!(
                            "state {ordinal} target [{}:{}] exceeds input width {}",
                            state.target.access.msb, state.target.access.lsb, target_input.width
                        ),
                    ));
                }
                if let Some(previous) = previous {
                    if previous.input > state.target.input
                        || (previous.input == state.target.input
                            && previous.access.lsb >= state.target.access.lsb)
                    {
                        return Err(PhaseArenaError::new(
                            "FOR_FOLD.STATE_ROWS_CANONICAL",
                            Some(owner),
                            format!("state row {ordinal} is not in strict input/range order"),
                        ));
                    }
                    if previous.input == state.target.input
                        && previous.access.msb >= state.target.access.lsb
                    {
                        return Err(PhaseArenaError::new(
                            "FOR_FOLD.STATE_TARGETS_DISJOINT",
                            Some(owner),
                            format!("state rows {} and {ordinal} overlap", ordinal - 1),
                        ));
                    }
                }
                previous = Some(&state.target);
                let initial_type = verify_value_use(owner, &state.initial, facts)?;
                let update_type = verify_value_use(owner, &state.update, facts)?;
                let target_type = (
                    target_width,
                    selected_signed(target_input.signed, target_input.width, state.target.access),
                );
                if initial_type != target_type || update_type != target_type {
                    return Err(PhaseArenaError::new(
                        "FOR_FOLD.STATE_COERCION_MATCHES_TARGET",
                        Some(owner),
                        format!(
                            "state {ordinal} target type {target_type:?}, initial/update types {initial_type:?}/{update_type:?}"
                        ),
                    ));
                }
            }
            for effect in effects {
                if let Some(guard) = effect.guard {
                    let guard_fact = facts.child(owner, guard.index)?;
                    require_nonzero(owner, guard_fact.width, "ForFold effect guard", guard.index)?;
                }
                for arg in &effect.args {
                    let arg_fact = facts.child(owner, arg.index)?;
                    require_nonzero(owner, arg_fact.width, "ForFold effect argument", arg.index)?;
                }
            }
            let condition = facts.child(owner, continue_cond.index)?;
            require_nonzero(
                owner,
                condition.width,
                "ForFold continue condition",
                continue_cond.index,
            )?;
            let result_input = inputs.get(result.target.input).ok_or_else(|| {
                PhaseArenaError::new(
                    "INPUT.ID_EXISTS",
                    Some(owner),
                    "ForFold result state input disappeared",
                )
            })?;
            (
                map_rule(
                    owner,
                    node_rules::access_width(result.target.access, "ForFold result state"),
                )?,
                selected_signed(
                    result_input.signed,
                    result_input.width,
                    result.target.access,
                ),
                result_input.domain == InputElementDomain::Bit && all_children_zero_mask,
            )
        }
        PhaseSLTNode::Concat(parts) => {
            let mut total = 0usize;
            for part in parts {
                let source = facts.child(owner, part.value.value.index)?;
                let part_type = verify_value_use(owner, &part.value, facts)?;
                if part_type != (source.width, source.signed) {
                    return Err(PhaseArenaError::new(
                        "COERCION.CONCAT_PART_SELF_DETERMINED",
                        Some(owner),
                        format!(
                            "concat part source type {:?} was coerced to {part_type:?}",
                            (source.width, source.signed)
                        ),
                    ));
                }
                total = map_rule(owner, node_rules::concat_width([total, part_type.0]))?;
            }
            (total, false, all_children_zero_mask)
        }
        PhaseSLTNode::Slice { expr, access } => {
            let expression = facts.child(owner, expr.index)?;
            (
                map_rule(
                    owner,
                    node_rules::slice_width(
                        *access,
                        expression.width,
                        format_args!("pn{}", expr.index),
                    ),
                )?,
                false,
                all_children_zero_mask,
            )
        }
    };

    Ok(NodeFactsRow {
        width,
        signed,
        zero_mask,
        lowerable: node_rules::direct_lowerable(width, false) && all_children_lowerable,
    })
}

fn verify_value_use<P: SLTPhase>(
    owner: usize,
    value: &PhaseValueUse<P>,
    facts: FactSlices<'_>,
) -> Result<(usize, bool), PhaseArenaError<P>> {
    verify_value_use_with_basis(owner, value, facts, node_rules::CoercionBasis::SourceSigned)
}

fn verify_value_use_with_basis<P: SLTPhase>(
    owner: usize,
    value: &PhaseValueUse<P>,
    facts: FactSlices<'_>,
    basis: node_rules::CoercionBasis,
) -> Result<(usize, bool), PhaseArenaError<P>> {
    let source = facts.child(owner, value.value.index)?;
    verify_coercion(owner, (source.width, source.signed), value.coercion, basis)
}

fn selected_signed(declared_signed: bool, declared_width: usize, access: BitAccess) -> bool {
    declared_signed && access.lsb == 0 && access.msb == declared_width - 1
}

fn verify_coercion<P: SLTPhase>(
    owner: usize,
    source: (usize, bool),
    coercion: PhaseCoercion,
    basis: node_rules::CoercionBasis,
) -> Result<(usize, bool), PhaseArenaError<P>> {
    let expected = match map_rule(
        owner,
        node_rules::required_coercion(
            source.0,
            source.1,
            coercion.target_width,
            coercion.target_signed,
            basis,
        ),
    )? {
        node_rules::RequiredCoercion::Identity => PhaseCoercionKind::Identity,
        node_rules::RequiredCoercion::ZeroExtend => PhaseCoercionKind::ZeroExtend,
        node_rules::RequiredCoercion::SignExtend => PhaseCoercionKind::SignExtend,
        node_rules::RequiredCoercion::Truncate => PhaseCoercionKind::Truncate,
    };
    if coercion.kind != expected {
        return Err(PhaseArenaError::new(
            "COERCION.KIND_MATCHES_WIDTHS",
            Some(owner),
            format!(
                "width {} to {} requires {expected:?}, got {:?}",
                source.0, coercion.target_width, coercion.kind
            ),
        ));
    }
    Ok((coercion.target_width, coercion.target_signed))
}

fn typed_constant_type<P: SLTPhase>(
    owner: usize,
    role: &str,
    value: &PhaseTypedConstant,
) -> Result<(usize, bool), PhaseArenaError<P>> {
    let zero_mask = BigUint::from(0u8);
    let width = map_rule(
        owner,
        node_rules::constant_width(&value.payload, &zero_mask, value.width),
    )?;
    map_rule(
        owner,
        node_rules::require_nonzero(width, "FOR_FOLD.OPERAND_NON_ZERO", || {
            format!("ForFold {role} constant has zero width")
        }),
    )?;
    Ok((width, value.signed))
}

fn bound_source_type<'a, P: SLTPhase>(
    owner: usize,
    role: &str,
    bound: &'a PhaseSLTLoopBound<P>,
    facts: FactSlices<'_>,
) -> Result<((usize, bool), &'a PhaseCoercion), PhaseArenaError<P>> {
    match bound {
        PhaseSLTLoopBound::Const { value, coercion } => {
            Ok((typed_constant_type(owner, role, value)?, coercion))
        }
        PhaseSLTLoopBound::Expr(value) => {
            let source = facts.child(owner, value.value.index)?;
            require_nonzero(owner, source.width, role, value.value.index)?;
            Ok(((source.width, source.signed), &value.coercion))
        }
    }
}

fn require_nonzero<P: SLTPhase>(
    owner: usize,
    width: usize,
    role: &str,
    child: usize,
) -> Result<usize, PhaseArenaError<P>> {
    map_rule(
        owner,
        node_rules::require_nonzero(width, "OPERAND.NON_ZERO", || {
            format!("{role} pn{child} has zero width")
        }),
    )
}

fn map_rule<P: SLTPhase, T>(
    owner: usize,
    result: Result<T, NodeRuleError>,
) -> Result<T, PhaseArenaError<P>> {
    result.map_err(|error| PhaseArenaError::new(error.invariant, Some(owner), error.message))
}

fn try_for_each_child<P: SLTPhase, E>(
    node: &PhaseSLTNode<P>,
    mut visit: impl FnMut(PhaseNodeId<P>) -> Result<(), E>,
) -> Result<(), E> {
    match node {
        PhaseSLTNode::Input(node) => {
            for entry in &node.get().index {
                visit(entry.node)?;
            }
        }
        PhaseSLTNode::Constant(_) => {}
        PhaseSLTNode::Binary { lhs, rhs, .. } => {
            visit(*lhs)?;
            visit(*rhs)?;
        }
        PhaseSLTNode::Unary { inner, .. } => visit(*inner)?,
        PhaseSLTNode::Mux(node) => {
            let node = node.get();
            visit(node.cond)?;
            visit(node.then_value.value)?;
            visit(node.else_value.value)?;
        }
        PhaseSLTNode::ForFold(node) => {
            let node = node.get();
            if let PhaseSLTLoopBound::Expr(value) = &node.start {
                visit(value.value)?;
            }
            if let PhaseSLTLoopBound::Expr(value) = &node.end {
                visit(value.value)?;
            }
            for state in &node.states {
                visit(state.initial.value)?;
                visit(state.update.value)?;
            }
            for effect in &node.effects {
                if let Some(guard) = effect.guard {
                    visit(guard)?;
                }
                for arg in &effect.args {
                    visit(*arg)?;
                }
            }
            visit(node.continue_cond)?;
        }
        PhaseSLTNode::Concat(parts) => {
            for part in parts {
                visit(part.value.value)?;
            }
        }
        PhaseSLTNode::Slice { expr, .. } => visit(*expr)?,
    }
    Ok(())
}

fn avl_find<P: SLTPhase>(
    mut root: Option<usize>,
    candidate: &PhaseSLTNode<P>,
    nodes: &[PhaseSLTNode<P>],
    links: &[AvlLink],
) -> Option<usize> {
    while let Some(index) = root {
        match candidate.cmp(&nodes[index]) {
            Ordering::Less => root = links[index].left(),
            Ordering::Equal => return Some(index),
            Ordering::Greater => root = links[index].right(),
        }
    }
    None
}

fn avl_height(root: Option<usize>, links: &[AvlLink]) -> u16 {
    root.map_or(0, |index| links[index].height)
}

fn avl_refresh(index: usize, links: &mut [AvlLink]) {
    links[index].height =
        1 + avl_height(links[index].left(), links).max(avl_height(links[index].right(), links));
}

fn avl_rotate_left(root: usize, links: &mut [AvlLink]) -> usize {
    let Some(pivot) = links[root].right() else {
        return root;
    };
    let parent = links[root].parent();
    let middle = links[pivot].left();
    links[root].set_right(middle);
    if let Some(middle) = middle {
        links[middle].set_parent(Some(root));
    }
    links[pivot].set_left(Some(root));
    links[pivot].set_parent(parent);
    links[root].set_parent(Some(pivot));
    avl_refresh(root, links);
    avl_refresh(pivot, links);
    pivot
}

fn avl_rotate_right(root: usize, links: &mut [AvlLink]) -> usize {
    let Some(pivot) = links[root].left() else {
        return root;
    };
    let parent = links[root].parent();
    let middle = links[pivot].right();
    links[root].set_left(middle);
    if let Some(middle) = middle {
        links[middle].set_parent(Some(root));
    }
    links[pivot].set_right(Some(root));
    links[pivot].set_parent(parent);
    links[root].set_parent(Some(pivot));
    avl_refresh(root, links);
    avl_refresh(pivot, links);
    pivot
}

fn reconnect_rotated_root(
    old_root: usize,
    new_root: usize,
    parent: Option<usize>,
    tree_root: &mut usize,
    links: &mut [AvlLink],
) {
    let Some(parent) = parent else {
        *tree_root = new_root;
        return;
    };
    if links[parent].left() == Some(old_root) {
        links[parent].set_left(Some(new_root));
    } else if links[parent].right() == Some(old_root) {
        links[parent].set_right(Some(new_root));
    }
}

fn avl_insert_iterative<P: SLTPhase>(
    root: Option<usize>,
    inserted: usize,
    nodes: &[PhaseSLTNode<P>],
    links: &mut [AvlLink],
) -> usize {
    let Some(mut tree_root) = root else {
        return inserted;
    };

    let mut cursor = tree_root;
    let parent = loop {
        if nodes[inserted] < nodes[cursor] {
            if let Some(next) = links[cursor].left() {
                cursor = next;
            } else {
                links[cursor].set_left(Some(inserted));
                break cursor;
            }
        } else if let Some(next) = links[cursor].right() {
            cursor = next;
        } else {
            links[cursor].set_right(Some(inserted));
            break cursor;
        }
    };
    links[inserted].set_parent(Some(parent));

    let mut next = Some(parent);
    while let Some(current) = next {
        let parent = links[current].parent();
        avl_refresh(current, links);
        let balance = i32::from(avl_height(links[current].left(), links))
            - i32::from(avl_height(links[current].right(), links));
        let mut replacement = current;
        if balance > 1 {
            if let Some(left) = links[current].left() {
                if nodes[inserted] > nodes[left] {
                    let rotated = avl_rotate_left(left, links);
                    links[current].set_left(Some(rotated));
                    links[rotated].set_parent(Some(current));
                }
                replacement = avl_rotate_right(current, links);
            }
        } else if balance < -1
            && let Some(right) = links[current].right()
        {
            if nodes[inserted] < nodes[right] {
                let rotated = avl_rotate_right(right, links);
                links[current].set_right(Some(rotated));
                links[rotated].set_parent(Some(current));
            }
            replacement = avl_rotate_left(current, links);
        }
        if replacement != current {
            reconnect_rotated_root(current, replacement, parent, &mut tree_root, links);
        }
        next = parent;
    }
    tree_root
}

/// Private cache-free shell. Aggregate artifacts will own this value; there is
/// deliberately no public constructor, standalone freeze, or serde impl.
struct FrozenSLTNodeArena<P: SLTPhase> {
    nodes: Vec<PhaseSLTNode<P>>,
    facts: PhaseSLTNodeFacts<P>,
}

impl<P: SLTPhase> FrozenSLTNodeArena<P> {
    #[cfg(test)]
    fn node(&self, id: PhaseNodeId<P>) -> Option<&PhaseSLTNode<P>> {
        self.nodes.get(id.index)
    }
}

struct PrivateSealPlan<P: SLTPhase> {
    replay: StructuralReplay<P>,
    compact_nodes: Vec<PhaseSLTNode<P>>,
}

fn prepare_seal<P: SLTPhase>(
    arena: &MutableSLTNodeArena<P>,
) -> Result<PrivateSealPlan<P>, PhaseArenaError<P>> {
    let replay = replay_typed(&arena.inputs, &arena.nodes)?;
    if replay.facts.widths != arena.widths
        || replay.facts.signed != arena.signed
        || replay.facts.zero_mask != arena.zero_mask
        || replay.facts.lowerable != arena.lowerable
    {
        return Err(PhaseArenaError::new(
            "FACTS.REPLAY_MATCHES_CONSTRUCTION",
            None,
            "replayed facts differ from construction facts",
        ));
    }
    if replay.ordinary_root != arena.ordinary_root || replay.ordinary_links != arena.ordinary_links
    {
        return Err(PhaseArenaError::new(
            "INTERN.REPLAY_INDEX_MATCHES_CONSTRUCTION",
            None,
            "replayed ordinary AVL differs from the live construction index",
        ));
    }
    for (index, node) in arena.nodes.iter().enumerate() {
        if avl_find(
            arena.ordinary_root,
            node,
            &arena.nodes,
            &arena.ordinary_links,
        ) != Some(index)
        {
            return Err(PhaseArenaError::new(
                "INTERN.CONSTRUCTION_INDEX_BIDIRECTIONAL",
                Some(index),
                "ordinary construction index does not resolve this owned node to itself",
            ));
        }
    }
    let mut compact_nodes = Vec::new();
    compact_nodes
        .try_reserve_exact(arena.nodes.len())
        .map_err(|error| {
            PhaseArenaError::new(
                "FREEZE.STORAGE_AVAILABLE",
                None,
                format!(
                    "cannot reserve exact cache-free storage for {} nodes: {error}",
                    arena.nodes.len()
                ),
            )
        })?;
    Ok(PrivateSealPlan {
        replay,
        compact_nodes,
    })
}

struct PreparedSeal<P: SLTPhase> {
    arena: MutableSLTNodeArena<P>,
    plan: PrivateSealPlan<P>,
}

impl<P: SLTPhase> PreparedSeal<P> {
    fn commit(mut self) -> FrozenSLTNodeArena<P> {
        self.plan.compact_nodes.extend(self.arena.nodes);
        FrozenSLTNodeArena {
            nodes: self.plan.compact_nodes,
            facts: self.plan.replay.facts,
        }
    }
}

fn try_prepare_seal<P: SLTPhase>(
    arena: MutableSLTNodeArena<P>,
) -> Result<PreparedSeal<P>, (MutableSLTNodeArena<P>, PhaseArenaError<P>)> {
    match prepare_seal(&arena) {
        Ok(plan) => Ok(PreparedSeal { arena, plan }),
        Err(error) => Err((arena, error)),
    }
}

#[derive(Debug)]
struct StructuralReplay<P: SLTPhase> {
    facts: PhaseSLTNodeFacts<P>,
    ordinary_links: Vec<AvlLink>,
    ordinary_root: Option<usize>,
}

fn replay_typed<P: SLTPhase>(
    inputs: &InputSemanticFacts<P>,
    nodes: &[PhaseSLTNode<P>],
) -> Result<StructuralReplay<P>, PhaseArenaError<P>> {
    // First pass checks all edges without dereferencing a child ID.
    for (owner, node) in nodes.iter().enumerate() {
        try_for_each_child(node, |child| {
            if child.index >= nodes.len() {
                return Err(PhaseArenaError::raw(
                    "GRAPH.CHILD_EXISTS",
                    owner,
                    format!(
                        "owner pn{owner} references missing child pn{} in {} nodes",
                        child.index,
                        nodes.len()
                    ),
                ));
            }
            if child.index >= owner {
                return Err(PhaseArenaError::raw(
                    "GRAPH.CHILD_PRECEDES_OWNER",
                    owner,
                    format!("child pn{} does not precede pn{owner}", child.index),
                ));
            }
            Ok(())
        })?;
    }

    let mut widths = Vec::new();
    let mut signed = Vec::new();
    let mut zero_mask = Vec::new();
    let mut lowerable = Vec::new();
    let mut links = Vec::new();
    widths.try_reserve_exact(nodes.len()).map_err(|error| {
        PhaseArenaError::new(
            "FACTS.STORAGE_AVAILABLE",
            None,
            format!("cannot reserve replay widths: {error}"),
        )
    })?;
    for (storage, role) in [
        (&mut signed, "replay signedness"),
        (&mut zero_mask, "replay zero-mask"),
        (&mut lowerable, "replay lowerability"),
    ] {
        storage.try_reserve_exact(nodes.len()).map_err(|error| {
            PhaseArenaError::new(
                "FACTS.STORAGE_AVAILABLE",
                None,
                format!("cannot reserve {role}: {error}"),
            )
        })?;
    }
    links.try_reserve_exact(nodes.len()).map_err(|error| {
        PhaseArenaError::new(
            "FACTS.STORAGE_AVAILABLE",
            None,
            format!("cannot reserve replay AVL links: {error}"),
        )
    })?;
    let mut root = None;
    for (owner, node) in nodes.iter().enumerate() {
        if let Some(existing) = avl_find(root, node, nodes, &links) {
            return Err(PhaseArenaError::raw(
                "INTERN.ORDINARY_UNIQUE",
                owner,
                format!("ordinary node duplicates canonical pn{existing}"),
            ));
        }
        let row = verify_node(
            owner,
            node,
            inputs,
            FactSlices {
                widths: &widths,
                signed: &signed,
                zero_mask: &zero_mask,
                lowerable: &lowerable,
            },
        )?;
        widths.push(row.width);
        signed.push(row.signed);
        zero_mask.push(row.zero_mask);
        lowerable.push(row.lowerable);
        links.push(AvlLink::leaf(None));
        root = Some(avl_insert_iterative(root, owner, nodes, &mut links));
    }
    Ok(StructuralReplay {
        facts: PhaseSLTNodeFacts {
            widths,
            signed,
            zero_mask,
            lowerable,
            phase: PhantomData,
        },
        ordinary_links: links,
        ordinary_root: root,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> InputSemanticFacts<SourcePhase> {
        InputSemanticFacts::try_from_verified_rows(vec![
            InputSemanticFact::try_new(64, false, InputElementDomain::Bit, Vec::new()).unwrap(),
            InputSemanticFact::try_new(64, true, InputElementDomain::Logic, Vec::new()).unwrap(),
            InputSemanticFact::try_new(8, false, InputElementDomain::Bit, vec![8]).unwrap(),
        ])
        .unwrap()
    }

    fn constant(value: u64, width: usize) -> PhaseSLTNode<SourcePhase> {
        PhaseSLTNode::try_constant(BigUint::from(value), BigUint::from(0u8), width, false).unwrap()
    }

    fn input_node(
        input: PhaseInputId<SourcePhase>,
        access: BitAccess,
    ) -> PhaseSLTNode<SourcePhase> {
        PhaseSLTNode::try_input(input, Vec::new(), access).unwrap()
    }

    fn mux_node(
        cond: PhaseNodeId<SourcePhase>,
        then_value: PhaseValueUse<SourcePhase>,
        else_value: PhaseValueUse<SourcePhase>,
    ) -> PhaseSLTNode<SourcePhase> {
        PhaseSLTNode::try_mux(cond, then_value, else_value).unwrap()
    }

    fn identity<P: SLTPhase>(value: PhaseNodeId<P>, width: usize) -> PhaseValueUse<P> {
        PhaseValueUse {
            value,
            coercion: PhaseCoercion {
                target_width: width,
                target_signed: false,
                kind: PhaseCoercionKind::Identity,
            },
        }
    }

    fn typed_constant(value: u64, width: usize, signed: bool) -> PhaseTypedConstant {
        PhaseTypedConstant {
            payload: BigUint::from(value),
            width,
            signed,
        }
    }

    fn loop_bound(value: u64, width: usize, signed: bool) -> PhaseSLTLoopBound<SourcePhase> {
        PhaseSLTLoopBound::Const {
            value: typed_constant(value, width, signed),
            coercion: PhaseCoercion {
                target_width: width,
                target_signed: signed,
                kind: PhaseCoercionKind::Identity,
            },
        }
    }

    #[test]
    fn phase_ids_and_input_semantics_are_checked() {
        let inputs = inputs();
        let bit = inputs.id_at(0).unwrap();
        let logic = inputs.id_at(1).unwrap();
        let mut arena = MutableSLTNodeArena::new(inputs);
        let bit_node = arena
            .try_intern_ordinary(input_node(bit, BitAccess { lsb: 0, msb: 7 }))
            .unwrap()
            .id();
        let logic_node = arena
            .try_intern_ordinary(input_node(logic, BitAccess { lsb: 0, msb: 7 }))
            .unwrap()
            .id();
        assert_eq!(arena.width(bit_node), Some(8));
        assert_eq!(arena.zero_mask[bit_node.index], true);
        assert_eq!(arena.zero_mask[logic_node.index], false);

        let error = arena
            .try_intern_ordinary(input_node(bit, BitAccess { lsb: 0, msb: 64 }))
            .unwrap_err();
        assert_eq!(error.invariant, "INPUT.ACCESS_IN_BOUNDS");
        assert_eq!(arena.len(), 2);
    }

    #[test]
    fn ordinary_interning_reuses_without_cloning_payload_into_a_key() {
        let mut arena = MutableSLTNodeArena::new(inputs());
        let first = arena.try_intern_ordinary(constant(7, 8)).unwrap();
        let second = arena.try_intern_ordinary(constant(7, 8)).unwrap();
        assert!(matches!(first, InternOutcome::Inserted(_)));
        assert_eq!(second, InternOutcome::Existing(first.id()));
        assert_eq!(arena.len(), 1);
    }

    #[test]
    fn persistent_node_and_avl_layout_stay_compact() {
        assert!(std::mem::size_of::<PhaseSLTNode<SourcePhase>>() <= 32);
        assert!(std::mem::size_of::<AvlLink>() <= 32);
    }

    #[test]
    fn malformed_children_fail_without_mutating_arena() {
        let mut arena = MutableSLTNodeArena::new(inputs());
        let first = arena.try_intern_ordinary(constant(1, 1)).unwrap().id();
        let error = arena
            .try_intern_ordinary(PhaseSLTNode::Unary {
                op: UnaryOp::Ident,
                inner: PhaseNodeId::new(9),
            })
            .unwrap_err();
        assert_eq!(error.invariant, "GRAPH.CHILD_EXISTS");
        assert_eq!(arena.len(), 1);
        assert_eq!(arena.width(first), Some(1));
        let error = arena
            .try_intern_ordinary(PhaseSLTNode::Unary {
                op: UnaryOp::Ident,
                inner: PhaseNodeId::new(1),
            })
            .unwrap_err();
        assert_eq!(error.invariant, "GRAPH.CHILD_EXISTS");
        assert_eq!(arena.len(), 1);
    }

    #[test]
    fn failed_capacity_policy_leaves_facts_and_index_unchanged() {
        let mut arena = MutableSLTNodeArena::new(inputs());
        let first = arena.try_intern_ordinary(constant(1, 1)).unwrap().id();
        arena.set_max_nodes_for_test(1);
        let before_root = arena.ordinary_root;
        let error = arena.try_intern_ordinary(constant(2, 2)).unwrap_err();
        assert_eq!(error.invariant, "ARENA.STORAGE_AVAILABLE");
        assert_eq!(arena.len(), 1);
        assert_eq!(arena.width(first), Some(1));
        assert_eq!(arena.ordinary_root, before_root);
        assert_eq!(
            arena.try_intern_ordinary(constant(1, 1)).unwrap(),
            InternOutcome::Existing(first)
        );
    }

    #[test]
    fn explicit_coercion_is_verified() {
        let inputs = inputs();
        let signed_input = inputs.id_at(1).unwrap();
        let mut arena = MutableSLTNodeArena::new(inputs);
        let c = arena.try_intern_ordinary(constant(1, 1)).unwrap().id();
        let invalid = mux_node(
            c,
            PhaseValueUse {
                value: c,
                coercion: PhaseCoercion {
                    target_width: 8,
                    target_signed: false,
                    kind: PhaseCoercionKind::Identity,
                },
            },
            PhaseValueUse {
                value: c,
                coercion: PhaseCoercion {
                    target_width: 8,
                    target_signed: false,
                    kind: PhaseCoercionKind::ZeroExtend,
                },
            },
        );
        let error = arena.try_intern_ordinary(invalid).unwrap_err();
        assert_eq!(error.invariant, "COERCION.KIND_MATCHES_WIDTHS");
        assert_eq!(arena.len(), 1);

        let signed = arena
            .try_intern_ordinary(input_node(signed_input, BitAccess { lsb: 0, msb: 0 }))
            .unwrap()
            .id();
        let invalid_signed_widen = mux_node(
            c,
            PhaseValueUse {
                value: signed,
                coercion: PhaseCoercion {
                    target_width: 8,
                    target_signed: true,
                    kind: PhaseCoercionKind::ZeroExtend,
                },
            },
            PhaseValueUse {
                value: c,
                coercion: PhaseCoercion {
                    target_width: 8,
                    target_signed: true,
                    kind: PhaseCoercionKind::ZeroExtend,
                },
            },
        );
        let error = arena.try_intern_ordinary(invalid_signed_widen).unwrap_err();
        assert_eq!(error.invariant, "COERCION.MUX_SIGNEDNESS_DERIVED");
        assert_eq!(arena.len(), 2);

        let mixed = arena
            .try_intern_ordinary(mux_node(
                c,
                PhaseValueUse {
                    value: signed,
                    coercion: PhaseCoercion {
                        target_width: 8,
                        target_signed: false,
                        kind: PhaseCoercionKind::ZeroExtend,
                    },
                },
                PhaseValueUse {
                    value: c,
                    coercion: PhaseCoercion {
                        target_width: 8,
                        target_signed: false,
                        kind: PhaseCoercionKind::ZeroExtend,
                    },
                },
            ))
            .unwrap()
            .id();
        assert!(!arena.signed[mixed.index]);

        let wide = arena.try_intern_ordinary(constant(0, 8)).unwrap().id();
        let truncated_mux = mux_node(
            c,
            PhaseValueUse {
                value: wide,
                coercion: PhaseCoercion {
                    target_width: 1,
                    target_signed: false,
                    kind: PhaseCoercionKind::Truncate,
                },
            },
            PhaseValueUse {
                value: wide,
                coercion: PhaseCoercion {
                    target_width: 1,
                    target_signed: false,
                    kind: PhaseCoercionKind::Truncate,
                },
            },
        );
        assert_eq!(
            arena
                .try_intern_ordinary(truncated_mux)
                .unwrap_err()
                .invariant,
            "COERCION.MUX_ARMS_MATCH"
        );
    }

    #[test]
    fn signedness_and_zero_mask_follow_language_rules() {
        let inputs = inputs();
        let signed_input = inputs.id_at(1).unwrap();
        let mut arena = MutableSLTNodeArena::new(inputs);
        let signed_full = arena
            .try_intern_ordinary(input_node(signed_input, BitAccess { lsb: 0, msb: 63 }))
            .unwrap()
            .id();
        let signed_part = arena
            .try_intern_ordinary(input_node(signed_input, BitAccess { lsb: 0, msb: 7 }))
            .unwrap()
            .id();
        let unsigned = arena.try_intern_ordinary(constant(0, 64)).unwrap().id();
        assert!(arena.signed[signed_full.index]);
        assert!(!arena.signed[signed_part.index]);

        let mixed = arena
            .try_intern_ordinary(PhaseSLTNode::Binary {
                lhs: signed_full,
                op: BinaryOp::Add,
                rhs: unsigned,
            })
            .unwrap()
            .id();
        assert!(!arena.signed[mixed.index]);

        let reduction = arena
            .try_intern_ordinary(PhaseSLTNode::Unary {
                op: UnaryOp::And,
                inner: signed_full,
            })
            .unwrap()
            .id();
        assert!(!arena.signed[reduction.index]);

        let unsigned_minus = arena
            .try_intern_ordinary(PhaseSLTNode::Unary {
                op: UnaryOp::Minus,
                inner: unsigned,
            })
            .unwrap()
            .id();
        assert!(!arena.signed[unsigned_minus.index]);

        let signed_bit_not = arena
            .try_intern_ordinary(PhaseSLTNode::Unary {
                op: UnaryOp::BitNot,
                inner: signed_full,
            })
            .unwrap()
            .id();
        assert!(arena.signed[signed_bit_not.index]);

        let slice = arena
            .try_intern_ordinary(PhaseSLTNode::Slice {
                expr: signed_full,
                access: BitAccess { lsb: 0, msb: 7 },
            })
            .unwrap()
            .id();
        assert!(!arena.signed[slice.index]);

        let wildcard = arena
            .try_intern_ordinary(PhaseSLTNode::Binary {
                lhs: signed_full,
                op: BinaryOp::EqWildcard,
                rhs: unsigned,
            })
            .unwrap()
            .id();
        assert!(
            !arena.zero_mask[wildcard.index],
            "wildcard equality with a four-state lhs is not known two-state"
        );
    }

    #[test]
    fn for_fold_state_rows_are_canonical_and_result_is_explicit() {
        let inputs = inputs();
        let loop_input = inputs.id_at(0).unwrap();
        let state_input = inputs.id_at(0).unwrap();
        let mut arena = MutableSLTNodeArena::new(inputs);
        let value = arena.try_intern_ordinary(constant(0, 8)).unwrap().id();
        let condition = arena.try_intern_ordinary(constant(1, 1)).unwrap().id();
        let state = |lsb, msb| PhaseForFoldState {
            target: PhaseInputAtom {
                input: state_input,
                access: BitAccess { lsb, msb },
            },
            initial: identity(value, 8),
            update: identity(value, 8),
        };
        let valid = PhaseSLTNode::try_for_fold(PhaseForFoldNode {
            loop_input,
            loop_width: 64,
            loop_signed: false,
            start: loop_bound(0, 64, false),
            end: loop_bound(2, 64, false),
            inclusive: false,
            step: typed_constant(1, 64, false),
            step_coercion: PhaseCoercion {
                target_width: 64,
                target_signed: false,
                kind: PhaseCoercionKind::Identity,
            },
            step_op: SLTStepOp::Add,
            reverse: false,
            states: vec![state(0, 7), state(8, 15)],
            result_state: 1,
            effects: Vec::new(),
            continue_cond: condition,
        })
        .unwrap();
        let id = arena.try_intern_ordinary(valid).unwrap().id();
        assert_eq!(arena.width(id), Some(8));

        let invalid = PhaseSLTNode::try_for_fold(PhaseForFoldNode {
            loop_input,
            loop_width: 64,
            loop_signed: false,
            start: loop_bound(0, 64, false),
            end: loop_bound(2, 64, false),
            inclusive: false,
            step: typed_constant(1, 64, false),
            step_coercion: PhaseCoercion {
                target_width: 64,
                target_signed: false,
                kind: PhaseCoercionKind::Identity,
            },
            step_op: SLTStepOp::Add,
            reverse: false,
            states: vec![state(8, 15), state(0, 7)],
            result_state: 0,
            effects: Vec::new(),
            continue_cond: condition,
        })
        .unwrap();
        assert_eq!(
            arena.try_intern_ordinary(invalid).unwrap_err().invariant,
            "FOR_FOLD.STATE_ROWS_CANONICAL"
        );
    }

    #[test]
    fn for_fold_retains_arbitrary_width_operands_for_transition_verification() {
        let inputs = inputs();
        let loop_input = inputs.id_at(0).unwrap();
        let mut arena = MutableSLTNodeArena::new(inputs);
        let huge = arena
            .try_intern_ordinary(constant(0, usize::MAX))
            .unwrap()
            .id();
        let condition = arena.try_intern_ordinary(constant(1, 1)).unwrap().id();
        let state_value = arena.try_intern_ordinary(constant(0, 8)).unwrap().id();
        let node = PhaseSLTNode::try_for_fold(PhaseForFoldNode {
            loop_input,
            loop_width: 64,
            loop_signed: false,
            start: PhaseSLTLoopBound::Expr(PhaseValueUse {
                value: huge,
                coercion: PhaseCoercion {
                    target_width: usize::MAX,
                    target_signed: false,
                    kind: PhaseCoercionKind::Identity,
                },
            }),
            end: loop_bound(1, 64, false),
            inclusive: true,
            step: typed_constant(1, 64, false),
            step_coercion: PhaseCoercion {
                target_width: 64,
                target_signed: false,
                kind: PhaseCoercionKind::Identity,
            },
            step_op: SLTStepOp::Add,
            reverse: false,
            states: vec![PhaseForFoldState {
                target: PhaseInputAtom {
                    input: loop_input,
                    access: BitAccess { lsb: 0, msb: 7 },
                },
                initial: identity(state_value, 8),
                update: identity(state_value, 8),
            }],
            result_state: 0,
            effects: Vec::new(),
            continue_cond: condition,
        })
        .unwrap();
        let id = arena.try_intern_ordinary(node).unwrap().id();
        assert_eq!(arena.width(id), Some(8));
        let PhaseSLTNode::ForFold(payload) = arena.node(id).unwrap() else {
            panic!("stored ForFold must retain its typed expression bound")
        };
        let PhaseForFoldNode {
            start: PhaseSLTLoopBound::Expr(start),
            inclusive: true,
            ..
        } = payload.get()
        else {
            panic!("stored ForFold must retain its typed expression bound")
        };
        assert_eq!(start.coercion.target_width, usize::MAX);
    }

    #[test]
    fn replay_rejects_noncanonical_ordinary_duplicate() {
        let nodes = vec![constant(3, 2), constant(3, 2)];
        let error = replay_typed(&inputs(), &nodes).unwrap_err();
        assert_eq!(error.invariant, "INTERN.ORDINARY_UNIQUE");
        assert_eq!(error.phase, PhaseKind::Source);
        assert_eq!(error.owner, Some(PhaseArenaOwner::Raw(1)));
    }

    #[test]
    fn replay_scans_all_raw_edges_before_reading_facts() {
        let missing = vec![PhaseSLTNode::Unary {
            op: UnaryOp::Ident,
            inner: PhaseNodeId::new(7),
        }];
        let error = replay_typed(&inputs(), &missing).unwrap_err();
        assert_eq!(error.invariant, "GRAPH.CHILD_EXISTS");
        assert_eq!(error.owner, Some(PhaseArenaOwner::Raw(0)));

        let forward = vec![
            PhaseSLTNode::Unary {
                op: UnaryOp::Ident,
                inner: PhaseNodeId::new(1),
            },
            constant(1, 1),
        ];
        let error = replay_typed(&inputs(), &forward).unwrap_err();
        assert_eq!(error.invariant, "GRAPH.CHILD_PRECEDES_OWNER");
        assert_eq!(error.owner, Some(PhaseArenaOwner::Raw(0)));

        let cyclic = vec![
            PhaseSLTNode::Unary {
                op: UnaryOp::Ident,
                inner: PhaseNodeId::new(1),
            },
            PhaseSLTNode::Unary {
                op: UnaryOp::Ident,
                inner: PhaseNodeId::new(0),
            },
        ];
        let error = replay_typed(&inputs(), &cyclic).unwrap_err();
        assert_eq!(error.invariant, "GRAPH.CHILD_PRECEDES_OWNER");
        assert_eq!(error.owner, Some(PhaseArenaOwner::Raw(0)));
    }

    #[test]
    fn deep_graph_replay_is_iterative() {
        let mut arena = MutableSLTNodeArena::new(inputs());
        let mut node = arena.try_intern_ordinary(constant(1, 1)).unwrap().id();
        for _ in 0..100_000 {
            node = arena
                .try_intern_ordinary(PhaseSLTNode::Unary {
                    op: UnaryOp::Ident,
                    inner: node,
                })
                .unwrap()
                .id();
        }
        assert_eq!(arena.width(node), Some(1));
        let prepared = match try_prepare_seal(arena) {
            Ok(prepared) => prepared,
            Err((_arena, error)) => panic!("valid arena must prepare: {error}"),
        };
        let frozen = prepared.commit();
        assert!(frozen.node(node).is_some());
        assert_eq!(frozen.facts.width(node), Some(1));
        assert_eq!(frozen.nodes.capacity(), frozen.nodes.len());
    }

    #[test]
    fn large_concat_payload_has_one_owned_copy() {
        let mut arena = MutableSLTNodeArena::new(inputs());
        let bit = arena.try_intern_ordinary(constant(1, 1)).unwrap().id();
        let mut parts = Vec::new();
        parts.try_reserve_exact(32_768).unwrap();
        for _ in 0..32_768 {
            parts.push(PhaseConcatPart {
                value: identity(bit, 1),
            });
        }
        let concat = arena
            .try_intern_ordinary(PhaseSLTNode::Concat(parts))
            .unwrap()
            .id();
        assert_eq!(arena.width(concat), Some(32_768));
        let PhaseSLTNode::Concat(stored) = arena.node(concat).unwrap() else {
            unreachable!()
        };
        assert_eq!(stored.len(), 32_768);
        assert_eq!(arena.len(), 2);
    }

    #[test]
    fn avl_remains_balanced_for_ordered_payloads() {
        let mut arena = MutableSLTNodeArena::new(inputs());
        for value in 0..10_000u64 {
            arena.try_intern_ordinary(constant(value, 64)).unwrap();
        }
        let root = arena.ordinary_root.unwrap();
        assert!(arena.ordinary_links[root].height < 32);
    }

    #[test]
    fn seal_replays_and_checks_the_live_index_bidirectionally() {
        let mut arena = MutableSLTNodeArena::new(inputs());
        for value in 0..3u64 {
            arena.try_intern_ordinary(constant(value, 64)).unwrap();
        }
        arena.ordinary_root = Some(0);
        let Err((arena, error)) = try_prepare_seal(arena) else {
            panic!("corrupt live AVL must not seal")
        };
        assert_eq!(error.invariant, "INTERN.REPLAY_INDEX_MATCHES_CONSTRUCTION");
        assert_eq!(arena.len(), 3, "failed prepare must preserve the builder");
    }

    #[test]
    #[ignore = "manual million-node scale/RSS check"]
    fn manual_million_node_build_prepare_commit() {
        eprintln!(
            "phase arena sizes: node={} avl_link={} facts_row={} value_use={}",
            std::mem::size_of::<PhaseSLTNode<SourcePhase>>(),
            std::mem::size_of::<AvlLink>(),
            std::mem::size_of::<NodeFactsRow>(),
            std::mem::size_of::<PhaseValueUse<SourcePhase>>(),
        );
        let started = std::time::Instant::now();
        let mut arena = MutableSLTNodeArena::new(inputs());
        let mut node = arena.try_intern_ordinary(constant(1, 1)).unwrap().id();
        for _ in 1..1_000_000 {
            node = arena
                .try_intern_ordinary(PhaseSLTNode::Unary {
                    op: UnaryOp::Ident,
                    inner: node,
                })
                .unwrap()
                .id();
        }
        let built = started.elapsed();
        assert_eq!(arena.len(), 1_000_000);
        let prepared = match try_prepare_seal(arena) {
            Ok(prepared) => prepared,
            Err((_arena, error)) => panic!("million-node arena must prepare: {error}"),
        };
        let prepared_at = started.elapsed();
        let frozen = prepared.commit();
        let committed_at = started.elapsed();
        assert_eq!(frozen.nodes.len(), 1_000_000);
        assert_eq!(frozen.facts.width(node), Some(1));
        eprintln!(
            "million-node phase arena: build={built:?}, prepare={:?}, commit={:?}, total={committed_at:?}",
            prepared_at - built,
            committed_at - prepared_at,
        );
    }

    fn large_for_fold_node(
        loop_input: PhaseInputId<SourcePhase>,
        bit: PhaseNodeId<SourcePhase>,
        payload_width: usize,
        state_count: usize,
        effect_count: usize,
    ) -> PhaseSLTNode<SourcePhase> {
        let typed_payload = |low: u8| PhaseTypedConstant {
            payload: (BigUint::from(1u8) << (payload_width - 1)) | BigUint::from(low),
            width: payload_width,
            signed: false,
        };
        let identity_coercion = PhaseCoercion {
            target_width: payload_width,
            target_signed: false,
            kind: PhaseCoercionKind::Identity,
        };
        let mut states = Vec::new();
        states.try_reserve_exact(state_count).unwrap();
        for ordinal in 0..state_count {
            states.push(PhaseForFoldState {
                target: PhaseInputAtom {
                    input: loop_input,
                    access: BitAccess {
                        lsb: ordinal,
                        msb: ordinal,
                    },
                },
                initial: identity(bit, 1),
                update: identity(bit, 1),
            });
        }
        let mut effects = Vec::new();
        effects.try_reserve_exact(effect_count).unwrap();
        for site in 0..effect_count {
            effects.push(PhaseForFoldEffect {
                site_id: PhaseRuntimeEventSiteId::new(u32::try_from(site).unwrap()),
                guard: Some(bit),
                emit_on_true: site % 2 == 0,
                args: vec![bit; 8],
                fatal_error_code: (site % 17 == 0).then_some(site as i64),
            });
        }
        PhaseSLTNode::try_for_fold(PhaseForFoldNode {
            loop_input,
            loop_width: payload_width,
            loop_signed: false,
            start: PhaseSLTLoopBound::Const {
                value: typed_payload(1),
                coercion: identity_coercion,
            },
            end: PhaseSLTLoopBound::Const {
                value: typed_payload(2),
                coercion: identity_coercion,
            },
            inclusive: true,
            step: typed_payload(3),
            step_coercion: identity_coercion,
            step_op: SLTStepOp::Mul,
            reverse: true,
            states,
            result_state: state_count - 1,
            effects,
            continue_cond: bit,
        })
        .unwrap()
    }

    #[test]
    #[ignore = "manual large ForFold key/RSS check"]
    fn manual_large_for_fold_key_and_duplicate_lookup() {
        const PAYLOAD_WIDTH: usize = 131_072;
        const STATE_COUNT: usize = 50_000;
        const EFFECT_COUNT: usize = 10_000;

        let started = std::time::Instant::now();
        let inputs = InputSemanticFacts::try_from_verified_rows(vec![
            InputSemanticFact::try_new(PAYLOAD_WIDTH, false, InputElementDomain::Bit, Vec::new())
                .unwrap(),
        ])
        .unwrap();
        let loop_input = inputs.id_at(0).unwrap();
        let mut arena = MutableSLTNodeArena::new(inputs);
        let bit = arena.try_intern_ordinary(constant(1, 1)).unwrap().id();
        let node = large_for_fold_node(loop_input, bit, PAYLOAD_WIDTH, STATE_COUNT, EFFECT_COUNT);
        let inserted = arena.try_intern_ordinary(node).unwrap().id();
        let inserted_at = started.elapsed();
        let duplicate =
            large_for_fold_node(loop_input, bit, PAYLOAD_WIDTH, STATE_COUNT, EFFECT_COUNT);
        assert_eq!(
            arena.try_intern_ordinary(duplicate).unwrap(),
            InternOutcome::Existing(inserted)
        );
        let duplicate_at = started.elapsed();
        assert_eq!(arena.len(), 2);
        let prepared = match try_prepare_seal(arena) {
            Ok(prepared) => prepared,
            Err((_arena, error)) => panic!("large ForFold arena must prepare: {error}"),
        };
        let prepared_at = started.elapsed();
        let frozen = prepared.commit();
        let committed_at = started.elapsed();
        assert_eq!(frozen.facts.width(inserted), Some(1));
        eprintln!(
            "large ForFold phase key: insert={inserted_at:?}, duplicate={:?}, prepare={:?}, commit={:?}, total={committed_at:?}",
            duplicate_at - inserted_at,
            prepared_at - duplicate_at,
            committed_at - prepared_at,
        );
    }
}
