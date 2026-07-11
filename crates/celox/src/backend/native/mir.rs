//! Machine-level IR: word-level SSA with virtual registers.
//!
//! MIR sits between SIR (bit-level) and x86-64 machine code. Instructions
//! operate on word-sized values; bit-level access information is preserved
//! only in [`SpillDesc`] side-tables so the register allocator can make
//! cost-aware spill decisions without knowing about bit layouts.

use std::fmt;

use crate::ir::RegionedAbsoluteAddr;

// ────────────────────────────────────────────────────────────────
// Uses: stack-allocated list of VReg operands (no heap allocation)
// ────────────────────────────────────────────────────────────────

/// Stack-allocated list of up to 5 VReg uses. Avoids Vec heap allocation
/// in the regalloc inner loop.
pub struct Uses {
    buf: [VReg; 5],
    len: u8,
}

impl Uses {
    #[inline]
    pub fn none() -> Self {
        Self {
            buf: [VReg(0); 5],
            len: 0,
        }
    }
    #[inline]
    pub fn one(a: VReg) -> Self {
        Self {
            buf: [a, VReg(0), VReg(0), VReg(0), VReg(0)],
            len: 1,
        }
    }
    #[inline]
    pub fn two(a: VReg, b: VReg) -> Self {
        Self {
            buf: [a, b, VReg(0), VReg(0), VReg(0)],
            len: 2,
        }
    }
    #[inline]
    pub fn three(a: VReg, b: VReg, c: VReg) -> Self {
        Self {
            buf: [a, b, c, VReg(0), VReg(0)],
            len: 3,
        }
    }
    #[inline]
    pub fn four(a: VReg, b: VReg, c: VReg, d: VReg) -> Self {
        Self {
            buf: [a, b, c, d, VReg(0)],
            len: 4,
        }
    }
    #[inline]
    pub fn five(a: VReg, b: VReg, c: VReg, d: VReg, e: VReg) -> Self {
        Self {
            buf: [a, b, c, d, e],
            len: 5,
        }
    }
    #[inline]
    pub fn len(&self) -> usize {
        self.len as usize
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    #[inline]
    pub fn contains(&self, v: &VReg) -> bool {
        self.iter().any(|u| u == v)
    }
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = &VReg> {
        self.buf[..self.len as usize].iter()
    }
}

impl std::ops::Deref for Uses {
    type Target = [VReg];
    fn deref(&self) -> &[VReg] {
        &self.buf[..self.len as usize]
    }
}

impl<'a> IntoIterator for &'a Uses {
    type Item = &'a VReg;
    type IntoIter = std::slice::Iter<'a, VReg>;
    fn into_iter(self) -> Self::IntoIter {
        self.buf[..self.len as usize].iter()
    }
}

impl IntoIterator for Uses {
    type Item = VReg;
    type IntoIter = std::iter::Take<std::array::IntoIter<VReg, 5>>;
    fn into_iter(self) -> Self::IntoIter {
        self.buf.into_iter().take(self.len as usize)
    }
}

// ────────────────────────────────────────────────────────────────
// Virtual register
// ────────────────────────────────────────────────────────────────

/// Virtual register index. Allocated linearly during ISel; the spilling
/// phase may allocate additional VRegs for reload results.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VReg(pub u32);

impl fmt::Debug for VReg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.0)
    }
}
impl fmt::Display for VReg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.0)
    }
}

/// Allocator for virtual registers.
#[derive(Debug, Clone)]
pub struct VRegAllocator {
    next: u32,
}

/// Exhaustion of the dense `u32` VReg namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VRegAllocError;

impl fmt::Display for VRegAllocError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("VReg namespace exhausted")
    }
}

impl std::error::Error for VRegAllocError {}

impl Default for VRegAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl VRegAllocator {
    pub fn new() -> Self {
        Self { next: 0 }
    }

    pub fn alloc(&mut self) -> VReg {
        self.try_alloc().expect("VReg overflow")
    }

    /// Allocate one VReg without panicking or changing state on exhaustion.
    pub fn try_alloc(&mut self) -> Result<VReg, VRegAllocError> {
        let Some(next) = self.next.checked_add(1) else {
            return Err(VRegAllocError);
        };
        let id = self.next;
        self.next = next;
        Ok(VReg(id))
    }

    /// Total number of allocated VRegs. Used for sizing per-vreg arrays.
    pub fn count(&self) -> u32 {
        self.next
    }

    #[cfg(test)]
    pub(crate) fn set_next_for_test(&mut self, next: u32) {
        self.next = next;
    }
}

// ────────────────────────────────────────────────────────────────
// Spill descriptors
// ────────────────────────────────────────────────────────────────

/// Where a value can be spilled / reloaded from.
#[derive(Debug, Clone)]
pub enum SpillKind {
    /// Value lives in simulation state at a known location.
    /// Reload = load from `[sim_base + byte_offset]` (+ optional shift/mask).
    SimState {
        addr: RegionedAbsoluteAddr,
        bit_offset: usize,
        width_bits: usize,
    },
    /// Alias of a simulation-state-backed value.
    /// Unlike `SimState`, this is intended for backend-internal aliases that
    /// may safely share the same reload home.
    SimStateAlias {
        addr: RegionedAbsoluteAddr,
        bit_offset: usize,
        width_bits: usize,
    },
    /// Intermediate value with no home in simulation state.
    /// Spill to a stack slot.
    Stack,
    /// Constant that can be cheaply rematerialized (mov imm).
    Remat { value: u64 },
}

/// Cost information for spilling / reloading a virtual register.
#[derive(Debug, Clone)]
pub struct SpillDesc {
    pub kind: SpillKind,
    /// Estimated cost (in x86-64 instructions) to reload this value.
    pub reload_cost: u8,
    /// Estimated cost to spill. 0 if the value is already in memory
    /// (store-back-only or rematerializable).
    pub spill_cost: u8,
}

impl SpillDesc {
    /// Rematerializable constant (cost 0 to spill, cost 1 to reload).
    pub fn remat(value: u64) -> Self {
        Self {
            kind: SpillKind::Remat { value },
            reload_cost: 1,
            spill_cost: 0,
        }
    }

    /// Value backed by simulation state.
    pub fn sim_state(
        addr: RegionedAbsoluteAddr,
        bit_offset: usize,
        width_bits: usize,
        store_back_only: bool,
    ) -> Self {
        let reload_cost = if bit_offset.is_multiple_of(64) && matches!(width_bits, 8 | 16 | 32 | 64)
        {
            1 // word-aligned: single load
        } else {
            2 // needs shift/mask
        };
        Self {
            kind: SpillKind::SimState {
                addr,
                bit_offset,
                width_bits,
            },
            reload_cost,
            spill_cost: if store_back_only { 0 } else { reload_cost },
        }
    }

