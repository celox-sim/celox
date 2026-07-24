//! Verified handoff from lane-aggregate analysis to native instruction selection.
//!
//! These records are deliberately independent of the analysis graph. Every
//! operation contains enough information to emit code without inspecting the
//! original SIR instruction shape.

use crate::HashSet;
use crate::ir::{BinaryOp, BlockId, RegionedAbsoluteAddr, RegisterId, UnaryOp};

#[derive(Debug, Clone)]
pub(crate) struct LaneAggregatePlan {
    pub(crate) nodes: Vec<LaneAggregatePlanNode>,
    pub(crate) roots: Vec<LaneAggregatePlanRoot>,
    pub(crate) dead_scalar_registers: HashSet<RegisterId>,
}

#[derive(Debug, Clone)]
pub(crate) struct LaneAggregatePlanNode {
    pub(crate) operation: LaneAggregatePlanOp,
    pub(crate) children: Vec<usize>,
    pub(crate) lane_width: usize,
    pub(crate) lane_count: usize,
}

#[derive(Debug, Clone)]
pub(crate) enum LaneAggregatePlanOp {
    StateRead(LaneAggregateMaterialization),
    Constant(Vec<u64>),
    BroadcastScalar(RegisterId),
    Affine(Vec<u64>),
    PackedExtract(Vec<usize>),
    SsaPack {
        block: BlockId,
        values: Vec<RegisterId>,
    },
    Unary(UnaryOp),
    Binary(BinaryOp),
    ShiftConstant {
        operation: BinaryOp,
        amount: usize,
    },
    OneHotDecode {
        shift_width: usize,
    },
    Mux,
    ControlMux,
    ScalarInsert {
        block: BlockId,
        values: Vec<RegisterId>,
    },
    Slice {
        offset: usize,
        width: usize,
    },
    Concat {
        operand_widths: Vec<usize>,
    },
}

#[derive(Debug, Clone)]
pub(crate) enum LaneAggregateMaterialization {
    ReloadAtSink(Vec<LaneAggregateStateLoad>),
    ReloadAtFrontier {
        block: BlockId,
        loads: Vec<LaneAggregateStateLoad>,
    },
    DominatingSsa {
        block: BlockId,
        values: Vec<RegisterId>,
    },
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct LaneAggregateStateLoad {
    pub(crate) register: RegisterId,
    pub(crate) address: RegionedAbsoluteAddr,
    pub(crate) bit_offset: usize,
    pub(crate) width: usize,
    pub(crate) physical_byte: usize,
    pub(crate) physical_bit: usize,
    /// Stable identity of the exact placement-StateSSA version.
    pub(crate) state_slot: usize,
    pub(crate) state_version: usize,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct LaneAggregatePlanRoot {
    pub(crate) block: BlockId,
    pub(crate) original_root: RegisterId,
    pub(crate) recipe_root: usize,
    pub(crate) publication_address: RegionedAbsoluteAddr,
    pub(crate) publication_bit_offset: usize,
    pub(crate) lane_count: usize,
}
