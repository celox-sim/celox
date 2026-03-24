//! Spilling phase: Braun & Hack extended MIN algorithm.
//!
//! Walks the CFG in reverse post order, applying the MIN algorithm per block.
//! Inserts spill/reload MIR instructions to ensure register pressure ≤ k.
//!
//! Domain-specific optimizations:
//! - **Rematerialization**: constants (SpillKind::Remat) are never stored;
//!   reloads become LoadImm.
//! - **SimState reload**: values loaded from simulation state (SpillKind::SimState)
//!   are reloaded from their original location, not from the stack.
//! - **Spill slot allocation**: each spilled value gets a unique stack slot.

use std::collections::{BTreeMap, BTreeSet};

use crate::backend::native::mir::*;

use super::analysis::{self, AnalysisResult};

// ────────────────────────────────────────────────────────────────
// Spill slot allocator
// ────────────────────────────────────────────────────────────────

/// Assigns unique stack frame offsets to spilled VRegs.
struct SpillSlotAllocator {
    /// VReg → stack offset (bytes from frame base)
    slots: BTreeMap<VReg, i32>,
    /// Next available offset
    next_offset: i32,
}

impl SpillSlotAllocator {
    fn new() -> Self {
        Self {
            slots: BTreeMap::new(),
            next_offset: 0,
        }
    }

    /// Get or allocate a spill slot for a VReg. Returns the byte offset.
    fn slot_for(&mut self, vreg: VReg) -> i32 {
        *self.slots.entry(vreg).or_insert_with(|| {
            let off = self.next_offset;
            self.next_offset += 8; // all slots are 8 bytes (i64)
            off
        })
    }

    /// Total bytes of spill slots allocated.
    fn total_size(&self) -> i32 {
        self.next_offset
    }
}

// ────────────────────────────────────────────────────────────────
// Spill/reload instruction generation
// ────────────────────────────────────────────────────────────────

/// Generate a spill instruction for `vreg` based on its SpillDesc.
/// Returns None if no spill store is needed (remat, store-back-only).
fn make_spill(
    vreg: VReg,
    func: &MFunction,
    slots: &mut SpillSlotAllocator,
) -> Option<MInst> {
    let desc = func.spill_desc(vreg);
    match desc.map(|d| &d.kind) {
        Some(SpillKind::Remat { .. }) => {
            // Rematerializable: no spill store needed
            None
        }
        Some(SpillKind::SimState { .. }) if desc.unwrap().spill_cost == 0 => {
            // Store-back-only: value is already in simulation state
            None
        }
        Some(SpillKind::SimState { .. }) => {
            // Value was loaded from sim state but may have been modified;
            // spill to stack to be safe
            let offset = slots.slot_for(vreg);
            Some(MInst::Store {
                base: BaseReg::StackFrame,
                offset,
                src: vreg,
                size: OpSize::S64,
            })
        }
        Some(SpillKind::Stack) | None => {
            // Transient value: spill to stack
            let offset = slots.slot_for(vreg);
            Some(MInst::Store {
                base: BaseReg::StackFrame,
                offset,
                src: vreg,
                size: OpSize::S64,
            })
        }
    }
}

/// Generate a reload instruction for `vreg` based on its SpillDesc.
fn make_reload(
    vreg: VReg,
    func: &MFunction,
    slots: &mut SpillSlotAllocator,
) -> MInst {
    let desc = func.spill_desc(vreg);
    match desc.map(|d| &d.kind) {
        Some(SpillKind::Remat { value }) => {
            // Rematerialize: just reload the constant
            MInst::LoadImm {
                dst: vreg,
                value: *value,
            }
        }
        Some(SpillKind::SimState {
            bit_offset,
            width_bits,
            ..
        }) if desc.unwrap().spill_cost == 0 => {
            // Store-back-only: reload from simulation state.
            // The original Load instruction had the correct offset;
            // we reconstruct it from SpillDesc.
            let byte_offset = (bit_offset / 8) as i32;
            let op_size = match *width_bits {
                0..=8 => OpSize::S8,
                9..=16 => OpSize::S16,
                17..=32 => OpSize::S32,
                _ => OpSize::S64,
            };
            MInst::Load {
                dst: vreg,
                base: BaseReg::SimState,
                offset: byte_offset,
                size: op_size,
            }
        }
        _ => {
            // Reload from stack slot
            let offset = slots.slot_for(vreg);
            MInst::Load {
                dst: vreg,
                base: BaseReg::StackFrame,
                offset,
                size: OpSize::S64,
            }
        }
    }
}

// ────────────────────────────────────────────────────────────────
// Main spilling entry point
// ────────────────────────────────────────────────────────────────