    /// Backend-internal alias of a simulation-state-backed value.
    pub fn sim_state_alias(
        addr: RegionedAbsoluteAddr,
        bit_offset: usize,
        width_bits: usize,
        store_back_only: bool,
    ) -> Self {
        let reload_cost = if bit_offset.is_multiple_of(64) && matches!(width_bits, 8 | 16 | 32 | 64)
        {
            1
        } else {
            2
        };
        Self {
            kind: SpillKind::SimStateAlias {
                addr,
                bit_offset,
                width_bits,
            },
            reload_cost,
            spill_cost: if store_back_only { 0 } else { reload_cost },
        }
    }

    /// Copy semantics for a value snapshot created by `Mov`.
    pub fn copy_for_snapshot(&self) -> Self {
        match self.kind {
            SpillKind::Remat { .. } => self.clone(),
            _ => Self::transient(),
        }
    }

    /// Transient value (spill to stack).
    pub fn transient() -> Self {
        Self {
            kind: SpillKind::Stack,
            reload_cost: 2,
            spill_cost: 2,
        }
    }
}

// ────────────────────────────────────────────────────────────────
// Block identifiers
// ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockId(pub u32);

impl fmt::Debug for BlockId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bb{}", self.0)
    }
}
impl fmt::Display for BlockId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bb{}", self.0)
    }
}

// ────────────────────────────────────────────────────────────────
// MIR operand sizes
// ────────────────────────────────────────────────────────────────

/// Operand size for memory and ALU operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpSize {
    S8,
    S16,
    S32,
    S64,
}

impl OpSize {
    pub fn bytes(self) -> u32 {
        match self {
            OpSize::S8 => 1,
            OpSize::S16 => 2,
            OpSize::S32 => 4,
            OpSize::S64 => 8,
        }
    }

    pub fn from_bits(bits: usize) -> Option<Self> {
        match bits {
            8 => Some(OpSize::S8),
            16 => Some(OpSize::S16),
            32 => Some(OpSize::S32),
            64 => Some(OpSize::S64),
            _ => None,
        }
    }
}

// ────────────────────────────────────────────────────────────────
// Base register for memory access
// ────────────────────────────────────────────────────────────────

/// Base pointer for memory operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BaseReg {
    /// Simulation state base pointer (passed as function argument).
    SimState,
    /// Stack frame pointer (RSP-relative).
    StackFrame,
}

// ────────────────────────────────────────────────────────────────
// Comparison kinds
// ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CmpKind {
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

// ────────────────────────────────────────────────────────────────
// MIR instructions
// ────────────────────────────────────────────────────────────────

