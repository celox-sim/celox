//! Unified single-pass register allocator.
//!
//! Replaces the separate spilling → assignment pipeline with a single
//! forward walk that simultaneously decides which VRegs to spill AND
//! which physical registers to assign. This eliminates the analysis
//! divergence that required the k-1 hack.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::backend::native::mir::*;

use super::analysis::AnalysisResult;
use super::assignment::{
    ALLOCATABLE_REGS, AssignmentMap, PhysReg, PhysRegSet, RegConstraint, clobbers, is_reg_shift,
    use_constraints,
};

// Re-use spill slot allocator and spill/reload generation from spilling.rs
use super::NUM_REGS;
use super::spilling::{SpillSlotAllocator, make_reload, make_spill};

// ────────────────────────────────────────────────────────────────
// RegFile: bidirectional PhysReg ↔ VReg map
// ────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct RegFile {
    preg_to_vreg: [Option<VReg>; 14],
    vreg_to_preg: HashMap<VReg, PhysReg>,
}

/// Map a PhysReg discriminant (which may have gaps, e.g. RSI=6) to a dense
/// index in 0..14 for the preg_to_vreg array.
const fn preg_dense_index(preg: PhysReg) -> usize {
    match preg {
        PhysReg::RAX => 0,
        PhysReg::RCX => 1,
        PhysReg::RDX => 2,
        PhysReg::RBX => 3,
        PhysReg::RBP => 4,
        PhysReg::RSI => 5,
        PhysReg::RDI => 6,
        PhysReg::R8 => 7,
        PhysReg::R9 => 8,
        PhysReg::R10 => 9,
        PhysReg::R11 => 10,
        PhysReg::R12 => 11,
        PhysReg::R13 => 12,
        PhysReg::R14 => 13,
    }
}

impl RegFile {
    fn new() -> Self {
        Self {
            preg_to_vreg: [None; 14],
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
            !self.vreg_to_preg.contains_key(&vreg),
            "{vreg} is already assigned to {:?} when assigning {preg}",
            self.vreg_to_preg.get(&vreg)
        );
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

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
struct TraceKey {
    event: &'static str,
    reason: &'static str,
    kind: &'static str,
    def: &'static str,
    next: &'static str,
}

#[derive(Default)]
struct TraceCount {
    count: usize,
    stack_mem: usize,
    sim_mem: usize,
    remat: usize,
    no_store: usize,
}

struct RegallocTrace {
    label: String,
    def_opcodes: Vec<Option<&'static str>>,
    rows: BTreeMap<TraceKey, TraceCount>,
}

impl RegallocTrace {
    fn new_if_enabled(label: &str, func: &MFunction) -> Option<Self> {
        std::env::var_os("CELOX_REGALLOC_TRACE").map(|_| {
            let mut def_opcodes = vec![None; func.vregs.count() as usize];
            for block in &func.blocks {
                for inst in &block.insts {
                    if let Some(def) = inst.def() {
                        if let Some(slot) = def_opcodes.get_mut(def.0 as usize) {
                            *slot = Some(inst_opcode(inst));
                        }
                    }
                }
            }
            Self {
                label: label.to_string(),
                def_opcodes,
                rows: BTreeMap::new(),
            }
        })
    }

    fn record_spill(
        &mut self,
        vreg: VReg,
        func: &MFunction,
        reason: &'static str,
        next_use: u32,
        inst: Option<&MInst>,
    ) {
        let mut count = TraceCount {
            count: 1,
            ..TraceCount::default()
        };
        match inst {
            Some(MInst::Store {
                base: BaseReg::StackFrame,
                ..
            }) => count.stack_mem = 1,
            Some(MInst::Store {
                base: BaseReg::SimState,
                ..
            }) => count.sim_mem = 1,
            Some(_) => {}
            None => count.no_store = 1,
        }
        self.add("spill", vreg, func, reason, next_use, count);
    }

    fn record_reload(
        &mut self,
        source: VReg,
        func: &MFunction,
        reason: &'static str,
        next_use: u32,
        inst: &MInst,
    ) {
        let mut count = TraceCount {
            count: 1,
            ..TraceCount::default()
        };
        match inst {
            MInst::Load {
                base: BaseReg::StackFrame,
                ..
            } => count.stack_mem = 1,
            MInst::Load {
                base: BaseReg::SimState,
                ..
            } => count.sim_mem = 1,
            MInst::LoadImm { .. } => count.remat = 1,
            _ => {}
        }
        self.add("reload", source, func, reason, next_use, count);
    }

    fn add(
        &mut self,
        event: &'static str,
        vreg: VReg,
        func: &MFunction,
        reason: &'static str,
        next_use: u32,
        count: TraceCount,
    ) {
        let key = TraceKey {
            event,
            reason,
            kind: spill_kind_name(func.spill_desc(vreg)),
            def: self
                .def_opcodes
                .get(vreg.0 as usize)
                .copied()
                .flatten()
                .unwrap_or("allocator"),
            next: next_use_bucket(next_use),
        };
        let row = self.rows.entry(key).or_default();
        row.count += count.count;
        row.stack_mem += count.stack_mem;
        row.sim_mem += count.sim_mem;
        row.remat += count.remat;
        row.no_store += count.no_store;
    }

    fn log(self) {
        let mut rows = self.rows.into_iter().collect::<Vec<_>>();
        rows.sort_by_key(|(_, count)| std::cmp::Reverse(count.count));
        let total: usize = rows.iter().map(|(_, count)| count.count).sum();
        eprintln!(
            "[regalloc-trace] label={} total_events={} groups={}",
            self.label,
            total,
            rows.len()
        );
        for (rank, (key, count)) in rows.into_iter().take(40).enumerate() {
            eprintln!(
                "[regalloc-trace] label={} rank={} event={} reason={} kind={} def={} next={} count={} stack_mem={} sim_mem={} remat={} no_store={}",
                self.label,
                rank + 1,
                key.event,
                key.reason,
                key.kind,
                key.def,
                key.next,
                count.count,
                count.stack_mem,
                count.sim_mem,
                count.remat,
                count.no_store
            );
        }
    }
}

fn spill_kind_name(desc: Option<&SpillDesc>) -> &'static str {
    match desc {
        Some(SpillDesc {
            kind: SpillKind::Remat { .. },
            ..
        }) => "remat",
        Some(SpillDesc {
            kind: SpillKind::Stack,
            ..
        }) => "stack",
        Some(SpillDesc {
            kind: SpillKind::SimState { .. },
            spill_cost: 0,
            ..
        }) => "sim_state_home",
        Some(SpillDesc {
            kind: SpillKind::SimState { .. },
            ..
        }) => "sim_state_snapshot",
        Some(SpillDesc {
            kind: SpillKind::SimStateAlias { .. },
            spill_cost: 0,
            ..
        }) => "sim_alias_home",
        Some(SpillDesc {
            kind: SpillKind::SimStateAlias { .. },
            ..
        }) => "sim_alias_snapshot",
        None => "missing",
    }
}

fn next_use_bucket(next_use: u32) -> &'static str {
    match next_use {
        u32::MAX => "dead",
        0 => "now",
        1..=4 => "1-4",
        5..=16 => "5-16",
        17..=64 => "17-64",
        65..=256 => "65-256",
        257..=1024 => "257-1024",
        _ => ">1024",
    }
}

