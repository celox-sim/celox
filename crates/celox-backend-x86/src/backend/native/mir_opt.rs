//! MIR optimization passes: run between ISel and regalloc.
//!
//! Pass implementations are grouped by responsibility in child modules;
//! `pipeline` owns pass ordering, and this module provides shared CFG and
//! register utilities plus the remaining lightweight passes.
//!
//! - Copy propagation: `v2 = mov v1` → replace all uses of v2 with v1
//! - Dead code elimination: remove instructions whose defs are unused

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::memory_effect;
use super::mir::*;
use super::regalloc::assignment::{AssignmentMap, PhysReg, clobbers};
use crate::{HashMap, HashSet};

mod algebraic;
mod bit_copy;
mod bit_permutation;
mod bitmap_worklist;
mod boolean;
mod branch_merge;
mod circular_scan;
mod constants;
mod counted_loop;
mod dead_code;
mod exclusive_loop;
mod gvn;
mod gvn_liveness;
mod immediates;
mod known_bits;
mod loop_guard;
mod masks;
mod memory_forward;
mod memory_peephole;
mod pipeline;
mod post_regalloc;
mod selection;

use algebraic::algebraic_simplify;
use bit_copy::{
    eliminate_redundant_or_terms, fold_reconstructed_bit_partitions, fold_relocated_bit_copy_groups,
};
use bit_permutation::{
    fold_add_chain_to_popcnt, fold_bit_toggle_insert, fold_byte_enable_spread_to_pdep,
    fold_deposit_chain_to_pdep, fold_extract_chain_to_pext, fold_xor_chain_to_pext,
};
use constants::{constant_dedup, constant_fold};
use gvn::global_gvn;
use immediates::{
    and_imm_ok, fold_imm_use, fold_late_serial_and_immediates, lower_to_imm_forms,
    sign_extended_i32,
};
use masks::{
    ValueDefinition, global_possible_one_bits, machine_width_mask, mask_width, possible_bits,
    redundant_mask_eliminate,
};
pub(super) use memory_forward::eliminate_redundant_local_stores;
use memory_forward::{
    alloc_transient_vreg, forward_live_partial_stores, forward_local_store_loads,
    promote_partial_store_round_trips,
};
use memory_peephole::{fold_contiguous_load_packs, fold_contiguous_memory_copies};
pub(crate) use post_regalloc::post_regalloc_direct_load_cse;
pub use post_regalloc::{post_regalloc_cleanup, post_regalloc_peephole};
use selection::{fuse_compare_selects, sink_selected_indexed_loads};

#[cfg(test)]
mod memory_forward_tests;
#[cfg(any(target_arch = "x86_64", feature = "cross-codegen"))]
pub(crate) use pipeline::optimize_baseline;
pub use pipeline::{optimize, optimize_with_diagnostics};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VRegCompaction {
    pub before: u32,
    pub after: u32,
}

/// Remove holes left in the VReg namespace by MIR optimization.
///
/// ISel can create many temporary machine values which disappear during DCE.
/// Register allocation indexes several analysis tables by raw VReg ID, so
/// retaining those dead IDs makes its memory cost proportional to historical
/// ISel output rather than to the optimized MIR it actually allocates.
///
/// The remap is stable and monotonic: surviving old IDs are assigned dense new
/// IDs in ascending order. This preserves deterministic MIR output and lets us
/// rewrite use operands in place without a temporary VReg namespace.
pub(crate) fn compact_vregs(func: &mut MFunction) -> VRegCompaction {
    let before = func.vregs.count();
    let old_count = before as usize;
    assert_eq!(
        func.spill_descs.len(),
        old_count,
        "MIR VReg compaction requires one spill descriptor per VReg"
    );

    let mut referenced = vec![false; old_count];
    let mark = |referenced: &mut [bool], value: VReg| {
        let slot = referenced
            .get_mut(value.0 as usize)
            .expect("MIR VReg reference must be inside the allocated namespace");
        *slot = true;
    };
    for block in &func.blocks {
        for phi in &block.phis {
            mark(&mut referenced, phi.dst);
            for &(_, source) in &phi.sources {
                mark(&mut referenced, source);
            }
        }
        for inst in &block.insts {
            if let Some(destination) = inst.def() {
                mark(&mut referenced, destination);
            }
            for source in inst.uses() {
                mark(&mut referenced, source);
            }
        }
    }

    let mut old_to_new = vec![u32::MAX; old_count];
    let mut after = 0u32;
    for (old, is_referenced) in referenced.iter().copied().enumerate() {
        if is_referenced {
            old_to_new[old] = after;
            after = after.checked_add(1).expect("dense VReg count overflow");
        }
    }
    if after == before {
        return VRegCompaction { before, after };
    }

    let remap = |value: VReg| {
        let mapped = old_to_new[value.0 as usize];
        assert_ne!(
            mapped,
            u32::MAX,
            "executable MIR reference must survive VReg compaction"
        );
        VReg(mapped)
    };

    for block in &mut func.blocks {
        for phi in &mut block.phis {
            phi.dst = remap(phi.dst);
            for (_, source) in &mut phi.sources {
                *source = remap(*source);
            }
        }
        for inst in &mut block.insts {
            // The dense mapping never increases an ID. Rewriting original
            // operands from low to high therefore cannot revisit a newly
            // assigned ID as if it were an old operand.
            let mut sources = inst.uses().into_iter().collect::<Vec<_>>();
            sources.sort_unstable();
            sources.dedup();
            for source in sources {
                inst.rewrite_use(source, remap(source));
            }
            if let Some(destination) = inst.def_mut() {
                *destination = remap(*destination);
            }
        }
    }

    let mut spill_descs = Vec::with_capacity(after as usize);
    for (old, is_referenced) in referenced.into_iter().enumerate() {
        if !is_referenced {
            continue;
        }
        let mut descriptor = func.spill_descs[old].clone();
        if let Some(mut insert) = descriptor.state_insert {
            let mapped = old_to_new[insert.value.0 as usize];
            if mapped == u32::MAX {
                // Provenance naming an instruction removed by MIR DCE cannot
                // provide an executable reload recipe anymore.
                descriptor.state_insert = None;
            } else {
                insert.value = VReg(mapped);
                descriptor.state_insert = Some(insert);
            }
        }
        spill_descs.push(descriptor);
    }

    let mut allocator = VRegAllocator::new();
    for _ in 0..after {
        allocator.alloc();
    }
    func.vregs = allocator;
    func.spill_descs = spill_descs;
    VRegCompaction { before, after }
}

