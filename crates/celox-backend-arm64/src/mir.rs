//! AArch64 scalar machine IR.
//!
//! This is deliberately owned by the AArch64 backend. Instruction selection
//! lowers SIR directly into this form, then this backend performs spilling,
//! coloring, SSA destruction, and machine-code emission without depending on
//! another target's opcodes or allocation policy.

use std::collections::BTreeMap;
use std::fmt;

use crate::RegionedAbsoluteAddr;

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

#[derive(Debug, Clone, Default)]
pub(crate) struct VRegAllocator {
    next: u32,
}

impl VRegAllocator {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn alloc(&mut self) -> VReg {
        let value = VReg(self.next);
        self.next = self.next.checked_add(1).expect("AArch64 VReg overflow");
        value
    }

    pub(crate) fn count(&self) -> u32 {
        self.next
    }
}

#[derive(Debug, Clone)]
pub(crate) enum SpillKind {
    SimState {
        addr: RegionedAbsoluteAddr,
        bit_offset: usize,
        width_bits: usize,
    },
    SimStateAlias,
    Stack,
    Remat {
        value: u64,
    },
}

/// Instruction-selection provenance. AArch64 allocation deliberately does
/// not consume target-specific spill costs, but keeping rematerialization and
/// state-origin facts here lets the selector reason about constants and safe
/// aliases.
#[derive(Debug, Clone)]
pub(crate) struct SpillDesc {
    pub(crate) kind: SpillKind,
    pub(crate) spill_cost: u8,
}

impl SpillDesc {
    pub(crate) fn transient() -> Self {
        Self {
            kind: SpillKind::Stack,
            spill_cost: 1,
        }
    }

    pub(crate) fn remat(value: u64) -> Self {
        Self {
            kind: SpillKind::Remat { value },
            spill_cost: 0,
        }
    }

    pub(crate) fn sim_state(
        addr: RegionedAbsoluteAddr,
        bit_offset: usize,
        width_bits: usize,
        store_back_only: bool,
    ) -> Self {
        Self {
            kind: SpillKind::SimState {
                addr,
                bit_offset,
                width_bits,
            },
            spill_cost: u8::from(!store_back_only),
        }
    }

    pub(crate) fn sim_state_alias(
        _addr: RegionedAbsoluteAddr,
        _bit_offset: usize,
        _width_bits: usize,
        store_back_only: bool,
    ) -> Self {
        Self {
            kind: SpillKind::SimStateAlias,
            spill_cost: u8::from(!store_back_only),
        }
    }

    pub(crate) fn copy_for_snapshot(&self) -> Self {
        self.clone()
    }

    pub(crate) fn with_state_insert(self, _value: VReg, _bit_offset: usize, _width: usize) -> Self {
        self
    }

