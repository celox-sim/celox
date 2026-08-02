//! AArch64 scalar machine IR.
//!
//! This is deliberately owned by the AArch64 backend.  The current production
//! pipeline lowers the established x86 scalar MIR into this form after
//! allocation; future AArch64 selection, optimization, and allocation can
//! target the same boundary without teaching the emitter about x86 opcodes.

use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct VReg(pub(crate) u32);

impl fmt::Debug for VReg {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "v{}", self.0)
    }
}

impl fmt::Display for VReg {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "v{}", self.0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockId(pub u32);

impl fmt::Debug for BlockId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "bb{}", self.0)
    }
}

impl fmt::Display for BlockId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "bb{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct ConstantTableId(pub(crate) usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum OpSize {
    S8,
    S16,
    S32,
    S64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum BaseReg {
    SimState,
    StackFrame,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum CmpKind {
    Eq,
    Ne,
    LtU,
    LtS,
    LeU,
    LeS,
    GtU,
    GtS,
    GeU,
    GeS,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum BranchPredicate {
    Compare {
        lhs: VReg,
        rhs: VReg,
        kind: CmpKind,
    },
    CompareImm {
        lhs: VReg,
        imm: i32,
        kind: CmpKind,
    },
    MemoryNonZero {
        base: BaseReg,
        offset: i32,
        size: OpSize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PackedLaneCompareRhs {
    Scalar(VReg),
    Memory { offset: i32 },
}

pub(crate) const SPARSE_COMMIT_DESCRIPTOR_WORDS: usize = 8;

/// Word-level AArch64 operations before physical-register substitution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MInst {
    Mov {
        dst: VReg,
        src: VReg,
    },
    Mov32 {
        dst: VReg,
        src: VReg,
    },
    LoadImm {
        dst: VReg,
        value: u64,
    },
    Scratch {
        dst: VReg,
    },
    LoadConstantTableAddr {
        dst: VReg,
        table: ConstantTableId,
    },
    Load {
        dst: VReg,
        base: BaseReg,
        offset: i32,
        size: OpSize,
    },
    Store {
        base: BaseReg,
        offset: i32,
        src: VReg,
        size: OpSize,
    },
    AndStoreImm {
        base: BaseReg,
        offset: i32,
        size: OpSize,
        imm: u64,
    },
    OrStoreImm {
        base: BaseReg,
        offset: i32,
        size: OpSize,
        imm: u64,
    },
    LoadPtr {
        dst: VReg,
        ptr: VReg,
        offset: i32,
        size: OpSize,
    },
    StorePtr {
        ptr: VReg,
        offset: i32,
        src: VReg,
        size: OpSize,
    },
    ReleaseStorePtr {
        ptr: VReg,
        offset: i32,
        src: VReg,
        size: OpSize,
    },
    LoadIndexed {
        dst: VReg,
        base: BaseReg,
        offset: i32,
        index: VReg,
        scale: u8,
        size: OpSize,
    },
    PackedLaneCompare {
        dst: VReg,
        rhs: PackedLaneCompareRhs,
        kind: CmpKind,
        offset: i32,
        lane_count: u8,
        element_stride: u8,
        bit_offset: u8,
        field_width: u8,
    },
    PackedByteAffineCompare {
        dst: VReg,
        base: VReg,
        rhs: VReg,
        kind: CmpKind,
    },
    StoreIndexed {
        base: BaseReg,
        offset: i32,
        index: VReg,
        src: VReg,
        size: OpSize,
    },
    OrStoreIndexed {
        base: BaseReg,
        offset: i32,
        index: VReg,
        src: VReg,
        size: OpSize,
    },
    LoadPtrIndexed {
        dst: VReg,
        ptr: VReg,
        offset: i32,
        index: VReg,
        size: OpSize,
    },
    StorePtrIndexed {
        ptr: VReg,
        offset: i32,
        index: VReg,
        src: VReg,
        size: OpSize,
    },
    ReleaseStorePtrIndexed {
        ptr: VReg,
        offset: i32,
        index: VReg,
        src: VReg,
        size: OpSize,
    },
    MemCopy {
        src_offset: i32,
        dst_offset: i32,
        byte_len: usize,
    },
    MemFill {
        dst_offset: i32,
        byte_len: usize,
        value: u8,
    },
    SparseCommit {
        src_offset: i32,
        dst_offset: i32,
        byte_size: usize,
        dirty_words_offset: i32,
        dirty_word_count: usize,
        summary_words_offset: i32,
        summary_word_count: usize,
        four_state: bool,
    },
    SparseMarkActive {
        active_index: u32,
        active_bits_offset: i32,
        active_capacity: usize,
    },
    SparseCommitWorklist {
        descriptor_table: ConstantTableId,
        active_bits_offset: i32,
        active_capacity: usize,
    },
    Add {
        dst: VReg,
        lhs: VReg,
        rhs: VReg,
    },
    Add32 {
        dst: VReg,
        lhs: VReg,
        rhs: VReg,
    },
    Sub {
        dst: VReg,
        lhs: VReg,
        rhs: VReg,
    },
    Sub32 {
        dst: VReg,
        lhs: VReg,
        rhs: VReg,
    },
    Mul {
        dst: VReg,
        lhs: VReg,
        rhs: VReg,
    },
    Mul32 {
        dst: VReg,
        lhs: VReg,
        rhs: VReg,
    },
    UMulHi {
        dst: VReg,
        lhs: VReg,
        rhs: VReg,
    },
    And {
        dst: VReg,
        lhs: VReg,
        rhs: VReg,
    },
    And32 {
        dst: VReg,
        lhs: VReg,
        rhs: VReg,
    },
    Or {
        dst: VReg,
        lhs: VReg,
        rhs: VReg,
    },
    Or32 {
        dst: VReg,
        lhs: VReg,
        rhs: VReg,
    },
    Xor {
        dst: VReg,
        lhs: VReg,
        rhs: VReg,
    },
    Xor32 {
        dst: VReg,
        lhs: VReg,
        rhs: VReg,
    },
    Shr {
        dst: VReg,
        lhs: VReg,
        rhs: VReg,
    },
    Shl {
        dst: VReg,
        lhs: VReg,
        rhs: VReg,
    },
    Sar {
        dst: VReg,
        lhs: VReg,
        rhs: VReg,
    },
    AndImm {
        dst: VReg,
        src: VReg,
        imm: u64,
    },
    AndImm32 {
        dst: VReg,
        src: VReg,
        imm: u32,
    },
    OrImm {
        dst: VReg,
        src: VReg,
        imm: u64,
    },
    ShrImm {
        dst: VReg,
        src: VReg,
        imm: u8,
    },
    ShlImm {
        dst: VReg,
        src: VReg,
        imm: u8,
    },
    SarImm {
        dst: VReg,
        src: VReg,
        imm: u8,
    },
    AddImm {
        dst: VReg,
        src: VReg,
        imm: i32,
    },
    SubImm {
        dst: VReg,
        src: VReg,
        imm: i32,
    },
    Cmp {
        dst: VReg,
        lhs: VReg,
        rhs: VReg,
        kind: CmpKind,
    },
    CmpImm {
        dst: VReg,
        lhs: VReg,
        imm: i32,
        kind: CmpKind,
    },
    UDiv {
        dst: VReg,
        lhs: VReg,
        rhs: VReg,
    },
    URem {
        dst: VReg,
        lhs: VReg,
        rhs: VReg,
    },
    SDiv {
        dst: VReg,
        lhs: VReg,
        rhs: VReg,
    },
    SRem {
        dst: VReg,
        lhs: VReg,
        rhs: VReg,
    },
    BitNot {
        dst: VReg,
        src: VReg,
    },
    Neg {
        dst: VReg,
        src: VReg,
    },
    Popcnt {
        dst: VReg,
        src: VReg,
    },
    Bsf {
        dst: VReg,
        src: VReg,
    },
    Bsr {
        dst: VReg,
        src: VReg,
    },
    BsrOr {
        dst: VReg,
        src: VReg,
        zero_value: u8,
    },
    Pext {
        dst: VReg,
        src: VReg,
        mask: VReg,
    },
    Pdep {
        dst: VReg,
        src: VReg,
        mask: VReg,
    },
    Select {
        dst: VReg,
        cond: VReg,
        true_val: VReg,
        false_val: VReg,
    },
    CmpSelect {
        dst: VReg,
        lhs: VReg,
        rhs: VReg,
        kind: CmpKind,
        true_val: VReg,
        false_val: VReg,
    },
    CmpImmSelect {
        dst: VReg,
        lhs: VReg,
        imm: i32,
        kind: CmpKind,
        true_val: VReg,
        false_val: VReg,
    },
    GuardedCmpSelect {
        dst: VReg,
        guard: VReg,
        lhs: VReg,
        rhs: VReg,
        kind: CmpKind,
        true_val: VReg,
        false_val: VReg,
    },
    Branch {
        cond: VReg,
        true_bb: BlockId,
        false_bb: BlockId,
    },
    BranchPred {
        predicate: BranchPredicate,
        true_bb: BlockId,
        false_bb: BlockId,
    },
    JumpTable {
        index: VReg,
        targets: Box<[BlockId]>,
    },
    Jump {
        target: BlockId,
    },
    Return,
    ReturnError {
        code: i64,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct MBlock {
    pub(crate) id: BlockId,
    pub(crate) insts: Vec<MInst>,
}

#[derive(Debug, Clone)]
pub(crate) struct MFunction {
    pub(crate) blocks: Vec<MBlock>,
    constant_tables: Vec<Vec<u64>>,
}

impl MFunction {
    pub(crate) fn new(blocks: Vec<MBlock>, constant_tables: Vec<Vec<u64>>) -> Self {
        Self {
            blocks,
            constant_tables,
        }
    }

    pub(crate) fn constant_tables(&self) -> &[Vec<u64>] {
        &self.constant_tables
    }
}

/// Complete codegen-ready artifact produced at the AArch64 MIR boundary.
pub(crate) struct AllocatedFunction {
    pub(crate) function: MFunction,
    pub(crate) assignment: crate::allocation::Assignment<VReg>,
    pub(crate) edge_copies: crate::allocation::EdgeCopyPlan<BlockId>,
}
