//! Unified single-pass register allocator.
//!
//! Replaces the separate spilling → assignment pipeline with a single
//! forward walk that simultaneously decides which VRegs to spill AND
//! which physical registers to assign. This eliminates the analysis
//! divergence that required the k-1 hack.

use std::collections::{BTreeMap, BTreeSet};

use crate::backend::native::mir::*;

use super::analysis::{self, AnalysisResult};
use super::assignment::{
    clobbers, is_reg_shift, use_constraints, AssignmentMap, PhysReg, RegConstraint,
    ALLOCATABLE_REGS,
};

// Re-use spill slot allocator and spill/reload generation from spilling.rs
use super::spilling::{SpillSlotAllocator, make_spill, make_reload};
use super::NUM_REGS;

// ────────────────────────────────────────────────────────────────
// RegFile: bidirectional PhysReg ↔ VReg map
// ────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct RegFile {
    preg_to_vreg: BTreeMap<PhysReg, VReg>,
    vreg_to_preg: BTreeMap<VReg, PhysReg>,
}

impl RegFile {
    fn new() -> Self {
        Self {
            preg_to_vreg: BTreeMap::new(),
            vreg_to_preg: BTreeMap::new(),
        }
    }

    fn occupancy(&self) -> usize {
        self.vreg_to_preg.len()
    }

    fn get_preg(&self, vreg: VReg) -> Option<PhysReg> {
        self.vreg_to_preg.get(&vreg).copied()
    }

    fn get_vreg(&self, preg: PhysReg) -> Option<VReg> {
        self.preg_to_vreg.get(&preg).copied()
    }

    fn assign(&mut self, vreg: VReg, preg: PhysReg) {
        debug_assert!(
            !self.preg_to_vreg.contains_key(&preg),
            "PhysReg {preg} already occupied by {:?} when assigning {vreg}",
            self.preg_to_vreg.get(&preg)
        );
        self.preg_to_vreg.insert(preg, vreg);
        self.vreg_to_preg.insert(vreg, preg);
    }

    fn evict(&mut self, vreg: VReg) {
        if let Some(preg) = self.vreg_to_preg.remove(&vreg) {
            self.preg_to_vreg.remove(&preg);
        }
    }

    fn contains(&self, vreg: VReg) -> bool {
        self.vreg_to_preg.contains_key(&vreg)
    }

    fn find_free_excluding(&self, blocked: &BTreeSet<PhysReg>) -> Option<PhysReg> {
        ALLOCATABLE_REGS
            .iter()
            .copied()
            .find(|r| !self.preg_to_vreg.contains_key(r) && !blocked.contains(r))
    }

    fn vregs(&self) -> impl Iterator<Item = VReg> + '_ {
        self.vreg_to_preg.keys().copied()
    }
}

// ────────────────────────────────────────────────────────────────
// Unified allocator
// ────────────────────────────────────────────────────────────────

pub fn unified_alloc(
    func: &mut MFunction,
    analysis: &AnalysisResult,
) -> (AssignmentMap, u32) {
    let num_blocks = func.blocks.len();
    let k = NUM_REGS;
    let mut result = AssignmentMap::default();
    let mut slots = SpillSlotAllocator::new();

    let mut regfile_exit: Vec<RegFile> = vec![RegFile::new(); num_blocks];
    let mut s_exit: Vec<BTreeSet<VReg>> = vec![BTreeSet::new(); num_blocks];

    for bi in 0..num_blocks {
        let (entry_rf, entry_s) = compute_entry_regfile(
            func, analysis, bi, k, &regfile_exit, &s_exit, &result,
        );

        // Insert coupling code
        insert_coupling_code(func, analysis, bi, &entry_rf, &regfile_exit, &mut slots, &mut result);

        // Record entry assignments
        for (vreg, preg) in &entry_rf.vreg_to_preg {
            result.set(*vreg, *preg);
        }

        let (exit_rf, exit_s, new_insts) = process_block(
            func, analysis, bi, entry_rf, entry_s, k, &mut slots, &mut result,
        );

        func.blocks[bi].insts = new_insts;
        regfile_exit[bi] = exit_rf;
        s_exit[bi] = exit_s;
    }

    (result, slots.total_size() as u32)
}

