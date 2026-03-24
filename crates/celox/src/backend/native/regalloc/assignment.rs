//! Register assignment: maps VRegs to x86-64 physical registers.
//!
//! After the spilling phase guarantees pressure ≤ k, this phase assigns
//! physical registers using greedy coloring on the (chordal) SSA
//! interference graph.
//!
//! Handles x86-64 register constraints:
//! - Shift instructions (Shr, Shl, Sar) with register rhs require RCX.
//! - Future: Div/Rem require RAX/RDX.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::backend::native::mir::*;

use super::analysis::AnalysisResult;

// ────────────────────────────────────────────────────────────────
// Physical registers
// ────────────────────────────────────────────────────────────────

/// x86-64 general-purpose registers available for allocation.
/// Excludes RSP (stack pointer), RBP (frame pointer), and
/// one register reserved for the simulation state base pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum PhysReg {
    RAX = 0,
    RCX = 1,
    RDX = 2,
    RBX = 3,
    // RSP = 4, // reserved: stack pointer
    // RBP = 5, // reserved: frame pointer
    RSI = 6,
    RDI = 7,
    R8 = 8,
    R9 = 9,
    R10 = 10,
    R11 = 11,
    R12 = 12,
    R13 = 13,
    R14 = 14,
    // R15 = 15, // reserved: simulation state base pointer
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

/// All allocatable registers, in preference order.
/// Caller-saved first (cheaper to use — no save/restore needed).
pub const ALLOCATABLE_REGS: &[PhysReg] = &[
    // Caller-saved (volatile)
    PhysReg::RAX,
    PhysReg::RDX,
    PhysReg::RSI,
    PhysReg::RDI,
    PhysReg::R8,
    PhysReg::R9,
    PhysReg::R10,
    PhysReg::R11,
    // RCX is caller-saved but we put it last among volatiles
    // so it's only used when needed (shift constraint) or when others are exhausted
    PhysReg::RCX,
    // Callee-saved (need save/restore in prologue/epilogue)
    PhysReg::RBX,
    PhysReg::R12,
    PhysReg::R13,
    PhysReg::R14,
];

// ────────────────────────────────────────────────────────────────
// Register constraints
// ────────────────────────────────────────────────────────────────

/// Constraint on a particular operand of an instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegConstraint {
    /// Any allocatable register.
    Any,
    /// Must be this specific physical register.
    Fixed(PhysReg),
}

/// Return the register constraint for each use operand of an instruction.
/// The returned vec has the same length and order as `inst.uses()`.
pub fn use_constraints(inst: &MInst) -> Vec<RegConstraint> {
    // Shift rhs → RCX is handled in the emit phase (mov rcx, rhs) rather
    // than as an assignment constraint, because multiple shifts with different
    // amounts would all compete for RCX and clobber each other.
    let _ = inst;
    inst.uses().iter().map(|_| RegConstraint::Any).collect()
}

/// Return the register constraint for the def operand of an instruction.
pub fn def_constraint(inst: &MInst) -> RegConstraint {
    // Currently no def constraints. Future: div → RAX, etc.
    let _ = inst;
    RegConstraint::Any
}

// ────────────────────────────────────────────────────────────────
// Assignment result
// ────────────────────────────────────────────────────────────────

/// The result of register assignment: a mapping from VReg → PhysReg.
#[derive(Debug, Clone, Default)]
pub struct AssignmentMap {
    pub map: BTreeMap<VReg, PhysReg>,
}

impl AssignmentMap {
    pub fn get(&self, vreg: VReg) -> Option<PhysReg> {
        self.map.get(&vreg).copied()
    }

    pub fn set(&mut self, vreg: VReg, preg: PhysReg) {
        self.map.insert(vreg, preg);
    }
}

// ────────────────────────────────────────────────────────────────
// Assignment algorithm
// ────────────────────────────────────────────────────────────────