/// Select x86 direct-memory updates for exact local load/ALU/store chains.
///
/// The SSA temporaries must have no other users. Keeping the match local
/// also fixes the memory observation point: there is no intervening effect to
/// justify with instruction reordering. Wider immediates are truncated to the
/// access width, exactly as the original final store truncates the register.
pub(crate) fn fold_direct_immediate_stores(func: &mut MFunction) -> usize {
    let mut use_counts = HashMap::<VReg, usize>::default();
    for block in &func.blocks {
        for phi in &block.phis {
            for (_, source) in &phi.sources {
                *use_counts.entry(*source).or_default() += 1;
            }
        }
        for inst in &block.insts {
            for source in inst.uses() {
                *use_counts.entry(source).or_default() += 1;
            }
        }
    }

    let mut folded = 0usize;
    for block in &mut func.blocks {
        let original = std::mem::take(&mut block.insts);
        let mut rewritten = Vec::with_capacity(original.len());
        let mut index = 0usize;
        while index < original.len() {
            let replacement = original
                .get(index..index.saturating_add(3))
                .and_then(|window| {
                    let [
                        MInst::Load {
                            dst: loaded,
                            base: load_base,
                            offset: load_offset,
                            size: load_size,
                        },
                        update,
                        MInst::Store {
                            base: store_base,
                            offset: store_offset,
                            src: stored,
                            size: store_size,
                        },
                    ] = window
                    else {
                        return None;
                    };
                    if load_base != store_base
                        || load_offset != store_offset
                        || load_size != store_size
                        || use_counts.get(loaded).copied() != Some(1)
                        || use_counts.get(stored).copied() != Some(1)
                    {
                        return None;
                    }
                    let (result, source, immediate, is_or, word32) = match update {
                        MInst::AndImm { dst, src, imm } => (*dst, *src, *imm, false, false),
                        MInst::AndImm32 { dst, src, imm } => {
                            (*dst, *src, u64::from(*imm), false, true)
                        }
                        MInst::OrImm { dst, src, imm } => (*dst, *src, *imm, true, false),
                        _ => return None,
                    };
                    if source != *loaded || result != *stored {
                        return None;
                    }
                    let width_mask = match load_size {
                        OpSize::S8 => u64::from(u8::MAX),
                        OpSize::S16 => u64::from(u16::MAX),
                        OpSize::S32 => u64::from(u32::MAX),
                        OpSize::S64 => u64::MAX,
                    };
                    let immediate = immediate & width_mask;
                    // x86-64 encodes a qword ALU immediate by sign-extending
                    // imm32. AndImm32 additionally promises a zero-extended
                    // 32-bit result, so its qword form is equivalent only
                    // while that sign extension also has zero upper bits.
                    if *load_size == OpSize::S64
                        && if word32 {
                            immediate > i32::MAX as u64
                        } else {
                            sign_extended_i32(immediate).is_none()
                        }
                    {
                        return None;
                    }
                    Some(if is_or {
                        MInst::OrStoreImm {
                            base: *load_base,
                            offset: *load_offset,
                            size: *load_size,
                            imm: immediate,
                        }
                    } else {
                        MInst::AndStoreImm {
                            base: *load_base,
                            offset: *load_offset,
                            size: *load_size,
                            imm: immediate,
                        }
                    })
                });
            if let Some(replacement) = replacement {
                rewritten.push(replacement);
                folded += 1;
                index += 3;
            } else {
                rewritten.push(original[index].clone());
                index += 1;
            }
        }
        block.insts = rewritten;
    }
    folded
}