// ────────────────────────────────────────────────────────────────
// Entry state computation
// ────────────────────────────────────────────────────────────────

fn compute_entry_regfile(
    func: &MFunction,
    analysis: &AnalysisResult,
    block_idx: usize,
    k: usize,
    regfile_exit: &[RegFile],
    _s_exit: &[BTreeSet<VReg>],
    result: &AssignmentMap,
) -> (RegFile, BTreeSet<VReg>) {
    let preds = &analysis.predecessors[block_idx];
    let mut rf = RegFile::new();
    let s = BTreeSet::new();

    if preds.is_empty() {
        return (rf, s);
    }

    // Collect VRegs in predecessor exits (forward edges only)
    let mut all: Option<BTreeSet<VReg>> = None;
    let mut some: BTreeSet<VReg> = BTreeSet::new();

    for &pred_idx in preds {
        if pred_idx >= block_idx { continue; }
        let pred_vregs: BTreeSet<VReg> = regfile_exit[pred_idx].vregs().collect();
        some = some.union(&pred_vregs).copied().collect();
        all = Some(match all {
            None => pred_vregs,
            Some(a) => a.intersection(&pred_vregs).copied().collect(),
        });
    }

    let all = all.unwrap_or_default();

    // Start with intersection: VRegs in registers in ALL predecessors
    for vreg in &all {
        if rf.occupancy() >= k { break; }
        // Use existing PhysReg assignment if available
        if let Some(preg) = result.get(*vreg) {
            if !rf.preg_to_vreg.contains_key(&preg) {
                rf.assign(*vreg, preg);
            }
        }
    }

    // Add phi defs
    for phi in &func.blocks[block_idx].phis {
        if rf.occupancy() >= k { break; }
        if rf.contains(phi.dst) { continue; }
        // Try to coalesce with a phi source's register
        let mut preferred: Option<PhysReg> = None;
        for (_pred_id, src_vreg) in &phi.sources {
            if let Some(preg) = result.get(*src_vreg) {
                if !rf.preg_to_vreg.contains_key(&preg) {
                    preferred = Some(preg);
                    break;
                }
            }
        }
        let preg = preferred
            .or_else(|| rf.find_free_excluding(&BTreeSet::new()))
            .expect("no free register for phi dst");
        rf.assign(phi.dst, preg);
    }

    // Fill remaining slots from `some` set by next-use distance
    if rf.occupancy() < k {
        let mut candidates: Vec<VReg> = some.difference(&all).copied()
            .filter(|v| !rf.contains(*v))
            .collect();
        candidates.sort_by_key(|v| {
            analysis.entry_distances[block_idx].get(v).copied().unwrap_or(u32::MAX)
        });
        for vreg in candidates {
            if rf.occupancy() >= k { break; }
            if !analysis.entry_distances[block_idx].contains_key(&vreg) { continue; }
            if let Some(preg) = result.get(vreg) {
                if !rf.preg_to_vreg.contains_key(&preg) {
                    rf.assign(vreg, preg);
                    continue;
                }
            }
            if let Some(preg) = rf.find_free_excluding(&BTreeSet::new()) {
                rf.assign(vreg, preg);
            }
        }
    }

    (rf, s)
}

// ────────────────────────────────────────────────────────────────
// Coupling code
// ────────────────────────────────────────────────────────────────

