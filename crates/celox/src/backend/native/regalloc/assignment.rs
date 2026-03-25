//! Register assignment: maps VRegs to x86-64 physical registers.
//!
//! After the spilling phase guarantees pressure ≤ k, this phase assigns
//! physical registers using greedy coloring on the (chordal) SSA
//! interference graph.
//!
//! Handles x86-64 register constraints:
//! - Shift instructions (Shr, Shl, Sar) with register rhs require RCX
//!   (handled at emit time).
//! - UDiv/URem clobber RAX and RDX. Live values in clobbered registers
//!   are saved via live-range splitting (Mov insertion + use rewriting).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::backend::native::mir::*;

use super::analysis::AnalysisResult;

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

pub fn def_constraint(inst: &MInst) -> RegConstraint {
    let _ = inst;
    RegConstraint::Any
}

/// Returns physical registers clobbered by this instruction (besides dst).
pub fn clobbers(inst: &MInst) -> &'static [PhysReg] {
    match inst {
        MInst::UDiv { .. } | MInst::URem { .. } => &[PhysReg::RAX, PhysReg::RDX],
        _ => &[],
    }
}

/// Number of physical registers reserved by constraints at this program point.
/// The spilling phase uses this to reduce effective k, guaranteeing the
/// assignment never needs to displace live VRegs for constraint resolution.
///
/// Returns 2 for Fixed constraints (not 1) because the post-spilling
/// `split_live_ranges_at_fixed_constraints` pass inserts a Mov that
/// temporarily increases live count by 1 at the split point.
pub fn constraint_headroom(inst: &MInst) -> usize {
    let fixed = use_constraints(inst)
        .iter()
        .filter(|c| matches!(c, RegConstraint::Fixed(_)))
        .count();
    if fixed > 0 { fixed + 1 } else { 0 }
}

/// Returns true if the instruction is a register-register shift (needs RCX).
pub fn is_reg_shift(inst: &MInst) -> bool {
    matches!(inst, MInst::Shr { .. } | MInst::Shl { .. } | MInst::Sar { .. })
}

// ────────────────────────────────────────────────────────────────
// Live-range splitting for Fixed constraints
// ────────────────────────────────────────────────────────────────

/// Post-spilling pass: isolate Fixed-constrained uses to 1-instruction
/// lifetimes by inserting Mov copies.
///
/// When a Fixed-constrained use (e.g., shift rhs → RCX) has a multi-
/// instruction lifetime, the assignment's constraint handling changes
/// its global register to the constrained PhysReg. This retroactive
/// change can conflict with other VRegs that occupied that PhysReg
/// earlier in the lifetime. The fix: insert a Mov copy right before
/// the constraint instruction so only the short-lived copy gets the
/// constrained register.
pub fn split_live_ranges_at_fixed_constraints(func: &mut MFunction) {
    for block in &mut func.blocks {
        // Quick check: any Fixed constraints in this block?
        if !block.insts.iter().any(|inst| {
            use_constraints(inst).iter().any(|c| matches!(c, RegConstraint::Fixed(_)))
        }) {
            continue;
        }

        // Compute def position for each VReg (phis count as position 0).
        let mut def_pos: BTreeMap<VReg, usize> = BTreeMap::new();
        for phi in &block.phis {
            def_pos.insert(phi.dst, 0);
        }
        for (i, inst) in block.insts.iter().enumerate() {
            if let Some(def) = inst.def() {
                def_pos.insert(def, i);
            }
        }

        // Collect splits needed: (inst_idx, old_vreg, fresh_vreg)
        let mut splits: Vec<(usize, VReg, VReg)> = Vec::new();

        for i in 0..block.insts.len() {
            let constraints = use_constraints(&block.insts[i]);
            let uses = block.insts[i].uses();

            for (use_vreg, constraint) in uses.iter().zip(constraints.iter()) {
                if let RegConstraint::Fixed(_) = constraint {
                    // Split unless the def is at the immediately preceding
                    // instruction (lifetime = 1). A 1-instruction lifetime is
                    // safe because the constraint handling's result.set only
                    // spans that single instruction — no earlier VReg can
                    // conflict at the constrained PhysReg.
                    let needs_split = i == 0
                        || def_pos.get(use_vreg) != Some(&(i - 1));
                    if needs_split {
                        let fresh = func.vregs.alloc();
                        while func.spill_descs.len() <= fresh.0 as usize {
                            func.spill_descs.push(SpillDesc::transient());
                        }
                        splits.push((i, *use_vreg, fresh));
                    }
                }
            }
        }

        if splits.is_empty() {
            continue;
        }

        // Apply splits in reverse order to preserve indices.
        for (inst_idx, old, fresh) in splits.into_iter().rev() {
            // Insert Mov(fresh, old) before the constraint instruction.
            block.insts.insert(inst_idx, MInst::Mov { dst: fresh, src: old });
            // Rewrite the constraint instruction's use (now at inst_idx + 1).
            block.insts[inst_idx + 1].rewrite_use(old, fresh);
        }
    }
}