/// Fold single-use byte displacements and power-of-two indexes into x86
/// memory operands. The offset must remain an encodable signed displacement;
/// scaling preserves the original modulo-64-bit address arithmetic. The alias
/// range still describes the entire original object.
fn fold_indexed_load_addresses(func: &mut MFunction) {
    let mut use_counts = HashMap::<VReg, usize>::default();
    for block in &func.blocks {
        for phi in &block.phis {
            for (_, source) in &phi.sources {
                *use_counts.entry(*source).or_default() += 1;
            }
        }
        for inst in &block.insts {
            for source in inst.uses() {
                *use_counts.entry(source).or_default() += 1;
            }
        }
    }

    for block in &mut func.blocks {
        let mut shifts = HashMap::<VReg, (VReg, u8)>::default();
        let mut displacements = HashMap::<VReg, (VReg, i32)>::default();
        for inst in &mut block.insts {
            if let MInst::AddImm { dst, src, imm } = inst {
                displacements.insert(*dst, (*src, *imm));
                continue;
            }
            if let MInst::ShlImm { dst, src, imm } = inst {
                if (1..=3).contains(imm) {
                    shifts.insert(*dst, (*src, *imm));
                }
                continue;
            }
            let MInst::LoadIndexed {
                offset,
                index,
                scale,
                ..
            } = inst
            else {
                continue;
            };
            if use_counts.get(index).copied() == Some(1)
                && let Some(&(source, displacement)) = displacements.get(index)
                && let Some(displacement) = displacement.checked_mul(i32::from(*scale))
                && let Some(adjusted) = offset.checked_add(displacement)
            {
                *index = source;
                *offset = adjusted;
            }
            if *scale != 1 || use_counts.get(index).copied() != Some(1) {
                continue;
            }
            let Some((unscaled, shift)) = shifts.get(index).copied() else {
                continue;
            };
            *index = unscaled;
            *scale = 1 << shift;
        }
    }
}

/// Keep allocation metadata in sync with constants created by MIR rewrites.
///
/// ISel attaches `Remat` descriptors to constants it creates directly, but
/// constant folding and algebraic simplification can turn a transient value
/// into a `LoadImm`.  Leaving the old `Stack` descriptor on that destination
/// makes register allocation emit a real spill for a value that should simply
/// be reconstructed at its use.
fn refresh_constant_spill_descs(func: &mut MFunction) {
    for block in &func.blocks {
        for inst in &block.insts {
            if let MInst::LoadImm { dst, value } = inst {
                func.spill_descs[dst.0 as usize] = SpillDesc::remat(*value);
            }
        }
    }
}

/// Fold comparisons whose result follows from a conservative unsigned upper
/// bound. Legalization expresses an x86 variable-shift guard as
/// `count < 64 ? raw_shift : 0`; bit-offset lowering commonly defines count as
/// `offset & 7`, so retaining that guard is unnecessary. Bounds here are
/// intentionally limited to operations that cannot underestimate a value.
fn fold_proven_comparisons(func: &mut MFunction) {
    let mut defs = HashMap::default();
    for block in &func.blocks {
        for inst in &block.insts {
            if let Some(dst) = inst.def() {
                defs.insert(dst, inst);
            }
        }
    }
    // Prove against the original definitions, then apply only the rewrites.
    // Cloning every instruction here multiplies memory for large SMP designs.
    let mut replacements = Vec::new();
    let mut upper_bounds = HashMap::default();
    for (block_index, block) in func.blocks.iter().enumerate() {
        for (inst_index, inst) in block.insts.iter().enumerate() {
            let replacement = match inst {
                MInst::CmpSelect {
                    dst,
                    lhs,
                    rhs,
                    kind: CmpKind::LtU,
                    true_val,
                    ..
                } if matches!(defs.get(rhs), Some(MInst::LoadImm { value, .. }) if *value > 0
                    && unsigned_upper_bound(
                        *lhs,
                        &defs,
                        &mut upper_bounds,
                        &mut HashSet::default(),
                    )
                    .is_some_and(|bound| bound < *value)) =>
                {
                    Some(MInst::Mov {
                        dst: *dst,
                        src: *true_val,
                    })
                }
                MInst::CmpImmSelect {
                    dst,
                    lhs,
                    imm,
                    kind: CmpKind::LtU,
                    true_val,
                    ..
                } if *imm > 0
                    && unsigned_upper_bound(
                        *lhs,
                        &defs,
                        &mut upper_bounds,
                        &mut HashSet::default(),
                    )
                    .is_some_and(|bound| bound < *imm as u64) =>
                {
                    Some(MInst::Mov {
                        dst: *dst,
                        src: *true_val,
                    })
                }
                _ => None,
            };
            if let Some(replacement) = replacement {
                replacements.push((block_index, inst_index, replacement));
            }
        }
    }
    drop(defs);
    for (block, inst, replacement) in replacements {
        func.blocks[block].insts[inst] = replacement;
    }
}

