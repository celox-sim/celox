//! Machine-level IR: word-level SSA with virtual registers.
//!
//! MIR sits between SIR (bit-level) and x86-64 machine code. Instructions
//! operate on word-sized values; bit-level access information is preserved
//! only in [`SpillDesc`] side-tables so the register allocator can make
//! cost-aware spill decisions without knowing about bit layouts.

use std::fmt;

use crate::ir::RegionedAbsoluteAddr;

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

impl VRegAllocator {
    pub fn new() -> Self {
        Self { next: 0 }
    }

    pub fn alloc(&mut self) -> VReg {
        let id = self.next;
        self.next = self.next.checked_add(1).expect("VReg overflow");
        VReg(id)
    }

    /// Total number of allocated VRegs. Used for sizing per-vreg arrays.
    pub fn count(&self) -> u32 {
        self.next
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
        let reload_cost = if bit_offset % 64 == 0 && matches!(width_bits, 8 | 16 | 32 | 64) {
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
#[derive(Debug, Clone)]
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

    // ── Comparison ─────────────────────────────────────────────
    /// dst = (lhs cmp rhs) ? 1 : 0
    Cmp {
        dst: VReg,
        lhs: VReg,
        rhs: VReg,
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

    // ── Select (for div-by-zero guard, etc.) ───────────────────
    /// dst = cond ? true_val : false_val
    Select {
        dst: VReg,
        cond: VReg,
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
            MInst::Cmp {
                dst, lhs, rhs, kind,
            } => write!(f, "{dst} = cmp.{kind:?} {lhs}, {rhs}"),
            MInst::BitNot { dst, src } => write!(f, "{dst} = not {src}"),
            MInst::Neg { dst, src } => write!(f, "{dst} = neg {src}"),
            MInst::Select {
                dst,
                cond,
                true_val,
                false_val,
            } => write!(f, "{dst} = select {cond}, {true_val}, {false_val}"),
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
                let srcs: Vec<String> = phi.sources.iter().map(|(bid, v)| format!("{bid}: {v}")).collect();
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
            | MInst::LoadIndexed { dst, .. }
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
            | MInst::Cmp { dst, .. }
            | MInst::UDiv { dst, .. }
            | MInst::URem { dst, .. }
            | MInst::BitNot { dst, .. }
            | MInst::Neg { dst, .. }
            | MInst::Select { dst, .. } => Some(*dst),

            MInst::Store { .. }
            | MInst::StoreIndexed { .. }
            | MInst::Branch { .. }
            | MInst::Jump { .. }
            | MInst::Return
            | MInst::ReturnError { .. } => None,
        }
    }

    /// Returns all VRegs used (read) by this instruction.
    pub fn uses(&self) -> Vec<VReg> {
        match self {
            MInst::Mov { src, .. } => vec![*src],
            MInst::LoadImm { .. } => vec![],
            MInst::Load { .. } => vec![],
            MInst::Store { src, .. } => vec![*src],
            MInst::LoadIndexed { index, .. } => vec![*index],
            MInst::StoreIndexed { index, src, .. } => vec![*index, *src],
            MInst::Add { lhs, rhs, .. }
            | MInst::Sub { lhs, rhs, .. }
            | MInst::Mul { lhs, rhs, .. }
            | MInst::UMulHi { lhs, rhs, .. }
            | MInst::And { lhs, rhs, .. }
            | MInst::Or { lhs, rhs, .. }
            | MInst::Xor { lhs, rhs, .. }
            | MInst::Shr { lhs, rhs, .. }
            | MInst::Shl { lhs, rhs, .. }
            | MInst::Sar { lhs, rhs, .. } => vec![*lhs, *rhs],
            MInst::Cmp { lhs, rhs, .. }
            | MInst::UDiv { lhs, rhs, .. }
            | MInst::URem { lhs, rhs, .. } => vec![*lhs, *rhs],
            MInst::AndImm { src, .. }
            | MInst::OrImm { src, .. }
            | MInst::ShrImm { src, .. }
            | MInst::ShlImm { src, .. }
            | MInst::SarImm { src, .. } => vec![*src],
            MInst::BitNot { src, .. } | MInst::Neg { src, .. } => vec![*src],
            MInst::Select {
                cond,
                true_val,
                false_val,
                ..
            } => vec![*cond, *true_val, *false_val],
            MInst::Branch { cond, .. } => vec![*cond],
            MInst::Jump { .. } | MInst::Return | MInst::ReturnError { .. } => vec![],
        }
    }

    /// Replace all occurrences of `old` with `new` in the use operands.
    pub fn rewrite_use(&mut self, old: VReg, new: VReg) {
        match self {
            MInst::Mov { src, .. } => {
                if *src == old { *src = new; }
            }
            MInst::Store { src, .. } => {
                if *src == old { *src = new; }
            }
            MInst::LoadIndexed { index, .. } => {
                if *index == old { *index = new; }
            }
            MInst::StoreIndexed { index, src, .. } => {
                if *index == old { *index = new; }
                if *src == old { *src = new; }
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
                if *lhs == old { *lhs = new; }
                if *rhs == old { *rhs = new; }
            }
            MInst::AndImm { src, .. }
            | MInst::OrImm { src, .. }
            | MInst::ShrImm { src, .. }
            | MInst::ShlImm { src, .. }
            | MInst::SarImm { src, .. }
            | MInst::BitNot { src, .. }
            | MInst::Neg { src, .. } => {
                if *src == old { *src = new; }
            }
            MInst::Select { cond, true_val, false_val, .. } => {
                if *cond == old { *cond = new; }
                if *true_val == old { *true_val = new; }
                if *false_val == old { *false_val = new; }
            }
            MInst::Branch { cond, .. } => {
                if *cond == old { *cond = new; }
            }
            MInst::LoadImm { .. }
            | MInst::Load { .. }
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
#[derive(Debug)]
pub struct MFunction {
    /// Basic blocks in layout order. blocks[0] is the entry block.
    pub blocks: Vec<MBlock>,
    /// Spill descriptors indexed by VReg number.
    pub spill_descs: Vec<SpillDesc>,
    /// VReg allocator (for the spilling phase to allocate reload regs).
    pub vregs: VRegAllocator,
}

impl MFunction {
    pub fn new(vregs: VRegAllocator, spill_descs: Vec<SpillDesc>) -> Self {
        Self {
            blocks: Vec::new(),
            spill_descs,
            vregs,
        }
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
}