/// Word-level SSA instruction. All operands are virtual registers.
///
/// Instructions use 3-operand form (dst, src1, src2). The emit phase
/// handles x86-64's 2-operand constraint by inserting mov when needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MInst {
    // ── Data movement ──────────────────────────────────────────
    /// dst = src
    Mov { dst: VReg, src: VReg },
    /// dst = immediate
    LoadImm { dst: VReg, value: u64 },

    // ── Memory access (word-level, byte offsets) ───────────────
    /// dst = load [base + offset]
    Load {
        dst: VReg,
        base: BaseReg,
        offset: i32,
        size: OpSize,
    },
    /// store [base + offset] = src
    Store {
        base: BaseReg,
        offset: i32,
        src: VReg,
        size: OpSize,
    },
    /// dst = load [ptr + offset]
    LoadPtr {
        dst: VReg,
        ptr: VReg,
        offset: i32,
        size: OpSize,
    },
    /// store [ptr + offset] = src
    StorePtr {
        ptr: VReg,
        offset: i32,
        src: VReg,
        size: OpSize,
    },
    /// release-store [ptr + offset] = src.
    ///
    /// This is used as a publish point for lock-free runtime-event buffers:
    /// payload words are stored normally, then the sequence word is release-stored.
    /// It is not a read-modify-write atomic operation.
    ReleaseStorePtr {
        ptr: VReg,
        offset: i32,
        src: VReg,
        size: OpSize,
    },
    /// dst = load [base + offset + index]  (register-indexed memory access)
    LoadIndexed {
        dst: VReg,
        base: BaseReg,
        offset: i32,
        index: VReg,
        size: OpSize,
    },
    /// store [base + offset + index] = src  (register-indexed memory access)
    StoreIndexed {
        base: BaseReg,
        offset: i32,
        index: VReg,
        src: VReg,
        size: OpSize,
    },
    /// dst = load [ptr + offset + index]
    LoadPtrIndexed {
        dst: VReg,
        ptr: VReg,
        offset: i32,
        index: VReg,
        size: OpSize,
    },
    /// store [ptr + offset + index] = src
    StorePtrIndexed {
        ptr: VReg,
        offset: i32,
        index: VReg,
        src: VReg,
        size: OpSize,
    },
    /// release-store [ptr + offset + index] = src.
    ///
    /// This is used as a publish point for lock-free runtime-event buffers:
    /// payload words are stored normally, then the sequence word is release-stored.
    /// It is not a read-modify-write atomic operation.
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

    // ── ALU (3-operand SSA) ────────────────────────────────────
    /// dst = lhs + rhs
    Add { dst: VReg, lhs: VReg, rhs: VReg },
    /// dst = lhs - rhs
    Sub { dst: VReg, lhs: VReg, rhs: VReg },
    /// dst = lhs * rhs (lower 64 bits)
    Mul { dst: VReg, lhs: VReg, rhs: VReg },
    /// dst = upper 64 bits of lhs * rhs (unsigned)
    UMulHi { dst: VReg, lhs: VReg, rhs: VReg },
    /// dst = lhs & rhs
    And { dst: VReg, lhs: VReg, rhs: VReg },
    /// dst = lhs | rhs
    Or { dst: VReg, lhs: VReg, rhs: VReg },
    /// dst = lhs ^ rhs
    Xor { dst: VReg, lhs: VReg, rhs: VReg },
    /// dst = lhs >> rhs (logical)
    Shr { dst: VReg, lhs: VReg, rhs: VReg },
    /// dst = lhs << rhs
    Shl { dst: VReg, lhs: VReg, rhs: VReg },
    /// dst = lhs >> rhs (arithmetic)
    Sar { dst: VReg, lhs: VReg, rhs: VReg },

    // ── ALU with immediate ─────────────────────────────────────
    /// dst = src & imm
    AndImm { dst: VReg, src: VReg, imm: u64 },
    /// dst = src | imm
    OrImm { dst: VReg, src: VReg, imm: u64 },
    /// dst = src >> imm (logical)
    ShrImm { dst: VReg, src: VReg, imm: u8 },
    /// dst = src << imm
    ShlImm { dst: VReg, src: VReg, imm: u8 },
    /// dst = src >> imm (arithmetic)
    SarImm { dst: VReg, src: VReg, imm: u8 },
    /// dst = src + imm
    AddImm { dst: VReg, src: VReg, imm: i32 },
    /// dst = src - imm
    SubImm { dst: VReg, src: VReg, imm: i32 },

    // ── Comparison ─────────────────────────────────────────────
    /// dst = (lhs cmp rhs) ? 1 : 0
    Cmp {
        dst: VReg,
        lhs: VReg,
        rhs: VReg,
        kind: CmpKind,
    },
    /// dst = (lhs cmp imm) ? 1 : 0
    CmpImm {
        dst: VReg,
        lhs: VReg,
        imm: i32,
        kind: CmpKind,
    },

    // ── Division (uses RAX/RDX, handled at emit time) ────────
    /// dst = lhs / rhs (unsigned)
    UDiv { dst: VReg, lhs: VReg, rhs: VReg },
    /// dst = lhs % rhs (unsigned)
    URem { dst: VReg, lhs: VReg, rhs: VReg },

    // ── Unary ──────────────────────────────────────────────────
    /// dst = ~src (bitwise NOT)
    BitNot { dst: VReg, src: VReg },
    /// dst = -src (negate)
    Neg { dst: VReg, src: VReg },
    /// dst = popcnt(src) (population count — number of set bits)
    Popcnt { dst: VReg, src: VReg },
    /// dst = bsr(src). The result is unspecified when src == 0.
    ///
    /// This is intended for guarded lowering where the result is consumed only
    /// on a path or select arm that has already proven src != 0.
    Bsr { dst: VReg, src: VReg },
    /// dst = src != 0 ? bsr(src) : zero_value.
    ///
    /// This is a defined wrapper around x86 BSR, whose destination is
    /// otherwise undefined when the source is zero.
    BsrOr {
        dst: VReg,
        src: VReg,
        zero_value: u8,
    },
    /// dst = pext(src, mask) — parallel bit extract (BMI2).
    /// Extracts bits from src at positions where mask has 1s,
    /// and packs them contiguously starting at bit 0.
    Pext { dst: VReg, src: VReg, mask: VReg },
    /// dst = pdep(src, mask) — parallel bit deposit (BMI2).
    /// Deposits contiguous low bits from src into positions where mask has 1s.
    Pdep { dst: VReg, src: VReg, mask: VReg },

    // ── Select (for div-by-zero guard, etc.) ───────────────────
    /// dst = cond ? true_val : false_val
    Select {
        dst: VReg,
        cond: VReg,
        true_val: VReg,
        false_val: VReg,
    },
    /// dst = (lhs cmp rhs) ? true_val : false_val
    CmpSelect {
        dst: VReg,
        lhs: VReg,
        rhs: VReg,
        kind: CmpKind,
        true_val: VReg,
        false_val: VReg,
    },
    /// dst = (lhs cmp imm) ? true_val : false_val
    CmpImmSelect {
        dst: VReg,
        lhs: VReg,
        imm: i32,
        kind: CmpKind,
        true_val: VReg,
        false_val: VReg,
    },
    /// dst = (guard != 0 && lhs cmp rhs) ? true_val : false_val
    GuardedCmpSelect {
        dst: VReg,
        guard: VReg,
        lhs: VReg,
        rhs: VReg,
        kind: CmpKind,
        true_val: VReg,
        false_val: VReg,
    },

    // ── Control flow ───────────────────────────────────────────
    /// Conditional branch: if cond != 0 then goto true_bb else goto false_bb
    Branch {
        cond: VReg,
        true_bb: BlockId,
        false_bb: BlockId,
    },
    /// Unconditional jump
    Jump { target: BlockId },
    /// Return from function (success, code 0)
    Return,
    /// Return with error code (non-zero)
    ReturnError { code: i64 },
}