/// Remove redundant normalization of values already known to be boolean, and
/// fold an exclusively consumed `cmp.eq (cmp ...), 0` by inverting the inner
/// comparison. These forms become visible especially after immediate lowering;
/// eliminating them here avoids materializing an intermediate condition.
fn fold_boolean_normalizations(func: &mut MFunction) {
    let mut defs = HashMap::default();
    let mut use_counts = HashMap::<VReg, usize>::default();
    for block in &func.blocks {
        for phi in &block.phis {
            for &(_, source) in &phi.sources {
                *use_counts.entry(source).or_default() += 1;
            }
        }
        for inst in &block.insts {
            if let Some(dst) = inst.def() {
                defs.insert(dst, inst);
            }
            for source in inst.uses() {
                *use_counts.entry(source).or_default() += 1;
            }
        }
    }
    // Prove against the original definitions, then apply only the rewrites.
    // Cloning every instruction here multiplies memory for large SMP designs.
    let mut replacements = Vec::new();
    let mut upper_bounds = HashMap::default();
    for (block_index, block) in func.blocks.iter().enumerate() {
        for (inst_index, inst) in block.insts.iter().enumerate() {
            let replacement = match inst {
                MInst::CmpImm {
                    dst,
                    lhs,
                    imm: 0,
                    kind: CmpKind::Ne,
                } if unsigned_upper_bound(
                    *lhs,
                    &defs,
                    &mut upper_bounds,
                    &mut HashSet::default(),
                )
                .is_some_and(|bound| bound <= 1) =>
                {
                    Some(MInst::Mov {
                        dst: *dst,
                        src: *lhs,
                    })
                }
                MInst::Cmp {
                    dst,
                    lhs,
                    rhs,
                    kind: CmpKind::Ne,
                } if unsigned_upper_bound(
                    *rhs,
                    &defs,
                    &mut upper_bounds,
                    &mut HashSet::default(),
                ) == Some(0)
                    && unsigned_upper_bound(
                        *lhs,
                        &defs,
                        &mut upper_bounds,
                        &mut HashSet::default(),
                    )
                    .is_some_and(|bound| bound <= 1) =>
                {
                    Some(MInst::Mov {
                        dst: *dst,
                        src: *lhs,
                    })
                }
                MInst::CmpImm {
                    dst,
                    lhs,
                    imm: 0,
                    kind: CmpKind::Eq,
                } if use_counts.get(lhs).copied() == Some(1) => match defs.get(lhs) {
                    Some(MInst::Cmp { lhs, rhs, kind, .. }) => Some(MInst::Cmp {
                        dst: *dst,
                        lhs: *lhs,
                        rhs: *rhs,
                        kind: invert_compare_kind(*kind),
                    }),
                    Some(MInst::CmpImm { lhs, imm, kind, .. }) => Some(MInst::CmpImm {
                        dst: *dst,
                        lhs: *lhs,
                        imm: *imm,
                        kind: invert_compare_kind(*kind),
                    }),
                    _ => None,
                },
                _ => None,
            };
            if let Some(replacement) = replacement {
                replacements.push((block_index, inst_index, replacement));
            }
        }
    }
    drop(defs);
    for (block, inst, replacement) in replacements {
        func.blocks[block].insts[inst] = replacement;
    }
}

fn invert_compare_kind(kind: CmpKind) -> CmpKind {
    match kind {
        CmpKind::Eq => CmpKind::Ne,
        CmpKind::Ne => CmpKind::Eq,
        CmpKind::LtU => CmpKind::GeU,
        CmpKind::LtS => CmpKind::GeS,
        CmpKind::LeU => CmpKind::GtU,
        CmpKind::LeS => CmpKind::GtS,
        CmpKind::GtU => CmpKind::LeU,
        CmpKind::GtS => CmpKind::LeS,
        CmpKind::GeU => CmpKind::LtU,
        CmpKind::GeS => CmpKind::LtS,
    }
}

/// Read-only view of the defining instruction of each VReg, so bound proofs
/// can consume borrowed maps without cloning definitions.
trait DefLookup {
    fn def_inst(&self, reg: VReg) -> Option<&MInst>;
}

impl DefLookup for HashMap<VReg, MInst> {
    fn def_inst(&self, reg: VReg) -> Option<&MInst> {
        self.get(&reg)
    }
}