/// Run the spilling phase on an MFunction.
/// After this, every program point has at most `k` simultaneously live VRegs.
/// Returns the total spill frame size in bytes.
pub fn spill(func: &mut MFunction, analysis: &AnalysisResult, k: usize) -> u32 {
    let num_blocks = func.blocks.len();
    let mut slots = SpillSlotAllocator::new();

    // W^exit for each block (VRegs in registers at block exit)
    let mut w_exit: Vec<BTreeSet<VReg>> = vec![BTreeSet::new(); num_blocks];
    // S^exit for each block (VRegs that have been spilled on some path)
    let mut s_exit: Vec<BTreeSet<VReg>> = vec![BTreeSet::new(); num_blocks];

    // Process blocks in reverse post order (layout order for now)
    for bi in 0..num_blocks {
        let w_entry = compute_w_entry(func, analysis, bi, k, &w_exit);
        let s_entry = compute_s_entry(analysis, bi, &s_exit, &w_entry);

        // Insert coupling code on incoming edges
        insert_coupling_code(func, analysis, bi, &w_entry, &w_exit, &mut slots);

        // Run MIN algorithm on this block
        let (new_w_exit, new_s_exit, new_insts) =
            run_min_on_block(func, analysis, bi, w_entry, s_entry, k, &mut slots);

        // Replace block instructions with spill-annotated version
        func.blocks[bi].insts = new_insts;
        w_exit[bi] = new_w_exit;
        s_exit[bi] = new_s_exit;
    }

    slots.total_size() as u32
}

// ────────────────────────────────────────────────────────────────
// W^entry / S^entry computation
// ────────────────────────────────────────────────────────────────

/// Compute W^entry for a block (which VRegs should be in registers at entry).
/// Braun & Hack Section 4.2.
fn compute_w_entry(
    func: &MFunction,
    analysis: &AnalysisResult,
    block_idx: usize,
    k: usize,
    w_exit: &[BTreeSet<VReg>],
) -> BTreeSet<VReg> {
    let preds = &analysis.predecessors[block_idx];

    if preds.is_empty() {
        return BTreeSet::new();
    }

    // all_B = intersection of predecessors' W^exit
    // some_B = union of predecessors' W^exit
    let mut all: Option<BTreeSet<VReg>> = None;
    let mut some: BTreeSet<VReg> = BTreeSet::new();

    for &pred_idx in preds {
        if pred_idx >= block_idx {
            continue; // skip back edges
        }
        let pred_w = &w_exit[pred_idx];
        some = some.union(pred_w).copied().collect();
        all = Some(match all {
            None => pred_w.clone(),
            Some(a) => a.intersection(pred_w).copied().collect(),
        });
    }

    let all = all.unwrap_or_default();
    let mut w_entry = all.clone();

    // Phi defs: phi dst VRegs are defined at block entry and must be in
    // registers. Always include them in W^entry.
    for phi in &func.blocks[block_idx].phis {
        w_entry.insert(phi.dst);
    }

    // Fill remaining slots sorted by next-use distance (closest first)
    if w_entry.len() < k {
        let candidates: BTreeSet<VReg> = some.difference(&w_entry).copied().collect();
        let mut sorted_candidates: Vec<VReg> = candidates.into_iter().collect();
        sorted_candidates.sort_by_key(|v| {
            analysis.entry_distances[block_idx]
                .get(v)
                .copied()
                .unwrap_or(u32::MAX)
        });

        for vreg in sorted_candidates {
            if w_entry.len() >= k {
                break;
            }
            if analysis.entry_distances[block_idx].contains_key(&vreg) {
                w_entry.insert(vreg);
            }
        }
    }

    w_entry
}

/// Compute S^entry for a block.
/// S^entry = (union of predecessors' S^exit) ∩ W^entry
fn compute_s_entry(
    analysis: &AnalysisResult,
    block_idx: usize,
    s_exit: &[BTreeSet<VReg>],
    w_entry: &BTreeSet<VReg>,
) -> BTreeSet<VReg> {
    let mut s_union = BTreeSet::new();
    for &pred_idx in &analysis.predecessors[block_idx] {
        if pred_idx >= block_idx {
            continue;
        }
        s_union = s_union.union(&s_exit[pred_idx]).copied().collect();
    }
    s_union.intersection(w_entry).copied().collect()
}

// ────────────────────────────────────────────────────────────────
// Coupling code
// ────────────────────────────────────────────────────────────────

/// Insert coupling code (reloads) on edges from predecessors to this block.
fn insert_coupling_code(
    func: &mut MFunction,
    analysis: &AnalysisResult,
    block_idx: usize,
    w_entry: &BTreeSet<VReg>,
    w_exit: &[BTreeSet<VReg>],
    slots: &mut SpillSlotAllocator,
) {
    for &pred_idx in &analysis.predecessors[block_idx] {
        if pred_idx >= block_idx {
            continue;
        }

        let pred_w = &w_exit[pred_idx];
        // Exclude phi dsts — they are resolved by emit-time Movs, not spill reloads.
        let phi_dsts: BTreeSet<VReg> = func.blocks[block_idx].phis.iter().map(|p| p.dst).collect();
        let need_reload: Vec<VReg> = w_entry.difference(pred_w).copied()
            .filter(|v| !phi_dsts.contains(v))
            .collect();

        if !need_reload.is_empty() {
            let term_idx = func.blocks[pred_idx].insts.len().saturating_sub(1);
            for vreg in need_reload.iter().rev() {
                let reload = make_reload(*vreg, func, slots);
                func.blocks[pred_idx].insts.insert(term_idx, reload);
            }
        }
    }
}