impl fmt::Display for MInst {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MInst::Mov { dst, src } => write!(f, "{dst} = mov {src}"),
            MInst::LoadImm { dst, value } => write!(f, "{dst} = imm {value:#x}"),
            MInst::Load {
                dst,
                base,
                offset,
                size,
            } => write!(f, "{dst} = load.{size} [{base} + {offset}]"),
            MInst::Store {
                base,
                offset,
                src,
                size,
            } => write!(f, "store.{size} [{base} + {offset}], {src}"),
            MInst::LoadPtr {
                dst,
                ptr,
                offset,
                size,
            } => write!(f, "{dst} = load.{size} [{ptr} + {offset}]"),
            MInst::StorePtr {
                ptr,
                offset,
                src,
                size,
            } => write!(f, "store.{size} [{ptr} + {offset}], {src}"),
            MInst::ReleaseStorePtr {
                ptr,
                offset,
                src,
                size,
            } => write!(f, "release_store.{size} [{ptr} + {offset}], {src}"),
            MInst::LoadIndexed {
                dst,
                base,
                offset,
                index,
                size,
            } => write!(f, "{dst} = load.{size} [{base} + {offset} + {index}]"),
            MInst::StoreIndexed {
                base,
                offset,
                index,
                src,
                size,
            } => write!(f, "store.{size} [{base} + {offset} + {index}], {src}"),
            MInst::LoadPtrIndexed {
                dst,
                ptr,
                offset,
                index,
                size,
            } => write!(f, "{dst} = load.{size} [{ptr} + {offset} + {index}]"),
            MInst::StorePtrIndexed {
                ptr,
                offset,
                index,
                src,
                size,
            } => write!(f, "store.{size} [{ptr} + {offset} + {index}], {src}"),
            MInst::ReleaseStorePtrIndexed {
                ptr,
                offset,
                index,
                src,
                size,
            } => write!(
                f,
                "release_store.{size} [{ptr} + {offset} + {index}], {src}"
            ),
            MInst::MemCopy {
                src_offset,
                dst_offset,
                byte_len,
            } => write!(
                f,
                "memcopy [sim + {dst_offset}], [sim + {src_offset}], {byte_len}"
            ),
            MInst::Add { dst, lhs, rhs } => write!(f, "{dst} = add {lhs}, {rhs}"),
            MInst::Sub { dst, lhs, rhs } => write!(f, "{dst} = sub {lhs}, {rhs}"),
            MInst::Mul { dst, lhs, rhs } => write!(f, "{dst} = mul {lhs}, {rhs}"),
            MInst::UMulHi { dst, lhs, rhs } => write!(f, "{dst} = umulhi {lhs}, {rhs}"),
            MInst::And { dst, lhs, rhs } => write!(f, "{dst} = and {lhs}, {rhs}"),
            MInst::Or { dst, lhs, rhs } => write!(f, "{dst} = or {lhs}, {rhs}"),
            MInst::Xor { dst, lhs, rhs } => write!(f, "{dst} = xor {lhs}, {rhs}"),
            MInst::Shr { dst, lhs, rhs } => write!(f, "{dst} = shr {lhs}, {rhs}"),
            MInst::Shl { dst, lhs, rhs } => write!(f, "{dst} = shl {lhs}, {rhs}"),
            MInst::Sar { dst, lhs, rhs } => write!(f, "{dst} = sar {lhs}, {rhs}"),
            MInst::UDiv { dst, lhs, rhs } => write!(f, "{dst} = udiv {lhs}, {rhs}"),
            MInst::URem { dst, lhs, rhs } => write!(f, "{dst} = urem {lhs}, {rhs}"),
            MInst::AndImm { dst, src, imm } => write!(f, "{dst} = and {src}, {imm:#x}"),
            MInst::OrImm { dst, src, imm } => write!(f, "{dst} = or {src}, {imm:#x}"),
            MInst::ShrImm { dst, src, imm } => write!(f, "{dst} = shr {src}, {imm}"),
            MInst::ShlImm { dst, src, imm } => write!(f, "{dst} = shl {src}, {imm}"),
            MInst::SarImm { dst, src, imm } => write!(f, "{dst} = sar {src}, {imm}"),
            MInst::AddImm { dst, src, imm } => write!(f, "{dst} = add {src}, {imm}"),
            MInst::SubImm { dst, src, imm } => write!(f, "{dst} = sub {src}, {imm}"),
            MInst::Cmp {
                dst,
                lhs,
                rhs,
                kind,
            } => write!(f, "{dst} = cmp.{kind:?} {lhs}, {rhs}"),
            MInst::CmpImm {
                dst,
                lhs,
                imm,
                kind,
            } => write!(f, "{dst} = cmp.{kind:?} {lhs}, {imm}"),
            MInst::BitNot { dst, src } => write!(f, "{dst} = not {src}"),
            MInst::Neg { dst, src } => write!(f, "{dst} = neg {src}"),
            MInst::Popcnt { dst, src } => write!(f, "{dst} = popcnt {src}"),
            MInst::Bsr { dst, src } => write!(f, "{dst} = bsr {src}"),
            MInst::BsrOr {
                dst,
                src,
                zero_value,
            } => write!(f, "{dst} = bsr_or {src}, {zero_value}"),
            MInst::Pext { dst, src, mask } => write!(f, "{dst} = pext {src}, {mask}"),
            MInst::Pdep { dst, src, mask } => write!(f, "{dst} = pdep {src}, {mask}"),
            MInst::Select {
                dst,
                cond,
                true_val,
                false_val,
            } => write!(f, "{dst} = select {cond}, {true_val}, {false_val}"),
            MInst::CmpSelect {
                dst,
                lhs,
                rhs,
                kind,
                true_val,
                false_val,
            } => write!(
                f,
                "{dst} = cmp_select cmp.{kind:?} {lhs}, {rhs}, {true_val}, {false_val}"
            ),
            MInst::CmpImmSelect {
                dst,
                lhs,
                imm,
                kind,
                true_val,
                false_val,
            } => write!(
                f,
                "{dst} = cmp_select cmp.{kind:?} {lhs}, {imm}, {true_val}, {false_val}"
            ),
            MInst::GuardedCmpSelect {
                dst,
                guard,
                lhs,
                rhs,
                kind,
                true_val,
                false_val,
            } => write!(
                f,
                "{dst} = guarded_cmp_select {guard}, cmp.{kind:?} {lhs}, {rhs}, {true_val}, {false_val}"
            ),
            MInst::Branch {
                cond,
                true_bb,
                false_bb,
            } => write!(f, "br {cond}, {true_bb}, {false_bb}"),
            MInst::Jump { target } => write!(f, "jmp {target}"),
            MInst::Return => write!(f, "ret"),
            MInst::ReturnError { code } => write!(f, "ret_error {code}"),
        }
    }
}

impl fmt::Display for BaseReg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BaseReg::SimState => write!(f, "sim"),
            BaseReg::StackFrame => write!(f, "sp"),
        }
    }
}

impl fmt::Display for OpSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OpSize::S8 => write!(f, "i8"),
            OpSize::S16 => write!(f, "i16"),
            OpSize::S32 => write!(f, "i32"),
            OpSize::S64 => write!(f, "i64"),
        }
    }
}

impl fmt::Display for MFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for block in &self.blocks {
            writeln!(f, "{}:", block.id)?;
            for phi in &block.phis {
                let srcs: Vec<String> = phi
                    .sources
                    .iter()
                    .map(|(bid, v)| format!("{bid}: {v}"))
                    .collect();
                writeln!(f, "  {} = phi({})", phi.dst, srcs.join(", "))?;
            }
            for inst in &block.insts {
                writeln!(f, "  {inst}")?;
            }
        }
        Ok(())
    }
}

impl MInst {
    /// Returns the destination VReg, if any.
    pub fn def(&self) -> Option<VReg> {
        match self {
            MInst::Mov { dst, .. }
            | MInst::LoadImm { dst, .. }
            | MInst::Load { dst, .. }
            | MInst::LoadPtr { dst, .. }
            | MInst::LoadIndexed { dst, .. }
            | MInst::LoadPtrIndexed { dst, .. }
            | MInst::Add { dst, .. }
            | MInst::Sub { dst, .. }
            | MInst::Mul { dst, .. }
            | MInst::UMulHi { dst, .. }
            | MInst::And { dst, .. }
            | MInst::Or { dst, .. }
            | MInst::Xor { dst, .. }
            | MInst::Shr { dst, .. }
            | MInst::Shl { dst, .. }
            | MInst::Sar { dst, .. }
            | MInst::AndImm { dst, .. }
            | MInst::OrImm { dst, .. }
            | MInst::ShrImm { dst, .. }
            | MInst::ShlImm { dst, .. }
            | MInst::SarImm { dst, .. }
            | MInst::AddImm { dst, .. }
            | MInst::SubImm { dst, .. }
            | MInst::Cmp { dst, .. }
            | MInst::CmpImm { dst, .. }
            | MInst::UDiv { dst, .. }
            | MInst::URem { dst, .. }
            | MInst::BitNot { dst, .. }
            | MInst::Neg { dst, .. }
            | MInst::Popcnt { dst, .. }
            | MInst::Bsr { dst, .. }
            | MInst::BsrOr { dst, .. }
            | MInst::Pext { dst, .. }
            | MInst::Pdep { dst, .. }
            | MInst::Select { dst, .. }
            | MInst::CmpSelect { dst, .. }
            | MInst::CmpImmSelect { dst, .. }
            | MInst::GuardedCmpSelect { dst, .. } => Some(*dst),

            MInst::Store { .. }
            | MInst::StorePtr { .. }
            | MInst::ReleaseStorePtr { .. }
            | MInst::StoreIndexed { .. }
            | MInst::StorePtrIndexed { .. }
            | MInst::ReleaseStorePtrIndexed { .. }
            | MInst::MemCopy { .. }
            | MInst::Branch { .. }
            | MInst::Jump { .. }
            | MInst::Return
            | MInst::ReturnError { .. } => None,
        }
    }