impl DefLookup for HashMap<VReg, &MInst> {
    fn def_inst(&self, reg: VReg) -> Option<&MInst> {
        self.get(&reg).copied()
    }
}

fn unsigned_upper_bound<L: DefLookup>(
    reg: VReg,
    defs: &L,
    memo: &mut HashMap<VReg, Option<u64>>,
    visiting: &mut HashSet<VReg>,
) -> Option<u64> {
    if let Some(bound) = memo.get(&reg) {
        return *bound;
    }
    if !visiting.insert(reg) {
        return None;
    }
    let bound = match defs.def_inst(reg)? {
        MInst::LoadImm { value, .. } => Some(*value),
        MInst::Load { size, .. } | MInst::LoadIndexed { size, .. } => Some(match size {
            OpSize::S8 => u8::MAX as u64,
            OpSize::S16 => u16::MAX as u64,
            OpSize::S32 => u32::MAX as u64,
            OpSize::S64 => u64::MAX,
        }),
        MInst::Mov { src, .. } => unsigned_upper_bound(*src, defs, memo, visiting),
        MInst::Mov32 { .. }
        | MInst::Add32 { .. }
        | MInst::Sub32 { .. }
        | MInst::Mul32 { .. }
        | MInst::MulImm32 { .. }
        | MInst::And32 { .. }
        | MInst::Or32 { .. }
        | MInst::Xor32 { .. } => Some(u32::MAX as u64),
        MInst::AndImm32 { imm, .. } => Some(u64::from(*imm)),
        MInst::AndImm { src, imm, .. } => Some(
            unsigned_upper_bound(*src, defs, memo, visiting)
                .unwrap_or(u64::MAX)
                .min(*imm),
        ),
        MInst::And { lhs, rhs, .. } => {
            match (
                unsigned_upper_bound(*lhs, defs, memo, visiting),
                unsigned_upper_bound(*rhs, defs, memo, visiting),
            ) {
                (Some(lhs), Some(rhs)) => Some(lhs.min(rhs)),
                (Some(bound), None) | (None, Some(bound)) => Some(bound),
                (None, None) => None,
            }
        }
        MInst::Or { lhs, rhs, .. } | MInst::Xor { lhs, rhs, .. } => {
            match (
                unsigned_upper_bound(*lhs, defs, memo, visiting),
                unsigned_upper_bound(*rhs, defs, memo, visiting),
            ) {
                (Some(lhs), Some(rhs)) if lhs <= 1 && rhs <= 1 => Some(1),
                _ => None,
            }
        }
        MInst::ShrImm { src, imm, .. } => {
            unsigned_upper_bound(*src, defs, memo, visiting).map(|bound| bound >> *imm)
        }
        MInst::Cmp { .. } | MInst::CmpImm { .. } => Some(1),
        MInst::Select {
            true_val,
            false_val,
            ..
        }
        | MInst::CmpSelect {
            true_val,
            false_val,
            ..
        }
        | MInst::CmpImmSelect {
            true_val,
            false_val,
            ..
        }
        | MInst::GuardedCmpSelect {
            true_val,
            false_val,
            ..
        } => match (
            unsigned_upper_bound(*true_val, defs, memo, visiting),
            unsigned_upper_bound(*false_val, defs, memo, visiting),
        ) {
            (Some(lhs), Some(rhs)) => Some(lhs.max(rhs)),
            _ => None,
        },
        _ => None,
    };
    visiting.remove(&reg);
    memo.insert(reg, bound);
    bound
}

/// Replace selects whose result is independent of their predicate with a copy.
///
/// This is kept separate from emitter-side physical-register coalescing: doing
/// it on MIR lets DCE remove the compare, guard, and their complete producer
/// graphs before they create allocation pressure.
fn simplify_equal_value_selects(func: &mut MFunction) {
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            let replacement = match inst {
                MInst::Select {
                    dst,
                    true_val,
                    false_val,
                    ..
                }
                | MInst::CmpSelect {
                    dst,
                    true_val,
                    false_val,
                    ..
                }
                | MInst::CmpImmSelect {
                    dst,
                    true_val,
                    false_val,
                    ..
                }
                | MInst::GuardedCmpSelect {
                    dst,
                    true_val,
                    false_val,
                    ..
                } if true_val == false_val => Some(MInst::Mov {
                    dst: *dst,
                    src: *true_val,
                }),
                _ => None,
            };
            if let Some(replacement) = replacement {
                *inst = replacement;
            }
        }
    }
}