    pub(crate) fn with_state_insert_fragment(
        self,
        _value: VReg,
        _value_bit_offset: usize,
        _bit_offset: usize,
        _width: usize,
    ) -> Self {
        self
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

impl OpSize {
    pub(crate) fn from_bits(bits: usize) -> Option<Self> {
        match bits {
            8 => Some(Self::S8),
            16 => Some(Self::S16),
            32 => Some(Self::S32),
            64 => Some(Self::S64),
            _ => None,
        }
    }

    pub(crate) const fn bytes(self) -> u8 {
        match self {
            Self::S8 => 1,
            Self::S16 => 2,
            Self::S32 => 4,
            Self::S64 => 8,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum BaseReg {
    SimState,
    StackFrame,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct MemoryAliasRange {
    offset: i32,
    byte_len: usize,
}

impl MemoryAliasRange {
    pub(crate) fn new(offset: i32, byte_len: usize) -> Option<Self> {
        if byte_len == 0 {
            return None;
        }
        i64::from(offset)
            .checked_add(i64::try_from(byte_len).ok()?)
            .map(|_| Self { offset, byte_len })
    }
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
#[allow(dead_code)] // Reserved for target-owned MIR optimization and branch folding.
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
    Memory {
        offset: i32,
        alias_range: Option<MemoryAliasRange>,
    },
}

pub(crate) const SPARSE_COMMIT_DESCRIPTOR_WORDS: usize = 8;

pub(crate) struct SparseCommitDescriptor {
    pub(crate) src_offset: u64,
    pub(crate) dst_offset: u64,
    pub(crate) byte_size: u64,
    pub(crate) dirty_words_offset: u64,
    pub(crate) dirty_word_count: u64,
    pub(crate) summary_words_offset: u64,
    pub(crate) summary_word_count: u64,
    pub(crate) four_state: u64,
}

impl SparseCommitDescriptor {
    pub(crate) const WORDS: usize = SPARSE_COMMIT_DESCRIPTOR_WORDS;

    pub(crate) fn words(self) -> [u64; Self::WORDS] {
        [
            self.src_offset,
            self.dst_offset,
            self.byte_size,
            self.dirty_words_offset,
            self.dirty_word_count,
            self.summary_words_offset,
            self.summary_word_count,
            self.four_state,
        ]
    }
}

/// Word-level AArch64 operations before physical-register substitution.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // Some recipes are introduced only by optional late MIR passes.
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
    /// Allocation-only edge use. Emission intentionally produces no code.
    KeepAlive {
        src: VReg,
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
        alias_range: Option<MemoryAliasRange>,
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
        alias_range: Option<MemoryAliasRange>,
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
        alias_range: Option<MemoryAliasRange>,
    },
    OrStoreIndexed {
        base: BaseReg,
        offset: i32,
        index: VReg,
        src: VReg,
        size: OpSize,
        alias_range: Option<MemoryAliasRange>,
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

impl MInst {
    pub(crate) fn def(&self) -> Option<VReg> {
        match self {
            Self::Mov { dst, .. }
            | Self::Mov32 { dst, .. }
            | Self::LoadImm { dst, .. }
            | Self::Scratch { dst }
            | Self::LoadConstantTableAddr { dst, .. }
            | Self::Load { dst, .. }
            | Self::LoadPtr { dst, .. }
            | Self::LoadIndexed { dst, .. }
            | Self::PackedLaneCompare { dst, .. }
            | Self::PackedByteAffineCompare { dst, .. }
            | Self::LoadPtrIndexed { dst, .. }
            | Self::Add { dst, .. }
            | Self::Add32 { dst, .. }
            | Self::Sub { dst, .. }
            | Self::Sub32 { dst, .. }
            | Self::Mul { dst, .. }
            | Self::Mul32 { dst, .. }
            | Self::UMulHi { dst, .. }
            | Self::And { dst, .. }
            | Self::And32 { dst, .. }
            | Self::Or { dst, .. }
            | Self::Or32 { dst, .. }
            | Self::Xor { dst, .. }
            | Self::Xor32 { dst, .. }
            | Self::Shr { dst, .. }
            | Self::Shl { dst, .. }
            | Self::Sar { dst, .. }
            | Self::AndImm { dst, .. }
            | Self::AndImm32 { dst, .. }
            | Self::OrImm { dst, .. }
            | Self::ShrImm { dst, .. }
            | Self::ShlImm { dst, .. }
            | Self::SarImm { dst, .. }
            | Self::AddImm { dst, .. }
            | Self::SubImm { dst, .. }
            | Self::Cmp { dst, .. }
            | Self::CmpImm { dst, .. }
            | Self::UDiv { dst, .. }
            | Self::URem { dst, .. }
            | Self::SDiv { dst, .. }
            | Self::SRem { dst, .. }
            | Self::BitNot { dst, .. }
            | Self::Neg { dst, .. }
            | Self::Popcnt { dst, .. }
            | Self::Bsf { dst, .. }
            | Self::Bsr { dst, .. }
            | Self::BsrOr { dst, .. }
            | Self::Select { dst, .. }
            | Self::CmpSelect { dst, .. }
            | Self::CmpImmSelect { dst, .. }
            | Self::GuardedCmpSelect { dst, .. } => Some(*dst),
            Self::Store { .. }
            | Self::KeepAlive { .. }
            | Self::AndStoreImm { .. }
            | Self::OrStoreImm { .. }
            | Self::StorePtr { .. }
            | Self::ReleaseStorePtr { .. }
            | Self::StoreIndexed { .. }
            | Self::OrStoreIndexed { .. }
            | Self::StorePtrIndexed { .. }
            | Self::ReleaseStorePtrIndexed { .. }
            | Self::MemCopy { .. }
            | Self::MemFill { .. }
            | Self::SparseCommit { .. }
            | Self::SparseMarkActive { .. }
            | Self::SparseCommitWorklist { .. }
            | Self::Branch { .. }
            | Self::BranchPred { .. }
            | Self::JumpTable { .. }
            | Self::Jump { .. }
            | Self::Return
            | Self::ReturnError { .. } => None,
        }
    }

    pub(crate) fn def_mut(&mut self) -> Option<&mut VReg> {
        match self {
            Self::Mov { dst, .. }
            | Self::Mov32 { dst, .. }
            | Self::LoadImm { dst, .. }
            | Self::Scratch { dst }
            | Self::LoadConstantTableAddr { dst, .. }
            | Self::Load { dst, .. }
            | Self::LoadPtr { dst, .. }
            | Self::LoadIndexed { dst, .. }
            | Self::PackedLaneCompare { dst, .. }
            | Self::PackedByteAffineCompare { dst, .. }
            | Self::LoadPtrIndexed { dst, .. }
            | Self::Add { dst, .. }
            | Self::Add32 { dst, .. }
            | Self::Sub { dst, .. }
            | Self::Sub32 { dst, .. }
            | Self::Mul { dst, .. }
            | Self::Mul32 { dst, .. }
            | Self::UMulHi { dst, .. }
            | Self::And { dst, .. }
            | Self::And32 { dst, .. }
            | Self::Or { dst, .. }
            | Self::Or32 { dst, .. }
            | Self::Xor { dst, .. }
            | Self::Xor32 { dst, .. }
            | Self::Shr { dst, .. }
            | Self::Shl { dst, .. }
            | Self::Sar { dst, .. }
            | Self::AndImm { dst, .. }
            | Self::AndImm32 { dst, .. }
            | Self::OrImm { dst, .. }
            | Self::ShrImm { dst, .. }
            | Self::ShlImm { dst, .. }
            | Self::SarImm { dst, .. }
            | Self::AddImm { dst, .. }
            | Self::SubImm { dst, .. }
            | Self::Cmp { dst, .. }
            | Self::CmpImm { dst, .. }
            | Self::UDiv { dst, .. }
            | Self::URem { dst, .. }
            | Self::SDiv { dst, .. }
            | Self::SRem { dst, .. }
            | Self::BitNot { dst, .. }
            | Self::Neg { dst, .. }
            | Self::Popcnt { dst, .. }
            | Self::Bsf { dst, .. }
            | Self::Bsr { dst, .. }
            | Self::BsrOr { dst, .. }
            | Self::Select { dst, .. }
            | Self::CmpSelect { dst, .. }
            | Self::CmpImmSelect { dst, .. }
            | Self::GuardedCmpSelect { dst, .. } => Some(dst),
            Self::Store { .. }
            | Self::KeepAlive { .. }
            | Self::AndStoreImm { .. }
            | Self::OrStoreImm { .. }
            | Self::StorePtr { .. }
            | Self::ReleaseStorePtr { .. }
            | Self::StoreIndexed { .. }
            | Self::OrStoreIndexed { .. }
            | Self::StorePtrIndexed { .. }
            | Self::ReleaseStorePtrIndexed { .. }
            | Self::MemCopy { .. }
            | Self::MemFill { .. }
            | Self::SparseCommit { .. }
            | Self::SparseMarkActive { .. }
            | Self::SparseCommitWorklist { .. }
            | Self::Branch { .. }
            | Self::BranchPred { .. }
            | Self::JumpTable { .. }
            | Self::Jump { .. }
            | Self::Return
            | Self::ReturnError { .. } => None,
        }
    }

    pub(crate) fn uses(&self) -> Vec<VReg> {
        match self {
            Self::Mov { src, .. } | Self::Mov32 { src, .. } | Self::KeepAlive { src } => vec![*src],
            Self::LoadImm { .. }
            | Self::Scratch { .. }
            | Self::LoadConstantTableAddr { .. }
            | Self::Load { .. }
            | Self::AndStoreImm { .. }
            | Self::OrStoreImm { .. }
            | Self::MemCopy { .. }
            | Self::MemFill { .. }
            | Self::SparseCommit { .. }
            | Self::SparseMarkActive { .. }
            | Self::SparseCommitWorklist { .. } => Vec::new(),
            Self::Store { src, .. } => vec![*src],
            Self::LoadPtr { ptr, .. } => vec![*ptr],
            Self::StorePtr { ptr, src, .. } | Self::ReleaseStorePtr { ptr, src, .. } => {
                vec![*ptr, *src]
            }
            Self::LoadIndexed { index, .. } => vec![*index],
            Self::PackedLaneCompare {
                rhs: PackedLaneCompareRhs::Scalar(value),
                ..
            } => vec![*value],
            Self::PackedLaneCompare {
                rhs: PackedLaneCompareRhs::Memory { .. },
                ..
            } => Vec::new(),
            Self::PackedByteAffineCompare { base, rhs, .. } => vec![*base, *rhs],
            Self::StoreIndexed { index, src, .. } | Self::OrStoreIndexed { index, src, .. } => {
                vec![*index, *src]
            }
            Self::LoadPtrIndexed { ptr, index, .. } => vec![*ptr, *index],
            Self::StorePtrIndexed {
                ptr, index, src, ..
            }
            | Self::ReleaseStorePtrIndexed {
                ptr, index, src, ..
            } => vec![*ptr, *index, *src],
            Self::Add { lhs, rhs, .. }
            | Self::Add32 { lhs, rhs, .. }
            | Self::Sub { lhs, rhs, .. }
            | Self::Sub32 { lhs, rhs, .. }
            | Self::Mul { lhs, rhs, .. }
            | Self::Mul32 { lhs, rhs, .. }
            | Self::UMulHi { lhs, rhs, .. }
            | Self::And { lhs, rhs, .. }
            | Self::And32 { lhs, rhs, .. }
            | Self::Or { lhs, rhs, .. }
            | Self::Or32 { lhs, rhs, .. }
            | Self::Xor { lhs, rhs, .. }
            | Self::Xor32 { lhs, rhs, .. }
            | Self::Shr { lhs, rhs, .. }
            | Self::Shl { lhs, rhs, .. }
            | Self::Sar { lhs, rhs, .. }
            | Self::Cmp { lhs, rhs, .. }
            | Self::UDiv { lhs, rhs, .. }
            | Self::URem { lhs, rhs, .. }
            | Self::SDiv { lhs, rhs, .. }
            | Self::SRem { lhs, rhs, .. } => vec![*lhs, *rhs],
            Self::AndImm { src, .. }
            | Self::AndImm32 { src, .. }
            | Self::OrImm { src, .. }
            | Self::ShrImm { src, .. }
            | Self::ShlImm { src, .. }
            | Self::SarImm { src, .. }
            | Self::AddImm { src, .. }
            | Self::SubImm { src, .. }
            | Self::BitNot { src, .. }
            | Self::Neg { src, .. }
            | Self::Popcnt { src, .. }
            | Self::Bsf { src, .. }
            | Self::Bsr { src, .. }
            | Self::BsrOr { src, .. } => vec![*src],
            Self::CmpImm { lhs, .. } => vec![*lhs],
            Self::Select {
                cond,
                true_val,
                false_val,
                ..
            } => vec![*cond, *true_val, *false_val],
            Self::CmpSelect {
                lhs,
                rhs,
                true_val,
                false_val,
                ..
            } => vec![*lhs, *rhs, *true_val, *false_val],
            Self::CmpImmSelect {
                lhs,
                true_val,
                false_val,
                ..
            } => vec![*lhs, *true_val, *false_val],
            Self::GuardedCmpSelect {
                guard,
                lhs,
                rhs,
                true_val,
                false_val,
                ..
            } => vec![*guard, *lhs, *rhs, *true_val, *false_val],
            Self::Branch { cond, .. } => vec![*cond],
            Self::BranchPred {
                predicate: BranchPredicate::Compare { lhs, rhs, .. },
                ..
            } => vec![*lhs, *rhs],
            Self::BranchPred {
                predicate: BranchPredicate::CompareImm { lhs, .. },
                ..
            } => vec![*lhs],
            Self::BranchPred {
                predicate: BranchPredicate::MemoryNonZero { .. },
                ..
            } => Vec::new(),
            Self::JumpTable { index, .. } => vec![*index],
            Self::Jump { .. } | Self::Return | Self::ReturnError { .. } => Vec::new(),
        }
    }

    pub(crate) fn rewrite_use(&mut self, old: VReg, new: VReg) {
        let rewrite = |value: &mut VReg| {
            if *value == old {
                *value = new;
            }
        };
        match self {
            Self::Mov { src, .. }
            | Self::Mov32 { src, .. }
            | Self::KeepAlive { src }
            | Self::Store { src, .. } => {
                rewrite(src);
            }
            Self::LoadPtr { ptr, .. } => rewrite(ptr),
            Self::StorePtr { ptr, src, .. } | Self::ReleaseStorePtr { ptr, src, .. } => {
                rewrite(ptr);
                rewrite(src);
            }
            Self::LoadIndexed { index, .. } => rewrite(index),
            Self::PackedLaneCompare {
                rhs: PackedLaneCompareRhs::Scalar(value),
                ..
            } => rewrite(value),
            Self::PackedByteAffineCompare { base, rhs, .. } => {
                rewrite(base);
                rewrite(rhs);
            }
            Self::StoreIndexed { index, src, .. } | Self::OrStoreIndexed { index, src, .. } => {
                rewrite(index);
                rewrite(src);
            }
            Self::LoadPtrIndexed { ptr, index, .. } => {
                rewrite(ptr);
                rewrite(index);
            }
            Self::StorePtrIndexed {
                ptr, index, src, ..
            }
            | Self::ReleaseStorePtrIndexed {
                ptr, index, src, ..
            } => {
                rewrite(ptr);
                rewrite(index);
                rewrite(src);
            }
            Self::Add { lhs, rhs, .. }
            | Self::Add32 { lhs, rhs, .. }
            | Self::Sub { lhs, rhs, .. }
            | Self::Sub32 { lhs, rhs, .. }
            | Self::Mul { lhs, rhs, .. }
            | Self::Mul32 { lhs, rhs, .. }
            | Self::UMulHi { lhs, rhs, .. }
            | Self::And { lhs, rhs, .. }
            | Self::And32 { lhs, rhs, .. }
            | Self::Or { lhs, rhs, .. }
            | Self::Or32 { lhs, rhs, .. }
            | Self::Xor { lhs, rhs, .. }
            | Self::Xor32 { lhs, rhs, .. }
            | Self::Shr { lhs, rhs, .. }
            | Self::Shl { lhs, rhs, .. }
            | Self::Sar { lhs, rhs, .. }
            | Self::Cmp { lhs, rhs, .. }
            | Self::UDiv { lhs, rhs, .. }
            | Self::URem { lhs, rhs, .. }
            | Self::SDiv { lhs, rhs, .. }
            | Self::SRem { lhs, rhs, .. } => {
                rewrite(lhs);
                rewrite(rhs);
            }
            Self::AndImm { src, .. }
            | Self::AndImm32 { src, .. }
            | Self::OrImm { src, .. }
            | Self::ShrImm { src, .. }
            | Self::ShlImm { src, .. }
            | Self::SarImm { src, .. }
            | Self::AddImm { src, .. }
            | Self::SubImm { src, .. }
            | Self::BitNot { src, .. }
            | Self::Neg { src, .. }
            | Self::Popcnt { src, .. }
            | Self::Bsf { src, .. }
            | Self::Bsr { src, .. }
            | Self::BsrOr { src, .. } => rewrite(src),
            Self::CmpImm { lhs, .. } => rewrite(lhs),
            Self::Select {
                cond,
                true_val,
                false_val,
                ..
            } => {
                rewrite(cond);
                rewrite(true_val);
                rewrite(false_val);
            }
            Self::CmpSelect {
                lhs,
                rhs,
                true_val,
                false_val,
                ..
            } => {
                rewrite(lhs);
                rewrite(rhs);
                rewrite(true_val);
                rewrite(false_val);
            }
            Self::CmpImmSelect {
                lhs,
                true_val,
                false_val,
                ..
            } => {
                rewrite(lhs);
                rewrite(true_val);
                rewrite(false_val);
            }
            Self::GuardedCmpSelect {
                guard,
                lhs,
                rhs,
                true_val,
                false_val,
                ..
            } => {
                rewrite(guard);
                rewrite(lhs);
                rewrite(rhs);
                rewrite(true_val);
                rewrite(false_val);
            }
            Self::Branch { cond, .. } => rewrite(cond),
            Self::BranchPred {
                predicate: BranchPredicate::Compare { lhs, rhs, .. },
                ..
            } => {
                rewrite(lhs);
                rewrite(rhs);
            }
            Self::BranchPred {
                predicate: BranchPredicate::CompareImm { lhs, .. },
                ..
            } => rewrite(lhs),
            Self::JumpTable { index, .. } => rewrite(index),
            Self::LoadImm { .. }
            | Self::Scratch { .. }
            | Self::LoadConstantTableAddr { .. }
            | Self::Load { .. }
            | Self::AndStoreImm { .. }
            | Self::OrStoreImm { .. }
            | Self::PackedLaneCompare {
                rhs: PackedLaneCompareRhs::Memory { .. },
                ..
            }
            | Self::MemCopy { .. }
            | Self::MemFill { .. }
            | Self::SparseCommit { .. }
            | Self::SparseMarkActive { .. }
            | Self::SparseCommitWorklist { .. }
            | Self::BranchPred {
                predicate: BranchPredicate::MemoryNonZero { .. },
                ..
            }
            | Self::Jump { .. }
            | Self::Return
            | Self::ReturnError { .. } => {}
        }
    }

    pub(crate) fn is_copy(&self) -> bool {
        matches!(self, Self::Mov { .. })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PhiNode {
    pub(crate) dst: VReg,
    pub(crate) sources: Vec<(BlockId, VReg)>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum SpilledPhiSource {
    Value(VReg),
    Stack(i32),
}

#[derive(Debug, Clone)]
pub(crate) struct SpilledPhiNode {
    pub(crate) successor: BlockId,
    pub(crate) destination: i32,
    pub(crate) sources: Vec<(BlockId, SpilledPhiSource)>,
}

#[derive(Debug, Clone)]
pub(crate) struct MBlock {
    pub(crate) id: BlockId,
    pub(crate) phis: Vec<PhiNode>,
    pub(crate) insts: Vec<MInst>,
}

impl MBlock {
    pub(crate) fn new(id: BlockId) -> Self {
        Self {
            id,
            phis: Vec::new(),
            insts: Vec::new(),
        }
    }

    pub(crate) fn push(&mut self, instruction: MInst) {
        self.insts.push(instruction);
    }

    pub(crate) fn successors(&self) -> Vec<BlockId> {
        match self.insts.last() {
            Some(MInst::Branch {
                true_bb, false_bb, ..
            })
            | Some(MInst::BranchPred {
                true_bb, false_bb, ..
            }) => vec![*true_bb, *false_bb],
            Some(MInst::JumpTable { targets, .. }) => targets.to_vec(),
            Some(MInst::Jump { target }) => vec![*target],
            _ => Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct MFunction {
    pub(crate) blocks: Vec<MBlock>,
    constant_tables: Vec<Vec<u64>>,
    pub(crate) vregs: VRegAllocator,
    pub(crate) spill_descs: Vec<SpillDesc>,
    pub(crate) spill_homes: BTreeMap<VReg, i32>,
    pub(crate) spilled_phis: Vec<SpilledPhiNode>,
}

impl MFunction {
    #[allow(dead_code)]
    pub(crate) fn new(blocks: Vec<MBlock>, constant_tables: Vec<Vec<u64>>) -> Self {
        Self {
            blocks,
            constant_tables,
            vregs: VRegAllocator::new(),
            spill_descs: Vec::new(),
            spill_homes: BTreeMap::new(),
            spilled_phis: Vec::new(),
        }
    }

    pub(crate) fn for_isel(vregs: VRegAllocator, spill_descs: Vec<SpillDesc>) -> Self {
        Self {
            blocks: Vec::new(),
            constant_tables: Vec::new(),
            vregs,
            spill_descs,
            spill_homes: BTreeMap::new(),
            spilled_phis: Vec::new(),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn push_block(&mut self, block: MBlock) {
        self.blocks.push(block);
    }

    pub(crate) fn intern_constant_table(&mut self, values: Vec<u64>) -> ConstantTableId {
        if let Some(index) = self
            .constant_tables
            .iter()
            .position(|table| table == &values)
        {
            return ConstantTableId(index);
        }
        let id = ConstantTableId(self.constant_tables.len());
        self.constant_tables.push(values);
        id
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