    /// Returns all VRegs used (read) by this instruction.
    /// Returns the VRegs used by this instruction (max 3, no heap allocation).
    pub fn uses(&self) -> Uses {
        match self {
            MInst::Mov { src, .. } => Uses::one(*src),
            MInst::LoadImm { .. } | MInst::Load { .. } | MInst::MemCopy { .. } => Uses::none(),
            MInst::Store { src, .. } => Uses::one(*src),
            MInst::LoadPtr { ptr, .. } => Uses::one(*ptr),
            MInst::StorePtr { ptr, src, .. } => Uses::two(*ptr, *src),
            MInst::ReleaseStorePtr { ptr, src, .. } => Uses::two(*ptr, *src),
            MInst::LoadIndexed { index, .. } => Uses::one(*index),
            MInst::StoreIndexed { index, src, .. } => Uses::two(*index, *src),
            MInst::LoadPtrIndexed { ptr, index, .. } => Uses::two(*ptr, *index),
            MInst::StorePtrIndexed {
                ptr, index, src, ..
            } => Uses::three(*ptr, *index, *src),
            MInst::ReleaseStorePtrIndexed {
                ptr, index, src, ..
            } => Uses::three(*ptr, *index, *src),
            MInst::Add { lhs, rhs, .. }
            | MInst::Sub { lhs, rhs, .. }
            | MInst::Mul { lhs, rhs, .. }
            | MInst::UMulHi { lhs, rhs, .. }
            | MInst::And { lhs, rhs, .. }
            | MInst::Or { lhs, rhs, .. }
            | MInst::Xor { lhs, rhs, .. }
            | MInst::Shr { lhs, rhs, .. }
            | MInst::Shl { lhs, rhs, .. }
            | MInst::Sar { lhs, rhs, .. }
            | MInst::Cmp { lhs, rhs, .. }
            | MInst::UDiv { lhs, rhs, .. }
            | MInst::URem { lhs, rhs, .. } => Uses::two(*lhs, *rhs),
            MInst::Pext { src, mask, .. } | MInst::Pdep { src, mask, .. } => Uses::two(*src, *mask),
            MInst::AndImm { src, .. }
            | MInst::OrImm { src, .. }
            | MInst::ShrImm { src, .. }
            | MInst::ShlImm { src, .. }
            | MInst::SarImm { src, .. }
            | MInst::AddImm { src, .. }
            | MInst::SubImm { src, .. }
            | MInst::BitNot { src, .. }
            | MInst::Neg { src, .. }
            | MInst::Popcnt { src, .. }
            | MInst::Bsr { src, .. }
            | MInst::BsrOr { src, .. } => Uses::one(*src),
            MInst::CmpImm { lhs, .. } => Uses::one(*lhs),
            MInst::Select {
                cond,
                true_val,
                false_val,
                ..
            } => Uses::three(*cond, *true_val, *false_val),
            MInst::CmpSelect {
                lhs,
                rhs,
                true_val,
                false_val,
                ..
            } => Uses::four(*lhs, *rhs, *true_val, *false_val),
            MInst::CmpImmSelect {
                lhs,
                true_val,
                false_val,
                ..
            } => Uses::three(*lhs, *true_val, *false_val),
            MInst::GuardedCmpSelect {
                guard,
                lhs,
                rhs,
                true_val,
                false_val,
                ..
            } => Uses::five(*guard, *lhs, *rhs, *true_val, *false_val),
            MInst::Branch { cond, .. } => Uses::one(*cond),
            MInst::Jump { .. } | MInst::Return | MInst::ReturnError { .. } => Uses::none(),
        }
    }

