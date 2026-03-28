//! Unified single-pass register allocator.
//!
//! Replaces the separate spilling → assignment pipeline with a single
//! forward walk that simultaneously decides which VRegs to spill AND
//! which physical registers to assign. This eliminates the analysis
//! divergence that required the k-1 hack.

use std::collections::{HashMap, HashSet};

use crate::backend::native::mir::*;

use super::analysis::{self, AnalysisResult};
use super::assignment::{
    clobbers, is_reg_shift, use_constraints, AssignmentMap, PhysReg, PhysRegSet, RegConstraint,
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
    preg_to_vreg: [Option<VReg>; 13],
    vreg_to_preg: HashMap<VReg, PhysReg>,
}

/// Map a PhysReg discriminant (which may have gaps, e.g. RSI=6) to a dense
/// index in 0..13 for the preg_to_vreg array.
const fn preg_dense_index(preg: PhysReg) -> usize {
    match preg {
        PhysReg::RAX => 0,
        PhysReg::RCX => 1,
        PhysReg::RDX => 2,
        PhysReg::RBX => 3,
        PhysReg::RSI => 4,
        PhysReg::RDI => 5,
        PhysReg::R8 => 6,
        PhysReg::R9 => 7,
        PhysReg::R10 => 8,
        PhysReg::R11 => 9,
        PhysReg::R12 => 10,
        PhysReg::R13 => 11,
        PhysReg::R14 => 12,
    }
}

impl RegFile {
    fn new() -> Self {
        Self {
            preg_to_vreg: [None; 13],
            vreg_to_preg: HashMap::new(),
        }
    }

    fn occupancy(&self) -> usize {
        self.vreg_to_preg.len()
    }

    fn get_preg(&self, vreg: VReg) -> Option<PhysReg> {
        self.vreg_to_preg.get(&vreg).copied()
    }

    fn get_vreg(&self, preg: PhysReg) -> Option<VReg> {
        self.preg_to_vreg[preg_dense_index(preg)]
    }

    fn assign(&mut self, vreg: VReg, preg: PhysReg) {
        let idx = preg_dense_index(preg);
        assert!(
            self.preg_to_vreg[idx].is_none(),
            "PhysReg {preg} already occupied by {:?} when assigning {vreg}",
            self.preg_to_vreg[idx]
        );
        self.preg_to_vreg[idx] = Some(vreg);
        self.vreg_to_preg.insert(vreg, preg);
    }

    fn evict(&mut self, vreg: VReg) {
        if let Some(preg) = self.vreg_to_preg.remove(&vreg) {
            self.preg_to_vreg[preg_dense_index(preg)] = None;
        }
    }

    fn contains(&self, vreg: VReg) -> bool {
        self.vreg_to_preg.contains_key(&vreg)
    }

    fn find_free_excluding(&self, blocked: &PhysRegSet) -> Option<PhysReg> {
        ALLOCATABLE_REGS
            .iter()
            .copied()
            .find(|r| self.preg_to_vreg[preg_dense_index(*r)].is_none() && !blocked.contains(r))
    }