/// Compute immediate dominators using the iterative algorithm.
/// Returns idom[i] = Some(j) where j immediately dominates i, or None for entry.
fn compute_dominators(n: usize, preds: &[Vec<usize>], succs: &[Vec<usize>]) -> Vec<Option<usize>> {
    // Cooper-Harvey-Kennedy immediate dominators. `intersect` requires reverse
    // postorder numbers; MFunction block storage order is not a CFG ordering.
    let mut visited = vec![false; n];
    let mut postorder = Vec::with_capacity(n);
    let mut stack = vec![(0usize, 0usize)];
    visited[0] = true;
    while let Some((node, next_successor)) = stack.last_mut() {
        if *next_successor < succs[*node].len() {
            let successor = succs[*node][*next_successor];
            *next_successor += 1;
            if !visited[successor] {
                visited[successor] = true;
                stack.push((successor, 0));
            }
        } else {
            postorder.push(*node);
            stack.pop();
        }
    }
    postorder.reverse();
    let rpo = postorder;
    let mut rpo_number = vec![usize::MAX; n];
    for (number, &block) in rpo.iter().enumerate() {
        rpo_number[block] = number;
    }

    let mut idom: Vec<Option<usize>> = vec![None; n];
    idom[0] = Some(0); // Entry dominates itself (sentinel)

    let mut changed = true;
    while changed {
        changed = false;
        for &b in rpo.iter().skip(1) {
            // Find first processed predecessor
            let mut new_idom: Option<usize> = None;
            for &p in &preds[b] {
                if idom[p].is_some() {
                    new_idom = Some(match new_idom {
                        None => p,
                        Some(cur) => intersect_dom(cur, p, &idom, &rpo_number),
                    });
                }
            }
            if new_idom != idom[b] {
                idom[b] = new_idom;
                changed = true;
            }
        }
    }

    // Fix entry: idom[0] = None (no dominator)
    idom[0] = None;
    idom
}

fn intersect_dom(
    mut a: usize,
    mut b: usize,
    idom: &[Option<usize>],
    rpo_number: &[usize],
) -> usize {
    while a != b {
        while rpo_number[a] > rpo_number[b] {
            a = idom[a].unwrap_or(0);
        }
        while rpo_number[b] > rpo_number[a] {
            b = idom[b].unwrap_or(0);
        }
    }
    a
}

/// Select flag-consuming branch forms before allocation.
///
/// A compare or direct load whose only use is a branch has no independently
/// observable SSA result. Keeping that result
/// until emission invents a live range and can make the allocator spill around
/// a value which the machine code never materializes.
///
/// Direct-memory predicates are selected by a separate late pass after
/// StateSSA forwarding. Register comparisons are selected exactly once at the
/// register-allocation boundary, before pressure scheduling.
/// An immediate comparison can move past intervening instructions: carrying
/// its one SSA input instead of its boolean result does not add register
/// pressure. Two-register comparisons and memory reads remain adjacent to
/// avoid extending two input live ranges or moving a read across a write.
pub(crate) fn fold_register_branch_predicates(func: &mut MFunction) -> usize {
    fold_branch_predicates(func, BranchPredicateClass::Register)
}

pub(crate) fn fold_memory_branch_predicates(func: &mut MFunction) -> usize {
    fold_branch_predicates(func, BranchPredicateClass::Memory)
}

#[derive(Clone, Copy)]
enum BranchPredicateClass {
    Register,
    Memory,
}

fn fold_branch_predicates(func: &mut MFunction, class: BranchPredicateClass) -> usize {
    // Only distinguish unused, single-use, and shared values.
    let mut use_counts = vec![0u8; func.vregs.count() as usize];
    for block in &func.blocks {
        for phi in &block.phis {
            for &(_, source) in &phi.sources {
                let count = &mut use_counts[source.0 as usize];
                *count = count.saturating_add(1);
            }
        }
        for instruction in &block.insts {
            for source in instruction.uses() {
                let count = &mut use_counts[source.0 as usize];
                *count = count.saturating_add(1);
            }
        }
    }

    let mut folded = 0usize;
    for block in &mut func.blocks {
        if block.insts.len() < 2 {
            continue;
        }
        let MInst::Branch {
            cond,
            true_bb,
            false_bb,
        } = block.insts[block.insts.len() - 1].clone()
        else {
            continue;
        };
        if use_counts[cond.0 as usize] != 1 {
            continue;
        }

        let adjacent = block.insts.len() - 2;
        let definition = if matches!(class, BranchPredicateClass::Register) {
            let Some(index) = block.insts[..=adjacent]
                .iter()
                .rposition(|inst| inst.def() == Some(cond))
            else {
                continue;
            };
            index
        } else {
            adjacent
        };
        let predicate = match block.insts[definition].clone() {
            MInst::Cmp {
                dst,
                lhs,
                rhs,
                kind,
            } if matches!(class, BranchPredicateClass::Register)
                && definition == adjacent
                && dst == cond =>
            {
                BranchPredicate::Compare { lhs, rhs, kind }
            }
            MInst::CmpImm {
                dst,
                lhs,
                imm,
                kind,
            } if matches!(class, BranchPredicateClass::Register) && dst == cond => {
                BranchPredicate::CompareImm { lhs, imm, kind }
            }
            MInst::Load {
                dst,
                base,
                offset,
                size,
            } if matches!(class, BranchPredicateClass::Memory) && dst == cond => {
                BranchPredicate::MemoryNonZero { base, offset, size }
            }
            _ => continue,
        };
        block.insts.pop();
        block.insts.remove(definition);
        block.insts.push(MInst::BranchPred {
            predicate,
            true_bb,
            false_bb,
        });
        folded += 1;
    }
    folded
}

