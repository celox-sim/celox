//! Register assignment types and constraint queries for x86-64 physical registers.
//!
//! Defines `PhysReg`, `RegConstraint`, `AssignmentMap`, and helpers for
//! querying instruction constraints and clobbers.

use std::collections::{HashMap, HashSet};
use std::fmt;

use celox_backend_common::regalloc::{
    MachineRegister, RegConstraint as CommonRegConstraint, RegisterSet, ValueLocation,
};

use crate::native::features::VariableShiftEncoding;
use crate::native::mir::*;

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
    RBP = 5,
    RSI = 6,
    RDI = 7,
    R8 = 8,
    R9 = 9,
    R10 = 10,
    R11 = 11,
    R12 = 12,
    R13 = 13,
    R14 = 14,
    R15 = 15,
}

impl MachineRegister for PhysReg {
    fn index(self) -> u8 {
        self as u8
    }
}

/// Physical register in the x86 128-bit vector class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct X86PhysVec(pub u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum X86VectorLocation {
    Register(X86PhysVec),
    Stack(i32),
}

impl fmt::Display for PhysReg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            PhysReg::RAX => "rax",
            PhysReg::RCX => "rcx",
            PhysReg::RDX => "rdx",
            PhysReg::RBX => "rbx",
            PhysReg::RBP => "rbp",
            PhysReg::RSI => "rsi",
            PhysReg::RDI => "rdi",
            PhysReg::R8 => "r8",
            PhysReg::R9 => "r9",
            PhysReg::R10 => "r10",
            PhysReg::R11 => "r11",
            PhysReg::R12 => "r12",
            PhysReg::R13 => "r13",
            PhysReg::R14 => "r14",
            PhysReg::R15 => "r15",
        };
        write!(f, "{name}")
    }
}

pub type PhysRegSet = RegisterSet;

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
    PhysReg::RBP,
    PhysReg::R12,
    PhysReg::R13,
    PhysReg::R14,
    PhysReg::R15,
];

// ────────────────────────────────────────────────────────────────
// Register constraints
// ────────────────────────────────────────────────────────────────

pub type RegConstraint = CommonRegConstraint<PhysReg>;

/// Physical location of a phi source at one predecessor edge.
pub type EdgeLocation = ValueLocation<PhysReg>;

pub(super) fn use_constraints(
    inst: &MInst,
    shift_encoding: VariableShiftEncoding,
) -> Vec<RegConstraint> {
    match inst {
        // BMI2's three-operand shifts accept the count in any GPR. Baseline
        // x86 shifts require it in CL (the low byte of RCX).
        MInst::Shr { .. } | MInst::Shl { .. } | MInst::Sar { .. } => {
            let rhs = match shift_encoding {
                VariableShiftEncoding::Bmi2 => RegConstraint::Any,
                VariableShiftEncoding::LegacyCl => RegConstraint::Fixed(PhysReg::RCX),
            };
            // uses() = [lhs, rhs].
            vec![RegConstraint::Any, rhs]
        }
        _ => inst.uses().iter().map(|_| RegConstraint::Any).collect(),
    }
}

/// Returns physical registers clobbered by this instruction (besides dst).
pub fn clobbers(inst: &MInst) -> &'static [PhysReg] {
    match inst {
        MInst::UDiv { .. }
        | MInst::URem { .. }
        | MInst::SDiv { .. }
        | MInst::SRem { .. }
        | MInst::UMulHi { .. } => &[PhysReg::RAX, PhysReg::RDX],
        MInst::SparseCommit {
            summary_word_count: 0,
            ..
        }
        | MInst::SparseCommitWorklist {
            active_capacity: 0, ..
        } => &[],
        // The per-region sparse loop uses only caller-saved scratch registers.
        // Express them as an allocation barrier instead of saving all seven
        // unconditionally in the emitter, including on the common empty-
        // bitmap path.
        MInst::SparseCommit { .. } => &[
            PhysReg::RAX,
            PhysReg::RCX,
            PhysReg::RDX,
            PhysReg::RSI,
            PhysReg::RDI,
            PhysReg::R8,
            PhysReg::R9,
        ],
        // The shared sparse-commit loop is an inline machine-code region, not
        // a call.  It nevertheless owns every allocatable GPR while it walks
        // the active/summary/dirty worklists.  Keeping that ownership hidden
        // in the emitter forced an unconditional save/restore of all fourteen
        // registers, even when the pseudo was the final operation before
        // Return.  Model the clobber at the allocation boundary instead so
        // only genuinely live-through values are moved to their homes.
        MInst::SparseCommitWorklist { .. } => ALLOCATABLE_REGS,
        _ => &[],
    }
}

/// Returns true if the instruction is a register-register shift (needs RCX).
pub fn is_reg_shift(inst: &MInst) -> bool {
    matches!(
        inst,
        MInst::Shr { .. } | MInst::Shl { .. } | MInst::Sar { .. }
    )
}