    /// Replace all occurrences of `old` with `new` in the use operands.
    pub fn rewrite_use(&mut self, old: VReg, new: VReg) {
        match self {
            MInst::Mov { src, .. } => {
                if *src == old {
                    *src = new;
                }
            }
            MInst::Store { src, .. } => {
                if *src == old {
                    *src = new;
                }
            }
            MInst::LoadPtr { ptr, .. } => {
                if *ptr == old {
                    *ptr = new;
                }
            }
            MInst::StorePtr { ptr, src, .. } => {
                if *ptr == old {
                    *ptr = new;
                }
                if *src == old {
                    *src = new;
                }
            }
            MInst::ReleaseStorePtr { ptr, src, .. } => {
                if *ptr == old {
                    *ptr = new;
                }
                if *src == old {
                    *src = new;
                }
            }
            MInst::LoadIndexed { index, .. } => {
                if *index == old {
                    *index = new;
                }
            }
            MInst::StoreIndexed { index, src, .. } => {
                if *index == old {
                    *index = new;
                }
                if *src == old {
                    *src = new;
                }
            }
            MInst::LoadPtrIndexed { ptr, index, .. } => {
                if *ptr == old {
                    *ptr = new;
                }
                if *index == old {
                    *index = new;
                }
            }
            MInst::StorePtrIndexed {
                ptr, index, src, ..
            } => {
                if *ptr == old {
                    *ptr = new;
                }
                if *index == old {
                    *index = new;
                }
                if *src == old {
                    *src = new;
                }
            }
            MInst::ReleaseStorePtrIndexed {
                ptr, index, src, ..
            } => {
                if *ptr == old {
                    *ptr = new;
                }
                if *index == old {
                    *index = new;
                }
                if *src == old {
                    *src = new;
                }
            }
            MInst::Add { lhs, rhs, .. }
            | MInst::Sub { lhs, rhs, .. }
            | MInst::Mul { lhs, rhs, .. }
            | MInst::UMulHi { lhs, rhs, .. }
            | MInst::And { lhs, rhs, .. }
            | MInst::Or { lhs, rhs, .. }
            | MInst::Xor { lhs, rhs, .. }
            | MInst::Shr { lhs, rhs, .. }
            | MInst::Shl { lhs, rhs, .. }
            | MInst::Sar { lhs, rhs, .. }
            | MInst::Cmp { lhs, rhs, .. }
            | MInst::UDiv { lhs, rhs, .. }
            | MInst::URem { lhs, rhs, .. } => {
                if *lhs == old {
                    *lhs = new;
                }
                if *rhs == old {
                    *rhs = new;
                }
            }
            MInst::AndImm { src, .. }
            | MInst::OrImm { src, .. }
            | MInst::ShrImm { src, .. }
            | MInst::ShlImm { src, .. }
            | MInst::SarImm { src, .. }
            | MInst::AddImm { src, .. }
            | MInst::SubImm { src, .. }
            | MInst::BitNot { src, .. }
            | MInst::Neg { src, .. }
            | MInst::Popcnt { src, .. }
            | MInst::Bsr { src, .. }
            | MInst::BsrOr { src, .. } => {
                if *src == old {
                    *src = new;
                }
            }
            MInst::CmpImm { lhs, .. } => {
                if *lhs == old {
                    *lhs = new;
                }
            }
            MInst::Pext { src, mask, .. } | MInst::Pdep { src, mask, .. } => {
                if *src == old {
                    *src = new;
                }
                if *mask == old {
                    *mask = new;
                }
            }
            MInst::Select {
                cond,
                true_val,
                false_val,
                ..
            } => {
                if *cond == old {
                    *cond = new;
                }
                if *true_val == old {
                    *true_val = new;
                }
                if *false_val == old {
                    *false_val = new;
                }
            }
            MInst::CmpSelect {
                lhs,
                rhs,
                true_val,
                false_val,
                ..
            } => {
                if *lhs == old {
                    *lhs = new;
                }
                if *rhs == old {
                    *rhs = new;
                }
                if *true_val == old {
                    *true_val = new;
                }
                if *false_val == old {
                    *false_val = new;
                }
            }
            MInst::CmpImmSelect {
                lhs,
                true_val,
                false_val,
                ..
            } => {
                if *lhs == old {
                    *lhs = new;
                }
                if *true_val == old {
                    *true_val = new;
                }
                if *false_val == old {
                    *false_val = new;
                }
            }
            MInst::GuardedCmpSelect {
                guard,
                lhs,
                rhs,
                true_val,
                false_val,
                ..
            } => {
                if *guard == old {
                    *guard = new;
                }
                if *lhs == old {
                    *lhs = new;
                }
                if *rhs == old {
                    *rhs = new;
                }
                if *true_val == old {
                    *true_val = new;
                }
                if *false_val == old {
                    *false_val = new;
                }
            }
            MInst::Branch { cond, .. } => {
                if *cond == old {
                    *cond = new;
                }
            }
            MInst::LoadImm { .. }
            | MInst::Load { .. }
            | MInst::MemCopy { .. }
            | MInst::Jump { .. }
            | MInst::Return
            | MInst::ReturnError { .. } => {}
        }
    }

    /// Returns true if this instruction is a terminator (branch/jump/return).
    pub fn is_terminator(&self) -> bool {
        matches!(
            self,
            MInst::Branch { .. } | MInst::Jump { .. } | MInst::Return | MInst::ReturnError { .. }
        )
    }
}

// ────────────────────────────────────────────────────────────────
// Basic block
// ────────────────────────────────────────────────────────────────

/// A phi node at block entry: `dst = phi(pred1: src1, pred2: src2, ...)`.
/// Maintains SSA: each VReg has exactly one definition point.
#[derive(Debug, Clone)]
pub struct PhiNode {
    pub dst: VReg,
    pub sources: Vec<(BlockId, VReg)>,
}

/// A basic block containing a sequence of MIR instructions.
/// The last instruction must be a terminator.
#[derive(Debug, Clone)]
pub struct MBlock {
    pub id: BlockId,
    /// Phi nodes at block entry (SSA merge points).
    pub phis: Vec<PhiNode>,
    pub insts: Vec<MInst>,
}

impl MBlock {
    pub fn new(id: BlockId) -> Self {
        Self {
            id,
            phis: Vec::new(),
            insts: Vec::new(),
        }
    }

    pub fn push(&mut self, inst: MInst) {
        self.insts.push(inst);
    }

    pub fn terminator(&self) -> Option<&MInst> {
        self.insts.last().filter(|i| i.is_terminator())
    }

    /// Successor block IDs (from the terminator).
    pub fn successors(&self) -> Vec<BlockId> {
        match self.terminator() {
            Some(MInst::Branch {
                true_bb, false_bb, ..
            }) => vec![*true_bb, *false_bb],
            Some(MInst::Jump { target }) => vec![*target],
            _ => vec![],
        }
    }
}

// ────────────────────────────────────────────────────────────────
// Function (compilation unit)
// ────────────────────────────────────────────────────────────────

/// A MIR function: the unit of compilation for the native backend.
/// Corresponds to one SIR execution unit.
#[derive(Debug, Clone)]
pub struct MFunction {
    /// Basic blocks in layout order. `blocks[0]` is the unique CFG entry and
    /// no block may branch back to it; loops therefore have a distinct header.
    pub blocks: Vec<MBlock>,
    /// Spill descriptors indexed by VReg number.
    pub spill_descs: Vec<SpillDesc>,
    /// VReg allocator (for the spilling phase to allocate reload regs).
    pub vregs: VRegAllocator,
    /// Known value widths for each VReg (None = unknown/64-bit).
    /// When Some(w) with w <= 32, the emit phase can use 32-bit registers.
    pub value_widths: Vec<Option<u8>>,
    /// Target facts shared by optimization, register allocation, and emission.
    pub(crate) target_features: super::features::X86Features,
}

impl MFunction {
    pub fn new(vregs: VRegAllocator, spill_descs: Vec<SpillDesc>) -> Self {
        Self {
            blocks: Vec::new(),
            spill_descs,
            vregs,
            value_widths: Vec::new(),
            target_features: super::features::X86Features::detect(),
        }
    }

    /// Returns true if VReg is known to fit in 32 bits (upper 32 guaranteed zero).
    pub fn is_narrow32(&self, vreg: VReg) -> bool {
        self.value_widths
            .get(vreg.0 as usize)
            .and_then(|w| *w)
            .is_some_and(|w| w <= 32)
    }