fn insert_coupling_code(
    func: &mut MFunction,
    analysis: &AnalysisResult,
    block_idx: usize,
    entry_rf: &RegFile,
    regfile_exit: &[RegFile],
    slots: &mut SpillSlotAllocator,
    result: &mut AssignmentMap,
) {
    for &pred_idx in &analysis.predecessors[block_idx] {
        if pred_idx >= block_idx { continue; }

        let pred_rf = &regfile_exit[pred_idx];
        let phi_dsts: BTreeSet<VReg> = func.blocks[block_idx].phis.iter().map(|p| p.dst).collect();

        let need_reload: Vec<VReg> = entry_rf.vregs()
            .filter(|v| !pred_rf.contains(*v) && !phi_dsts.contains(v))
            .collect();

        if !need_reload.is_empty() {
            let term_idx = func.blocks[pred_idx].insts.len().saturating_sub(1);
            for vreg in need_reload.iter().rev() {
                let reload = make_reload(*vreg, func, slots);
                // Assign the reload the same PhysReg as in entry
                if let Some(preg) = entry_rf.get_preg(*vreg) {
                    result.set(*vreg, preg);
                }
                func.blocks[pred_idx].insts.insert(term_idx, reload);
            }
        }
    }
}

// ────────────────────────────────────────────────────────────────
// Per-block processing
// ────────────────────────────────────────────────────────────────