/// Compute clobber points for a block (for use by unified allocator).
pub fn block_clobber_points_for(
    block: &crate::native::mir::MBlock,
) -> Vec<(usize, &'static [PhysReg])> {
    block
        .insts
        .iter()
        .enumerate()
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
    /// XMM assignment produced by the same allocation pipeline. Keeping both
    /// classes in one artifact prevents emission from running an independent
    /// late allocator with a different view of scheduling and clobbers.
    x86_vectors: HashMap<X86VecReg, X86VectorLocation>,
    pub edge_spill_slots: HashMap<VReg, i32>,
    pub edge_locations: HashMap<(BlockId, VReg), EdgeLocation>,
    pub edge_location_points: HashMap<(BlockId, VReg), usize>,
    /// Exact out-of-SSA source location for one semantic phi row. This is
    /// intentionally destination-qualified: the same source VReg may feed
    /// multiple phi rows on one edge through independently materialized
    /// stack/immediate locations.
    pub phi_edge_locations: HashMap<(BlockId, BlockId, VReg, VReg), EdgeLocation>,
    /// Phi results retained only as strict-SSA identities. Every physical use
    /// has been resolved through an exact stack/immediate edge location, so
    /// out-of-SSA must validate the row but emit no destination copy.
    semantic_phi_definitions: HashSet<VReg>,
}

impl AssignmentMap {
    pub fn get(&self, vreg: VReg) -> Option<PhysReg> {
        self.map.get(&vreg).copied()
    }

    pub fn set(&mut self, vreg: VReg, preg: PhysReg) {
        self.map.insert(vreg, preg);
    }

    pub fn x86_vector(&self, value: X86VecReg) -> Option<X86VectorLocation> {
        self.x86_vectors.get(&value).copied()
    }

    pub(crate) fn set_x86_vector(&mut self, value: X86VecReg, location: X86VectorLocation) {
        self.x86_vectors.insert(value, location);
    }

    pub(crate) fn x86_vector_count(&self) -> usize {
        self.x86_vectors.len()
    }

    pub(crate) fn sorted_x86_vectors(&self) -> Vec<(X86VecReg, X86VectorLocation)> {
        let mut entries = self
            .x86_vectors
            .iter()
            .map(|(&value, &location)| (value, location))
            .collect::<Vec<_>>();
        entries.sort_by_key(|(value, _)| *value);
        entries
    }

    pub fn edge_spill_slot(&self, vreg: VReg) -> Option<i32> {
        self.edge_spill_slots.get(&vreg).copied()
    }

    pub fn set_edge_spill_slot(&mut self, vreg: VReg, offset: i32) {
        self.edge_spill_slots.insert(vreg, offset);
    }

    pub fn set_semantic_phi_definition(&mut self, vreg: VReg) {
        self.semantic_phi_definitions.insert(vreg);
    }

    pub fn is_semantic_phi_definition(&self, vreg: VReg) -> bool {
        self.semantic_phi_definitions.contains(&vreg)
    }

    pub fn edge_location(&self, pred: BlockId, vreg: VReg) -> Option<EdgeLocation> {
        self.edge_locations.get(&(pred, vreg)).copied()
    }

    pub fn phi_edge_location(
        &self,
        pred: BlockId,
        succ: BlockId,
        destination: VReg,
        source: VReg,
    ) -> Option<EdgeLocation> {
        self.phi_edge_locations
            .get(&(pred, succ, destination, source))
            .copied()
    }

    /// Resolve the physical source of one semantic phi row.
    ///
    /// Destination-qualified edge locations take precedence because one VReg
    /// may feed multiple rows through different stack/immediate homes. The
    /// remaining fallbacks describe value-wide edge, stack, and register
    /// residency respectively. Keeping this precedence in the assignment
    /// model prevents verifiers and out-of-SSA lowering from interpreting the
    /// same completed assignment differently.
    pub fn resolved_phi_source_location(
        &self,
        pred: BlockId,
        succ: BlockId,
        destination: VReg,
        source: VReg,
    ) -> Option<EdgeLocation> {
        self.phi_edge_location(pred, succ, destination, source)
            .or_else(|| self.edge_location(pred, source))
            .or_else(|| self.edge_spill_slot(source).map(EdgeLocation::Stack))
            .or_else(|| self.get(source).map(EdgeLocation::Register))
    }

    pub fn set_phi_edge_location(
        &mut self,
        pred: BlockId,
        succ: BlockId,
        destination: VReg,
        source: VReg,
        location: EdgeLocation,
    ) {
        self.phi_edge_locations
            .insert((pred, succ, destination, source), location);
    }

    pub fn set_edge_location(&mut self, pred: BlockId, vreg: VReg, location: EdgeLocation) {
        self.set_edge_location_at(pred, vreg, location, 0);
    }

    pub fn set_edge_location_at(
        &mut self,
        pred: BlockId,
        vreg: VReg,
        location: EdgeLocation,
        program_point: usize,
    ) {
        self.edge_locations.insert((pred, vreg), location);
        self.edge_location_points
            .insert((pred, vreg), program_point);
    }

    pub fn edge_location_at(
        &self,
        pred: BlockId,
        vreg: VReg,
        program_point: usize,
    ) -> Option<EdgeLocation> {
        let valid_from = self.edge_location_points.get(&(pred, vreg)).copied()?;
        (program_point >= valid_from).then(|| self.edge_locations[&(pred, vreg)])
    }

    /// Returns entries sorted by VReg for deterministic display.
    pub fn sorted_entries(&self) -> Vec<(VReg, PhysReg)> {
        let mut entries: Vec<(VReg, PhysReg)> = self.map.iter().map(|(&v, &p)| (v, p)).collect();
        entries.sort_by_key(|(v, _)| *v);
        entries
    }
}