enum Simplification {
    Mov(VReg, VReg),
    Mov32(VReg, VReg),
    Const(VReg, u64),
    Shl(VReg, VReg, u8),
    OrImm(VReg, VReg, u64),
    AndImm(VReg, VReg, u64),
    AndImm32(VReg, VReg, u32),
}

fn const32(consts: &HashMap<VReg, u64>, value: VReg) -> Option<u32> {
    consts.get(&value).map(|&value| value as u32)
}

fn try_simplify_mul(
    dst: VReg,
    lhs: VReg,
    rhs: VReg,
    consts: &HashMap<VReg, u64>,
) -> Option<Simplification> {
    // Check each operand for constant
    for &(val_vreg, const_vreg) in &[(lhs, rhs), (rhs, lhs)] {
        if let Some(&c) = consts.get(&const_vreg) {
            if c == 0 {
                return Some(Simplification::Const(dst, 0));
            }
            if c == 1 {
                return Some(Simplification::Mov(dst, val_vreg));
            }
            // Power of 2: mul → shl
            if c.is_power_of_two() {
                let shift = c.trailing_zeros() as u8;
                return Some(Simplification::Shl(dst, val_vreg, shift));
            }
        }
    }
    None
}

// ────────────────────────────────────────────────────────────────
// CFG simplification
// ────────────────────────────────────────────────────────────────

/// Simplify the control flow graph:
/// - Thread jumps through empty blocks (jmp-only blocks)
/// - Fold branch targets through jump chains
fn simplify_cfg(func: &mut MFunction) {
    let entry = func.blocks.first().map(|block| block.id);
    let phi_predecessors = func
        .blocks
        .iter()
        .flat_map(|block| &block.phis)
        .flat_map(|phi| phi.sources.iter().map(|(pred, _)| *pred))
        .collect::<HashSet<_>>();

    // Build jump-through map: if a block contains only `jmp target`,
    // redirect all references to this block directly to `target`.
    let mut redirect: HashMap<BlockId, BlockId> = HashMap::default();
    for block in &func.blocks {
        if Some(block.id) != entry
            && !phi_predecessors.contains(&block.id)
            && block.phis.is_empty()
            && block.insts.len() == 1
        {
            if let MInst::Jump { target } = &block.insts[0] {
                redirect.insert(block.id, *target);
            }
        }
    }

    if !redirect.is_empty() {
        // Transitively resolve redirects
        let mut resolved: HashMap<BlockId, BlockId> = HashMap::default();
        for &src in redirect.keys() {
            let mut target = src;
            let mut seen = HashSet::default();
            while let Some(&next) = redirect.get(&target) {
                if !seen.insert(next) {
                    break;
                } // cycle
                target = next;
            }
            if target != src {
                resolved.insert(src, target);
            }
        }

        // Rewrite all jump/branch targets
        for block in &mut func.blocks {
            for inst in &mut block.insts {
                inst.rewrite_successors(|target| resolved.get(&target).copied().unwrap_or(target));
            }
        }

        // Remove empty blocks that are now unreachable (keep entry block)
        func.blocks
            .retain(|block| Some(block.id) == entry || !resolved.contains_key(&block.id));
    }

    // Target threading can turn both arms, or every jump-table entry, into the
    // same edge.  Canonicalize those terminators immediately: retaining the
    // dead predicate/index graph until allocation creates pressure for values
    // which the emitted control flow cannot observe.
    for block in &mut func.blocks {
        let Some(terminator) = block.insts.last_mut() else {
            continue;
        };
        let target = match terminator {
            MInst::Branch {
                true_bb, false_bb, ..
            }
            | MInst::BranchPred {
                true_bb, false_bb, ..
            } if true_bb == false_bb => Some(*true_bb),
            MInst::JumpTable { targets, .. } => targets
                .first()
                .copied()
                .filter(|target| targets.iter().all(|candidate| candidate == target)),
            _ => None,
        };
        if let Some(target) = target {
            *terminator = MInst::Jump { target };
        }
    }
}

// ────────────────────────────────────────────────────────────────
// Load sinking (instruction reordering for shorter live ranges)
// ────────────────────────────────────────────────────────────────