    fn preg_occupied(&self, preg: PhysReg) -> bool {
        self.preg_to_vreg[preg_dense_index(preg)].is_some()
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
    let mut s_exit: Vec<HashSet<VReg>> = vec![HashSet::new(); num_blocks];

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
    _s_exit: &[HashSet<VReg>],
    result: &AssignmentMap,
) -> (RegFile, HashSet<VReg>) {
    let preds = &analysis.predecessors[block_idx];
    let mut rf = RegFile::new();
    let s = HashSet::new();

    if preds.is_empty() {
        return (rf, s);
    }

    // Collect VRegs in predecessor exits (forward edges only)
    let mut all: Option<HashSet<VReg>> = None;
    let mut some: HashSet<VReg> = HashSet::new();

    for &pred_idx in preds {
        if pred_idx >= block_idx { continue; } // skip back edges (assumes layout ≈ RPO)
        let pred_vregs: HashSet<VReg> = regfile_exit[pred_idx].vregs().collect();
        some = some.union(&pred_vregs).copied().collect();
        all = Some(match all {
            None => pred_vregs,
            Some(a) => a.intersection(&pred_vregs).copied().collect(),
        });
    }

    let all = all.unwrap_or_default();

    // Start with intersection: VRegs in registers in ALL predecessors.
    // If the preferred PhysReg is already taken, skip — the VReg will be
    // reloaded on demand by process_block when actually used.
    // Sort for deterministic register assignment (HashSet iteration is unordered).
    let mut all_sorted: Vec<VReg> = all.iter().copied().collect();
    all_sorted.sort();
    for vreg in &all_sorted {
        if rf.occupancy() >= k { break; }
        if let Some(preg) = result.get(*vreg) {
            if !rf.preg_occupied(preg) {
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
                if !rf.preg_occupied(preg) {
                    preferred = Some(preg);
                    break;
                }
            }
        }
        let preg = preferred
            .or_else(|| rf.find_free_excluding(&PhysRegSet::new()))
            .expect("no free register for phi dst");
        rf.assign(phi.dst, preg);
    }

    // Fill remaining slots from `some` set by next-use distance
    if rf.occupancy() < k {
        let mut candidates: Vec<VReg> = some.difference(&all).copied()
            .filter(|v| !rf.contains(*v))
            .collect();
        candidates.sort_by_key(|v| {
            (analysis.entry_distances[block_idx].get(v).copied().unwrap_or(u32::MAX), *v)
        });
        for vreg in candidates {
            if rf.occupancy() >= k { break; }
            if !analysis.entry_distances[block_idx].contains_key(&vreg) { continue; }
            if let Some(preg) = result.get(vreg) {
                if !rf.preg_occupied(preg) {
                    rf.assign(vreg, preg);
                    continue;
                }
                // PhysReg conflict — skip; process_block will reload on demand
            } else if let Some(preg) = rf.find_free_excluding(&PhysRegSet::new()) {
                // No prior assignment — first time this VReg is assigned
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
        let phi_dsts: HashSet<VReg> = func.blocks[block_idx].phis.iter().map(|p| p.dst).collect();

        let mut need_reload: Vec<VReg> = entry_rf.vregs()
            .filter(|v| !pred_rf.contains(*v) && !phi_dsts.contains(v))
            .collect();
        need_reload.sort();

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
    mut s: HashSet<VReg>,
    k: usize,
    slots: &mut SpillSlotAllocator,
    result: &mut AssignmentMap,
) -> (RegFile, HashSet<VReg>, Vec<MInst>) {
    let block = &func.blocks[block_idx];
    let mut new_insts: Vec<MInst> = Vec::with_capacity(block.insts.len());

    // Pre-compute next-use table: for each VReg, sorted list of use positions.
    // This replaces O(n) forward scans in next_use_at with O(log n) binary search.
    let mut use_positions: HashMap<VReg, Vec<usize>> = HashMap::new();
    for (i, inst) in block.insts.iter().enumerate() {
        for vreg in inst.uses() {
            use_positions.entry(vreg).or_default().push(i);
        }
    }

    // Pre-compute shift and clobber points for blocked set
    let shift_points: Vec<usize> = block.insts.iter().enumerate()
        .filter_map(|(idx, inst)| if is_reg_shift(inst) { Some(idx) } else { None })
        .collect();
    let clobber_points = super::assignment::block_clobber_points_for(block);

    // Pre-compute last-use positions for blocked set
    let mut last_use_in_block: HashMap<VReg, usize> = HashMap::new();
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
        let mut pinned: HashSet<VReg> = HashSet::new();

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
                        // Spill the occupant first so it can be reloaded later.
                        // emit_spill is idempotent (checks s.contains), so calling
                        // it on an already-spilled VReg is a no-op.
                        emit_spill(&mut new_insts, occupant, &mut s, func, slots, result);

                        if pinned.contains(&occupant) {
                            // Occupant is used by current instruction — keep it
                            // in a different register for this instruction only.
                            let move_blocked = { let mut s = PhysRegSet::new(); s.insert(*required_preg); s };
                            if rf.occupancy() >= k {
                                evict_farthest(&mut rf, &mut s, &mut new_insts, func, analysis, block_idx, inst_idx, block.insts.len(), &use_positions, slots, &pinned, &PhysRegSet::new(), result);
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
                        evict_farthest(&mut rf, &mut s, &mut new_insts, func, analysis, block_idx, inst_idx, block.insts.len(), &use_positions, slots, &pinned, &PhysRegSet::new(), result);
                    }

                    // Find a free register (respecting shift blocked set)
                    let blocked = compute_blocked_for_vreg(
                        fresh, inst_idx, &last_use_in_block, &shift_points,
                    );
                    let preg = rf.find_free_excluding(&blocked)
                        .or_else(|| rf.find_free_excluding(&PhysRegSet::new()))
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
            evict_farthest(&mut rf, &mut s, &mut new_insts, func, analysis, block_idx, inst_idx, block.insts.len(), &use_positions, slots, &pinned, &PhysRegSet::new(), result);
        }

        // Step D: Handle def
        if let Some(def_vreg) = def {
            let clobber_extra = clobbers(inst).len().saturating_sub(1);

            // Make room: need occupancy + 1 + clobber_extra ≤ k
            while rf.occupancy() + 1 + clobber_extra > k {
                evict_farthest(&mut rf, &mut s, &mut new_insts, func, analysis, block_idx, inst_idx + 1, block.insts.len(), &use_positions, slots, &pinned, &PhysRegSet::new(), result);
            }

            // Pick a PhysReg for the def.
            // Prefer the lhs operand's register (avoids mov in x86 2-operand form).
            let last_use_pos = last_use_in_block.get(&def_vreg).copied().unwrap_or(inst_idx);
            let blocked = compute_blocked_for_def(
                inst_idx, last_use_pos, &shift_points,
                &clobber_points,
            );

            // Coalescing hint: reuse a dying operand's PhysReg for dst.
            // Prefer lhs (avoids mov in x86 2-operand form), then try rhs
            // (emit_binop_rr swaps for commutative ops).
            let hint_preg = uses.iter().find_map(|&use_vreg| {
                let preg = rf.get_preg(use_vreg)?;
                let next = fast_next_use(&use_positions, analysis, block_idx, block.insts.len(), inst_idx + 1, use_vreg);
                if next == u32::MAX && !blocked.contains(&preg) {
                    Some((use_vreg, preg))
                } else {
                    None
                }
            });

            let preg = if let Some((hint_vreg, hp)) = hint_preg {
                // Evict the dying operand to free its PhysReg for the def
                if rf.get_preg(hint_vreg) == Some(hp) {
                    rf.evict(hint_vreg);
                }
                hp
            } else {
                rf.find_free_excluding(&blocked)
                    .or_else(|| rf.find_free_excluding(&PhysRegSet::new()))
                    .expect("no free register for def")
            };

            rf.assign(def_vreg, preg);
            result.set(def_vreg, preg);
        }

        // Emit instruction
        new_insts.push(rewritten_inst);

        // Step E: Remove dead VRegs
        let block_len = block.insts.len();
        let dead: Vec<VReg> = rf.vregs()
            .filter(|&v| fast_next_use(&use_positions, analysis, block_idx, block_len, inst_idx + 1, v) == u32::MAX)
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
    s: &mut HashSet<VReg>,
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
    s: &mut HashSet<VReg>,
    new_insts: &mut Vec<MInst>,
    func: &MFunction,
    analysis: &AnalysisResult,
    block_idx: usize,
    inst_idx: usize,
    block_len: usize,
    use_positions: &HashMap<VReg, Vec<usize>>,
    slots: &mut SpillSlotAllocator,
    pinned: &HashSet<VReg>,
    _blocked_pregs: &PhysRegSet,
    result: &mut AssignmentMap,
) {
    // Choose victim: farthest next-use, prefer rematerializable/cheap.
    // Secondary key: VReg id for deterministic tie-breaking (HashMap iteration is unordered).
    let victim = rf.vregs()
        .filter(|v| !pinned.contains(v))
        .max_by_key(|&v| {
            let next_use = fast_next_use(use_positions, analysis, block_idx, block_len, inst_idx, v);
            let desc = func.spill_desc(v);
            let eviction_class = match desc {
                Some(d) if matches!(d.kind, SpillKind::Remat { .. }) => 3,
                Some(d) if d.spill_cost == 0 && d.reload_cost <= 1 => 2,
                Some(d) if d.spill_cost == 0 => 1,
                _ => 0,
            };
            let effective_class = if s.contains(&v) { eviction_class.max(1) } else { eviction_class };
            (effective_class, next_use, v)
        })
        .expect("no eviction victim: all VRegs in RegFile are pinned (used by current instruction)");

    emit_spill(new_insts, victim, s, func, slots, result);
    rf.evict(victim);
}

/// O(log n) next-use lookup using pre-computed use position lists.
fn fast_next_use(
    use_positions: &HashMap<VReg, Vec<usize>>,
    analysis: &AnalysisResult,
    block_idx: usize,
    block_len: usize,
    inst_idx: usize,
    vreg: VReg,
) -> u32 {
    if let Some(positions) = use_positions.get(&vreg) {
        // Binary search for first position >= inst_idx
        match positions.binary_search(&inst_idx) {
            Ok(_) => 0, // Used at exactly inst_idx
            Err(idx) => {
                if idx < positions.len() {
                    (positions[idx] - inst_idx) as u32
                } else {
                    // No more uses in this block; check exit distance
                    let remaining = (block_len - inst_idx) as u32;
                    analysis.exit_distances[block_idx]
                        .get(&vreg)
                        .map(|d| remaining + d)
                        .unwrap_or(u32::MAX)
                }
            }
        }
    } else {
        // VReg not used in this block at all (fresh VReg from spilling)
        // Check exit distances
        let remaining = (block_len - inst_idx) as u32;
        analysis.exit_distances[block_idx]
            .get(&vreg)
            .map(|d| remaining + d)
            .unwrap_or(u32::MAX)
    }
}

fn compute_blocked_for_vreg(
    _vreg: VReg,
    _inst_idx: usize,
    _last_use: &HashMap<VReg, usize>,
    _shift_points: &[usize],
) -> PhysRegSet {
    // For reloaded VRegs, we don't have last_use info yet.
    // Return empty — the caller falls back to find_free_excluding(&empty).
    PhysRegSet::new()
}

fn compute_blocked_for_def(
    inst_idx: usize,
    last_use_pos: usize,
    shift_points: &[usize],
    clobber_points: &[(usize, &'static [PhysReg])],
) -> PhysRegSet {
    let mut blocked = PhysRegSet::new();
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