// ────────────────────────────────────────────────────────────────
// Assignment result
// ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct AssignmentMap {
    pub map: BTreeMap<VReg, PhysReg>,
    /// True if assignment had to evict a VReg (indicates spilling bug).
    pub had_eviction: bool,
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
// Live-range splitting for clobbers
// ────────────────────────────────────────────────────────────────

/// Pre-pass: split live ranges at clobber points (O(n) per block).
///
/// For each clobber instruction (UDiv/URem), VRegs used by the clobber that
/// are also used later get split: a Mov copies the value to a fresh VReg,
/// and all subsequent references use the fresh VReg.
///
/// Single-pass algorithm using a rename map:
/// 1. Scan forward, applying accumulated renames to each instruction's uses.
/// 2. At clobber points, determine which uses need splitting (via a precomputed
///    last-use table), allocate fresh VRegs, and record renames.
/// 3. Insert Mov instructions at the appropriate positions.
pub fn split_live_ranges_at_clobbers(func: &mut MFunction) {
    for block in &mut func.blocks {
        // Quick check: any clobbers in this block?
        if !block.insts.iter().any(|inst| !clobbers(inst).is_empty()) {
            continue;
        }

        // Precompute last-use position for each VReg (O(n)).
        let mut last_use: BTreeMap<VReg, usize> = BTreeMap::new();
        for (i, inst) in block.insts.iter().enumerate() {
            for vreg in inst.uses() {
                last_use.insert(vreg, i);
            }
        }

        // Single forward pass: collect splits and apply renames.
        let mut rename: BTreeMap<VReg, VReg> = BTreeMap::new();
        // (insert_before_idx, Mov instruction) — collected, applied after
        let mut splits: Vec<(usize, MInst)> = Vec::new();
        // Offset: how many Movs have been scheduled before this point
        // (needed to adjust insertion indices)
        let mut offset = 0usize;

        for i in 0..block.insts.len() {
            // Apply accumulated renames to this instruction's uses.
            for (&old, &new) in &rename {
                block.insts[i].rewrite_use(old, new);
            }

            // If this is a clobber instruction, split uses that are live past it.
            if !clobbers(&block.insts[i]).is_empty() {
                let uses = block.insts[i].uses();
                for use_vreg in uses {
                    // Look up last use. For renamed VRegs, the last_use was
                    // inherited from the original at rename time.
                    if last_use.get(&use_vreg).copied().unwrap_or(0) > i {
                        let fresh = func.vregs.alloc();
                        while func.spill_descs.len() <= fresh.0 as usize {
                            func.spill_descs.push(SpillDesc::transient());
                        }
                        splits.push((i + offset, MInst::Mov { dst: fresh, src: use_vreg }));
                        offset += 1;
                        // Inherit last_use from the current VReg to the fresh one
                        if let Some(&lu) = last_use.get(&use_vreg) {
                            last_use.insert(fresh, lu);
                        }
                        rename.insert(use_vreg, fresh);
                    }
                }
            }
        }

        // Apply splits: insert Mov instructions at collected positions.
        // Process in reverse so indices stay valid.
        for (idx, mov) in splits.into_iter().rev() {
            block.insts.insert(idx, mov);
        }
    }
}

// ────────────────────────────────────────────────────────────────
// Assignment algorithm
// ────────────────────────────────────────────────────────────────