fn process_block(
    func: &mut MFunction,
    analysis: &AnalysisResult,
    block_idx: usize,
    mut rf: RegFile,
    mut s: BTreeSet<VReg>,
    k: usize,
    slots: &mut SpillSlotAllocator,
    result: &mut AssignmentMap,
) -> (RegFile, BTreeSet<VReg>, Vec<MInst>) {
    let block = &func.blocks[block_idx];
    let mut new_insts: Vec<MInst> = Vec::with_capacity(block.insts.len());

    // Pre-compute shift points for blocked set
    let shift_points: Vec<usize> = block.insts.iter().enumerate()
        .filter_map(|(idx, inst)| if is_reg_shift(inst) { Some(idx) } else { None })
        .collect();

    // Pre-compute last-use positions for blocked set
    let mut last_use_in_block: BTreeMap<VReg, usize> = BTreeMap::new();
    for (i, inst) in block.insts.iter().enumerate() {
        for vreg in inst.uses() {
            last_use_in_block.insert(vreg, i);
        }
    }
    for &vreg in analysis.exit_distances[block_idx].keys() {
        last_use_in_block.entry(vreg).and_modify(|v| *v = (*v).max(block.insts.len()))
            .or_insert(block.insts.len());
    }

    for (inst_idx, inst) in block.insts.iter().enumerate() {
        let mut rewritten_inst = inst.clone();
        let uses = inst.uses();
        let def = inst.def();
        let constraints = use_constraints(inst);

        // Step A+B: Ensure all uses are in registers
        let mut pinned: BTreeSet<VReg> = BTreeSet::new();

        for (_ui, (&use_vreg, constraint)) in uses.iter().zip(constraints.iter()).enumerate() {
            if let RegConstraint::Fixed(required_preg) = constraint {
                // Fixed constraint: need use_vreg in required_preg
                if rf.get_preg(use_vreg) == Some(*required_preg) {
                    // Already there
                    pinned.insert(use_vreg);
                } else {
                    // Need to get use_vreg into required_preg
                    // First, free required_preg if occupied
                    if let Some(occupant) = rf.get_vreg(*required_preg) {
                        // Spill the occupant first (so it can be reloaded later
                        // if needed by subsequent instructions).
                        emit_spill(&mut new_insts, occupant, &mut s, func, slots, result);

                        if pinned.contains(&occupant) {
                            // Occupant is used by current instruction — keep it
                            // in a different register for this instruction only.
                            let move_blocked: BTreeSet<PhysReg> = [*required_preg].into();
                            if rf.occupancy() >= k {
                                evict_farthest(&mut rf, &mut s, &mut new_insts, func, analysis,
                                    block_idx, inst_idx, slots, &pinned, &BTreeSet::new(), result);
                            }
                            let new_preg = rf.find_free_excluding(&move_blocked)
                                .expect("no free register for occupant move");
                            let fresh_occ = func.vregs.alloc();
                            while func.spill_descs.len() <= fresh_occ.0 as usize {
                                func.spill_descs.push(func.spill_desc(occupant).cloned().unwrap_or(SpillDesc::transient()));
                            }
                            new_insts.push(MInst::Mov { dst: fresh_occ, src: occupant });
                            rf.evict(occupant);
                            rf.assign(fresh_occ, new_preg);
                            result.set(fresh_occ, new_preg);
                            rewritten_inst.rewrite_use(occupant, fresh_occ);
                            pinned.remove(&occupant);
                            pinned.insert(fresh_occ);
                        } else {
                            rf.evict(occupant);
                        }
                    }

                    if rf.contains(use_vreg) {
                        // use_vreg is in some other register, create a copy to RCX
                        let fresh = func.vregs.alloc();
                        while func.spill_descs.len() <= fresh.0 as usize {
                            func.spill_descs.push(SpillDesc::transient());
                        }
                        new_insts.push(MInst::Mov { dst: fresh, src: use_vreg });
                        rf.assign(fresh, *required_preg);
                        result.set(fresh, *required_preg);
                        rewritten_inst.rewrite_use(use_vreg, fresh);
                        pinned.insert(fresh);
                    } else {
                        // use_vreg is spilled, reload directly to required_preg
                        let fresh = func.vregs.alloc();
                        while func.spill_descs.len() <= fresh.0 as usize {
                            func.spill_descs.push(func.spill_desc(use_vreg).cloned().unwrap_or(SpillDesc::transient()));
                        }
                        let mut reload = make_reload(use_vreg, func, slots);
                        match &mut reload {
                            MInst::LoadImm { dst, .. } | MInst::Load { dst, .. } => *dst = fresh,
                            _ => {}
                        }
                        new_insts.push(reload);
                        rf.assign(fresh, *required_preg);
                        s.insert(fresh);
                        result.set(fresh, *required_preg);
                        rewritten_inst.rewrite_use(use_vreg, fresh);
                        pinned.insert(fresh);
                    }
                }
            } else {
                // Any constraint
                if !rf.contains(use_vreg) {
                    // Need to reload
                    let fresh = func.vregs.alloc();
                    while func.spill_descs.len() <= fresh.0 as usize {
                        func.spill_descs.push(func.spill_desc(use_vreg).cloned().unwrap_or(SpillDesc::transient()));
                    }

                    // Evict if needed to make room
                    if rf.occupancy() >= k {
                        evict_farthest(&mut rf, &mut s, &mut new_insts, func, analysis, block_idx, inst_idx, slots, &pinned, &BTreeSet::new(), result);
                    }

                    // Find a free register (respecting shift blocked set)
                    let blocked = compute_blocked_for_vreg(
                        fresh, inst_idx, &last_use_in_block, &shift_points,
                    );
                    let preg = rf.find_free_excluding(&blocked)
                        .or_else(|| rf.find_free_excluding(&BTreeSet::new()))
                        .expect("no free register for reload");

                    let mut reload = make_reload(use_vreg, func, slots);
                    match &mut reload {
                        MInst::LoadImm { dst, .. } | MInst::Load { dst, .. } => *dst = fresh,
                        _ => {}
                    }
                    new_insts.push(reload);
                    rf.assign(fresh, preg);
                    s.insert(fresh);
                    result.set(fresh, preg);
                    rewritten_inst.rewrite_use(use_vreg, fresh);
                    pinned.insert(fresh);
                } else {
                    pinned.insert(use_vreg);
                }
            }
        }

        // Step C: Evict to pressure ≤ k
        while rf.occupancy() > k {
            evict_farthest(&mut rf, &mut s, &mut new_insts, func, analysis, block_idx, inst_idx, slots, &pinned, &BTreeSet::new(), result);
        }

        // Step D: Handle def
        if let Some(def_vreg) = def {
            let clobber_extra = clobbers(inst).len().saturating_sub(1);

            // Make room: need occupancy + 1 + clobber_extra ≤ k
            while rf.occupancy() + 1 + clobber_extra > k {
                evict_farthest(&mut rf, &mut s, &mut new_insts, func, analysis, block_idx, inst_idx + 1, slots, &pinned, &BTreeSet::new(), result);
            }

            // Pick a PhysReg for the def
            let last_use_pos = last_use_in_block.get(&def_vreg).copied().unwrap_or(inst_idx);
            let blocked = compute_blocked_for_def(
                inst_idx, last_use_pos, &shift_points,
                &super::assignment::block_clobber_points_for(block),
            );

            let preg = rf.find_free_excluding(&blocked)
                .or_else(|| rf.find_free_excluding(&BTreeSet::new()))
                .expect("no free register for def");

            rf.assign(def_vreg, preg);
            result.set(def_vreg, preg);
        }

        // Emit instruction
        new_insts.push(rewritten_inst);

        // Step E: Remove dead VRegs
        let dead: Vec<VReg> = rf.vregs()
            .filter(|&v| analysis::next_use_at(func, analysis, block_idx, inst_idx + 1, v) == u32::MAX)
            .collect();
        for v in dead {
            rf.evict(v);
        }
    }

    (rf, s, new_insts)
}

