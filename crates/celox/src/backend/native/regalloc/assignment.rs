//! Register assignment types and constraint queries for x86-64 physical registers.
//!
//! Defines `PhysReg`, `RegConstraint`, `AssignmentMap`, and helpers for
//! querying instruction constraints and clobbers.

use std::collections::HashMap;
use std::fmt;

use crate::backend::native::mir::*;

// ────────────────────────────────────────────────────────────────
// Physical registers
// ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum PhysReg {
    RAX = 0,
    RCX = 1,
    RDX = 2,
    RBX = 3,
    RSI = 6,
    RDI = 7,
    R8 = 8,
    R9 = 9,
    R10 = 10,
    R11 = 11,
    R12 = 12,
    R13 = 13,
    R14 = 14,
}

impl fmt::Display for PhysReg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            PhysReg::RAX => "rax",
            PhysReg::RCX => "rcx",
            PhysReg::RDX => "rdx",
            PhysReg::RBX => "rbx",
            PhysReg::RSI => "rsi",
            PhysReg::RDI => "rdi",
            PhysReg::R8 => "r8",
            PhysReg::R9 => "r9",
            PhysReg::R10 => "r10",
            PhysReg::R11 => "r11",
            PhysReg::R12 => "r12",
            PhysReg::R13 => "r13",
            PhysReg::R14 => "r14",
        };
        write!(f, "{name}")
    }
}

// ────────────────────────────────────────────────────────────────
// PhysRegSet: u16 bitset for small PhysReg sets
// ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Default)]
pub struct PhysRegSet(u16);

impl PhysRegSet {
    pub fn new() -> Self {
        Self(0)
    }
    pub fn insert(&mut self, r: PhysReg) {
        self.0 |= 1 << (r as u16);
    }
    pub fn contains(&self, r: &PhysReg) -> bool {
        self.0 & (1 << (*r as u16)) != 0
    }
    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }
}

pub const ALLOCATABLE_REGS: &[PhysReg] = &[
    PhysReg::RAX,
    PhysReg::RDX,
    PhysReg::RSI,
    PhysReg::RDI,
    PhysReg::R8,
    PhysReg::R9,
    PhysReg::R10,
    PhysReg::R11,
    PhysReg::RCX,
    PhysReg::RBX,
    PhysReg::R12,
    PhysReg::R13,
    PhysReg::R14,
];

// ────────────────────────────────────────────────────────────────
// Register constraints
// ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegConstraint {
    Any,
    Fixed(PhysReg),
}

pub fn use_constraints(inst: &MInst) -> Vec<RegConstraint> {
    match inst {
        // x86 variable shifts require shift amount in CL (low byte of RCX).
        MInst::Shr { .. } | MInst::Shl { .. } | MInst::Sar { .. } => {
            // uses() = [lhs, rhs]. rhs must be in RCX.
            vec![RegConstraint::Any, RegConstraint::Fixed(PhysReg::RCX)]
        }
        _ => inst.uses().iter().map(|_| RegConstraint::Any).collect(),
    }
}

/// Returns physical registers clobbered by this instruction (besides dst).
pub fn clobbers(inst: &MInst) -> &'static [PhysReg] {
    match inst {
        MInst::UDiv { .. } | MInst::URem { .. } | MInst::UMulHi { .. } => &[PhysReg::RAX, PhysReg::RDX],
        _ => &[],
    }
}

/// Returns true if the instruction is a register-register shift (needs RCX).
pub fn is_reg_shift(inst: &MInst) -> bool {
    matches!(inst, MInst::Shr { .. } | MInst::Shl { .. } | MInst::Sar { .. })
}

/// Compute clobber points for a block (for use by unified allocator).
pub fn block_clobber_points_for(block: &crate::backend::native::mir::MBlock) -> Vec<(usize, &'static [PhysReg])> {
    block.insts.iter().enumerate()
        .filter_map(|(idx, inst)| {
            let c = clobbers(inst);
            if c.is_empty() { None } else { Some((idx, c)) }
        })
        .collect()
}

// ────────────────────────────────────────────────────────────────
// Assignment result
// ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct AssignmentMap {
    pub map: HashMap<VReg, PhysReg>,
}

impl AssignmentMap {
    pub fn get(&self, vreg: VReg) -> Option<PhysReg> {
        self.map.get(&vreg).copied()
    }

    pub fn set(&mut self, vreg: VReg, preg: PhysReg) {
        self.map.insert(vreg, preg);
    }

    /// Returns entries sorted by VReg for deterministic display.
    pub fn sorted_entries(&self) -> Vec<(VReg, PhysReg)> {
        let mut entries: Vec<(VReg, PhysReg)> = self.map.iter().map(|(&v, &p)| (v, p)).collect();
        entries.sort_by_key(|(v, _)| *v);
        entries
    }
}