fn inst_opcode(inst: &MInst) -> &'static str {
    match inst {
        MInst::Mov { .. } => "mov",
        MInst::LoadImm { .. } => "imm",
        MInst::Load { .. } => "load",
        MInst::LoadPtr { .. } => "load_ptr",
        MInst::LoadIndexed { .. } => "load_indexed",
        MInst::LoadPtrIndexed { .. } => "load_ptr_indexed",
        MInst::Add { .. } => "add",
        MInst::Sub { .. } => "sub",
        MInst::Mul { .. } => "mul",
        MInst::UMulHi { .. } => "umulhi",
        MInst::And { .. } => "and",
        MInst::Or { .. } => "or",
        MInst::Xor { .. } => "xor",
        MInst::Shr { .. } => "shr",
        MInst::Shl { .. } => "shl",
        MInst::Sar { .. } => "sar",
        MInst::AndImm { .. } => "and_imm",
        MInst::OrImm { .. } => "or_imm",
        MInst::ShrImm { .. } => "shr_imm",
        MInst::ShlImm { .. } => "shl_imm",
        MInst::SarImm { .. } => "sar_imm",
        MInst::AddImm { .. } => "add_imm",
        MInst::SubImm { .. } => "sub_imm",
        MInst::Cmp { .. } => "cmp",
        MInst::CmpImm { .. } => "cmp_imm",
        MInst::UDiv { .. } => "udiv",
        MInst::URem { .. } => "urem",
        MInst::BitNot { .. } => "not",
        MInst::Neg { .. } => "neg",
        MInst::Popcnt { .. } => "popcnt",
        MInst::Bsr { .. } => "bsr",
        MInst::BsrOr { .. } => "bsr_or",
        MInst::Pext { .. } => "pext",
        MInst::Pdep { .. } => "pdep",
        MInst::Select { .. } => "select",
        MInst::GuardedCmpSelect { .. } => "guarded_cmp_select",
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
        | MInst::ReturnError { .. } => "none",
    }
}

// ────────────────────────────────────────────────────────────────
// Unified allocator
// ────────────────────────────────────────────────────────────────

#[cfg(test)]
pub fn unified_alloc(func: &mut MFunction, analysis: &AnalysisResult) -> (AssignmentMap, u32) {
    unified_alloc_with_label(func, analysis, "unknown")
}