// ────────────────────────────────────────────────────────────────
// Helpers
// ────────────────────────────────────────────────────────────────

fn emit_spill(
    new_insts: &mut Vec<MInst>,
    vreg: VReg,
    s: &mut BTreeSet<VReg>,
    func: &MFunction,
    slots: &mut SpillSlotAllocator,
    _result: &mut AssignmentMap,
) {
    if !s.contains(&vreg) {
        if let Some(spill_inst) = make_spill(vreg, func, slots) {
            new_insts.push(spill_inst);
        }
        s.insert(vreg);
    }
}

fn evict_farthest(
    rf: &mut RegFile,
    s: &mut BTreeSet<VReg>,
    new_insts: &mut Vec<MInst>,
    func: &MFunction,
    analysis: &AnalysisResult,
    block_idx: usize,
    inst_idx: usize,
    slots: &mut SpillSlotAllocator,
    pinned: &BTreeSet<VReg>,
    _blocked_pregs: &BTreeSet<PhysReg>,
    result: &mut AssignmentMap,
) {
    // Choose victim: farthest next-use, prefer rematerializable/cheap
    let victim = rf.vregs()
        .filter(|v| !pinned.contains(v))
        .max_by_key(|&v| {
            let next_use = analysis::next_use_at(func, analysis, block_idx, inst_idx, v);
            let desc = func.spill_desc(v);
            let eviction_class = match desc {
                Some(d) if matches!(d.kind, SpillKind::Remat { .. }) => 3,
                Some(d) if d.spill_cost == 0 && d.reload_cost <= 1 => 2,
                Some(d) if d.spill_cost == 0 => 1,
                _ => 0,
            };
            let effective_class = if s.contains(&v) { eviction_class.max(1) } else { eviction_class };
            (effective_class, next_use)
        })
        .expect("no eviction victim");

    emit_spill(new_insts, victim, s, func, slots, result);
    rf.evict(victim);
}

fn compute_blocked_for_vreg(
    _vreg: VReg,
    _inst_idx: usize,
    _last_use: &BTreeMap<VReg, usize>,
    _shift_points: &[usize],
) -> BTreeSet<PhysReg> {
    // For reloaded VRegs, we don't have last_use info yet.
    // Return empty — the caller falls back to find_free_excluding(&empty).
    BTreeSet::new()
}

fn compute_blocked_for_def(
    inst_idx: usize,
    last_use_pos: usize,
    shift_points: &[usize],
    clobber_points: &[(usize, &'static [PhysReg])],
) -> BTreeSet<PhysReg> {
    let mut blocked = BTreeSet::new();
    for &(pos, regs) in clobber_points {
        if pos > inst_idx && pos <= last_use_pos {
            for &r in regs { blocked.insert(r); }
        }
    }
    for &pos in shift_points {
        if pos >= inst_idx && pos <= last_use_pos {
            blocked.insert(PhysReg::RCX);
        }
    }
    blocked
}