    pub fn push_block(&mut self, block: MBlock) {
        self.blocks.push(block);
    }

    pub fn entry_block(&self) -> Option<&MBlock> {
        self.blocks.first()
    }

    /// Get SpillDesc for a VReg. Returns None for VRegs allocated after
    /// the initial ISel (e.g. reload temporaries created by the spilling phase).
    pub fn spill_desc(&self, vreg: VReg) -> Option<&SpillDesc> {
        self.spill_descs.get(vreg.0 as usize)
    }

    /// Verify the canonical MIR contract without modifying the function.
    pub fn verify_result(&self) -> Result<(), super::mir_verify::MirVerifyError> {
        super::mir_verify::verify_function(self)
    }

    /// Verify the canonical MIR contract and panic with a structured diagnostic.
    pub fn verify(&self) {
        if let Err(error) = self.verify_result() {
            panic!("{error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_alloc_reports_exhaustion_without_changing_state() {
        let mut allocator = VRegAllocator::new();
        allocator.set_next_for_test(u32::MAX);

        assert_eq!(allocator.try_alloc(), Err(VRegAllocError));
        assert_eq!(allocator.count(), u32::MAX);
    }

    struct UseCase {
        name: &'static str,
        inst: MInst,
        expected: Vec<VReg>,
    }

    fn vreg(index: u32) -> VReg {
        VReg(index)
    }

    fn use_cases() -> Vec<UseCase> {
        let dst = vreg(100);
        let a = vreg(1);
        let b = vreg(2);
        let c = vreg(3);
        let d = vreg(4);
        let e = vreg(5);
        let block_a = BlockId(1);
        let block_b = BlockId(2);

        vec![
            UseCase {
                name: "Mov",
                inst: MInst::Mov { dst, src: a },
                expected: vec![a],
            },
            UseCase {
                name: "LoadImm",
                inst: MInst::LoadImm { dst, value: 42 },
                expected: vec![],
            },
            UseCase {
                name: "Load",
                inst: MInst::Load {
                    dst,
                    base: BaseReg::SimState,
                    offset: 8,
                    size: OpSize::S64,
                },
                expected: vec![],
            },
            UseCase {
                name: "Store",
                inst: MInst::Store {
                    base: BaseReg::SimState,
                    offset: 8,
                    src: a,
                    size: OpSize::S64,
                },
                expected: vec![a],
            },
            UseCase {
                name: "LoadPtr",
                inst: MInst::LoadPtr {
                    dst,
                    ptr: a,
                    offset: 8,
                    size: OpSize::S64,
                },
                expected: vec![a],
            },
            UseCase {
                name: "StorePtr",
                inst: MInst::StorePtr {
                    ptr: a,
                    offset: 8,
                    src: b,
                    size: OpSize::S64,
                },
                expected: vec![a, b],
            },
            UseCase {
                name: "ReleaseStorePtr",
                inst: MInst::ReleaseStorePtr {
                    ptr: a,
                    offset: 8,
                    src: b,
                    size: OpSize::S64,
                },
                expected: vec![a, b],
            },
            UseCase {
                name: "LoadIndexed",
                inst: MInst::LoadIndexed {
                    dst,
                    base: BaseReg::SimState,
                    offset: 8,
                    index: a,
                    size: OpSize::S64,
                },
                expected: vec![a],
            },
            UseCase {
                name: "StoreIndexed",
                inst: MInst::StoreIndexed {
                    base: BaseReg::SimState,
                    offset: 8,
                    index: a,
                    src: b,
                    size: OpSize::S64,
                },
                expected: vec![a, b],
            },
            UseCase {
                name: "LoadPtrIndexed",
                inst: MInst::LoadPtrIndexed {
                    dst,
                    ptr: a,
                    offset: 8,
                    index: b,
                    size: OpSize::S64,
                },
                expected: vec![a, b],
            },
            UseCase {
                name: "StorePtrIndexed",
                inst: MInst::StorePtrIndexed {
                    ptr: a,
                    offset: 8,
                    index: b,
                    src: c,
                    size: OpSize::S64,
                },
                expected: vec![a, b, c],
            },
            UseCase {
                name: "ReleaseStorePtrIndexed",
                inst: MInst::ReleaseStorePtrIndexed {
                    ptr: a,
                    offset: 8,
                    index: b,
                    src: c,
                    size: OpSize::S64,
                },
                expected: vec![a, b, c],
            },
            UseCase {
                name: "MemCopy",
                inst: MInst::MemCopy {
                    src_offset: 0,
                    dst_offset: 8,
                    byte_len: 16,
                },
                expected: vec![],
            },
            UseCase {
                name: "Add",
                inst: MInst::Add {
                    dst,
                    lhs: a,
                    rhs: b,
                },
                expected: vec![a, b],
            },
            UseCase {
                name: "Sub",
                inst: MInst::Sub {
                    dst,
                    lhs: a,
                    rhs: b,
                },
                expected: vec![a, b],
            },
            UseCase {
                name: "Mul",
                inst: MInst::Mul {
                    dst,
                    lhs: a,
                    rhs: b,
                },
                expected: vec![a, b],
            },
            UseCase {
                name: "UMulHi",
                inst: MInst::UMulHi {
                    dst,
                    lhs: a,
                    rhs: b,
                },
                expected: vec![a, b],
            },
            UseCase {
                name: "And",
                inst: MInst::And {
                    dst,
                    lhs: a,
                    rhs: b,
                },
                expected: vec![a, b],
            },
            UseCase {
                name: "Or",
                inst: MInst::Or {
                    dst,
                    lhs: a,
                    rhs: b,
                },
                expected: vec![a, b],
            },
            UseCase {
                name: "Xor",
                inst: MInst::Xor {
                    dst,
                    lhs: a,
                    rhs: b,
                },
                expected: vec![a, b],
            },
            UseCase {
                name: "Shr",
                inst: MInst::Shr {
                    dst,
                    lhs: a,
                    rhs: b,
                },
                expected: vec![a, b],
            },
            UseCase {
                name: "Shl",
                inst: MInst::Shl {
                    dst,
                    lhs: a,
                    rhs: b,
                },
                expected: vec![a, b],
            },
            UseCase {
                name: "Sar",
                inst: MInst::Sar {
                    dst,
                    lhs: a,
                    rhs: b,
                },
                expected: vec![a, b],
            },
            UseCase {
                name: "AndImm",
                inst: MInst::AndImm {
                    dst,
                    src: a,
                    imm: 0xff,
                },
                expected: vec![a],
            },
            UseCase {
                name: "OrImm",
                inst: MInst::OrImm {
                    dst,
                    src: a,
                    imm: 0xff,
                },
                expected: vec![a],
            },
            UseCase {
                name: "ShrImm",
                inst: MInst::ShrImm {
                    dst,
                    src: a,
                    imm: 3,
                },
                expected: vec![a],
            },
            UseCase {
                name: "ShlImm",
                inst: MInst::ShlImm {
                    dst,
                    src: a,
                    imm: 3,
                },
                expected: vec![a],
            },
            UseCase {
                name: "SarImm",
                inst: MInst::SarImm {
                    dst,
                    src: a,
                    imm: 3,
                },
                expected: vec![a],
            },
            UseCase {
                name: "AddImm",
                inst: MInst::AddImm {
                    dst,
                    src: a,
                    imm: 3,
                },
                expected: vec![a],
            },
            UseCase {
                name: "SubImm",
                inst: MInst::SubImm {
                    dst,
                    src: a,
                    imm: 3,
                },
                expected: vec![a],
            },
            UseCase {
                name: "Cmp",
                inst: MInst::Cmp {
                    dst,
                    lhs: a,
                    rhs: b,
                    kind: CmpKind::Eq,
                },
                expected: vec![a, b],
            },
            UseCase {
                name: "CmpImm",
                inst: MInst::CmpImm {
                    dst,
                    lhs: a,
                    imm: 3,
                    kind: CmpKind::Eq,
                },
                expected: vec![a],
            },
            UseCase {
                name: "UDiv",
                inst: MInst::UDiv {
                    dst,
                    lhs: a,
                    rhs: b,
                },
                expected: vec![a, b],
            },
            UseCase {
                name: "URem",
                inst: MInst::URem {
                    dst,
                    lhs: a,
                    rhs: b,
                },
                expected: vec![a, b],
            },
            UseCase {
                name: "BitNot",
                inst: MInst::BitNot { dst, src: a },
                expected: vec![a],
            },
            UseCase {
                name: "Neg",
                inst: MInst::Neg { dst, src: a },
                expected: vec![a],
            },
            UseCase {
                name: "Popcnt",
                inst: MInst::Popcnt { dst, src: a },
                expected: vec![a],
            },
            UseCase {
                name: "Bsr",
                inst: MInst::Bsr { dst, src: a },
                expected: vec![a],
            },
            UseCase {
                name: "BsrOr",
                inst: MInst::BsrOr {
                    dst,
                    src: a,
                    zero_value: 63,
                },
                expected: vec![a],
            },
            UseCase {
                name: "Pext",
                inst: MInst::Pext {
                    dst,
                    src: a,
                    mask: b,
                },
                expected: vec![a, b],
            },
            UseCase {
                name: "Pdep",
                inst: MInst::Pdep {
                    dst,
                    src: a,
                    mask: b,
                },
                expected: vec![a, b],
            },
            UseCase {
                name: "Select",
                inst: MInst::Select {
                    dst,
                    cond: a,
                    true_val: b,
                    false_val: c,
                },
                expected: vec![a, b, c],
            },
            UseCase {
                name: "CmpSelect",
                inst: MInst::CmpSelect {
                    dst,
                    lhs: a,
                    rhs: b,
                    kind: CmpKind::Eq,
                    true_val: c,
                    false_val: d,
                },
                expected: vec![a, b, c, d],
            },
            UseCase {
                name: "CmpImmSelect",
                inst: MInst::CmpImmSelect {
                    dst,
                    lhs: a,
                    imm: 3,
                    kind: CmpKind::Eq,
                    true_val: b,
                    false_val: c,
                },
                expected: vec![a, b, c],
            },
            UseCase {
                name: "GuardedCmpSelect",
                inst: MInst::GuardedCmpSelect {
                    dst,
                    guard: a,
                    lhs: b,
                    rhs: c,
                    kind: CmpKind::Eq,
                    true_val: d,
                    false_val: e,
                },
                expected: vec![a, b, c, d, e],
            },
            UseCase {
                name: "Branch",
                inst: MInst::Branch {
                    cond: a,
                    true_bb: block_a,
                    false_bb: block_b,
                },
                expected: vec![a],
            },
            UseCase {
                name: "Jump",
                inst: MInst::Jump { target: block_a },
                expected: vec![],
            },
            UseCase {
                name: "Return",
                inst: MInst::Return,
                expected: vec![],
            },
            UseCase {
                name: "ReturnError",
                inst: MInst::ReturnError { code: 1 },
                expected: vec![],
            },
        ]
    }

    #[test]
    fn uses_reports_every_use_operand_for_every_instruction_variant() {
        let cases = use_cases();
        assert_eq!(
            cases.len(),
            49,
            "the MInst variant table must stay exhaustive"
        );

        for case in cases {
            assert_eq!(
                case.inst.uses().into_iter().collect::<Vec<_>>(),
                case.expected,
                "{}",
                case.name
            );
        }
    }

    #[test]
    fn rewrite_use_rewrites_every_operand_reported_by_uses() {
        for case in use_cases() {
            let original_def = case.inst.def();
            for old in case.expected.iter().copied() {
                let replacement = vreg(old.0 + 200);
                let mut rewritten = case.inst.clone();
                rewritten.rewrite_use(old, replacement);

                let expected = case
                    .expected
                    .iter()
                    .copied()
                    .map(|used| if used == old { replacement } else { used })
                    .collect::<Vec<_>>();
                assert_eq!(
                    rewritten.uses().into_iter().collect::<Vec<_>>(),
                    expected,
                    "{} did not rewrite {old}",
                    case.name
                );
                assert_eq!(
                    rewritten.def(),
                    original_def,
                    "{} rewrote its definition while replacing {old}",
                    case.name
                );
            }
        }
    }

    #[test]
    fn rewrite_use_rewrites_all_occurrences_of_the_same_vreg() {
        let shared = vreg(50);
        let replacement = vreg(51);

        for case in use_cases() {
            if case.expected.is_empty() {
                continue;
            }
            let original_def = case.inst.def();
            let mut rewritten = case.inst;
            for used in case.expected {
                rewritten.rewrite_use(used, shared);
            }
            rewritten.rewrite_use(shared, replacement);

            assert!(
                rewritten.uses().into_iter().all(|used| used == replacement),
                "{} left an occurrence of the shared use unchanged",
                case.name
            );
            assert_eq!(
                rewritten.def(),
                original_def,
                "{} changed its def",
                case.name
            );
        }
    }
}