/// Move operand-free materializations closer to their first use within
/// each basic block. This shortens live ranges, reducing register pressure
/// and improving the quality of the single-pass register allocator.
///
/// Only moves instructions that have no side effects and whose operands
/// don't depend on intervening instructions.
fn sink_loads(func: &mut MFunction) {
    for block in &mut func.blocks {
        // Walk definitions backwards and find each target in the current
        // instruction sequence. Pre-computing all target indices is incorrect:
        // moving one definition changes the target index of another definition
        // and can place it after its use.
        for from in (0..block.insts.len()).rev() {
            let dst = match block.insts[from] {
                MInst::LoadImm { dst, .. } | MInst::LoadConstantTableAddr { dst, .. } => dst,
                _ => continue,
            };
            let Some(use_pos) = block.insts[from + 1..]
                .iter()
                .position(|inst| inst.uses().contains(&dst))
                .map(|relative| from + 1 + relative)
            else {
                continue;
            };
            if use_pos > from + 4 {
                let inst = block.insts.remove(from);
                block.insts.insert(use_pos - 1, inst);
            }
        }
    }
}

fn byte_range(offset: i32, byte_len: usize) -> Option<(i64, i64)> {
    let start = i64::from(offset);
    let byte_len = i64::try_from(byte_len).ok()?;
    Some((start, start.checked_add(byte_len)?))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct MemorySlot {
    base: BaseReg,
    offset: i32,
    size: OpSize,
}

fn resolve_alias(mut reg: VReg, aliases: &HashMap<VReg, VReg>) -> VReg {
    while let Some(&next) = aliases.get(&reg) {
        if next == reg {
            break;
        }
        reg = next;
    }
    reg
}

/// Copy propagation: replace every full-word copy, and every `Mov32` whose
/// source is already structurally proven zero-extended to 32 bits, with its
/// source throughout the function. A `Mov32` from an arbitrary 64-bit source
/// remains a real truncating definition.
fn copy_propagate(func: &mut MFunction) {
    // Build alias map: dst → src (transitively resolved)
    let mut aliases: HashMap<VReg, VReg> = HashMap::default();
    // Borrowed views of defining instructions suffice for the Mov32
    // zero-extension proof below; no need to clone every definition.
    let definitions = func
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .filter_map(|inst| inst.def().map(|dst| (dst, inst)))
        .collect::<HashMap<_, _>>();
    let mut upper_bounds = HashMap::default();

    for block in &func.blocks {
        for inst in &block.insts {
            let copy = match inst {
                MInst::Mov { dst, src } => Some((*dst, *src)),
                MInst::Mov32 { dst, src }
                    if unsigned_upper_bound(
                        *src,
                        &definitions,
                        &mut upper_bounds,
                        &mut HashSet::default(),
                    )
                    .is_some_and(|bound| bound <= u32::MAX as u64) =>
                {
                    Some((*dst, *src))
                }
                _ => None,
            };
            if let Some((dst, src)) = copy {
                // Resolve transitively: if src is itself an alias, follow the chain
                let mut target = src;
                while let Some(&next) = aliases.get(&target) {
                    target = next;
                }
                aliases.insert(dst, target);
            }
        }
    }

    if aliases.is_empty() {
        return;
    }

    // Apply aliases to all instructions
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            rewrite_uses(inst, &aliases);
        }
        // Also rewrite phi sources
        for phi in &mut block.phis {
            for (_pred, src) in &mut phi.sources {
                if let Some(&a) = aliases.get(src) {
                    *src = a;
                }
            }
        }
    }

    // Remove Mov instructions that are now identity (dst == src after alias resolution)
    // or whose dst is aliased away
    for block in &mut func.blocks {
        block.insts.retain(|inst| {
            if let MInst::Mov { dst, src } | MInst::Mov32 { dst, src } = inst {
                // Keep only if dst is not aliased (it's still needed)
                if aliases.contains_key(dst) {
                    return false; // Remove: dst was aliased to src
                }
                if dst == src {
                    return false; // Remove: identity mov
                }
            }
            true
        });
    }
}

/// Remove scalar definitions that cannot affect an observable instruction.
fn dead_code_eliminate(func: &mut MFunction) {
    dead_code::eliminate(func, true);
}

/// Post-allocation DCE must preserve the phi rows used to construct the
/// already-verified parallel-copy plan.
fn dead_code_eliminate_preserving_phis(func: &mut MFunction) {
    dead_code::eliminate(func, false);
}

/// Rewrite all use operands in an instruction according to the alias map.
fn rewrite_uses(inst: &mut MInst, aliases: &HashMap<VReg, VReg>) {
    // Iterate over uses and rewrite any that appear in aliases
    let current_uses = inst.uses();
    for u in current_uses {
        if let Some(&target) = aliases.get(&u) {
            inst.rewrite_use(u, target);
        }
    }
}

#[cfg(test)]
mod tests;