pub fn assign(func: &MFunction, analysis: &AnalysisResult) -> AssignmentMap {
    let mut result = AssignmentMap::default();

    // Pre-compute clobber points per block.
    let block_clobber_points: Vec<Vec<(usize, &'static [PhysReg])>> = func
        .blocks
        .iter()
        .map(|block| {
            block.insts.iter().enumerate()
                .filter_map(|(idx, inst)| {
                    let c = clobbers(inst);
                    if c.is_empty() { None } else { Some((idx, c)) }
                })
                .collect()
        })
        .collect();

    // Pre-compute shift points per block. VRegs whose lifetime spans a
    // shift point must avoid RCX so it is free for the constrained rhs.
    // Uses `>=` so the shift dst itself also avoids RCX (x86 `shr rcx, cl`
    // would alias result and count).
    let block_shift_points: Vec<Vec<usize>> = func
        .blocks
        .iter()
        .map(|block| {
            block.insts.iter().enumerate()
                .filter_map(|(idx, inst)| if is_reg_shift(inst) { Some(idx) } else { None })
                .collect()
        })
        .collect();

    for (bi, block) in func.blocks.iter().enumerate() {
        let mut active: BTreeMap<PhysReg, VReg> = BTreeMap::new();

        // Pre-compute last-use position for each VReg (O(n)).
        let mut last_use_in_block: BTreeMap<VReg, usize> = BTreeMap::new();
        for (i, inst) in block.insts.iter().enumerate() {
            for vreg in inst.uses() {
                last_use_in_block.insert(vreg, i);
            }
        }
        for &vreg in analysis.exit_distances[bi].keys() {
            last_use_in_block
                .entry(vreg)
                .and_modify(|v| *v = (*v).max(block.insts.len()))
                .or_insert(block.insts.len());
        }

        for vreg in analysis.entry_distances[bi].keys() {
            if let Some(preg) = result.get(*vreg) {
                active.insert(preg, *vreg);
            }
        }

        // Phi nodes
        for phi in &block.phis {
            let mut preferred: Option<PhysReg> = None;
            for (_pred_id, src_vreg) in &phi.sources {
                if let Some(preg) = result.get(*src_vreg) {
                    if !active.contains_key(&preg) || active.get(&preg) == Some(&phi.dst) {
                        preferred = Some(preg);
                        break;
                    }
                }
            }
            let preg = preferred
                .or_else(|| find_free_reg(&active, None))
                .expect("no free register for phi dst");
            active.insert(preg, phi.dst);
            result.set(phi.dst, preg);
        }

        for (inst_idx, inst) in block.insts.iter().enumerate() {
            let uses = inst.uses();
            let constraints = use_constraints(inst);
            let def = inst.def();

            // 1. Handle use constraints
            for (use_vreg, constraint) in uses.iter().zip(constraints.iter()) {
                if let RegConstraint::Fixed(required_preg) = constraint {
                    let current_preg = result.get(*use_vreg);
                    if current_preg != Some(*required_preg) {
                        if let Some(&occupant) = active.get(required_preg) {
                            if occupant != *use_vreg {
                                let new_reg = find_free_reg(&active, None);
                                if let Some(new_reg) = new_reg {
                                    active.remove(required_preg);
                                    active.insert(new_reg, occupant);
                                    result.set(occupant, new_reg);
                                }
                            }
                        }
                        if let Some(old_preg) = current_preg {
                            active.remove(&old_preg);
                        }
                        active.insert(*required_preg, *use_vreg);
                        result.set(*use_vreg, *required_preg);
                    }
                } else if result.get(*use_vreg).is_none() {
                    let preg = find_free_reg(&active, None)
                        .expect("no free register for use (spilling should prevent this)");
                    active.insert(preg, *use_vreg);
                    result.set(*use_vreg, preg);
                }
            }

            // 2. Free dead values
            let dead_regs: Vec<PhysReg> = active
                .iter()
                .filter(|&(_, &v)| {
                    super::analysis::next_use_at(func, analysis, bi, inst_idx + 1, v) == u32::MAX
                })
                .map(|(&p, _)| p)
                .collect();
            for preg in &dead_regs {
                active.remove(preg);
            }

            // 3. Allocate def
            if let Some(def_vreg) = def {
                let def_cons = def_constraint(inst);
                let preg = match def_cons {
                    RegConstraint::Fixed(required) => {
                        if let Some(&occupant) = active.get(&required) {
                            if occupant != def_vreg {
                                let new_reg = find_free_reg(&active, Some(required));
                                if let Some(new_reg) = new_reg {
                                    active.remove(&required);
                                    active.insert(new_reg, occupant);
                                    result.set(occupant, new_reg);
                                } else {
                                    active.remove(&required);
                                }
                            }
                        }
                        required
                    }
                    RegConstraint::Any => {
                        // Avoid clobbered registers during this VReg's live range.
                        let last_use_pos = last_use_in_block
                            .get(&def_vreg).copied().unwrap_or(inst_idx);
                        let blocked: BTreeSet<PhysReg> = block_clobber_points[bi]
                            .iter()
                            .filter(|(pos, _)| *pos > inst_idx && *pos <= last_use_pos)
                            .flat_map(|(_, regs)| regs.iter().copied())
                            .chain(
                                block_shift_points[bi].iter()
                                    .filter(|&&pos| pos >= inst_idx && pos <= last_use_pos)
                                    .map(|_| PhysReg::RCX)
                            )
                            .collect();

                        find_free_reg_excluding(&active, &blocked)
                            .or_else(|| find_free_reg(&active, None))
                            .unwrap_or_else(|| {
                                // Spilling should ensure pressure ≤ k_eff.
                                // Eviction here indicates a spilling bug.
                                result.had_eviction = true;
                                let victim = active
                                    .iter()
                                    .max_by_key(|&(_, &v)| {
                                        super::analysis::next_use_at(
                                            func, analysis, bi, inst_idx + 1, v,
                                        )
                                    })
                                    .map(|(&p, _)| p)
                                    .expect("no victim found");
                                active.remove(&victim);
                                victim
                            })
                    }
                };
                active.insert(preg, def_vreg);
                result.set(def_vreg, preg);
            }
        }
    }

    result
}

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

fn find_free_reg_excluding(
    active: &BTreeMap<PhysReg, VReg>,
    blocked: &BTreeSet<PhysReg>,
) -> Option<PhysReg> {
    let used: BTreeSet<PhysReg> = active.keys().copied().collect();
    ALLOCATABLE_REGS
        .iter()
        .copied()
        .find(|r| !used.contains(r) && !blocked.contains(r))
}