pub fn unified_alloc_with_label(
    func: &mut MFunction,
    analysis: &AnalysisResult,
    label: &str,
) -> (AssignmentMap, u32) {
    let num_blocks = func.blocks.len();
    let k = NUM_REGS;
    let mut result = AssignmentMap::default();
    let mut slots = SpillSlotAllocator::new();
    let mut trace = RegallocTrace::new_if_enabled(label, func);

    let mut regfile_exit: Vec<RegFile> = vec![RegFile::new(); num_blocks];
    let mut s_exit: Vec<HashSet<VReg>> = vec![HashSet::new(); num_blocks];

    for bi in 0..num_blocks {
        let (mut entry_rf, mut entry_s) =
            compute_entry_regfile(func, analysis, bi, k, &regfile_exit, &s_exit, &result);

        // Insert coupling code
        insert_coupling_code(
            func,
            analysis,
            bi,
            &mut entry_rf,
            &mut entry_s,
            &regfile_exit,
            &mut s_exit,
            &mut slots,
            trace.as_mut(),
        );

        // Record entry assignments
        for (vreg, preg) in &entry_rf.vreg_to_preg {
            result.set(*vreg, *preg);
        }

        let (exit_rf, exit_s, new_insts) = process_block(
            func,
            analysis,
            bi,
            entry_rf,
            entry_s,
            k,
            &mut slots,
            &mut result,
            trace.as_mut(),
        );

        func.blocks[bi].insts = new_insts;
        regfile_exit[bi] = exit_rf;
        s_exit[bi] = exit_s;
    }

    if let Some(trace) = trace {
        trace.log();
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
    s_exit: &[HashSet<VReg>],
    result: &AssignmentMap,
) -> (RegFile, HashSet<VReg>) {
    let preds = &analysis.predecessors[block_idx];
    let mut rf = RegFile::new();
    let forward_preds: Vec<usize> = preds.iter().copied().filter(|&p| p < block_idx).collect();

    if preds.is_empty() {
        return (rf, HashSet::new());
    }

    if forward_preds.len() == 1 {
        let pred_idx = forward_preds[0];
        let pred_rf = &regfile_exit[pred_idx];
        let mut s = s_exit[pred_idx].clone();
        s.retain(|v| analysis.entry_distances[block_idx].contains_key(v));
        let phi_dsts: HashSet<VReg> = func.blocks[block_idx].phis.iter().map(|p| p.dst).collect();

        let mut pred_live: Vec<VReg> = pred_rf
            .vregs()
            .filter(|v| analysis.entry_distances[block_idx].contains_key(v))
            .collect();
        pred_live.sort();
        for vreg in pred_live {
            if rf.contains(vreg) {
                continue;
            }
            if let Some(preg) = pred_rf.get_preg(vreg) {
                if !rf.preg_occupied(preg) {
                    rf.assign(vreg, preg);
                }
            }
        }

        for phi in &func.blocks[block_idx].phis {
            if rf.contains(phi.dst) {
                continue;
            }
            let src = phi
                .sources
                .iter()
                .find_map(|(pred_id, src)| (*pred_id == func.blocks[pred_idx].id).then_some(*src));
            let preferred = src.and_then(|src_vreg| {
                let preg = rf.get_preg(src_vreg)?;
                if analysis.entry_distances[block_idx].contains_key(&src_vreg) {
                    None
                } else {
                    rf.evict(src_vreg);
                    Some(preg)
                }
            });
            let preg = preferred
                .or_else(|| free_entry_reg_for_phi(&mut rf, &mut s, &phi_dsts))
                .expect("no free register for phi dst");
            rf.assign(phi.dst, preg);
        }

        return (rf, s);
    }

    // Collect VRegs available in predecessor exits (in register or already spilled).
    let mut all: Option<HashSet<VReg>> = None;
    let mut some: HashSet<VReg> = HashSet::new();
    let mut spilled_all: Option<HashSet<VReg>> = None;

    for &pred_idx in preds {
        if pred_idx >= block_idx {
            continue;
        } // skip back edges (assumes layout ≈ RPO)
        let pred_vregs: HashSet<VReg> = regfile_exit[pred_idx].vregs().collect();
        let pred_spilled = &s_exit[pred_idx];
        let pred_available: HashSet<VReg> = pred_vregs.union(pred_spilled).copied().collect();
        some = some.union(&pred_available).copied().collect();
        all = Some(match all {
            None => pred_available,
            Some(a) => a.intersection(&pred_available).copied().collect(),
        });
        spilled_all = Some(match spilled_all {
            None => pred_spilled.clone(),
            Some(a) => a.intersection(pred_spilled).copied().collect(),
        });
    }

    let all = all.unwrap_or_default();
    let mut s = spilled_all.unwrap_or_default();
    s.retain(|v| analysis.entry_distances[block_idx].contains_key(v));

    // Start with intersection: VRegs in registers in ALL predecessors.
    // If the preferred PhysReg is already taken, skip — the VReg will be
    // reloaded on demand by process_block when actually used.
    // Sort for deterministic register assignment (HashSet iteration is unordered).
    // Only include VRegs that are actually live at this block's entry.
    // This prevents stale VRegs from predecessor exits (e.g. after EU merge)
    // from occupying registers unnecessarily.
    let mut all_sorted: Vec<VReg> = all
        .iter()
        .copied()
        .filter(|v| analysis.entry_distances[block_idx].contains_key(v))
        .filter(|v| {
            regfile_exit.iter().enumerate().all(|(pred_idx, pred_rf)| {
                if !preds.contains(&pred_idx) || pred_idx >= block_idx {
                    return true;
                }
                pred_rf.contains(*v) || s_exit[pred_idx].contains(v)
            })
        })
        .collect();
    all_sorted.sort();
    for vreg in &all_sorted {
        if rf.occupancy() >= k {
            break;
        }
        if rf.contains(*vreg) {
            continue;
        }
        if let Some(preg) = result.get(*vreg) {
            if !rf.preg_occupied(preg) {
                rf.assign(*vreg, preg);
            }
        }
    }

    // Add phi defs after carrying live-ins. This preserves the original edge
    // semantics for loop joins where the phi source register may also be a
    // live-in on another incoming edge.
    let phi_dsts: HashSet<VReg> = func.blocks[block_idx].phis.iter().map(|p| p.dst).collect();
    for phi in &func.blocks[block_idx].phis {
        if rf.contains(phi.dst) {
            continue;
        }
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
            .or_else(|| free_entry_reg_for_phi(&mut rf, &mut s, &phi_dsts))
            .expect("no free register for phi dst");
        rf.assign(phi.dst, preg);
    }

    (rf, s)
}

fn free_entry_reg_for_phi(
    rf: &mut RegFile,
    s: &mut HashSet<VReg>,
    avoid: &HashSet<VReg>,
) -> Option<PhysReg> {
    if let Some(preg) = rf.find_free_excluding(&PhysRegSet::new()) {
        return Some(preg);
    }

    let mut candidates: Vec<VReg> = rf.vregs().filter(|v| !avoid.contains(v)).collect();
    candidates.sort();
    let victim = *candidates.first()?;
    let preg = rf.get_preg(victim)?;
    rf.evict(victim);
    s.insert(victim);
    Some(preg)
}

// ────────────────────────────────────────────────────────────────
// Coupling code
// ────────────────────────────────────────────────────────────────

fn insert_coupling_code(
    func: &mut MFunction,
    analysis: &AnalysisResult,
    block_idx: usize,
    entry_rf: &mut RegFile,
    entry_s: &mut HashSet<VReg>,
    regfile_exit: &[RegFile],
    s_exit: &mut [HashSet<VReg>],
    slots: &mut SpillSlotAllocator,
    mut trace: Option<&mut RegallocTrace>,
) {
    let phi_dsts: HashSet<VReg> = func.blocks[block_idx].phis.iter().map(|p| p.dst).collect();
    let mut reload_set: HashSet<VReg> = HashSet::new();
    let mut live_in_set: HashSet<VReg> = entry_rf.vregs().collect();
    live_in_set.extend(entry_s.iter().copied());
    let mut live_ins: Vec<VReg> = live_in_set
        .into_iter()
        .filter(|v| !phi_dsts.contains(v) && analysis.entry_distances[block_idx].contains_key(v))
        .collect();
    live_ins.sort();

    for &vreg in &live_ins {
        let mut resident_preds = Vec::new();
        let mut needs_memory = entry_s.contains(&vreg);

        for &pred_idx in &analysis.predecessors[block_idx] {
            if pred_idx >= block_idx {
                // Backedges are processed later, but process_block spills their
                // live-outs to memory before the branch. Force this header to
                // reload the shared live-in representation at entry.
                if analysis.exit_distances[pred_idx].contains_key(&vreg) {
                    needs_memory = true;
                }
                continue;
            }

            let pred_rf = &regfile_exit[pred_idx];
            if pred_rf.contains(vreg) {
                resident_preds.push(pred_idx);
            } else if s_exit[pred_idx].contains(&vreg) {
                needs_memory = true;
            } else {
                debug_assert!(
                    false,
                    "live-in {vreg} for bb{block_idx} is neither resident nor spilled on predecessor bb{pred_idx}"
                );
            }
        }

        if !needs_memory {
            continue;
        }

        reload_set.insert(vreg);
        for pred_idx in resident_preds {
            if s_exit[pred_idx].contains(&vreg) {
                continue;
            }
            if let Some(spill_inst) = make_spill(vreg, func, slots) {
                if let Some(trace) = trace.as_deref_mut() {
                    let next_use = analysis.exit_distances[pred_idx]
                        .get(&vreg)
                        .copied()
                        .unwrap_or(u32::MAX);
                    trace.record_spill(vreg, func, "coupling", next_use, Some(&spill_inst));
                }
                let term_idx = func.blocks[pred_idx].insts.len().saturating_sub(1);
                func.blocks[pred_idx].insts.insert(term_idx, spill_inst);
            } else if let Some(trace) = trace.as_deref_mut() {
                let next_use = analysis.exit_distances[pred_idx]
                    .get(&vreg)
                    .copied()
                    .unwrap_or(u32::MAX);
                trace.record_spill(vreg, func, "coupling", next_use, None);
            }
            s_exit[pred_idx].insert(vreg);
        }
    }

    if reload_set.is_empty() {
        return;
    }

    let mut reloads: Vec<VReg> = reload_set.into_iter().collect();
    reloads.sort();
    for vreg in reloads {
        entry_rf.evict(vreg);
        entry_s.insert(vreg);
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
    mut trace: Option<&mut RegallocTrace>,
) -> (RegFile, HashSet<VReg>, Vec<MInst>) {
    let block = func.blocks[block_idx].clone();
    let mut new_insts: Vec<MInst> = Vec::with_capacity(block.insts.len());
    let mut reload_alias: HashMap<VReg, VReg> = HashMap::new();
    let mut alias_source: HashMap<VReg, VReg> = HashMap::new();

    // Pre-compute next-use table: for each VReg, sorted list of use positions.
    // This replaces O(n) forward scans in next_use_at with O(log n) binary search.
    let mut use_positions: HashMap<VReg, Vec<usize>> = HashMap::new();
    for (i, inst) in block.insts.iter().enumerate() {
        for vreg in inst.uses() {
            use_positions.entry(vreg).or_default().push(i);
        }
        for vreg in edge_phi_sources(func, block.id, inst) {
            use_positions.entry(vreg).or_default().push(i);
        }
    }

    // Pre-compute shift and clobber points for blocked set
    let shift_points: Vec<usize> = block
        .insts
        .iter()
        .enumerate()
        .filter_map(|(idx, inst)| if is_reg_shift(inst) { Some(idx) } else { None })
        .collect();
    let clobber_points = super::assignment::block_clobber_points_for(&block);

    // Pre-compute last-use positions for blocked set
    let mut last_use_in_block: HashMap<VReg, usize> = HashMap::new();
    for (i, inst) in block.insts.iter().enumerate() {
        for vreg in inst.uses() {
            last_use_in_block.insert(vreg, i);
        }
    }
    for &vreg in analysis.exit_distances[block_idx].keys() {
        last_use_in_block
            .entry(vreg)
            .and_modify(|v| *v = (*v).max(block.insts.len()))
            .or_insert(block.insts.len());
    }

    for (inst_idx, inst) in block.insts.iter().enumerate() {
        let mut rewritten_inst = inst.clone();
        let mut uses: Vec<VReg> = inst.uses().into_iter().collect();
        let edge_sources = edge_phi_sources(func, block.id, inst);
        uses.extend(edge_sources.iter().copied());
        let def = inst.def();
        let mut constraints = use_constraints(inst);
        constraints.resize(uses.len(), RegConstraint::Any);
        for use_vreg in &mut uses {
            if let Some(&alias) = reload_alias.get(use_vreg) {
                if rf.contains(alias) {
                    rewritten_inst.rewrite_use(*use_vreg, alias);
                    *use_vreg = alias;
                } else {
                    reload_alias.remove(use_vreg);
                    alias_source.remove(&alias);
                }
            }
        }

        // Step A+B: Ensure all uses are in registers
        let mut pinned: HashSet<VReg> = HashSet::new();

        for (&use_vreg, constraint) in uses.iter().zip(constraints.iter()) {
            if let RegConstraint::Fixed(required_preg) = constraint {
                // Fixed constraint: need use_vreg in required_preg
                if rf.get_preg(use_vreg) == Some(*required_preg) {
                    // Already there
                    pinned.insert(use_vreg);
                } else {
                    // Need to get use_vreg into required_preg
                    // First, free required_preg if occupied
                    if let Some(occupant) = rf.get_vreg(*required_preg) {
                        // Preserve the occupant only if it is used again from
                        // this program point. Values with no next use can be
                        // dropped instead of pointlessly stored to the stack.
                        if fast_next_use(
                            &use_positions,
                            analysis,
                            block_idx,
                            block.insts.len(),
                            inst_idx,
                            occupant,
                        ) != u32::MAX
                        {
                            // emit_spill is idempotent (checks s.contains), so
                            // calling it on an already-spilled VReg is a no-op.
                            let next_use = fast_next_use(
                                &use_positions,
                                analysis,
                                block_idx,
                                block.insts.len(),
                                inst_idx,
                                occupant,
                            );
                            emit_spill(
                                &mut new_insts,
                                occupant,
                                &mut s,
                                func,
                                slots,
                                result,
                                "fixed-clobber",
                                next_use,
                                trace.as_deref_mut(),
                            );
                        }

                        if pinned.contains(&occupant) {
                            // Occupant is used by current instruction — keep it
                            // in a different register for this instruction only.
                            let move_blocked = {
                                let mut s = PhysRegSet::new();
                                s.insert(*required_preg);
                                s
                            };
                            let new_preg = find_or_evict_free(
                                &mut rf,
                                &mut s,
                                &mut new_insts,
                                func,
                                analysis,
                                block_idx,
                                inst_idx,
                                block.insts.len(),
                                &use_positions,
                                slots,
                                &pinned,
                                &move_blocked,
                                &mut reload_alias,
                                &mut alias_source,
                                result,
                                trace.as_deref_mut(),
                            );
                            let fresh_occ = func.vregs.alloc();
                            while func.spill_descs.len() <= fresh_occ.0 as usize {
                                func.spill_descs.push(
                                    func.spill_desc(occupant)
                                        .cloned()
                                        .unwrap_or(SpillDesc::transient()),
                                );
                            }
                            copy_value_width(func, fresh_occ, occupant);
                            new_insts.push(MInst::Mov {
                                dst: fresh_occ,
                                src: occupant,
                            });
                            evict_resident_alias(&mut reload_alias, &mut alias_source, occupant);
                            rf.evict(occupant);
                            rf.assign(fresh_occ, new_preg);
                            result.set(fresh_occ, new_preg);
                            rewritten_inst.rewrite_use(occupant, fresh_occ);
                            replace_resident_alias(
                                &mut reload_alias,
                                &mut alias_source,
                                occupant,
                                fresh_occ,
                            );
                            pinned.remove(&occupant);
                            pinned.insert(fresh_occ);
                        } else {
                            evict_resident_alias(&mut reload_alias, &mut alias_source, occupant);
                            rf.evict(occupant);
                        }
                    }

                    if rf.contains(use_vreg) {
                        // use_vreg is in some other register, create a copy to RCX
                        let fresh = func.vregs.alloc();
                        while func.spill_descs.len() <= fresh.0 as usize {
                            func.spill_descs.push(
                                func.spill_desc(use_vreg)
                                    .cloned()
                                    .unwrap_or(SpillDesc::transient()),
                            );
                        }
                        copy_value_width(func, fresh, use_vreg);
                        new_insts.push(MInst::Mov {
                            dst: fresh,
                            src: use_vreg,
                        });
                        rf.assign(fresh, *required_preg);
                        result.set(fresh, *required_preg);
                        rewritten_inst.rewrite_use(use_vreg, fresh);
                        pinned.insert(fresh);
                    } else {
                        // use_vreg is spilled, reload directly to required_preg
                        let fresh = func.vregs.alloc();
                        while func.spill_descs.len() <= fresh.0 as usize {
                            func.spill_descs.push(
                                func.spill_desc(use_vreg)
                                    .cloned()
                                    .unwrap_or(SpillDesc::transient()),
                            );
                        }
                        copy_value_width(func, fresh, use_vreg);
                        let mut reload = make_reload(use_vreg, func, slots);
                        if let Some(trace) = trace.as_deref_mut() {
                            trace.record_reload(use_vreg, func, "fixed-reload", 0, &reload);
                        }
                        match &mut reload {
                            MInst::LoadImm { dst, .. } | MInst::Load { dst, .. } => *dst = fresh,
                            _ => {}
                        }
                        new_insts.push(reload);
                        rf.assign(fresh, *required_preg);
                        result.set(fresh, *required_preg);
                        rewritten_inst.rewrite_use(use_vreg, fresh);
                        if can_reload_without_new_store(use_vreg, &s, func) {
                            reload_alias.insert(use_vreg, fresh);
                            alias_source.insert(fresh, use_vreg);
                        }
                        pinned.insert(fresh);
                    }
                }
            } else {
                // Any constraint
                if !rf.contains(use_vreg) {
                    // Need to reload
                    let fresh = func.vregs.alloc();
                    while func.spill_descs.len() <= fresh.0 as usize {
                        func.spill_descs.push(
                            func.spill_desc(use_vreg)
                                .cloned()
                                .unwrap_or(SpillDesc::transient()),
                        );
                    }
                    copy_value_width(func, fresh, use_vreg);

                    // Find a free register (respecting shift blocked set)
                    let blocked = compute_blocked_for_vreg(
                        fresh,
                        inst_idx,
                        &last_use_in_block,
                        &shift_points,
                    );
                    let preg = find_or_evict_free(
                        &mut rf,
                        &mut s,
                        &mut new_insts,
                        func,
                        analysis,
                        block_idx,
                        inst_idx,
                        block.insts.len(),
                        &use_positions,
                        slots,
                        &pinned,
                        &blocked,
                        &mut reload_alias,
                        &mut alias_source,
                        result,
                        trace.as_deref_mut(),
                    );

                    let mut reload = make_reload(use_vreg, func, slots);
                    if let Some(trace) = trace.as_deref_mut() {
                        trace.record_reload(use_vreg, func, "reload", 0, &reload);
                    }
                    match &mut reload {
                        MInst::LoadImm { dst, .. } | MInst::Load { dst, .. } => *dst = fresh,
                        _ => {}
                    }
                    new_insts.push(reload);
                    rf.assign(fresh, preg);
                    result.set(fresh, preg);
                    rewritten_inst.rewrite_use(use_vreg, fresh);
                    if can_reload_without_new_store(use_vreg, &s, func) {
                        reload_alias.insert(use_vreg, fresh);
                        alias_source.insert(fresh, use_vreg);
                    }
                    pinned.insert(fresh);
                } else {
                    pinned.insert(use_vreg);
                }
            }
        }

        let mut edge_rewrites = Vec::new();
        for &source in &edge_sources {
            let resident = reload_alias
                .get(&source)
                .copied()
                .filter(|alias| rf.contains(*alias))
                .unwrap_or(source);
            if resident != source {
                edge_rewrites.push((source, resident));
            }
        }
        if !edge_rewrites.is_empty() {
            rewrite_edge_phi_sources(func, block.id, inst, &edge_rewrites);
        }

        // Step C: Evict to pressure ≤ k
        while rf.occupancy() > k {
            evict_farthest(
                &mut rf,
                &mut s,
                &mut new_insts,
                func,
                analysis,
                block_idx,
                inst_idx,
                block.insts.len(),
                &use_positions,
                slots,
                &pinned,
                &PhysRegSet::new(),
                &mut reload_alias,
                &mut alias_source,
                result,
                trace.as_deref_mut(),
            );
        }

        // Step D: Handle def
        if let Some(def_vreg) = def {
            let clobber_extra = clobbers(inst).len().saturating_sub(1);

            // Make room: need occupancy + 1 + clobber_extra ≤ k
            while rf.occupancy() + 1 + clobber_extra > k {
                evict_farthest(
                    &mut rf,
                    &mut s,
                    &mut new_insts,
                    func,
                    analysis,
                    block_idx,
                    inst_idx + 1,
                    block.insts.len(),
                    &use_positions,
                    slots,
                    &pinned,
                    &PhysRegSet::new(),
                    &mut reload_alias,
                    &mut alias_source,
                    result,
                    trace.as_deref_mut(),
                );
            }

            // Pick a PhysReg for the def.
            // Prefer the lhs operand's register (avoids mov in x86 2-operand form).
            let last_use_pos = last_use_in_block
                .get(&def_vreg)
                .copied()
                .unwrap_or(inst_idx);
            let blocked =
                compute_blocked_for_def(inst_idx, last_use_pos, &shift_points, &clobber_points);

            // Coalescing hint: reuse a dying operand's PhysReg for dst.
            // Try all operands; prefer lhs first (avoids mov in x86 2-operand form).
            // Also try non-dying operands if they have a cheap remat path.
            let hint_preg = uses.iter().find_map(|&use_vreg| {
                let preg = rf.get_preg(use_vreg)?;
                let next = fast_next_use(
                    &use_positions,
                    analysis,
                    block_idx,
                    block.insts.len(),
                    inst_idx + 1,
                    use_vreg,
                );
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
                find_or_evict_free(
                    &mut rf,
                    &mut s,
                    &mut new_insts,
                    func,
                    analysis,
                    block_idx,
                    inst_idx + 1,
                    block.insts.len(),
                    &use_positions,
                    slots,
                    &pinned,
                    &blocked,
                    &mut reload_alias,
                    &mut alias_source,
                    result,
                    trace.as_deref_mut(),
                )
            };

            rf.assign(def_vreg, preg);
            result.set(def_vreg, preg);
        }

        let clobbered_residents = collect_clobbered_residents(&rf, inst, def);
        for &vreg in &clobbered_residents {
            let next_use = next_use_for_resident(
                &use_positions,
                &alias_source,
                analysis,
                block_idx,
                block.insts.len(),
                inst_idx + 1,
                vreg,
            );
            if !alias_source.contains_key(&vreg) && next_use != u32::MAX {
                emit_spill(
                    &mut new_insts,
                    vreg,
                    &mut s,
                    func,
                    slots,
                    result,
                    "clobber",
                    next_use,
                    trace.as_deref_mut(),
                );
            }
        }

        // Emit instruction
        new_insts.push(rewritten_inst);

        for vreg in clobbered_residents {
            evict_resident_alias(&mut reload_alias, &mut alias_source, vreg);
            rf.evict(vreg);
        }

        // Step E: Remove dead VRegs
        let block_len = block.insts.len();
        let dead: Vec<VReg> = rf
            .vregs()
            .filter(|&v| {
                next_use_for_resident(
                    &use_positions,
                    &alias_source,
                    analysis,
                    block_idx,
                    block_len,
                    inst_idx + 1,
                    v,
                ) == u32::MAX
            })
            .collect();
        for v in dead {
            evict_resident_alias(&mut reload_alias, &mut alias_source, v);
            rf.evict(v);
        }

        // (Live range splitting placeholder - currently no eager spill)
    }

    let needs_backedge_spills = !analysis.backedge_successors[block_idx].is_empty();
    if needs_backedge_spills {
        let mut spill_live_out: Vec<VReg> = rf
            .vregs()
            .filter(|v| analysis.exit_distances[block_idx].contains_key(v))
            .collect();
        spill_live_out.sort();
        let mut spill_insts = Vec::new();
        for vreg in spill_live_out {
            let next_use = analysis.exit_distances[block_idx]
                .get(&vreg)
                .copied()
                .unwrap_or(u32::MAX);
            emit_spill(
                &mut spill_insts,
                vreg,
                &mut s,
                func,
                slots,
                result,
                "backedge",
                next_use,
                trace.as_deref_mut(),
            );
        }
        if !spill_insts.is_empty() {
            let insert_at = new_insts.len().saturating_sub(1);
            new_insts.splice(insert_at..insert_at, spill_insts);
        }
    }

    (rf, s, new_insts)
}

// ────────────────────────────────────────────────────────────────
// Helpers
// ────────────────────────────────────────────────────────────────

fn edge_phi_sources(func: &MFunction, pred_id: BlockId, inst: &MInst) -> Vec<VReg> {
    let mut sources = Vec::new();
    match inst {
        MInst::Branch {
            true_bb, false_bb, ..
        } => {
            collect_edge_phi_sources(func, pred_id, *true_bb, &mut sources);
            collect_edge_phi_sources(func, pred_id, *false_bb, &mut sources);
        }
        MInst::Jump { target } => {
            collect_edge_phi_sources(func, pred_id, *target, &mut sources);
        }
        _ => {}
    }
    sources
}

fn collect_clobbered_residents(rf: &RegFile, inst: &MInst, def: Option<VReg>) -> Vec<VReg> {
    let mut residents = Vec::new();
    for &preg in clobbers(inst) {
        let Some(vreg) = rf.get_vreg(preg) else {
            continue;
        };
        if Some(vreg) == def {
            continue;
        }
        if !residents.contains(&vreg) {
            residents.push(vreg);
        }
    }
    residents
}

fn collect_edge_phi_sources(
    func: &MFunction,
    pred_id: BlockId,
    target: BlockId,
    sources: &mut Vec<VReg>,
) {
    let Some(block) = func.blocks.iter().find(|block| block.id == target) else {
        return;
    };
    for phi in &block.phis {
        for (source_pred, source) in &phi.sources {
            if *source_pred == pred_id && !sources.contains(source) {
                sources.push(*source);
            }
        }
    }
}

fn rewrite_edge_phi_sources(
    func: &mut MFunction,
    pred_id: BlockId,
    inst: &MInst,
    rewrites: &[(VReg, VReg)],
) {
    match inst {
        MInst::Branch {
            true_bb, false_bb, ..
        } => {
            rewrite_edge_phi_sources_for_target(func, pred_id, *true_bb, rewrites);
            rewrite_edge_phi_sources_for_target(func, pred_id, *false_bb, rewrites);
        }
        MInst::Jump { target } => {
            rewrite_edge_phi_sources_for_target(func, pred_id, *target, rewrites);
        }
        _ => {}
    }
}

fn rewrite_edge_phi_sources_for_target(
    func: &mut MFunction,
    pred_id: BlockId,
    target: BlockId,
    rewrites: &[(VReg, VReg)],
) {
    let Some(block) = func.blocks.iter_mut().find(|block| block.id == target) else {
        return;
    };
    for phi in &mut block.phis {
        for (source_pred, source) in &mut phi.sources {
            if *source_pred != pred_id {
                continue;
            }
            if let Some((_, new_source)) = rewrites.iter().find(|(old, _)| old == source) {
                *source = *new_source;
            }
        }
    }
}

fn emit_spill(
    new_insts: &mut Vec<MInst>,
    vreg: VReg,
    s: &mut HashSet<VReg>,
    func: &MFunction,
    slots: &mut SpillSlotAllocator,
    _result: &mut AssignmentMap,
    reason: &'static str,
    next_use: u32,
    trace: Option<&mut RegallocTrace>,
) {
    if !s.contains(&vreg) {
        let spill_inst = make_spill(vreg, func, slots);
        if let Some(trace) = trace {
            trace.record_spill(vreg, func, reason, next_use, spill_inst.as_ref());
        }
        if let Some(spill_inst) = spill_inst {
            new_insts.push(spill_inst);
        }
        s.insert(vreg);
    }
}

fn copy_value_width(func: &mut MFunction, dst: VReg, src: VReg) {
    let width = func.value_widths.get(src.0 as usize).copied().flatten();
    while func.value_widths.len() <= dst.0 as usize {
        func.value_widths.push(None);
    }
    func.value_widths[dst.0 as usize] = width;
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
    blocked_pregs: &PhysRegSet,
    reload_alias: &mut HashMap<VReg, VReg>,
    alias_source: &mut HashMap<VReg, VReg>,
    result: &mut AssignmentMap,
    trace: Option<&mut RegallocTrace>,
) {
    let candidates = rf
        .vregs()
        .filter(|v| !pinned.contains(v))
        .filter(|v| {
            rf.get_preg(*v)
                .is_none_or(|preg| !blocked_pregs.contains(&preg))
        })
        .collect::<Vec<_>>();
    let candidates = if candidates.is_empty() {
        rf.vregs()
            .filter(|v| !pinned.contains(v))
            .collect::<Vec<_>>()
    } else {
        candidates
    };
    let (victim, victim_next_use) = candidates
        .into_iter()
        .map(|v| {
            let next_use = next_use_for_resident(
                use_positions,
                alias_source,
                analysis,
                block_idx,
                block_len,
                inst_idx,
                v,
            );
            let desc = func.spill_desc(v);
            let eviction_class = match desc {
                Some(d) if matches!(d.kind, SpillKind::Remat { .. }) => 3,
                Some(d) if d.spill_cost == 0 && d.reload_cost <= 1 => 2,
                Some(d) if d.spill_cost == 0 => 1,
                _ => 0,
            };
            let effective_class = if s.contains(&v) {
                eviction_class.max(1)
            } else {
                eviction_class
            };
            let key = (next_use == u32::MAX, effective_class, next_use, v);
            (key, v, next_use)
        })
        .max_by_key(|(key, _, _)| *key)
        .map(|(_, v, next_use)| (v, next_use))
        .expect("no eviction victim: all VRegs in RegFile are pinned");

    if alias_source.contains_key(&victim) {
        evict_resident_alias(reload_alias, alias_source, victim);
    } else if victim_next_use != u32::MAX {
        emit_spill(
            new_insts,
            victim,
            s,
            func,
            slots,
            result,
            "evict",
            victim_next_use,
            trace,
        );
    }
    rf.evict(victim);
}

fn find_or_evict_free(
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
    blocked: &PhysRegSet,
    reload_alias: &mut HashMap<VReg, VReg>,
    alias_source: &mut HashMap<VReg, VReg>,
    result: &mut AssignmentMap,
    mut trace: Option<&mut RegallocTrace>,
) -> PhysReg {
    loop {
        if let Some(preg) = rf.find_free_excluding(blocked) {
            return preg;
        }

        evict_farthest(
            rf,
            s,
            new_insts,
            func,
            analysis,
            block_idx,
            inst_idx,
            block_len,
            use_positions,
            slots,
            pinned,
            blocked,
            reload_alias,
            alias_source,
            result,
            trace.as_deref_mut(),
        );
    }
}

fn next_use_for_resident(
    use_positions: &HashMap<VReg, Vec<usize>>,
    alias_source: &HashMap<VReg, VReg>,
    analysis: &AnalysisResult,
    block_idx: usize,
    block_len: usize,
    inst_idx: usize,
    vreg: VReg,
) -> u32 {
    if let Some(&source) = alias_source.get(&vreg) {
        return fast_next_use_in_block(use_positions, inst_idx, source);
    }
    fast_next_use(
        use_positions,
        analysis,
        block_idx,
        block_len,
        inst_idx,
        vreg,
    )
}

fn fast_next_use_in_block(
    use_positions: &HashMap<VReg, Vec<usize>>,
    inst_idx: usize,
    vreg: VReg,
) -> u32 {
    let Some(positions) = use_positions.get(&vreg) else {
        return u32::MAX;
    };
    match positions.binary_search(&inst_idx) {
        Ok(_) => 0,
        Err(idx) if idx < positions.len() => (positions[idx] - inst_idx) as u32,
        Err(_) => u32::MAX,
    }
}

fn can_reload_without_new_store(vreg: VReg, s: &HashSet<VReg>, func: &MFunction) -> bool {
    if s.contains(&vreg) {
        return true;
    }
    let Some(desc) = func.spill_desc(vreg) else {
        return false;
    };
    match &desc.kind {
        SpillKind::Remat { .. } => true,
        SpillKind::SimState { .. } | SpillKind::SimStateAlias { .. } => desc.spill_cost == 0,
        SpillKind::Stack => false,
    }
}

fn evict_resident_alias(
    reload_alias: &mut HashMap<VReg, VReg>,
    alias_source: &mut HashMap<VReg, VReg>,
    resident: VReg,
) {
    if let Some(source) = alias_source.remove(&resident) {
        if reload_alias.get(&source) == Some(&resident) {
            reload_alias.remove(&source);
        }
    }
}

fn replace_resident_alias(
    reload_alias: &mut HashMap<VReg, VReg>,
    alias_source: &mut HashMap<VReg, VReg>,
    old_resident: VReg,
    new_resident: VReg,
) {
    if let Some(source) = alias_source.remove(&old_resident) {
        if reload_alias.get(&source) == Some(&old_resident) {
            reload_alias.insert(source, new_resident);
            alias_source.insert(new_resident, source);
        }
    }
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
            for &r in regs {
                blocked.insert(r);
            }
        }
    }
    for &pos in shift_points {
        if pos >= inst_idx && pos <= last_use_pos {
            blocked.insert(PhysReg::RCX);
        }
    }
    blocked
}