// ────────────────────────────────────────────────────────────────
// MIN algorithm per block
// ────────────────────────────────────────────────────────────────

/// Run the MIN algorithm on a single basic block.
fn run_min_on_block(
    func: &MFunction,
    analysis: &AnalysisResult,
    block_idx: usize,
    mut w: BTreeSet<VReg>,
    mut s: BTreeSet<VReg>,
    k: usize,
    slots: &mut SpillSlotAllocator,
) -> (BTreeSet<VReg>, BTreeSet<VReg>, Vec<MInst>) {
    let block = &func.blocks[block_idx];
    let mut new_insts: Vec<MInst> = Vec::with_capacity(block.insts.len());

    for (inst_idx, inst) in block.insts.iter().enumerate() {
        let uses = inst.uses();
        let def = inst.def();

        // 1. Ensure all uses are in W. If not, insert reloads.
        let mut reloads_needed: Vec<VReg> = Vec::new();
        for &use_vreg in &uses {
            if !w.contains(&use_vreg) {
                reloads_needed.push(use_vreg);
                w.insert(use_vreg);
                s.insert(use_vreg);
            }
        }

        // If W is too large after adding uses, evict to make room
        limit(&mut w, &mut s, &mut new_insts, func, analysis, block_idx, inst_idx, k, slots);

        // Insert reload instructions
        for vreg in &reloads_needed {
            new_insts.push(make_reload(*vreg, func, slots));
        }

        // 2. Make room for def
        if let Some(def_vreg) = def {
            let needed = w.len() + 1;
            if needed > k {
                limit(
                    &mut w, &mut s, &mut new_insts,
                    func, analysis, block_idx, inst_idx + 1, k, slots,
                );
            }
            w.insert(def_vreg);
        }

        // Emit the original instruction
        new_insts.push(inst.clone());

        // Remove dead values
        let dead: Vec<VReg> = w
            .iter()
            .filter(|&&v| {
                analysis::next_use_at(func, analysis, block_idx, inst_idx + 1, v) == u32::MAX
            })
            .copied()
            .collect();
        for v in dead {
            w.remove(&v);
        }
    }

    (w, s, new_insts)
}

// ────────────────────────────────────────────────────────────────
// Eviction (limit)
// ────────────────────────────────────────────────────────────────

/// Evict variables from W until |W| ≤ m.
fn limit(
    w: &mut BTreeSet<VReg>,
    s: &mut BTreeSet<VReg>,
    new_insts: &mut Vec<MInst>,
    func: &MFunction,
    analysis: &AnalysisResult,
    block_idx: usize,
    inst_idx: usize,
    m: usize,
    slots: &mut SpillSlotAllocator,
) {
    while w.len() > m {
        let victim = choose_victim(w, s, func, analysis, block_idx, inst_idx);

        // Insert spill if not already spilled
        if !s.contains(&victim) {
            if let Some(spill_inst) = make_spill(victim, func, slots) {
                new_insts.push(spill_inst);
            }
            s.insert(victim);
        }

        w.remove(&victim);
    }
}

/// Choose the best variable to evict from W.
///
/// Priority (higher = evict first):
/// 1. Rematerializable (cost 0) → always evict first
/// 2. Store-back-only + aligned (spill_cost 0) → free eviction
/// 3. Furthest next-use / lowest reload cost
fn choose_victim(
    w: &BTreeSet<VReg>,
    s: &BTreeSet<VReg>,
    func: &MFunction,
    analysis: &AnalysisResult,
    block_idx: usize,
    inst_idx: usize,
) -> VReg {
    let mut best_victim = *w.iter().next().unwrap();
    let mut best_priority: (u32, u32) = (0, 0);

    for &vreg in w.iter() {
        let next_use = analysis::next_use_at(func, analysis, block_idx, inst_idx, vreg);
        let desc = func.spill_desc(vreg);

        let eviction_class = match desc {
            Some(d) if matches!(d.kind, SpillKind::Remat { .. }) => 3,    // remat: free
            Some(d) if d.spill_cost == 0 && d.reload_cost <= 1 => 2,      // free spill + cheap reload
            Some(d) if d.spill_cost == 0 => 1,                             // free spill
            _ => 0,                                                         // normal
        };

        // Already spilled → no additional spill cost
        let effective_class = if s.contains(&vreg) {
            eviction_class.max(1)
        } else {
            eviction_class
        };

        let priority = (effective_class, next_use);

        if priority > best_priority {
            best_priority = priority;
            best_victim = vreg;
        }
    }

    best_victim
}