/// Assign physical registers to all VRegs in the function.
///
/// Greedy coloring on SSA interference graph. Processes instructions in
/// program order, maintaining a set of "active" allocations (VRegs currently
/// occupying physical registers). When a VReg dies (no more uses), its
/// physical register is freed.
///
/// Constraints (e.g., shift rhs → RCX) are handled by:
/// 1. Reserving the constrained register before allocating.
/// 2. If the constrained register is occupied by another live VReg,
///    inserting a swap (emit phase handles this with mov+xchg).
pub fn assign(func: &MFunction, analysis: &AnalysisResult) -> AssignmentMap {
    let mut result = AssignmentMap::default();

    // Per-block assignment to handle control flow
    // For simplicity, we process blocks in layout order and propagate
    // assignments through the CFG.
    for (bi, block) in func.blocks.iter().enumerate() {
        // Track which physical registers are currently in use
        let mut active: BTreeMap<PhysReg, VReg> = BTreeMap::new();

        // Initialize active set from live-in VRegs that already have assignments
        // (from predecessor blocks)
        for vreg in analysis.entry_distances[bi].keys() {
            if let Some(preg) = result.get(*vreg) {
                active.insert(preg, *vreg);
            }
        }

        for (inst_idx, inst) in block.insts.iter().enumerate() {
            let uses = inst.uses();
            let constraints = use_constraints(inst);
            let def = inst.def();

            // 1. Handle use constraints: ensure constrained operands are in
            //    the right physical register
            for (use_vreg, constraint) in uses.iter().zip(constraints.iter()) {
                if let RegConstraint::Fixed(required_preg) = constraint {
                    let current_preg = result.get(*use_vreg);
                    if current_preg != Some(*required_preg) {
                        // Need to move this vreg to the required register.
                        // If the required register is occupied, evict its current owner.
                        if let Some(&occupant) = active.get(required_preg) {
                            if occupant != *use_vreg {
                                // Evict occupant to a different register
                                let new_reg = find_free_reg(&active, None);
                                if let Some(new_reg) = new_reg {
                                    active.remove(required_preg);
                                    active.insert(new_reg, occupant);
                                    result.set(occupant, new_reg);
                                }
                                // If no free reg, the occupant must be dead soon.
                                // The emit phase will handle the swap.
                            }
                        }

                        // Remove vreg from its old location
                        if let Some(old_preg) = current_preg {
                            active.remove(&old_preg);
                        }

                        // Assign vreg to required register
                        active.insert(*required_preg, *use_vreg);
                        result.set(*use_vreg, *required_preg);
                    }
                } else if result.get(*use_vreg).is_none() {
                    // First time seeing this vreg (e.g., block parameter or reload)
                    // Assign any free register
                    let preg = find_free_reg(&active, None)
                        .expect("no free register for use (spilling should prevent this)");
                    active.insert(preg, *use_vreg);
                    result.set(*use_vreg, preg);
                }
            }

            // 2. Free dead values: any VReg in active whose next use is infinity
            //    This includes dead uses of this instruction AND any other VRegs
            //    that happen to die at this point (e.g., values kept alive across
            //    spill stores).
            let dead_regs: Vec<(PhysReg, VReg)> = active
                .iter()
                .filter(|&(_, &v)| {
                    super::analysis::next_use_at(func, analysis, bi, inst_idx + 1, v)
                        == u32::MAX
                })
                .map(|(&p, &v)| (p, v))
                .collect();
            for (preg, _vreg) in &dead_regs {
                active.remove(preg);
            }

            // 3. Allocate def
            if let Some(def_vreg) = def {
                let def_cons = def_constraint(inst);
                let preg = match def_cons {
                    RegConstraint::Fixed(required) => {
                        // Evict if occupied
                        if let Some(&occupant) = active.get(&required) {
                            if occupant != def_vreg {
                                let new_reg = find_free_reg(&active, Some(required));
                                if let Some(new_reg) = new_reg {
                                    active.remove(&required);
                                    active.insert(new_reg, occupant);
                                    result.set(occupant, new_reg);
                                } else {
                                    // No free reg; occupant will be reassigned later
                                    active.remove(&required);
                                }
                            }
                        }
                        required
                    }
                    RegConstraint::Any => {
                        match find_free_reg(&active, None) {
                            Some(r) => r,
                            None => {
                                // All registers occupied. Evict the VReg with
                                // furthest next-use (it will be reloaded later).
                                let victim = active
                                    .iter()
                                    .max_by_key(|&(_, &v)| {
                                        super::analysis::next_use_at(
                                            func, analysis, bi, inst_idx + 1, v,
                                        )
                                    })
                                    .map(|(&p, _)| p)
                                    .expect("active set is non-empty but no victim found");
                                active.remove(&victim);
                                victim
                            }
                        }
                    }
                };
                active.insert(preg, def_vreg);
                result.set(def_vreg, preg);
            }
        }
    }

    result
}

/// Find a free physical register not in the active set.
/// If `exclude` is Some, also avoid that register.
fn find_free_reg(
    active: &BTreeMap<PhysReg, VReg>,
    exclude: Option<PhysReg>,
) -> Option<PhysReg> {
    let used: BTreeSet<PhysReg> = active.keys().copied().collect();
    ALLOCATABLE_REGS
        .iter()
        .copied()
        .find(|r| !used.contains(r) && Some(*r) != exclude)
}
