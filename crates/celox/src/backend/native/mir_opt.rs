//! MIR optimization passes: run between ISel and regalloc.
//!
//! - Copy propagation: `v2 = mov v1` → replace all uses of v2 with v1
//! - Dead code elimination: remove instructions whose defs are unused

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use super::memory_effect;
use super::mir::*;
use super::regalloc::assignment::{AssignmentMap, PhysReg, clobbers};

mod pipeline;
pub use pipeline::optimize;

/// Select x86 direct-memory updates for exact local load/ALU/store chains.
///
/// The SSA temporaries must have no other users. Keeping the match local
/// also fixes the memory observation point: there is no intervening effect to
/// justify with instruction reordering. Wider immediates are truncated to the
/// access width, exactly as the original final store truncates the register.
pub(crate) fn fold_direct_immediate_stores(func: &mut MFunction) -> usize {
    let mut use_counts = HashMap::<VReg, usize>::new();
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

/// Fold a proven power-of-two byte index into the x86 memory operand.
///
/// This pass does not infer an RTL range or replace a bit-offset expression.
/// It only rewrites an already selected 64-bit `ShlImm` whose sole use is an
/// indexed load. x86 effective-address scaling and the original shift both
/// use modulo-64-bit arithmetic, so the rewrite preserves wrapping exactly.
fn fold_scaled_indexed_loads(func: &mut MFunction) {
    let mut use_counts = HashMap::<VReg, usize>::new();
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
        let mut shifts = HashMap::<VReg, (VReg, u8)>::new();
        for inst in &mut block.insts {
            if let MInst::ShlImm { dst, src, imm } = inst {
                if (1..=3).contains(imm) {
                    shifts.insert(*dst, (*src, *imm));
                }
                continue;
            }
            let MInst::LoadIndexed { index, scale, .. } = inst else {
                continue;
            };
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
    let mut defs = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            if let Some(dst) = inst.def() {
                defs.insert(dst, inst.clone());
            }
        }
    }
    let mut upper_bounds = HashMap::new();
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            let replacement = match inst {
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
                        &mut HashSet::new(),
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
                *inst = replacement;
            }
        }
    }
}

/// Remove redundant normalization of values already known to be boolean, and
/// fold an exclusively consumed `cmp.eq (cmp ...), 0` by inverting the inner
/// comparison. These forms become visible especially after immediate lowering;
/// eliminating them here avoids materializing an intermediate condition.
fn fold_boolean_normalizations(func: &mut MFunction) {
    let mut defs = HashMap::new();
    let mut use_counts = HashMap::<VReg, usize>::new();
    for block in &func.blocks {
        for phi in &block.phis {
            for &(_, source) in &phi.sources {
                *use_counts.entry(source).or_default() += 1;
            }
        }
        for inst in &block.insts {
            if let Some(dst) = inst.def() {
                defs.insert(dst, inst.clone());
            }
            for source in inst.uses() {
                *use_counts.entry(source).or_default() += 1;
            }
        }
    }
    let mut upper_bounds = HashMap::new();
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            let replacement = match inst {
                MInst::CmpImm {
                    dst,
                    lhs,
                    imm: 0,
                    kind: CmpKind::Ne,
                } if unsigned_upper_bound(*lhs, &defs, &mut upper_bounds, &mut HashSet::new())
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
                } if unsigned_upper_bound(*rhs, &defs, &mut upper_bounds, &mut HashSet::new())
                    == Some(0)
                    && unsigned_upper_bound(
                        *lhs,
                        &defs,
                        &mut upper_bounds,
                        &mut HashSet::new(),
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
                *inst = replacement;
            }
        }
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

fn unsigned_upper_bound(
    reg: VReg,
    defs: &HashMap<VReg, MInst>,
    memo: &mut HashMap<VReg, Option<u64>>,
    visiting: &mut HashSet<VReg>,
) -> Option<u64> {
    if let Some(bound) = memo.get(&reg) {
        return *bound;
    }
    if !visiting.insert(reg) {
        return None;
    }
    let bound = match defs.get(&reg)? {
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

/// Run peepholes that are safe after register allocation.
///
/// Regalloc rematerializes constants as fresh `LoadImm` instructions. When such
/// a constant has exactly one nearby use, we can fold it back into an existing
/// immediate-form MIR instruction without changing liveness or adding new
/// VRegs. The assignment map may still contain the removed VReg; it is simply
/// no longer referenced by emitted code.
pub fn post_regalloc_peephole(func: &mut MFunction) {
    const IMM_FOLD_SCAN_LIMIT: usize = 8;

    let mut use_counts: HashMap<VReg, usize> = HashMap::new();
    for block in &func.blocks {
        for phi in &block.phis {
            for (_, src) in &phi.sources {
                *use_counts.entry(*src).or_default() += 1;
            }
        }
        for inst in &block.insts {
            for use_vreg in inst.uses() {
                *use_counts.entry(use_vreg).or_default() += 1;
            }
        }
    }

    for block in &mut func.blocks {
        let mut remove = vec![false; block.insts.len()];
        let mut replacements: HashMap<usize, MInst> = HashMap::new();

        // A state-home rematerialization immediately before a forwarded
        // width-normalizing copy is one machine load, not a load followed by
        // another normalization.  Keep the copy form while allocating so the
        // stored value may remain resident; once allocation chose the memory
        // recipe, retarget the unsigned load directly to the copy result.
        for idx in 0..block.insts.len().saturating_sub(1) {
            let MInst::Load {
                dst: loaded,
                base,
                offset,
                size,
            } = block.insts[idx]
            else {
                continue;
            };
            if use_counts.get(&loaded).copied().unwrap_or(0) != 1 {
                continue;
            }
            let destination = match (size, &block.insts[idx + 1]) {
                (
                    OpSize::S8,
                    MInst::AndImm32 {
                        dst,
                        src,
                        imm: 0xff,
                    },
                ) if *src == loaded => Some(*dst),
                (
                    OpSize::S16,
                    MInst::AndImm32 {
                        dst,
                        src,
                        imm: 0xffff,
                    },
                ) if *src == loaded => Some(*dst),
                (OpSize::S32, MInst::Mov32 { dst, src }) if *src == loaded => Some(*dst),
                (_, MInst::Mov { dst, src }) if *src == loaded => Some(*dst),
                _ => None,
            };
            let Some(destination) = destination else {
                continue;
            };
            replacements.insert(
                idx,
                MInst::Load {
                    dst: destination,
                    base,
                    offset,
                    size,
                },
            );
            remove[idx + 1] = true;
        }

        for (idx, remove_imm) in remove.iter_mut().enumerate() {
            let MInst::LoadImm {
                dst: imm_vreg,
                value,
            } = block.insts[idx]
            else {
                continue;
            };
            if use_counts.get(&imm_vreg).copied().unwrap_or(0) != 1 {
                continue;
            }

            let end = (idx + IMM_FOLD_SCAN_LIMIT + 1).min(block.insts.len());
            for use_idx in idx + 1..end {
                if !block.insts[use_idx].uses().contains(&imm_vreg) {
                    continue;
                }
                if let Some(folded) = fold_imm_use(&block.insts[use_idx], imm_vreg, value) {
                    *remove_imm = true;
                    replacements.insert(use_idx, folded);
                }
                break;
            }
        }

        let mut rewritten = Vec::with_capacity(block.insts.len());
        for (idx, inst) in block.insts.iter().enumerate() {
            if remove[idx] {
                continue;
            }
            rewritten.push(replacements.remove(&idx).unwrap_or_else(|| inst.clone()));
        }
        block.insts = rewritten;
    }
}

/// Remove allocation-created trivial values before machine emission.
///
/// Allocation can rewrite two distinct incoming values to the same split
/// representative. Preserve the assigned destination with a Mov, but do not
/// leave the now-irrelevant predicate graph or rematerializations in the
/// emitted function. Copy propagation is intentionally not run after
/// allocation because the source physical register may be clobbered after the
/// copy; ordinary DCE is safe.
pub fn post_regalloc_cleanup(func: &mut MFunction) {
    simplify_equal_value_selects(func);
    dead_code_eliminate_preserving_phis(func);
    simplify_cfg(func);
}

/// Reuse an exact direct load while its assigned physical register still
/// contains that value.
///
/// CSSA intentionally gives interfering phi rows distinct edge snapshots.
/// Several snapshots can nevertheless have the same MemorySSA recipe, and
/// allocation can materialize each snapshot as an independent state load.
/// Spill splitting can likewise reload one stack home repeatedly inside a
/// block. At this late boundary the completed assignment tells us exactly
/// whether a prior loaded value remains physically available, so a duplicate
/// load can become a copy without guessing at an extended live range.
///
/// Availability is local to one block. Any definition or explicit target
/// clobber kills the value in its assigned register; overlapping or unknown
/// writes to the same direct base kill the corresponding memory value. At most
/// one value is tracked per allocatable register, making the pass
/// O(instructions * target-registers) time and O(target-registers) space per
/// block.
pub(crate) fn post_regalloc_direct_load_cse(
    func: &mut MFunction,
    assignment: &AssignmentMap,
) -> usize {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct DirectLoadKey {
        base: BaseReg,
        offset: i32,
        size: OpSize,
    }

    #[derive(Debug, Clone, Copy)]
    struct AvailableDirectLoad {
        key: DirectLoadKey,
        value: VReg,
        available_since: usize,
    }

    fn overlaps(key: DirectLoadKey, range: memory_effect::MemoryRange) -> bool {
        if key.base != range.base {
            return false;
        }
        let load_start = i64::from(key.offset);
        let load_end = load_start + i64::from(key.size.bytes());
        range
            .end()
            .is_none_or(|write_end| load_start < write_end && range.offset < load_end)
    }

    let mut reused = 0usize;
    for block in &mut func.blocks {
        let mut available = HashMap::<PhysReg, AvailableDirectLoad>::new();
        let mut replacements = Vec::<(usize, VReg, VReg)>::new();
        for (position, instruction) in block.insts.iter().enumerate() {
            let writes = memory_effect::writes(instruction);
            if let Some(memory_effect::UnknownMemory::Direct(base)) = writes.unknown_memory() {
                available.retain(|_, loaded| loaded.key.base != base);
            }
            for range in writes.ranges() {
                available.retain(|_, loaded| !overlaps(loaded.key, range));
            }
            for &register in clobbers(instruction) {
                available.remove(&register);
            }

            let direct_load = match instruction {
                MInst::Load {
                    dst,
                    base,
                    offset,
                    size,
                } => Some((
                    *dst,
                    DirectLoadKey {
                        base: *base,
                        offset: *offset,
                        size: *size,
                    },
                )),
                _ => None,
            };
            let definition_register = instruction.def().and_then(|value| assignment.get(value));
            let source = direct_load.and_then(|(_, key)| {
                available
                    .iter()
                    .filter(|(_, loaded)| loaded.key == key)
                    .min_by_key(|(register, loaded)| {
                        (
                            Some(**register) != definition_register,
                            loaded.available_since,
                            **register,
                        )
                    })
                    .map(|(_, loaded)| loaded.value)
            });

            if let Some(register) = definition_register {
                available.remove(&register);
            }

            let Some((destination, key)) = direct_load else {
                continue;
            };
            let Some(register) = definition_register else {
                continue;
            };
            if let Some(source) = source {
                replacements.push((position, destination, source));
                reused += 1;
            }
            available.insert(
                register,
                AvailableDirectLoad {
                    key,
                    value: destination,
                    available_since: position,
                },
            );
        }

        for (position, destination, source) in replacements {
            block.insts[position] = MInst::Mov {
                dst: destination,
                src: source,
            };
        }
    }
    reused
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

fn fold_imm_use(inst: &MInst, imm_vreg: VReg, value: u64) -> Option<MInst> {
    match inst {
        MInst::Cmp {
            dst,
            lhs,
            rhs,
            kind,
        } if *rhs == imm_vreg => sign_extended_i32(value).map(|imm| MInst::CmpImm {
            dst: *dst,
            lhs: *lhs,
            imm,
            kind: *kind,
        }),
        MInst::Add { dst, lhs, rhs } if *rhs == imm_vreg => {
            sign_extended_i32(value).map(|imm| MInst::AddImm {
                dst: *dst,
                src: *lhs,
                imm,
            })
        }
        MInst::Add { dst, lhs, rhs } if *lhs == imm_vreg => {
            sign_extended_i32(value).map(|imm| MInst::AddImm {
                dst: *dst,
                src: *rhs,
                imm,
            })
        }
        MInst::Sub { dst, lhs, rhs } if *rhs == imm_vreg => {
            sign_extended_i32(value).map(|imm| MInst::SubImm {
                dst: *dst,
                src: *lhs,
                imm,
            })
        }
        MInst::And { dst, lhs, rhs } if *rhs == imm_vreg => {
            and_imm_ok(value).then_some(MInst::AndImm {
                dst: *dst,
                src: *lhs,
                imm: value,
            })
        }
        MInst::And { dst, lhs, rhs } if *lhs == imm_vreg => {
            and_imm_ok(value).then_some(MInst::AndImm {
                dst: *dst,
                src: *rhs,
                imm: value,
            })
        }
        MInst::And32 { dst, lhs, rhs } if *rhs == imm_vreg => Some(MInst::AndImm32 {
            dst: *dst,
            src: *lhs,
            imm: value as u32,
        }),
        MInst::And32 { dst, lhs, rhs } if *lhs == imm_vreg => Some(MInst::AndImm32 {
            dst: *dst,
            src: *rhs,
            imm: value as u32,
        }),
        MInst::Or { dst, lhs, rhs } if *rhs == imm_vreg => {
            sign_extended_i32(value).map(|imm| MInst::OrImm {
                dst: *dst,
                src: *lhs,
                imm: imm as u64,
            })
        }
        MInst::Or { dst, lhs, rhs } if *lhs == imm_vreg => {
            sign_extended_i32(value).map(|imm| MInst::OrImm {
                dst: *dst,
                src: *rhs,
                imm: imm as u64,
            })
        }
        MInst::Shr { dst, lhs, rhs } if *rhs == imm_vreg && value < 64 => Some(MInst::ShrImm {
            dst: *dst,
            src: *lhs,
            imm: value as u8,
        }),
        MInst::Shl { dst, lhs, rhs } if *rhs == imm_vreg && value < 64 => Some(MInst::ShlImm {
            dst: *dst,
            src: *lhs,
            imm: value as u8,
        }),
        MInst::Sar { dst, lhs, rhs } if *rhs == imm_vreg && value < 64 => Some(MInst::SarImm {
            dst: *dst,
            src: *lhs,
            imm: value as u8,
        }),
        MInst::LoadIndexed {
            dst,
            base,
            offset,
            index,
            size,
            ..
        } if *index == imm_vreg => sign_extended_i32(value)
            .and_then(|index| offset.checked_add(index))
            .map(|offset| MInst::Load {
                dst: *dst,
                base: *base,
                offset,
                size: *size,
            }),
        MInst::StoreIndexed {
            base,
            offset,
            index,
            src,
            size,
            ..
        } if *index == imm_vreg => sign_extended_i32(value)
            .and_then(|index| offset.checked_add(index))
            .map(|offset| MInst::Store {
                base: *base,
                offset,
                src: *src,
                size: *size,
            }),
        MInst::LoadPtrIndexed {
            dst,
            ptr,
            offset,
            index,
            size,
        } if *index == imm_vreg => sign_extended_i32(value)
            .and_then(|index| offset.checked_add(index))
            .map(|offset| MInst::LoadPtr {
                dst: *dst,
                ptr: *ptr,
                offset,
                size: *size,
            }),
        MInst::StorePtrIndexed {
            ptr,
            offset,
            index,
            src,
            size,
        } if *index == imm_vreg => sign_extended_i32(value)
            .and_then(|index| offset.checked_add(index))
            .map(|offset| MInst::StorePtr {
                ptr: *ptr,
                offset,
                src: *src,
                size: *size,
            }),
        MInst::ReleaseStorePtrIndexed {
            ptr,
            offset,
            index,
            src,
            size,
        } if *index == imm_vreg => sign_extended_i32(value)
            .and_then(|index| offset.checked_add(index))
            .map(|offset| MInst::ReleaseStorePtr {
                ptr: *ptr,
                offset,
                src: *src,
                size: *size,
            }),
        _ => None,
    }
}

fn sign_extended_i32(value: u64) -> Option<i32> {
    let imm = value as i32;
    ((imm as i64 as u64) == value).then_some(imm)
}

fn and_imm_ok(value: u64) -> bool {
    sign_extended_i32(value).is_some() || value <= u32::MAX as u64
}

fn fuse_compare_selects(func: &mut MFunction) {
    let mut use_counts: HashMap<VReg, usize> = HashMap::new();
    for block in &func.blocks {
        for phi in &block.phis {
            for (_, src) in &phi.sources {
                *use_counts.entry(*src).or_default() += 1;
            }
        }
        for inst in &block.insts {
            for use_vreg in inst.uses() {
                *use_counts.entry(use_vreg).or_default() += 1;
            }
        }
    }

    for block in &mut func.blocks {
        let mut def_pos: HashMap<VReg, usize> = HashMap::new();
        for (idx, inst) in block.insts.iter().enumerate() {
            if let Some(def) = inst.def() {
                def_pos.insert(def, idx);
            }
        }

        let mut remove = vec![false; block.insts.len()];
        let mut replacements: HashMap<usize, MInst> = HashMap::new();

        for (idx, inst) in block.insts.iter().enumerate() {
            let MInst::Select {
                dst,
                cond,
                true_val,
                false_val,
            } = inst
            else {
                continue;
            };
            if use_counts.get(cond).copied().unwrap_or(0) != 1 {
                continue;
            }
            let Some(&cmp_idx) = def_pos.get(cond) else {
                continue;
            };
            if cmp_idx >= idx || remove[cmp_idx] {
                continue;
            }

            let fused = match block.insts[cmp_idx] {
                MInst::Cmp { lhs, rhs, kind, .. } => Some(MInst::CmpSelect {
                    dst: *dst,
                    lhs,
                    rhs,
                    kind,
                    true_val: *true_val,
                    false_val: *false_val,
                }),
                MInst::CmpImm { lhs, imm, kind, .. } => Some(MInst::CmpImmSelect {
                    dst: *dst,
                    lhs,
                    imm,
                    kind,
                    true_val: *true_val,
                    false_val: *false_val,
                }),
                _ => None,
            };

            if let Some(fused) = fused {
                remove[cmp_idx] = true;
                replacements.insert(idx, fused);
            }
        }

        if replacements.is_empty() {
            continue;
        }

        let mut rewritten = Vec::with_capacity(block.insts.len());
        for (idx, inst) in block.insts.iter().enumerate() {
            if remove[idx] {
                continue;
            }
            rewritten.push(replacements.remove(&idx).unwrap_or_else(|| inst.clone()));
        }
        block.insts = rewritten;
    }
}

fn select_values(inst: &MInst) -> Option<(VReg, VReg)> {
    match inst {
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
        } => Some((*true_val, *false_val)),
        _ => None,
    }
}

fn rebuild_select(inst: &MInst, dst: VReg, true_val: VReg, false_val: VReg) -> Option<MInst> {
    match inst {
        MInst::Select { cond, .. } => Some(MInst::Select {
            dst,
            cond: *cond,
            true_val,
            false_val,
        }),
        MInst::CmpSelect { lhs, rhs, kind, .. } => Some(MInst::CmpSelect {
            dst,
            lhs: *lhs,
            rhs: *rhs,
            kind: *kind,
            true_val,
            false_val,
        }),
        MInst::CmpImmSelect { lhs, imm, kind, .. } => Some(MInst::CmpImmSelect {
            dst,
            lhs: *lhs,
            imm: *imm,
            kind: *kind,
            true_val,
            false_val,
        }),
        MInst::GuardedCmpSelect {
            guard,
            lhs,
            rhs,
            kind,
            ..
        } => Some(MInst::GuardedCmpSelect {
            dst,
            guard: *guard,
            lhs: *lhs,
            rhs: *rhs,
            kind: *kind,
            true_val,
            false_val,
        }),
        _ => None,
    }
}

struct IndexedLoadSelectionPlan {
    root: usize,
    remove: Vec<usize>,
    replacement: Vec<MInst>,
}

#[derive(Clone, Copy)]
struct IndexedLoadTreeSummary {
    base: BaseReg,
    index: VReg,
    size: OpSize,
    first_definition: usize,
}

impl IndexedLoadTreeSummary {
    fn same_address_shape(self, other: Self) -> bool {
        self.base == other.base && self.index == other.index && self.size == other.size
    }
}

/// Turn a tree of selected indexed loads into one indexed load from a selected
/// address.
///
/// This is the machine-level form of LLVM's load/select sinking:
///
/// `select p, load [base + a + i], load [base + b + i]`
/// becomes
/// `load [base + select(p, a, b) + i]`.
///
/// Simulator memory is non-faulting inside the alias envelopes carried by the
/// original loads.  The rewrite nevertheless stays within one block, requires
/// every arm and intermediate select to be exclusively consumed by the tree,
/// and rejects any intervening write.  The selected load receives the union of
/// the original alias envelopes.  Each instruction and operand edge belongs to
/// at most one accepted component, so discovery and rewriting are linear in
/// the MIR size.  A forward summary rejects malformed subtrees once, before
/// maximal roots are traversed, so failed nested candidates cannot cause
/// quadratic rediscovery.
fn sink_selected_indexed_loads(func: &mut MFunction) {
    let mut use_counts = HashMap::<VReg, usize>::new();
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

    let mut plans = Vec::<Vec<IndexedLoadSelectionPlan>>::with_capacity(func.blocks.len());
    for block in &func.blocks {
        let mut definitions = HashMap::<VReg, usize>::new();
        for (index, inst) in block.insts.iter().enumerate() {
            if let Some(dst) = inst.def() {
                definitions.insert(dst, index);
            }
        }

        let mut write_prefix = Vec::with_capacity(block.insts.len() + 1);
        write_prefix.push(0usize);
        for inst in &block.insts {
            write_prefix.push(
                write_prefix.last().copied().unwrap()
                    + usize::from(memory_effect::writes(inst).has_effect()),
            );
        }
        let mut selectable = HashMap::<VReg, IndexedLoadTreeSummary>::new();
        for (instruction, inst) in block.insts.iter().enumerate() {
            if let MInst::LoadIndexed {
                dst,
                base,
                index,
                scale,
                size,
                ..
            } = inst
            {
                if *scale != 1 {
                    continue;
                }
                selectable.insert(
                    *dst,
                    IndexedLoadTreeSummary {
                        base: *base,
                        index: *index,
                        size: *size,
                        first_definition: instruction,
                    },
                );
                continue;
            }
            let Some((true_val, false_val)) = select_values(inst) else {
                continue;
            };
            if use_counts.get(&true_val).copied() != Some(1)
                || use_counts.get(&false_val).copied() != Some(1)
            {
                continue;
            }
            let Some(true_tree) = selectable.get(&true_val).copied() else {
                continue;
            };
            let Some(false_tree) = selectable.get(&false_val).copied() else {
                continue;
            };
            if !true_tree.same_address_shape(false_tree) {
                continue;
            }
            let first_definition = true_tree.first_definition.min(false_tree.first_definition);
            if write_prefix[instruction] != write_prefix[first_definition] {
                continue;
            }
            selectable.insert(
                inst.def().unwrap(),
                IndexedLoadTreeSummary {
                    first_definition,
                    ..true_tree
                },
            );
        }

        let mut claimed = HashSet::<usize>::new();
        let mut block_plans = Vec::new();
        for root in (0..block.insts.len()).rev() {
            if claimed.contains(&root)
                || select_values(&block.insts[root]).is_none()
                || block.insts[root]
                    .def()
                    .is_none_or(|dst| !selectable.contains_key(&dst))
            {
                continue;
            }

            let mut pending = Vec::new();
            let (true_val, false_val) = select_values(&block.insts[root]).unwrap();
            pending.push(true_val);
            pending.push(false_val);
            let mut select_indices = vec![root];
            let mut load_indices = Vec::new();
            let mut seen_values = HashSet::new();
            let mut failed = false;

            while let Some(value) = pending.pop() {
                if !seen_values.insert(value) {
                    continue;
                }
                let Some(&definition) = definitions.get(&value) else {
                    failed = true;
                    break;
                };
                if definition >= root || claimed.contains(&definition) {
                    failed = true;
                    break;
                }
                match &block.insts[definition] {
                    MInst::LoadIndexed { .. } => {
                        if use_counts.get(&value).copied() != Some(1) {
                            failed = true;
                            break;
                        }
                        load_indices.push(definition);
                    }
                    inst if select_values(inst).is_some()
                        && use_counts.get(&value).copied() == Some(1) =>
                    {
                        select_indices.push(definition);
                        let (true_val, false_val) = select_values(inst).unwrap();
                        pending.push(true_val);
                        pending.push(false_val);
                    }
                    _ => {
                        failed = true;
                        break;
                    }
                }
            }
            if failed || load_indices.len() < 2 {
                continue;
            }

            let MInst::LoadIndexed {
                base,
                offset: first_offset,
                index,
                size,
                ..
            } = block.insts[load_indices[0]]
            else {
                unreachable!()
            };
            let mut min_offset = first_offset;
            let mut alias_start = i64::MAX;
            let mut alias_end = i64::MIN;
            let mut complete_alias = true;
            for &load_index in &load_indices {
                let MInst::LoadIndexed {
                    base: candidate_base,
                    offset,
                    index: candidate_index,
                    size: candidate_size,
                    alias_range,
                    ..
                } = block.insts[load_index]
                else {
                    unreachable!()
                };
                if candidate_base != base || candidate_index != index || candidate_size != size {
                    failed = true;
                    break;
                }
                min_offset = min_offset.min(offset);
                if let Some(range) = alias_range {
                    alias_start = alias_start.min(i64::from(range.offset()));
                    alias_end = alias_end.max(range.end());
                } else {
                    complete_alias = false;
                }
            }
            if failed {
                continue;
            }

            let first_definition = load_indices.iter().copied().min().unwrap();
            if block.insts[first_definition..root]
                .iter()
                .any(|inst| memory_effect::writes(inst).has_effect())
            {
                continue;
            }

            select_indices.sort_unstable();
            load_indices.sort_unstable();
            let mut replacement = Vec::new();
            let mut remapped = HashMap::<VReg, VReg>::new();
            let mut offset_constants = HashMap::<i32, VReg>::new();
            for &load_index in &load_indices {
                let MInst::LoadIndexed { dst, offset, .. } = block.insts[load_index] else {
                    unreachable!()
                };
                let delta = offset
                    .checked_sub(min_offset)
                    .map(|value| value as u32 as u64);
                let Some(delta) = delta else {
                    failed = true;
                    break;
                };
                let selected_offset = *offset_constants.entry(offset).or_insert_with(|| {
                    let value = func.vregs.alloc();
                    while func.spill_descs.len() <= value.0 as usize {
                        func.spill_descs.push(SpillDesc::remat(delta));
                    }
                    replacement.push(MInst::LoadImm {
                        dst: value,
                        value: delta,
                    });
                    value
                });
                remapped.insert(dst, selected_offset);
            }
            if failed {
                continue;
            }

            for &select_index in &select_indices {
                let inst = &block.insts[select_index];
                let (old_true, old_false) = select_values(inst).unwrap();
                let Some(&true_val) = remapped.get(&old_true) else {
                    failed = true;
                    break;
                };
                let Some(&false_val) = remapped.get(&old_false) else {
                    failed = true;
                    break;
                };
                let old_dst = inst.def().unwrap();
                let selected_offset = func.vregs.alloc();
                while func.spill_descs.len() <= selected_offset.0 as usize {
                    func.spill_descs.push(SpillDesc::transient());
                }
                replacement
                    .push(rebuild_select(inst, selected_offset, true_val, false_val).unwrap());
                remapped.insert(old_dst, selected_offset);
            }
            if failed {
                continue;
            }

            let root_dst = block.insts[root].def().unwrap();
            let selected_offset = remapped[&root_dst];
            let combined_index = func.vregs.alloc();
            while func.spill_descs.len() <= combined_index.0 as usize {
                func.spill_descs.push(SpillDesc::transient());
            }
            replacement.push(MInst::Add {
                dst: combined_index,
                lhs: index,
                rhs: selected_offset,
            });
            let alias_range = if complete_alias {
                usize::try_from(alias_end - alias_start)
                    .ok()
                    .and_then(|byte_len| i32::try_from(alias_start).ok().zip(Some(byte_len)))
                    .and_then(|(offset, byte_len)| MemoryAliasRange::new(offset, byte_len))
            } else {
                None
            };
            replacement.push(MInst::LoadIndexed {
                dst: root_dst,
                base,
                offset: min_offset,
                index: combined_index,
                scale: 1,
                size,
                alias_range,
            });

            let mut remove = select_indices;
            remove.extend(load_indices);
            remove.sort_unstable();
            remove.dedup();
            claimed.extend(remove.iter().copied());
            block_plans.push(IndexedLoadSelectionPlan {
                root,
                remove,
                replacement,
            });
        }
        plans.push(block_plans);
    }

    for (block, block_plans) in func.blocks.iter_mut().zip(plans) {
        if block_plans.is_empty() {
            continue;
        }
        let mut removals = HashSet::new();
        let mut replacements = HashMap::new();
        for plan in block_plans {
            removals.extend(plan.remove);
            replacements.insert(plan.root, plan.replacement);
        }
        let original = std::mem::take(&mut block.insts);
        let mut rewritten = Vec::with_capacity(original.len());
        for (index, inst) in original.into_iter().enumerate() {
            if let Some(replacement) = replacements.remove(&index) {
                rewritten.extend(replacement);
            } else if !removals.contains(&index) {
                rewritten.push(inst);
            }
        }
        block.insts = rewritten;
    }
}

// ────────────────────────────────────────────────────────────────
// Phase 1A: Constant folding
// ────────────────────────────────────────────────────────────────

/// Constant folding: evaluate operations with constant operands at compile time.
fn constant_fold(func: &mut MFunction) {
    // Build def map: VReg → LoadImm value
    let mut consts: HashMap<VReg, u64> = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            if let MInst::LoadImm { dst, value } = inst {
                consts.insert(*dst, *value);
            }
        }
    }
    if consts.is_empty() {
        return;
    }

    let mut changed = true;
    while changed {
        changed = false;
        for block in &mut func.blocks {
            for inst in &mut block.insts {
                let folded = match inst {
                    // Binary reg-reg with both constant
                    MInst::Add { dst, lhs, rhs } => {
                        fold_bin(&consts, *dst, *lhs, *rhs, u64::wrapping_add)
                    }
                    MInst::Add32 { dst, lhs, rhs } => {
                        fold_bin32(&consts, *dst, *lhs, *rhs, u32::wrapping_add)
                    }
                    MInst::Sub { dst, lhs, rhs } => {
                        fold_bin(&consts, *dst, *lhs, *rhs, u64::wrapping_sub)
                    }
                    MInst::Sub32 { dst, lhs, rhs } => {
                        fold_bin32(&consts, *dst, *lhs, *rhs, u32::wrapping_sub)
                    }
                    MInst::Mul { dst, lhs, rhs } => {
                        fold_bin(&consts, *dst, *lhs, *rhs, u64::wrapping_mul)
                    }
                    MInst::Mul32 { dst, lhs, rhs } => {
                        fold_bin32(&consts, *dst, *lhs, *rhs, u32::wrapping_mul)
                    }
                    MInst::And { dst, lhs, rhs } => {
                        fold_bin(&consts, *dst, *lhs, *rhs, |a, b| a & b)
                    }
                    MInst::And32 { dst, lhs, rhs } => {
                        fold_bin32(&consts, *dst, *lhs, *rhs, |a, b| a & b)
                    }
                    MInst::Or { dst, lhs, rhs } => {
                        fold_bin(&consts, *dst, *lhs, *rhs, |a, b| a | b)
                    }
                    MInst::Or32 { dst, lhs, rhs } => {
                        fold_bin32(&consts, *dst, *lhs, *rhs, |a, b| a | b)
                    }
                    MInst::Xor { dst, lhs, rhs } => {
                        fold_bin(&consts, *dst, *lhs, *rhs, |a, b| a ^ b)
                    }
                    MInst::Xor32 { dst, lhs, rhs } => {
                        fold_bin32(&consts, *dst, *lhs, *rhs, |a, b| a ^ b)
                    }
                    MInst::Shr { dst, lhs, rhs } => {
                        fold_bin(
                            &consts,
                            *dst,
                            *lhs,
                            *rhs,
                            |a, b| {
                                if b >= 64 { 0 } else { a >> b }
                            },
                        )
                    }
                    MInst::Shl { dst, lhs, rhs } => {
                        fold_bin(
                            &consts,
                            *dst,
                            *lhs,
                            *rhs,
                            |a, b| {
                                if b >= 64 { 0 } else { a << b }
                            },
                        )
                    }
                    MInst::Sar { dst, lhs, rhs } => fold_bin(&consts, *dst, *lhs, *rhs, |a, b| {
                        if b >= 64 {
                            ((a as i64) >> 63) as u64
                        } else {
                            ((a as i64) >> b) as u64
                        }
                    }),
                    // Binary imm with constant src
                    MInst::AndImm { dst, src, imm } => consts.get(src).map(|&v| (*dst, v & *imm)),
                    MInst::AndImm32 { dst, src, imm } => consts
                        .get(src)
                        .map(|&v| (*dst, u64::from((v as u32) & *imm))),
                    MInst::OrImm { dst, src, imm } => consts.get(src).map(|&v| (*dst, v | *imm)),
                    MInst::ShrImm { dst, src, imm } => consts
                        .get(src)
                        .map(|&v| (*dst, if *imm >= 64 { 0 } else { v >> *imm })),
                    MInst::ShlImm { dst, src, imm } => consts
                        .get(src)
                        .map(|&v| (*dst, if *imm >= 64 { 0 } else { v << *imm })),
                    MInst::SarImm { dst, src, imm } => consts.get(src).map(|&v| {
                        (
                            *dst,
                            if *imm >= 64 {
                                ((v as i64) >> 63) as u64
                            } else {
                                ((v as i64) >> *imm) as u64
                            },
                        )
                    }),
                    // Unary with constant src
                    MInst::BitNot { dst, src } => consts.get(src).map(|&v| (*dst, !v)),
                    MInst::Neg { dst, src } => consts.get(src).map(|&v| (*dst, v.wrapping_neg())),
                    MInst::Popcnt { dst, src } => {
                        consts.get(src).map(|&v| (*dst, v.count_ones() as u64))
                    }
                    MInst::Bsf { dst, src } => consts
                        .get(src)
                        .and_then(|&v| (v != 0).then_some((*dst, v.trailing_zeros() as u64))),
                    MInst::Bsr { dst, src } => consts
                        .get(src)
                        .and_then(|&v| (v != 0).then_some((*dst, 63 - v.leading_zeros() as u64))),
                    MInst::BsrOr {
                        dst,
                        src,
                        zero_value,
                    } => consts.get(src).map(|&v| {
                        (
                            *dst,
                            if v == 0 {
                                *zero_value as u64
                            } else {
                                63 - v.leading_zeros() as u64
                            },
                        )
                    }),
                    // Comparison with both constant
                    MInst::Cmp {
                        dst,
                        lhs,
                        rhs,
                        kind,
                    } => {
                        if let (Some(&l), Some(&r)) = (consts.get(lhs), consts.get(rhs)) {
                            let result = match kind {
                                CmpKind::Eq => l == r,
                                CmpKind::Ne => l != r,
                                CmpKind::LtU => l < r,
                                CmpKind::LeU => l <= r,
                                CmpKind::GtU => l > r,
                                CmpKind::GeU => l >= r,
                                CmpKind::LtS => (l as i64) < (r as i64),
                                CmpKind::LeS => (l as i64) <= (r as i64),
                                CmpKind::GtS => (l as i64) > (r as i64),
                                CmpKind::GeS => (l as i64) >= (r as i64),
                            };
                            Some((*dst, result as u64))
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                if let Some((dst, value)) = folded {
                    *inst = MInst::LoadImm { dst, value };
                    consts.insert(dst, value);
                    changed = true;
                }
            }
        }
    }
}

fn fold_bin(
    consts: &HashMap<VReg, u64>,
    dst: VReg,
    lhs: VReg,
    rhs: VReg,
    op: impl Fn(u64, u64) -> u64,
) -> Option<(VReg, u64)> {
    if let (Some(&l), Some(&r)) = (consts.get(&lhs), consts.get(&rhs)) {
        Some((dst, op(l, r)))
    } else {
        None
    }
}

fn fold_bin32(
    consts: &HashMap<VReg, u64>,
    dst: VReg,
    lhs: VReg,
    rhs: VReg,
    op: impl Fn(u32, u32) -> u32,
) -> Option<(VReg, u64)> {
    if let (Some(&l), Some(&r)) = (consts.get(&lhs), consts.get(&rhs)) {
        Some((dst, u64::from(op(l as u32, r as u32))))
    } else {
        None
    }
}

// ────────────────────────────────────────────────────────────────
// Phase 1B: Redundant mask elimination
// ────────────────────────────────────────────────────────────────

/// Return `w` when `mask` is the contiguous low-bit mask `(1 << w) - 1`.
/// Other MIR peepholes also use this shape test when recognizing bit fields.
fn mask_width(mask: u64) -> Option<usize> {
    if mask == 0 {
        return Some(0);
    }
    if mask == u64::MAX {
        return Some(64);
    }
    let width = mask.trailing_ones() as usize;
    (mask == (1u64 << width) - 1).then_some(width)
}

/// Redundant mask elimination over the two machine widths represented by MIR.
///
/// A scalar "known width" misses non-contiguous masks and, more importantly,
/// used to forget the zero-extension semantics of the explicit 32-bit MIR
/// operations.  Track a conservative set of bits which may be one instead.
/// This is the ordinary known-bits lattice restricted to the fact needed by
/// this pass: `x & mask == x` exactly when every possible one-bit of `x` is in
/// `mask`. Facts follow SSA values through the complete CFG, including phis.
/// Rewrites remain at the original definition and use sites, so propagating a
/// fact across a block boundary does not extend a live range or add an
/// ordering constraint.
fn redundant_mask_eliminate(func: &mut MFunction) {
    let mut constants: HashMap<VReg, u64> = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            if let MInst::LoadImm { dst, value } = inst {
                constants.insert(*dst, *value);
            }
        }
    }
    let possible_ones = global_possible_one_bits(func, &constants);

    for block in &mut func.blocks {
        let mut definitions: HashMap<VReg, MaskDefinition> = HashMap::new();

        for inst in &mut block.insts {
            let should_replace =
                redundant_mask_action(inst, &possible_ones, &constants, &definitions);

            if let Some(action) = should_replace {
                match action {
                    MaskElimAction::Mov(dst, src) => {
                        *inst = MInst::Mov { dst, src };
                    }
                    MaskElimAction::Mov32(dst, src) => *inst = MInst::Mov32 { dst, src },
                    MaskElimAction::FoldAnd(dst, inner, folded_mask) => {
                        *inst = MInst::AndImm {
                            dst,
                            src: inner,
                            imm: folded_mask,
                        };
                    }
                    MaskElimAction::FoldAnd32(dst, inner, folded_mask) => {
                        *inst = MInst::AndImm32 {
                            dst,
                            src: inner,
                            imm: folded_mask,
                        };
                    }
                }
            }

            if let Some(dst) = inst.def() {
                if let Some(definition) = MaskDefinition::from_inst(inst) {
                    definitions.insert(dst, definition);
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum PossibleOneDefinition {
    Phi { block: usize, phi: usize },
    Instruction { block: usize, instruction: usize },
}

/// Solve possible-one facts on the SSA def-use graph.
///
/// Facts only move from unknown (`u64::MAX`) toward a smaller set of possible
/// bits. A changed definition schedules exactly its SSA users, so an acyclic
/// chain is visited once after its inputs settle and a loop is revisited only
/// when a backedge fact actually improves.
fn global_possible_one_bits(func: &MFunction, constants: &HashMap<VReg, u64>) -> Vec<u64> {
    let value_count = func.vregs.count() as usize;
    let mut definitions = vec![None::<PossibleOneDefinition>; value_count];
    let mut users = vec![Vec::<VReg>::new(); value_count];
    let mut queue = VecDeque::new();
    let mut queued = vec![false; value_count];

    for (block_index, block) in func.blocks.iter().enumerate() {
        for (phi_index, phi) in block.phis.iter().enumerate() {
            let destination = phi.dst.0 as usize;
            definitions[destination] = Some(PossibleOneDefinition::Phi {
                block: block_index,
                phi: phi_index,
            });
            queue.push_back(phi.dst);
            queued[destination] = true;
            for &(_, source) in &phi.sources {
                users[source.0 as usize].push(phi.dst);
            }
        }
        for (instruction_index, instruction) in block.insts.iter().enumerate() {
            let Some(dst) = instruction.def() else {
                continue;
            };
            let destination = dst.0 as usize;
            definitions[destination] = Some(PossibleOneDefinition::Instruction {
                block: block_index,
                instruction: instruction_index,
            });
            queue.push_back(dst);
            queued[destination] = true;
            for source in instruction.uses() {
                users[source.0 as usize].push(dst);
            }
        }
    }

    let mut possible_ones = vec![u64::MAX; value_count];
    while let Some(value) = queue.pop_front() {
        let value_index = value.0 as usize;
        queued[value_index] = false;
        let Some(definition) = definitions[value_index] else {
            continue;
        };
        let computed = match definition {
            PossibleOneDefinition::Phi { block, phi } => func.blocks[block].phis[phi]
                .sources
                .iter()
                .map(|(_, source)| possible_bits(*source, &possible_ones, constants))
                .fold(0, |possible, source| possible | source),
            PossibleOneDefinition::Instruction { block, instruction } => compute_possible_one_bits(
                &func.blocks[block].insts[instruction],
                &possible_ones,
                constants,
            ),
        };
        let old = possible_ones[value_index];
        let improved = old & computed;
        if improved == old {
            continue;
        }
        possible_ones[value_index] = improved;
        for &user in &users[value_index] {
            let user_index = user.0 as usize;
            if !queued[user_index] {
                queued[user_index] = true;
                queue.push_back(user);
            }
        }
    }
    possible_ones
}

enum MaskElimAction {
    Mov(VReg, VReg),
    Mov32(VReg, VReg),
    FoldAnd(VReg, VReg, u64),
    FoldAnd32(VReg, VReg, u32),
}

#[derive(Clone, Copy)]
enum MaskDefinition {
    Register { lhs: VReg, rhs: VReg, word32: bool },
    Immediate { src: VReg, mask: u64, word32: bool },
}

impl MaskDefinition {
    fn from_inst(inst: &MInst) -> Option<Self> {
        match inst {
            MInst::And { lhs, rhs, .. } => Some(Self::Register {
                lhs: *lhs,
                rhs: *rhs,
                word32: false,
            }),
            MInst::And32 { lhs, rhs, .. } => Some(Self::Register {
                lhs: *lhs,
                rhs: *rhs,
                word32: true,
            }),
            MInst::AndImm { src, imm, .. } => Some(Self::Immediate {
                src: *src,
                mask: *imm,
                word32: false,
            }),
            MInst::AndImm32 { src, imm, .. } => Some(Self::Immediate {
                src: *src,
                mask: u64::from(*imm),
                word32: true,
            }),
            _ => None,
        }
    }
}

fn possible_bits(value: VReg, possible_ones: &[u64], constants: &HashMap<VReg, u64>) -> u64 {
    possible_ones
        .get(value.0 as usize)
        .filter(|&&possible| possible != u64::MAX)
        .or_else(|| constants.get(&value))
        .copied()
        .unwrap_or(u64::MAX)
}

fn redundant_32_bit_mask_action(
    dst: VReg,
    src: VReg,
    mask: u32,
    possible_ones: &[u64],
    constants: &HashMap<VReg, u64>,
) -> Option<MaskElimAction> {
    let source_bits = possible_bits(src, possible_ones, constants);
    let low_mask = u64::from(mask);
    if source_bits & u64::from(u32::MAX) & !low_mask != 0 {
        return None;
    }
    if source_bits & !u64::from(u32::MAX) == 0 {
        Some(MaskElimAction::Mov(dst, src))
    } else {
        // The mask is redundant in the low word, but the operation's required
        // zero-extension is not.  Preserve that machine-width semantic.
        Some(MaskElimAction::Mov32(dst, src))
    }
}

fn and_repeats_operand(definition: Option<&MaskDefinition>, operand: VReg, word32: bool) -> bool {
    match definition {
        Some(MaskDefinition::Register {
            lhs,
            rhs,
            word32: definition_word32,
        }) if *definition_word32 == word32 => *lhs == operand || *rhs == operand,
        _ => false,
    }
}

fn redundant_mask_action(
    inst: &MInst,
    possible_ones: &[u64],
    constants: &HashMap<VReg, u64>,
    definitions: &HashMap<VReg, MaskDefinition>,
) -> Option<MaskElimAction> {
    match inst {
        MInst::AndImm { dst, src, imm } => {
            if possible_bits(*src, possible_ones, constants) & !*imm == 0 {
                return Some(MaskElimAction::Mov(*dst, *src));
            }
            match definitions.get(src) {
                Some(MaskDefinition::Immediate {
                    src: inner,
                    mask: first,
                    word32: false,
                }) => Some(MaskElimAction::FoldAnd(*dst, *inner, *first & *imm)),
                Some(MaskDefinition::Immediate {
                    src: inner,
                    mask: first,
                    word32: true,
                }) => Some(MaskElimAction::FoldAnd32(
                    *dst,
                    *inner,
                    *first as u32 & *imm as u32,
                )),
                _ => None,
            }
        }
        MInst::AndImm32 { dst, src, imm } => {
            redundant_32_bit_mask_action(*dst, *src, *imm, possible_ones, constants).or_else(|| {
                match definitions.get(src) {
                    Some(MaskDefinition::Immediate {
                        src: inner,
                        mask: first,
                        ..
                    }) => Some(MaskElimAction::FoldAnd32(
                        *dst,
                        *inner,
                        *first as u32 & *imm,
                    )),
                    _ => None,
                }
            })
        }
        MInst::And { dst, lhs, rhs } => {
            if let Some(&mask) = constants.get(rhs)
                && possible_bits(*lhs, possible_ones, constants) & !mask == 0
            {
                return Some(MaskElimAction::Mov(*dst, *lhs));
            }
            if let Some(&mask) = constants.get(lhs)
                && possible_bits(*rhs, possible_ones, constants) & !mask == 0
            {
                return Some(MaskElimAction::Mov(*dst, *rhs));
            }
            if and_repeats_operand(definitions.get(lhs), *rhs, false) {
                Some(MaskElimAction::Mov(*dst, *lhs))
            } else if and_repeats_operand(definitions.get(rhs), *lhs, false) {
                Some(MaskElimAction::Mov(*dst, *rhs))
            } else {
                None
            }
        }
        MInst::And32 { dst, lhs, rhs } => {
            if let Some(&mask) = constants.get(rhs) {
                return redundant_32_bit_mask_action(
                    *dst,
                    *lhs,
                    mask as u32,
                    possible_ones,
                    constants,
                );
            }
            if let Some(&mask) = constants.get(lhs) {
                return redundant_32_bit_mask_action(
                    *dst,
                    *rhs,
                    mask as u32,
                    possible_ones,
                    constants,
                );
            }
            if and_repeats_operand(definitions.get(lhs), *rhs, true) {
                Some(MaskElimAction::Mov(*dst, *lhs))
            } else if and_repeats_operand(definitions.get(rhs), *lhs, true) {
                Some(MaskElimAction::Mov(*dst, *rhs))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn machine_width_mask(size: OpSize) -> u64 {
    match size {
        OpSize::S8 => u64::from(u8::MAX),
        OpSize::S16 => u64::from(u16::MAX),
        OpSize::S32 => u64::from(u32::MAX),
        OpSize::S64 => u64::MAX,
    }
}

fn compute_possible_one_bits(
    inst: &MInst,
    possible_ones: &[u64],
    constants: &HashMap<VReg, u64>,
) -> u64 {
    let bits = |value| possible_bits(value, possible_ones, constants);
    let low32 = u64::from(u32::MAX);
    match inst {
        MInst::LoadImm { value, .. } => *value,
        MInst::Load { size, .. }
        | MInst::LoadIndexed { size, .. }
        | MInst::LoadPtr { size, .. }
        | MInst::LoadPtrIndexed { size, .. } => machine_width_mask(*size),
        MInst::Mov { src, .. } => bits(*src),
        MInst::Mov32 { src, .. } => bits(*src) & low32,
        MInst::And { lhs, rhs, .. } => bits(*lhs) & bits(*rhs),
        MInst::And32 { lhs, rhs, .. } => bits(*lhs) & bits(*rhs) & low32,
        MInst::AndImm { src, imm, .. } => bits(*src) & *imm,
        MInst::AndImm32 { src, imm, .. } => bits(*src) & u64::from(*imm),
        MInst::Or { lhs, rhs, .. } | MInst::Xor { lhs, rhs, .. } => bits(*lhs) | bits(*rhs),
        MInst::Or32 { lhs, rhs, .. } | MInst::Xor32 { lhs, rhs, .. } => {
            (bits(*lhs) | bits(*rhs)) & low32
        }
        MInst::OrImm { src, imm, .. } => bits(*src) | *imm,
        MInst::Add32 { .. } | MInst::Sub32 { .. } | MInst::Mul32 { .. } => low32,
        MInst::ShrImm { src, imm, .. } => bits(*src).checked_shr(u32::from(*imm)).unwrap_or(0),
        MInst::ShlImm { src, imm, .. } => bits(*src).checked_shl(u32::from(*imm)).unwrap_or(0),
        MInst::Cmp { .. } | MInst::CmpImm { .. } => 1,
        MInst::Popcnt { .. } => 0x7f,
        // Bit-scan destinations are unspecified for a zero input. This pass has
        // no path-sensitive nonzero fact, so every output bit remains possible.
        MInst::Bsf { .. } | MInst::Bsr { .. } => u64::MAX,
        MInst::BsrOr { zero_value, .. } => 0x3f | u64::from(*zero_value),
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
        } => bits(*true_val) | bits(*false_val),
        _ => u64::MAX,
    }
}

// ────────────────────────────────────────────────────────────────
// Global GVN (Global Value Numbering)
// ────────────────────────────────────────────────────────────────
//
// Dominator-tree-scoped CSE: walk blocks in dominator-tree pre-order,
// maintaining a scoped hash table. Entries from a dominator are visible
// to all dominated blocks, enabling cross-block redundancy elimination.

type ValueNumber = u32;

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
enum GvnOpcode {
    Add,
    Sub,
    Mul,
    UMulHi,
    And,
    Or,
    Xor,
    Shr,
    Shl,
    Sar,
    AndImm,
    AndImm32,
    OrImm,
    ShrImm,
    ShlImm,
    SarImm,
    AddImm,
    SubImm,
    UDiv,
    URem,
    SDiv,
    SRem,
    BitNot,
    Neg,
    Popcnt,
    BsrOr,
    Pext,
    Pdep,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
enum GvnMemoryVariable {
    UnknownAll,
    UnknownBase(BaseReg),
    Byte(BaseReg, i64),
}

/// Structural identity of one reaching physical-memory definition. Phi
/// versions distinguish loop iterations and joining paths without depending
/// on hash-table iteration order or on unrelated tracked byte ranges.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
enum GvnMemoryVersion {
    Entry(GvnMemoryVariable),
    Write {
        ordinal: usize,
        variable: GvnMemoryVariable,
    },
    Phi {
        block: usize,
        variable: GvnMemoryVariable,
    },
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct GvnLoadVersion {
    unknown_all: GvnMemoryVersion,
    unknown_base: GvnMemoryVersion,
    bytes: Box<[GvnMemoryVersion]>,
}

/// An expression over value numbers, not source VRegs.  Two different VRegs
/// that GVN has already proven equal therefore form the same later expression.
#[derive(Debug, Hash, PartialEq, Eq, Clone)]
enum GvnKey {
    Constant(u64),
    ConstantTable(ConstantTableId),
    Binary(GvnOpcode, ValueNumber, ValueNumber),
    BinaryImmU64(GvnOpcode, ValueNumber, u64),
    BinaryImmI32(GvnOpcode, ValueNumber, i32),
    ShiftImm(GvnOpcode, ValueNumber, u8),
    Unary(GvnOpcode, ValueNumber),
    Cmp(ValueNumber, ValueNumber, CmpKind),
    CmpImm(ValueNumber, i32, CmpKind),
    Select(ValueNumber, ValueNumber, ValueNumber),
    CmpSelect(ValueNumber, ValueNumber, CmpKind, ValueNumber, ValueNumber),
    CmpImmSelect(ValueNumber, i32, CmpKind, ValueNumber, ValueNumber),
    GuardedCmpSelect(
        ValueNumber,
        ValueNumber,
        ValueNumber,
        CmpKind,
        ValueNumber,
        ValueNumber,
    ),
    Load(BaseReg, i32, OpSize, GvnLoadVersion),
}

fn gvn_is_commutative(op: GvnOpcode) -> bool {
    matches!(
        op,
        GvnOpcode::Add
            | GvnOpcode::Mul
            | GvnOpcode::UMulHi
            | GvnOpcode::And
            | GvnOpcode::Or
            | GvnOpcode::Xor
    )
}

/// Whether allocation has target-level recovery choices for a same-block GVN
/// leader whose range is extended.
///
/// The one-source operations have exact allocator rematerialization recipes.
/// A SimState load instead has a versioned MemorySSA recipe at each valid use;
/// if a later write makes that recipe invalid, ordinary register or stack
/// residency remains available. GVN may therefore expose one shared value
/// even when its old leader has no later source-order use: pressure scheduling
/// can place users together, and allocation retains carry/split/home choices.
/// This is a cost freedom, not an ordering constraint.
fn allocator_can_recover_extended_gvn_leader(inst: &MInst) -> bool {
    matches!(
        inst,
        MInst::AndImm { .. }
            | MInst::AndImm32 { .. }
            | MInst::OrImm { .. }
            | MInst::ShrImm { .. }
            | MInst::ShlImm { .. }
            | MInst::SarImm { .. }
            | MInst::AddImm { .. }
            | MInst::SubImm { .. }
            | MInst::BitNot { .. }
            | MInst::Neg { .. }
            | MInst::Load {
                base: BaseReg::SimState,
                ..
            }
    )
}

fn gvn_value(value_numbers: &[ValueNumber], vreg: VReg) -> ValueNumber {
    value_numbers[vreg.0 as usize]
}

fn gvn_key(
    inst: &MInst,
    value_numbers: &[ValueNumber],
    load_version: Option<&GvnLoadVersion>,
) -> Option<GvnKey> {
    let value = |vreg| gvn_value(value_numbers, vreg);
    let binary = |op, lhs, rhs| gvn_binary(op, value(lhs), value(rhs));
    match inst {
        MInst::LoadImm { value, .. } => Some(GvnKey::Constant(*value)),
        MInst::LoadConstantTableAddr { table, .. } => Some(GvnKey::ConstantTable(*table)),
        MInst::Add { lhs, rhs, .. } => Some(binary(GvnOpcode::Add, *lhs, *rhs)),
        MInst::Sub { lhs, rhs, .. } => Some(binary(GvnOpcode::Sub, *lhs, *rhs)),
        MInst::Mul { lhs, rhs, .. } => Some(binary(GvnOpcode::Mul, *lhs, *rhs)),
        MInst::UMulHi { lhs, rhs, .. } => Some(binary(GvnOpcode::UMulHi, *lhs, *rhs)),
        MInst::And { lhs, rhs, .. } => Some(binary(GvnOpcode::And, *lhs, *rhs)),
        MInst::Or { lhs, rhs, .. } => Some(binary(GvnOpcode::Or, *lhs, *rhs)),
        MInst::Xor { lhs, rhs, .. } => Some(binary(GvnOpcode::Xor, *lhs, *rhs)),
        MInst::Shr { lhs, rhs, .. } => Some(binary(GvnOpcode::Shr, *lhs, *rhs)),
        MInst::Shl { lhs, rhs, .. } => Some(binary(GvnOpcode::Shl, *lhs, *rhs)),
        MInst::Sar { lhs, rhs, .. } => Some(binary(GvnOpcode::Sar, *lhs, *rhs)),
        MInst::UDiv { lhs, rhs, .. } => Some(binary(GvnOpcode::UDiv, *lhs, *rhs)),
        MInst::URem { lhs, rhs, .. } => Some(binary(GvnOpcode::URem, *lhs, *rhs)),
        MInst::SDiv { lhs, rhs, .. } => Some(binary(GvnOpcode::SDiv, *lhs, *rhs)),
        MInst::SRem { lhs, rhs, .. } => Some(binary(GvnOpcode::SRem, *lhs, *rhs)),
        MInst::AndImm { src, imm, .. } => {
            Some(GvnKey::BinaryImmU64(GvnOpcode::AndImm, value(*src), *imm))
        }
        MInst::AndImm32 { src, imm, .. } => Some(GvnKey::BinaryImmU64(
            GvnOpcode::AndImm32,
            value(*src),
            u64::from(*imm),
        )),
        MInst::OrImm { src, imm, .. } => {
            Some(GvnKey::BinaryImmU64(GvnOpcode::OrImm, value(*src), *imm))
        }
        MInst::ShrImm { src, imm, .. } => {
            Some(GvnKey::ShiftImm(GvnOpcode::ShrImm, value(*src), *imm))
        }
        MInst::ShlImm { src, imm, .. } => {
            Some(GvnKey::ShiftImm(GvnOpcode::ShlImm, value(*src), *imm))
        }
        MInst::SarImm { src, imm, .. } => {
            Some(GvnKey::ShiftImm(GvnOpcode::SarImm, value(*src), *imm))
        }
        MInst::AddImm { src, imm, .. } => {
            Some(GvnKey::BinaryImmI32(GvnOpcode::AddImm, value(*src), *imm))
        }
        MInst::SubImm { src, imm, .. } => {
            Some(GvnKey::BinaryImmI32(GvnOpcode::SubImm, value(*src), *imm))
        }
        MInst::BitNot { src, .. } => Some(GvnKey::Unary(GvnOpcode::BitNot, value(*src))),
        MInst::Neg { src, .. } => Some(GvnKey::Unary(GvnOpcode::Neg, value(*src))),
        MInst::Popcnt { src, .. } => Some(GvnKey::Unary(GvnOpcode::Popcnt, value(*src))),
        // Unchecked bit scans have an unspecified result for zero, so they
        // have no reusable value.
        MInst::Bsf { .. } | MInst::Bsr { .. } => None,
        MInst::BsrOr {
            src, zero_value, ..
        } => Some(GvnKey::BinaryImmU64(
            GvnOpcode::BsrOr,
            value(*src),
            u64::from(*zero_value),
        )),
        MInst::Pext { src, mask, .. } => Some(binary(GvnOpcode::Pext, *src, *mask)),
        MInst::Pdep { src, mask, .. } => Some(binary(GvnOpcode::Pdep, *src, *mask)),
        MInst::Cmp { lhs, rhs, kind, .. } => {
            let (mut lhs, mut rhs) = (value(*lhs), value(*rhs));
            if matches!(kind, CmpKind::Eq | CmpKind::Ne) && rhs < lhs {
                std::mem::swap(&mut lhs, &mut rhs);
            }
            Some(GvnKey::Cmp(lhs, rhs, *kind))
        }
        MInst::CmpImm { lhs, imm, kind, .. } => Some(GvnKey::CmpImm(value(*lhs), *imm, *kind)),
        MInst::Select {
            cond,
            true_val,
            false_val,
            ..
        } => Some(GvnKey::Select(
            value(*cond),
            value(*true_val),
            value(*false_val),
        )),
        MInst::CmpSelect {
            lhs,
            rhs,
            kind,
            true_val,
            false_val,
            ..
        } => Some(GvnKey::CmpSelect(
            value(*lhs),
            value(*rhs),
            *kind,
            value(*true_val),
            value(*false_val),
        )),
        MInst::CmpImmSelect {
            lhs,
            imm,
            kind,
            true_val,
            false_val,
            ..
        } => Some(GvnKey::CmpImmSelect(
            value(*lhs),
            *imm,
            *kind,
            value(*true_val),
            value(*false_val),
        )),
        MInst::GuardedCmpSelect {
            guard,
            lhs,
            rhs,
            kind,
            true_val,
            false_val,
            ..
        } => Some(GvnKey::GuardedCmpSelect(
            value(*guard),
            value(*lhs),
            value(*rhs),
            *kind,
            value(*true_val),
            value(*false_val),
        )),
        MInst::Load {
            base, offset, size, ..
        } => Some(GvnKey::Load(*base, *offset, *size, load_version?.clone())),
        _ => None,
    }
}

fn gvn_binary(op: GvnOpcode, mut lhs: ValueNumber, mut rhs: ValueNumber) -> GvnKey {
    if gvn_is_commutative(op) && rhs < lhs {
        std::mem::swap(&mut lhs, &mut rhs);
    }
    GvnKey::Binary(op, lhs, rhs)
}

#[derive(Default)]
struct GvnTrackedMemory {
    sim_state: BTreeSet<i64>,
    stack_frame: BTreeSet<i64>,
}

impl GvnTrackedMemory {
    fn bytes(&self, base: BaseReg) -> &BTreeSet<i64> {
        match base {
            BaseReg::SimState => &self.sim_state,
            BaseReg::StackFrame => &self.stack_frame,
        }
    }

    fn bytes_mut(&mut self, base: BaseReg) -> &mut BTreeSet<i64> {
        match base {
            BaseReg::SimState => &mut self.sim_state,
            BaseReg::StackFrame => &mut self.stack_frame,
        }
    }

    fn tracks_base(&self, base: BaseReg) -> bool {
        !self.bytes(base).is_empty()
    }
}

fn gvn_memory_variable_key(variable: GvnMemoryVariable) -> (u8, i64) {
    match variable {
        GvnMemoryVariable::UnknownAll => (0, 0),
        GvnMemoryVariable::UnknownBase(BaseReg::SimState) => (1, 0),
        GvnMemoryVariable::UnknownBase(BaseReg::StackFrame) => (2, 0),
        GvnMemoryVariable::Byte(BaseReg::SimState, byte) => (3, byte),
        GvnMemoryVariable::Byte(BaseReg::StackFrame, byte) => (4, byte),
    }
}

fn gvn_memory_version(
    variable: GvnMemoryVariable,
    current: &HashMap<GvnMemoryVariable, GvnMemoryVersion>,
) -> GvnMemoryVersion {
    current
        .get(&variable)
        .copied()
        .unwrap_or(GvnMemoryVersion::Entry(variable))
}

fn gvn_load_version(
    base: BaseReg,
    offset: i32,
    size: OpSize,
    current: &HashMap<GvnMemoryVariable, GvnMemoryVersion>,
) -> Option<GvnLoadVersion> {
    let start = i64::from(offset);
    let end = start.checked_add(i64::from(size.bytes()))?;
    Some(GvnLoadVersion {
        unknown_all: gvn_memory_version(GvnMemoryVariable::UnknownAll, current),
        unknown_base: gvn_memory_version(GvnMemoryVariable::UnknownBase(base), current),
        bytes: (start..end)
            .map(|byte| gvn_memory_version(GvnMemoryVariable::Byte(base, byte), current))
            .collect(),
    })
}

fn gvn_affected_memory_variables(
    inst: &MInst,
    tracked: &GvnTrackedMemory,
) -> Option<Vec<GvnMemoryVariable>> {
    let effect = memory_effect::writes(inst);
    if let Some(memory) = effect.unknown_memory() {
        return Some(match memory {
            memory_effect::UnknownMemory::Direct(base) if tracked.tracks_base(base) => {
                vec![GvnMemoryVariable::UnknownBase(base)]
            }
            memory_effect::UnknownMemory::Direct(_) | memory_effect::UnknownMemory::Indirect => {
                Vec::new()
            }
        });
    }
    let mut affected = HashSet::<GvnMemoryVariable>::new();
    for range in effect.ranges() {
        let end = range.end()?;
        affected.extend(
            tracked
                .bytes(range.base)
                .range(range.offset..end)
                .copied()
                .map(|byte| GvnMemoryVariable::Byte(range.base, byte)),
        );
    }
    let mut affected = affected.into_iter().collect::<Vec<_>>();
    affected.sort_unstable_by_key(|variable| gvn_memory_variable_key(*variable));
    Some(affected)
}

fn gvn_dominance_frontiers(
    predecessors: &[Vec<usize>],
    idom: &[Option<usize>],
) -> Option<Vec<BTreeSet<usize>>> {
    if predecessors.len() != idom.len() {
        return None;
    }
    let mut frontiers = vec![BTreeSet::new(); predecessors.len()];
    for (block, incoming) in predecessors.iter().enumerate() {
        if incoming.len() < 2 {
            continue;
        }
        let immediate = idom[block]?;
        for &predecessor in incoming {
            let mut runner = predecessor;
            let mut steps = 0usize;
            while runner != immediate {
                frontiers.get_mut(runner)?.insert(block);
                runner = idom.get(runner).copied().flatten()?;
                steps = steps.checked_add(1)?;
                if steps > idom.len() {
                    return None;
                }
            }
        }
    }
    Some(frontiers)
}

/// Build sparse byte-granular MemorySSA versions for every exact MIR load.
/// The result is keyed by the original block/instruction location and is
/// computed before GVN mutates any instruction into a copy.
fn compute_gvn_load_versions(
    func: &MFunction,
    predecessors: &[Vec<usize>],
    idom: &[Option<usize>],
) -> Option<HashMap<(usize, usize), GvnLoadVersion>> {
    let mut tracked = GvnTrackedMemory::default();
    for block in &func.blocks {
        for inst in &block.insts {
            let MInst::Load {
                base, offset, size, ..
            } = inst
            else {
                continue;
            };
            let start = i64::from(*offset);
            let end = start.checked_add(i64::from(size.bytes()))?;
            tracked.bytes_mut(*base).extend(start..end);
        }
    }
    if tracked.sim_state.is_empty() && tracked.stack_frame.is_empty() {
        return Some(HashMap::new());
    }

    let frontiers = gvn_dominance_frontiers(predecessors, idom)?;
    let mut definition_blocks = HashMap::<GvnMemoryVariable, BTreeSet<usize>>::new();
    let mut write_versions = HashMap::<(usize, usize, GvnMemoryVariable), GvnMemoryVersion>::new();
    let mut write_ordinal = 0usize;
    for (block, mir_block) in func.blocks.iter().enumerate() {
        for (instruction, inst) in mir_block.insts.iter().enumerate() {
            let effect = memory_effect::writes(inst);
            let ordinal = if effect.has_effect() {
                let ordinal = write_ordinal;
                write_ordinal = write_ordinal.checked_add(1)?;
                Some(ordinal)
            } else {
                None
            };
            for variable in gvn_affected_memory_variables(inst, &tracked)? {
                definition_blocks.entry(variable).or_default().insert(block);
                write_versions.insert(
                    (block, instruction, variable),
                    GvnMemoryVersion::Write {
                        ordinal: ordinal.expect("an affected variable belongs to a memory write"),
                        variable,
                    },
                );
            }
        }
    }

    let mut phis_by_block = vec![Vec::<GvnMemoryVariable>::new(); func.blocks.len()];
    for (variable, original_definitions) in definition_blocks {
        let mut definitions = original_definitions;
        let mut queue = definitions.iter().copied().collect::<VecDeque<_>>();
        let mut placed = BTreeSet::<usize>::new();
        while let Some(definition) = queue.pop_front() {
            for &frontier in &frontiers[definition] {
                if frontier == 0 || !placed.insert(frontier) {
                    continue;
                }
                phis_by_block[frontier].push(variable);
                if definitions.insert(frontier) {
                    queue.push_back(frontier);
                }
            }
        }
    }
    for phis in &mut phis_by_block {
        phis.sort_unstable_by_key(|variable| gvn_memory_variable_key(*variable));
    }

    let mut children = vec![Vec::<usize>::new(); func.blocks.len()];
    for (block, parent) in idom.iter().copied().enumerate().skip(1) {
        if let Some(parent) = parent {
            children[parent].push(block);
        }
    }

    enum Action {
        Enter(usize),
        Exit(Vec<(GvnMemoryVariable, Option<GvnMemoryVersion>)>),
    }
    let mut current = HashMap::<GvnMemoryVariable, GvnMemoryVersion>::new();
    let mut versions = HashMap::<(usize, usize), GvnLoadVersion>::new();
    let mut actions = vec![Action::Enter(0)];
    while let Some(action) = actions.pop() {
        let block = match action {
            Action::Exit(changes) => {
                for (variable, previous) in changes.into_iter().rev() {
                    if let Some(previous) = previous {
                        current.insert(variable, previous);
                    } else {
                        current.remove(&variable);
                    }
                }
                continue;
            }
            Action::Enter(block) => block,
        };
        let mut changes = Vec::new();
        for &variable in &phis_by_block[block] {
            let version = GvnMemoryVersion::Phi { block, variable };
            changes.push((variable, current.insert(variable, version)));
        }
        for (instruction, inst) in func.blocks[block].insts.iter().enumerate() {
            if let MInst::Load {
                base, offset, size, ..
            } = inst
            {
                versions.insert(
                    (block, instruction),
                    gvn_load_version(*base, *offset, *size, &current)?,
                );
            }
            for variable in gvn_affected_memory_variables(inst, &tracked)? {
                let version = *write_versions.get(&(block, instruction, variable))?;
                changes.push((variable, current.insert(variable, version)));
            }
        }
        actions.push(Action::Exit(changes));
        actions.extend(children[block].iter().rev().copied().map(Action::Enter));
    }
    Some(versions)
}

/// Global GVN: dominator-tree-scoped value numbering.
fn global_gvn(func: &mut MFunction) {
    let num_blocks = func.blocks.len();
    if num_blocks == 0 {
        return;
    }

    // Build block index map: BlockId → index
    let block_id_to_idx: HashMap<BlockId, usize> = func
        .blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.id, i))
        .collect();

    // Build predecessor lists and successor lists
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); num_blocks];
    let mut succs: Vec<Vec<usize>> = vec![Vec::new(); num_blocks];
    for (i, block) in func.blocks.iter().enumerate() {
        for succ_id in block.successors() {
            if let Some(&j) = block_id_to_idx.get(&succ_id) {
                succs[i].push(j);
                preds[j].push(i);
            }
        }
    }

    // Compute dominators using simple iterative algorithm (Cooper, Harvey, Kennedy)
    let idom = compute_dominators(num_blocks, &preds, &succs);
    let load_versions = compute_gvn_load_versions(func, &preds, &idom).unwrap_or_default();
    let (_, live_out) = compute_gvn_liveness(func, &block_id_to_idx, &succs);
    let last_uses = func
        .blocks
        .iter()
        .map(|block| {
            let mut uses = HashMap::new();
            for (instruction, inst) in block.insts.iter().enumerate() {
                for value in inst.uses() {
                    uses.insert(value, instruction);
                }
            }
            uses
        })
        .collect::<Vec<_>>();

    // Build dominator tree children
    let mut dom_children: Vec<Vec<usize>> = vec![Vec::new(); num_blocks];
    for (i, dom) in idom.iter().enumerate().skip(1) {
        if let Some(parent) = dom {
            dom_children[*parent].push(i);
        }
    }

    // Every VReg starts in its own value class.  Processing a copy or a
    // redundant expression merges the destination into an existing class.
    // Keeping this separate from the scoped expression table is what makes
    // this value numbering rather than an operand-identical CSE pass.
    let vreg_count = func.vregs.count() as usize;
    let mut value_numbers = (0..func.vregs.count()).collect::<Vec<ValueNumber>>();
    let mut value_leaders = (0..func.vregs.count()).map(VReg).collect::<Vec<_>>();
    let mut leader_blocks = vec![None; vreg_count];
    debug_assert_eq!(value_numbers.len(), vreg_count);

    let mut value_table: HashMap<GvnKey, ValueNumber> = HashMap::new();
    let mut table_changes: Vec<(GvnKey, Option<ValueNumber>)> = Vec::new();
    let mut leader_changes: Vec<(ValueNumber, VReg, Option<usize>)> = Vec::new();
    let mut replacements: Vec<(usize, usize, MInst)> = Vec::new(); // (block_idx, inst_idx, new_inst)

    // Dominator-scoped GVN. Every table mutation is undo-logged so sibling
    // subtrees see exactly the expression scope at their common dominator.
    // Load validity is carried by the structural MemorySSA version in its key,
    // rather than by a path-local store invalidation side table.
    fn gvn_dfs(
        node: usize,
        dom_children: &[Vec<usize>],
        func: &MFunction,
        value_numbers: &mut [ValueNumber],
        value_leaders: &mut [VReg],
        leader_blocks: &mut [Option<usize>],
        live_out: &[HashSet<VReg>],
        last_uses: &[HashMap<VReg, usize>],
        load_versions: &HashMap<(usize, usize), GvnLoadVersion>,
        value_table: &mut HashMap<GvnKey, ValueNumber>,
        table_changes: &mut Vec<(GvnKey, Option<ValueNumber>)>,
        leader_changes: &mut Vec<(ValueNumber, VReg, Option<usize>)>,
        replacements: &mut Vec<(usize, usize, MInst)>,
    ) {
        let checkpoint = table_changes.len();
        let leader_checkpoint = leader_changes.len();
        let block = &func.blocks[node];

        process_gvn_block(
            node,
            block,
            value_numbers,
            value_leaders,
            leader_blocks,
            &live_out[node],
            &last_uses[node],
            load_versions,
            value_table,
            table_changes,
            leader_changes,
            replacements,
        );

        for &child in &dom_children[node] {
            gvn_dfs(
                child,
                dom_children,
                func,
                value_numbers,
                value_leaders,
                leader_blocks,
                live_out,
                last_uses,
                load_versions,
                value_table,
                table_changes,
                leader_changes,
                replacements,
            );
        }

        while leader_changes.len() > leader_checkpoint {
            let (number, leader, leader_block) = leader_changes.pop().unwrap();
            value_leaders[number as usize] = leader;
            leader_blocks[number as usize] = leader_block;
        }

        while table_changes.len() > checkpoint {
            let (key, previous) = table_changes.pop().unwrap();
            if let Some(previous) = previous {
                value_table.insert(key, previous);
            } else {
                value_table.remove(&key);
            }
        }
    }

    fn process_gvn_block(
        node: usize,
        block: &MBlock,
        value_numbers: &mut [ValueNumber],
        value_leaders: &mut [VReg],
        leader_blocks: &mut [Option<usize>],
        live_out: &HashSet<VReg>,
        last_uses: &HashMap<VReg, usize>,
        load_versions: &HashMap<(usize, usize), GvnLoadVersion>,
        value_table: &mut HashMap<GvnKey, ValueNumber>,
        table_changes: &mut Vec<(GvnKey, Option<ValueNumber>)>,
        leader_changes: &mut Vec<(ValueNumber, VReg, Option<usize>)>,
        replacements: &mut Vec<(usize, usize, MInst)>,
    ) {
        for inst_idx in 0..block.insts.len() {
            let inst = &block.insts[inst_idx];

            if let MInst::Mov { dst, src } = inst {
                let number = gvn_value(value_numbers, *src);
                value_numbers[dst.0 as usize] = number;
                continue;
            }

            if let Some(key) = gvn_key(inst, value_numbers, load_versions.get(&(node, inst_idx))) {
                let dst = inst
                    .def()
                    .expect("every value-numbered MIR instruction must define a VReg");
                if let Some(&number) = value_table.get(&key) {
                    let leader = value_leaders[number as usize];
                    value_numbers[dst.0 as usize] = number;
                    let leader_block = leader_blocks[number as usize];
                    let reuse_does_not_extend_live_range = live_out.contains(&leader)
                        || last_uses
                            .get(&leader)
                            .is_some_and(|last_use| *last_use >= inst_idx);
                    let allocator_can_choose = leader_block == Some(node)
                        && allocator_can_recover_extended_gvn_leader(inst);
                    if dst != leader && (reuse_does_not_extend_live_range || allocator_can_choose) {
                        replacements.push((node, inst_idx, MInst::Mov { dst, src: leader }));
                    } else if dst != leader {
                        // The expression is available, but reusing its original
                        // leader would keep that VReg alive solely for this CSE.
                        // Keep the recomputation and make it the nearest leader
                        // for the current dominator subtree instead.
                        leader_changes.push((number, leader, leader_block));
                        value_leaders[number as usize] = dst;
                        leader_blocks[number as usize] = Some(node);
                    }
                } else {
                    let number = value_numbers[dst.0 as usize];
                    leader_changes.push((
                        number,
                        value_leaders[number as usize],
                        leader_blocks[number as usize],
                    ));
                    value_leaders[number as usize] = dst;
                    leader_blocks[number as usize] = Some(node);
                    let previous = value_table.insert(key.clone(), number);
                    debug_assert!(previous.is_none());
                    table_changes.push((key, previous));
                }
            }
        }
    }

    gvn_dfs(
        0,
        &dom_children,
        func,
        &mut value_numbers,
        &mut value_leaders,
        &mut leader_blocks,
        &live_out,
        &last_uses,
        &load_versions,
        &mut value_table,
        &mut table_changes,
        &mut leader_changes,
        &mut replacements,
    );
    debug_assert!(value_table.is_empty());
    debug_assert!(table_changes.is_empty());
    debug_assert!(leader_changes.is_empty());

    // Apply replacements
    for (bi, inst_idx, new_inst) in replacements {
        func.blocks[bi].insts[inst_idx] = new_inst;
    }
}

/// Compute conventional SSA block-entry and block-exit liveness for GVN's
/// profitability check. Phi sources are uses on predecessor edges; phi
/// destinations are definitions at the successor entry.
fn compute_gvn_liveness(
    func: &MFunction,
    block_id_to_idx: &HashMap<BlockId, usize>,
    succs: &[Vec<usize>],
) -> (Vec<HashSet<VReg>>, Vec<HashSet<VReg>>) {
    let block_count = func.blocks.len();
    let mut uses = vec![HashSet::new(); block_count];
    let mut defs = vec![HashSet::new(); block_count];

    for (block_index, block) in func.blocks.iter().enumerate() {
        defs[block_index].extend(block.phis.iter().map(|phi| phi.dst));
        for inst in &block.insts {
            for used in inst.uses() {
                if !defs[block_index].contains(&used) {
                    uses[block_index].insert(used);
                }
            }
            if let Some(defined) = inst.def() {
                defs[block_index].insert(defined);
            }
        }
    }

    let mut live_in = vec![HashSet::new(); block_count];
    let mut changed = true;
    while changed {
        changed = false;
        for block_index in (0..block_count).rev() {
            let block_id = func.blocks[block_index].id;
            let mut live_out = HashSet::new();
            for &successor in &succs[block_index] {
                live_out.extend(live_in[successor].iter().copied());
                for phi in &func.blocks[successor].phis {
                    if let Some((_, source)) = phi
                        .sources
                        .iter()
                        .find(|(predecessor, _)| *predecessor == block_id)
                    {
                        live_out.insert(*source);
                    }
                }
            }
            let mut next = uses[block_index].clone();
            next.extend(
                live_out
                    .into_iter()
                    .filter(|value| !defs[block_index].contains(value)),
            );
            if next != live_in[block_index] {
                live_in[block_index] = next;
                changed = true;
            }
        }
    }

    let mut live_out = vec![HashSet::new(); block_count];
    for (block_index, block) in func.blocks.iter().enumerate() {
        for &successor in &succs[block_index] {
            live_out[block_index].extend(live_in[successor].iter().copied());
            for phi in &func.blocks[successor].phis {
                if let Some((_, source)) = phi
                    .sources
                    .iter()
                    .find(|(predecessor, _)| *predecessor == block.id)
                {
                    live_out[block_index].insert(*source);
                }
            }
        }
    }

    debug_assert!(
        func.blocks
            .iter()
            .all(|block| block_id_to_idx.contains_key(&block.id))
    );
    (live_in, live_out)
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

// ────────────────────────────────────────────────────────────────
// Phase 1D: Algebraic simplification
// ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct MaskAndSite {
    block: usize,
    instruction: usize,
}

struct MaskAndPlan {
    block: usize,
    root: usize,
    nodes: Vec<usize>,
    combined_mask: u64,
}

fn is_mask_and_instruction(inst: &MInst) -> bool {
    matches!(
        inst,
        MInst::And { .. } | MInst::And32 { .. } | MInst::AndImm { .. } | MInst::AndImm32 { .. }
    )
}

fn resolve_mask_alias(mut value: VReg, aliases: &HashMap<VReg, VReg>) -> VReg {
    while let Some(&source) = aliases.get(&value) {
        value = source;
    }
    value
}

fn transient_vreg(func: &mut MFunction) -> VReg {
    let value = func.vregs.alloc();
    while func.spill_descs.len() <= value.0 as usize {
        func.spill_descs.push(SpillDesc::transient());
    }
    value
}

/// Sink leaf truncations through an exclusively consumed bitwise AND
/// component.
///
/// Element-strided state deliberately permits non-zero physical padding, so a
/// load of a one-bit element really does require `& 1`. Applying that mask to
/// every leaf is redundant, however:
///
/// `(a & m) & (b & m) & (c & m) == (a & b & c) & m`.
///
/// Definitions stay at their original positions. Mask destinations become
/// aliases of their unmasked sources, each existing AND consumes those aliases,
/// and one combined mask remains at the component root. Restricting traversal
/// to single-use definitions in one block prevents live-range extension across
/// CFG edges and leaves shared expressions untouched.
///
/// Definition, use, and consumer tables are built once. Exclusive components
/// are disjoint, so total traversal and rewriting are O(instructions + operand
/// edges) time and O(vregs + instructions) space.
fn sink_bitwise_and_masks(func: &mut MFunction) {
    let value_count = func.vregs.count() as usize;
    let mut definitions = vec![None::<MaskAndSite>; value_count];
    let mut use_counts = vec![0usize; value_count];
    let mut consumers = vec![None::<MaskAndSite>; value_count];

    for (block_index, block) in func.blocks.iter().enumerate() {
        for phi in &block.phis {
            for &(_, source) in &phi.sources {
                let index = source.0 as usize;
                use_counts[index] += 1;
                consumers[index] = None;
            }
        }
        for (instruction_index, instruction) in block.insts.iter().enumerate() {
            if let Some(dst) = instruction.def() {
                definitions[dst.0 as usize] = Some(MaskAndSite {
                    block: block_index,
                    instruction: instruction_index,
                });
            }
            let site = MaskAndSite {
                block: block_index,
                instruction: instruction_index,
            };
            for source in instruction.uses() {
                let index = source.0 as usize;
                if use_counts[index] == 0 {
                    consumers[index] = Some(site);
                } else {
                    consumers[index] = None;
                }
                use_counts[index] += 1;
            }
        }
    }

    let mut plans = Vec::new();
    for (block_index, block) in func.blocks.iter().enumerate() {
        for (root_index, root_instruction) in block.insts.iter().enumerate() {
            if !is_mask_and_instruction(root_instruction) {
                continue;
            }
            let root_value = root_instruction
                .def()
                .expect("mask/AND instruction must define a value");
            let root_value_index = root_value.0 as usize;
            let has_local_parent = use_counts[root_value_index] == 1
                && consumers[root_value_index].is_some_and(|consumer| {
                    consumer.block == block_index
                        && is_mask_and_instruction(
                            &func.blocks[consumer.block].insts[consumer.instruction],
                        )
                });
            if has_local_parent {
                continue;
            }

            let mut stack = vec![(root_value, true)];
            let mut nodes = Vec::new();
            let mut visited = HashSet::new();
            let mut combined_mask = u64::MAX;
            let mut immediate_masks = 0usize;
            let mut has_binary_and = false;

            while let Some((value, is_root)) = stack.pop() {
                let value_index = value.0 as usize;
                if !is_root && use_counts[value_index] != 1 {
                    continue;
                }
                let Some(site) = definitions[value_index] else {
                    continue;
                };
                if site.block != block_index || !visited.insert(site.instruction) {
                    continue;
                }
                match &func.blocks[site.block].insts[site.instruction] {
                    MInst::And { lhs, rhs, .. } => {
                        has_binary_and = true;
                        nodes.push(site.instruction);
                        stack.push((*rhs, false));
                        stack.push((*lhs, false));
                    }
                    MInst::And32 { lhs, rhs, .. } => {
                        has_binary_and = true;
                        combined_mask &= u64::from(u32::MAX);
                        nodes.push(site.instruction);
                        stack.push((*rhs, false));
                        stack.push((*lhs, false));
                    }
                    MInst::AndImm { src, imm, .. } => {
                        immediate_masks += 1;
                        combined_mask &= *imm;
                        nodes.push(site.instruction);
                        stack.push((*src, false));
                    }
                    MInst::AndImm32 { src, imm, .. } => {
                        immediate_masks += 1;
                        combined_mask &= u64::from(*imm);
                        nodes.push(site.instruction);
                        stack.push((*src, false));
                    }
                    _ => {}
                }
            }

            if has_binary_and && immediate_masks >= 2 {
                nodes.sort_unstable();
                plans.push(MaskAndPlan {
                    block: block_index,
                    root: root_index,
                    nodes,
                    combined_mask,
                });
            }
        }
    }

    let mut removals = vec![HashSet::<usize>::new(); func.blocks.len()];
    let mut replacements = vec![BTreeMap::<usize, Vec<MInst>>::new(); func.blocks.len()];
    for plan in plans {
        let root_instruction = func.blocks[plan.block].insts[plan.root].clone();
        let root_is_mask = matches!(
            root_instruction,
            MInst::AndImm { .. } | MInst::AndImm32 { .. }
        );
        let word32 = plan.combined_mask <= u64::from(u32::MAX);
        let root_raw_destination =
            (!root_is_mask && plan.combined_mask != u64::MAX).then(|| transient_vreg(func));
        let original = &func.blocks[plan.block].insts;
        let mut aliases = HashMap::new();

        for &instruction_index in &plan.nodes {
            match &original[instruction_index] {
                MInst::AndImm { dst, src, .. } | MInst::AndImm32 { dst, src, .. } => {
                    aliases.insert(*dst, resolve_mask_alias(*src, &aliases));
                    if instruction_index != plan.root {
                        removals[plan.block].insert(instruction_index);
                    }
                }
                _ => {}
            }
        }

        for &instruction_index in &plan.nodes {
            let replacement = match &original[instruction_index] {
                MInst::And { dst, lhs, rhs } | MInst::And32 { dst, lhs, rhs } => {
                    let lhs = resolve_mask_alias(*lhs, &aliases);
                    let rhs = resolve_mask_alias(*rhs, &aliases);
                    let raw_dst = if instruction_index == plan.root {
                        root_raw_destination.unwrap_or(*dst)
                    } else {
                        *dst
                    };
                    let raw = if word32 {
                        MInst::And32 {
                            dst: raw_dst,
                            lhs,
                            rhs,
                        }
                    } else {
                        MInst::And {
                            dst: raw_dst,
                            lhs,
                            rhs,
                        }
                    };
                    if instruction_index == plan.root
                        && !root_is_mask
                        && plan.combined_mask != u64::MAX
                    {
                        let mask = if word32 {
                            MInst::AndImm32 {
                                dst: *dst,
                                src: raw_dst,
                                imm: plan.combined_mask as u32,
                            }
                        } else {
                            MInst::AndImm {
                                dst: *dst,
                                src: raw_dst,
                                imm: plan.combined_mask,
                            }
                        };
                        vec![raw, mask]
                    } else {
                        vec![raw]
                    }
                }
                MInst::AndImm { dst, src, .. } | MInst::AndImm32 { dst, src, .. }
                    if instruction_index == plan.root =>
                {
                    let src = resolve_mask_alias(*src, &aliases);
                    if plan.combined_mask == u64::MAX {
                        vec![MInst::Mov { dst: *dst, src }]
                    } else if word32 {
                        vec![MInst::AndImm32 {
                            dst: *dst,
                            src,
                            imm: plan.combined_mask as u32,
                        }]
                    } else {
                        vec![MInst::AndImm {
                            dst: *dst,
                            src,
                            imm: plan.combined_mask,
                        }]
                    }
                }
                _ => continue,
            };
            replacements[plan.block].insert(instruction_index, replacement);
        }
    }

    for (block_index, block) in func.blocks.iter_mut().enumerate() {
        if removals[block_index].is_empty() && replacements[block_index].is_empty() {
            continue;
        }
        let original = std::mem::take(&mut block.insts);
        let mut rewritten = Vec::with_capacity(original.len());
        for (instruction_index, instruction) in original.into_iter().enumerate() {
            if removals[block_index].contains(&instruction_index) {
                continue;
            }
            if let Some(replacement) = replacements[block_index].remove(&instruction_index) {
                rewritten.extend(replacement);
            } else {
                rewritten.push(instruction);
            }
        }
        block.insts = rewritten;
    }
}

/// Algebraic simplification: identity, annihilation, self-inverse, and
/// strength reduction rules.
fn algebraic_simplify(func: &mut MFunction) {
    sink_bitwise_and_masks(func);

    // Build def map for constant lookups
    let mut consts: HashMap<VReg, u64> = HashMap::new();
    let mut and_immediates: HashMap<VReg, (VReg, u64)> = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            match inst {
                MInst::LoadImm { dst, value } => {
                    consts.insert(*dst, *value);
                }
                MInst::AndImm { dst, src, imm } => {
                    and_immediates.insert(*dst, (*src, *imm));
                }
                _ => {}
            }
        }
    }

    for block in &mut func.blocks {
        for inst in &mut block.insts {
            let replacement = match inst {
                // Identity: add x, 0 → x
                MInst::Add { dst, lhs, rhs } => {
                    if consts.get(rhs) == Some(&0) {
                        Some(Simplification::Mov(*dst, *lhs))
                    } else if consts.get(lhs) == Some(&0) {
                        Some(Simplification::Mov(*dst, *rhs))
                    } else {
                        None
                    }
                }
                // The 32-bit form includes a zero extension, so its identity
                // replacement must remain Mov32 rather than a full-word copy.
                MInst::Add32 { dst, lhs, rhs } => {
                    if const32(&consts, *rhs) == Some(0) {
                        Some(Simplification::Mov32(*dst, *lhs))
                    } else if const32(&consts, *lhs) == Some(0) {
                        Some(Simplification::Mov32(*dst, *rhs))
                    } else {
                        None
                    }
                }
                // Identity: sub x, 0 → x; self: sub x, x → 0
                MInst::Sub { dst, lhs, rhs } => {
                    if consts.get(rhs) == Some(&0) {
                        Some(Simplification::Mov(*dst, *lhs))
                    } else if lhs == rhs {
                        Some(Simplification::Const(*dst, 0))
                    } else {
                        None
                    }
                }
                MInst::Sub32 { dst, lhs, rhs } => {
                    if const32(&consts, *rhs) == Some(0) {
                        Some(Simplification::Mov32(*dst, *lhs))
                    } else if lhs == rhs {
                        Some(Simplification::Const(*dst, 0))
                    } else {
                        None
                    }
                }
                // Identity: mul x, 1 → x; annihilation: mul x, 0 → 0
                // Strength reduction: mul x, 2^n → shl x, n
                MInst::Mul { dst, lhs, rhs } => try_simplify_mul(*dst, *lhs, *rhs, &consts),
                MInst::Mul32 { dst, lhs, rhs } => {
                    if const32(&consts, *rhs) == Some(1) {
                        Some(Simplification::Mov32(*dst, *lhs))
                    } else if const32(&consts, *lhs) == Some(1) {
                        Some(Simplification::Mov32(*dst, *rhs))
                    } else if const32(&consts, *rhs) == Some(0) || const32(&consts, *lhs) == Some(0)
                    {
                        Some(Simplification::Const(*dst, 0))
                    } else {
                        None
                    }
                }
                // Identity: and x, -1 → x; annihilation: and x, 0 → 0
                MInst::And { dst, lhs, rhs } => {
                    if consts.get(rhs) == Some(&u64::MAX) {
                        Some(Simplification::Mov(*dst, *lhs))
                    } else if consts.get(lhs) == Some(&u64::MAX) {
                        Some(Simplification::Mov(*dst, *rhs))
                    } else if consts.get(rhs) == Some(&0) || consts.get(lhs) == Some(&0) {
                        Some(Simplification::Const(*dst, 0))
                    } else if lhs == rhs {
                        Some(Simplification::Mov(*dst, *lhs))
                    } else {
                        None
                    }
                }
                MInst::And32 { dst, lhs, rhs } => {
                    if const32(&consts, *rhs) == Some(u32::MAX) {
                        Some(Simplification::Mov32(*dst, *lhs))
                    } else if const32(&consts, *lhs) == Some(u32::MAX) {
                        Some(Simplification::Mov32(*dst, *rhs))
                    } else if const32(&consts, *rhs) == Some(0) || const32(&consts, *lhs) == Some(0)
                    {
                        Some(Simplification::Const(*dst, 0))
                    } else if lhs == rhs {
                        Some(Simplification::Mov32(*dst, *lhs))
                    } else {
                        None
                    }
                }
                // Identity: or x, 0 → x; self: or x, x → x
                MInst::Or { dst, lhs, rhs } => {
                    if consts.get(rhs) == Some(&0) {
                        Some(Simplification::Mov(*dst, *lhs))
                    } else if consts.get(lhs) == Some(&0) {
                        Some(Simplification::Mov(*dst, *rhs))
                    } else if lhs == rhs {
                        Some(Simplification::Mov(*dst, *lhs))
                    } else {
                        None
                    }
                }
                MInst::Or32 { dst, lhs, rhs } => {
                    if const32(&consts, *rhs) == Some(0) {
                        Some(Simplification::Mov32(*dst, *lhs))
                    } else if const32(&consts, *lhs) == Some(0) {
                        Some(Simplification::Mov32(*dst, *rhs))
                    } else if lhs == rhs {
                        Some(Simplification::Mov32(*dst, *lhs))
                    } else {
                        None
                    }
                }
                // Identity: xor x, 0 → x; self: xor x, x → 0
                MInst::Xor { dst, lhs, rhs } => {
                    if consts.get(rhs) == Some(&0) {
                        Some(Simplification::Mov(*dst, *lhs))
                    } else if consts.get(lhs) == Some(&0) {
                        Some(Simplification::Mov(*dst, *rhs))
                    } else if lhs == rhs {
                        Some(Simplification::Const(*dst, 0))
                    } else {
                        None
                    }
                }
                MInst::Xor32 { dst, lhs, rhs } => {
                    if const32(&consts, *rhs) == Some(0) {
                        Some(Simplification::Mov32(*dst, *lhs))
                    } else if const32(&consts, *lhs) == Some(0) {
                        Some(Simplification::Mov32(*dst, *rhs))
                    } else if lhs == rhs {
                        Some(Simplification::Const(*dst, 0))
                    } else {
                        None
                    }
                }
                // Identity: shr/shl/sar x, 0 → x
                MInst::Shr { dst, lhs, rhs }
                | MInst::Shl { dst, lhs, rhs }
                | MInst::Sar { dst, lhs, rhs } => {
                    if consts.get(rhs) == Some(&0) {
                        Some(Simplification::Mov(*dst, *lhs))
                    } else {
                        None
                    }
                }
                MInst::ShrImm { dst, src, imm: 0 }
                | MInst::ShlImm { dst, src, imm: 0 }
                | MInst::SarImm { dst, src, imm: 0 } => Some(Simplification::Mov(*dst, *src)),
                // AND chain: and(x, m) with immediate where m is mask
                MInst::AndImm { dst, src, imm } => {
                    if *imm == u64::MAX {
                        Some(Simplification::Mov(*dst, *src))
                    } else if *imm == 0 {
                        Some(Simplification::Const(*dst, 0))
                    } else {
                        None
                    }
                }
                MInst::AndImm32 { dst, src, imm } => {
                    if *imm == u32::MAX {
                        Some(Simplification::Mov32(*dst, *src))
                    } else if *imm == 0 {
                        Some(Simplification::Const(*dst, 0))
                    } else {
                        None
                    }
                }
                // OrImm identity and `(x & keep) | set → x | set` when the OR
                // restores every bit cleared by the preceding AND.
                MInst::OrImm { dst, src, imm } => {
                    if *imm == 0 {
                        Some(Simplification::Mov(*dst, *src))
                    } else if and_immediates
                        .get(src)
                        .is_some_and(|(_, keep_mask)| *keep_mask | *imm == u64::MAX)
                    {
                        let (original, _) = and_immediates[src];
                        Some(Simplification::OrImm(*dst, original, *imm))
                    } else {
                        None
                    }
                }
                // Double negate
                MInst::BitNot { dst, src } => {
                    if let Some(&c) = consts.get(src) {
                        Some(Simplification::Const(*dst, !c))
                    } else {
                        None
                    }
                }
                MInst::Neg { dst, src } => {
                    if let Some(&c) = consts.get(src) {
                        Some(Simplification::Const(*dst, c.wrapping_neg()))
                    } else {
                        None
                    }
                }
                // Select with constant condition
                MInst::Select {
                    dst,
                    cond,
                    true_val,
                    false_val,
                } => {
                    if let Some(&c) = consts.get(cond) {
                        if c != 0 {
                            Some(Simplification::Mov(*dst, *true_val))
                        } else {
                            Some(Simplification::Mov(*dst, *false_val))
                        }
                    } else {
                        None
                    }
                }
                // Mov of constant → LoadImm (enables further constant folding)
                MInst::Mov { dst, src } => {
                    if let Some(&c) = consts.get(src) {
                        Some(Simplification::Const(*dst, c))
                    } else {
                        None
                    }
                }
                _ => None,
            };

            if let Some(simp) = replacement {
                match simp {
                    Simplification::Mov(dst, src) => {
                        *inst = MInst::Mov { dst, src };
                    }
                    Simplification::Mov32(dst, src) => {
                        *inst = MInst::Mov32 { dst, src };
                    }
                    Simplification::Const(dst, value) => {
                        *inst = MInst::LoadImm { dst, value };
                        consts.insert(dst, value);
                    }
                    Simplification::Shl(dst, src, imm) => {
                        *inst = MInst::ShlImm { dst, src, imm };
                    }
                    Simplification::OrImm(dst, src, imm) => {
                        *inst = MInst::OrImm { dst, src, imm };
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
struct ContiguousLoadPack {
    base: BaseReg,
    memory_bytes: [Option<i64>; 8],
    earliest_instruction: usize,
}

impl ContiguousLoadPack {
    fn from_load(base: BaseReg, offset: i32, size: OpSize, instruction: usize) -> Self {
        let mut memory_bytes = [None; 8];
        for (byte, slot) in memory_bytes
            .iter_mut()
            .enumerate()
            .take(size.bytes() as usize)
        {
            *slot = Some(i64::from(offset) + byte as i64);
        }
        Self {
            base,
            memory_bytes,
            earliest_instruction: instruction,
        }
    }

    fn shifted_left(self, bits: u8) -> Option<Self> {
        if !bits.is_multiple_of(8) || bits >= 64 {
            return None;
        }
        let bytes = usize::from(bits / 8);
        if self.memory_bytes[8 - bytes..].iter().any(Option::is_some) {
            return None;
        }
        let mut memory_bytes = [None; 8];
        memory_bytes[bytes..].copy_from_slice(&self.memory_bytes[..8 - bytes]);
        Some(Self {
            memory_bytes,
            ..self
        })
    }

    fn merged(self, other: Self) -> Option<Self> {
        if self.base != other.base {
            return None;
        }
        let mut memory_bytes = self.memory_bytes;
        for (destination, source) in memory_bytes.iter_mut().zip(other.memory_bytes) {
            if destination.is_some() && source.is_some() {
                return None;
            }
            if destination.is_none() {
                *destination = source;
            }
        }
        Some(Self {
            base: self.base,
            memory_bytes,
            earliest_instruction: self.earliest_instruction.min(other.earliest_instruction),
        })
    }

    fn native_load(self) -> Option<(i32, OpSize)> {
        for (byte_len, size) in [
            (8usize, OpSize::S64),
            (4usize, OpSize::S32),
            (2usize, OpSize::S16),
        ] {
            if self.memory_bytes[byte_len..].iter().any(Option::is_some) {
                continue;
            }
            let start = self.memory_bytes[0]?;
            if self.memory_bytes[..byte_len]
                .iter()
                .enumerate()
                .all(|(byte, address)| *address == Some(start + byte as i64))
            {
                return Some((i32::try_from(start).ok()?, size));
            }
        }
        None
    }
}

/// Replace little-endian packs of adjacent scalar loads with one native load.
///
/// Frontend Concat lowering commonly produces:
///
///     load.i8 [p+0] | (load.i8 [p+1] << 8) | ...
///
/// Keeping those as separate SSA values creates seven unnecessary ALU
/// definitions and eight memory operations for one 64-bit value.  Summaries
/// are propagated forward once per instruction, so discovery is linear.  A
/// pack is materialized only if no memory write of any kind occurred between
/// its earliest scalar load and the root; this preserves the common memory
/// version without moving a read across an effect.
fn fold_contiguous_load_packs(func: &mut MFunction) {
    for block in &mut func.blocks {
        let mut summaries = HashMap::<VReg, ContiguousLoadPack>::new();
        let mut last_write = None::<usize>;

        for instruction_index in 0..block.insts.len() {
            let original = block.insts[instruction_index].clone();
            let summary = match original {
                MInst::Load {
                    base, offset, size, ..
                } => Some(ContiguousLoadPack::from_load(
                    base,
                    offset,
                    size,
                    instruction_index,
                )),
                MInst::ShlImm { src, imm, .. } => summaries
                    .get(&src)
                    .copied()
                    .and_then(|summary| summary.shifted_left(imm)),
                MInst::Or { lhs, rhs, .. } => summaries
                    .get(&lhs)
                    .copied()
                    .zip(summaries.get(&rhs).copied())
                    .and_then(|(lhs, rhs)| lhs.merged(rhs)),
                MInst::Mov { src, .. } => summaries.get(&src).copied(),
                MInst::Mov32 { src, .. } => summaries
                    .get(&src)
                    .copied()
                    .filter(|summary| summary.memory_bytes[4..].iter().all(Option::is_none)),
                MInst::OrImm { src, imm: 0, .. } => summaries.get(&src).copied(),
                _ => None,
            };

            if let (Some(dst), Some(summary)) = (original.def(), summary) {
                if matches!(original, MInst::Or { .. })
                    && last_write.is_none_or(|write| write < summary.earliest_instruction)
                    && let Some((offset, size)) = summary.native_load()
                {
                    block.insts[instruction_index] = MInst::Load {
                        dst,
                        base: summary.base,
                        offset,
                        size,
                    };
                }
                summaries.insert(dst, summary);
            }

            if memory_effect::writes(&original).has_effect() {
                last_write = Some(instruction_index);
            }
        }
    }
}

/// Select flag-consuming branch forms before allocation.
///
/// A compare or direct load whose only use is the immediately following
/// branch has no independently observable SSA result. Keeping that result
/// until emission invents a live range and can make the allocator spill around
/// a value which the machine code never materializes.
///
/// Direct-memory predicates are selected by a separate late pass after
/// StateSSA forwarding. Register comparisons are selected exactly once at the
/// register-allocation boundary, before pressure scheduling.
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
    let mut use_counts = HashMap::<VReg, usize>::new();
    for block in &func.blocks {
        for phi in &block.phis {
            for &(_, source) in &phi.sources {
                *use_counts.entry(source).or_default() += 1;
            }
        }
        for instruction in &block.insts {
            for source in instruction.uses() {
                *use_counts.entry(source).or_default() += 1;
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
        if use_counts.get(&cond).copied() != Some(1) {
            continue;
        }

        let predicate = match block.insts[block.insts.len() - 2].clone() {
            MInst::Cmp {
                dst,
                lhs,
                rhs,
                kind,
            } if matches!(class, BranchPredicateClass::Register) && dst == cond => {
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
        block.insts.truncate(block.insts.len() - 2);
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
    let mut redirect: HashMap<BlockId, BlockId> = HashMap::new();
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

    if redirect.is_empty() {
        return;
    }

    // Transitively resolve redirects
    let mut resolved: HashMap<BlockId, BlockId> = HashMap::new();
    for &src in redirect.keys() {
        let mut target = src;
        let mut seen = std::collections::HashSet::new();
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

// ────────────────────────────────────────────────────────────────
// Immediate-form lowering
// ────────────────────────────────────────────────────────────────

/// Convert operations with constant operands into immediate-form MIR.
/// This runs late (after CSE/constant fold) to maximize opportunities.
fn lower_to_imm_forms(func: &mut MFunction) {
    // Collect constants
    let mut consts: HashMap<VReg, u64> = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            if let MInst::LoadImm { dst, value } = inst {
                consts.insert(*dst, *value);
            }
        }
    }

    for block in &mut func.blocks {
        for inst in &mut block.insts {
            for use_vreg in inst.uses() {
                let Some(&value) = consts.get(&use_vreg) else {
                    continue;
                };
                let Some(folded) = fold_imm_use(inst, use_vreg, value) else {
                    continue;
                };
                *inst = folded;
                break;
            }
        }
    }
}

// ────────────────────────────────────────────────────────────────
// Existing passes
// ────────────────────────────────────────────────────────────────

/// Fold a single-bit clear-and-insert toggle into XOR.
///
/// Pattern:
///   `(x & ~(1 << s)) | ((((x >> s) & 1) ^ 1) << s)`
///
/// This is produced by dynamic bit-select XOR assignment such as
/// `x[s] ^= 1`. For 2-state values it is equivalent to `x ^ (1 << s)`.
fn fold_bit_toggle_insert(func: &mut MFunction) {
    let mut defs: HashMap<VReg, MInst> = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            if let Some(d) = inst.def() {
                defs.insert(d, inst.clone());
            }
        }
    }

    for block in &mut func.blocks {
        for inst in &mut block.insts {
            let MInst::Or { dst, lhs, rhs } = *inst else {
                continue;
            };

            if let Some((value, mask)) = match_bit_toggle_insert(lhs, rhs, &defs)
                .or_else(|| match_bit_toggle_insert(rhs, lhs, &defs))
            {
                *inst = MInst::Xor {
                    dst,
                    lhs: value,
                    rhs: mask,
                };
            }
        }
    }
}

fn match_bit_toggle_insert(
    clear_part: VReg,
    insert_part: VReg,
    defs: &HashMap<VReg, MInst>,
) -> Option<(VReg, VReg)> {
    let MInst::And {
        lhs: clear_lhs,
        rhs: clear_rhs,
        ..
    } = defs.get(&clear_part)?
    else {
        return None;
    };

    let (value, inverted_mask) = match defs.get(clear_lhs) {
        Some(MInst::BitNot { .. }) => (*clear_rhs, *clear_lhs),
        _ => match defs.get(clear_rhs) {
            Some(MInst::BitNot { .. }) => (*clear_lhs, *clear_rhs),
            _ => return None,
        },
    };

    let MInst::BitNot { src: mask, .. } = defs.get(&inverted_mask)? else {
        return None;
    };

    let MInst::Shl {
        lhs: one_for_mask,
        rhs: shift_for_mask,
        ..
    } = defs.get(mask)?
    else {
        return None;
    };
    if !is_const_one(*one_for_mask, defs) {
        return None;
    }

    let MInst::Shl {
        lhs: toggled_bit,
        rhs: shift_for_insert,
        ..
    } = defs.get(&insert_part)?
    else {
        return None;
    };
    if shift_for_insert != shift_for_mask {
        return None;
    }

    let MInst::Xor {
        lhs: xor_lhs,
        rhs: xor_rhs,
        ..
    } = defs.get(toggled_bit)?
    else {
        return None;
    };

    let extracted_bit = if is_const_one(*xor_lhs, defs) {
        *xor_rhs
    } else if is_const_one(*xor_rhs, defs) {
        *xor_lhs
    } else {
        return None;
    };

    let MInst::And {
        lhs: bit_lhs,
        rhs: bit_rhs,
        ..
    } = defs.get(&extracted_bit)?
    else {
        return None;
    };
    let shifted_value = if is_const_one(*bit_lhs, defs) {
        *bit_rhs
    } else if is_const_one(*bit_rhs, defs) {
        *bit_lhs
    } else {
        return None;
    };

    let MInst::Shr {
        lhs: shifted_src,
        rhs: shift_for_extract,
        ..
    } = defs.get(&shifted_value)?
    else {
        return None;
    };

    if *shifted_src == value && shift_for_extract == shift_for_mask {
        Some((value, *mask))
    } else {
        None
    }
}

fn is_const_one(reg: VReg, defs: &HashMap<VReg, MInst>) -> bool {
    matches!(defs.get(&reg), Some(MInst::LoadImm { value: 1, .. }))
}

/// Fold a bit-deposit OR chain into BMI2 PDEP.
///
/// Pattern:
///   `((src[0] << d0) | (src[1] << d1) | ...)`
/// where source bits are the contiguous low bits `0..N` and destination bits
/// are strictly increasing. This is exactly `pdep(src, mask)`.
fn fold_deposit_chain_to_pdep(func: &mut MFunction) {
    let mut defs: HashMap<VReg, MInst> = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            if let Some(d) = inst.def() {
                defs.insert(d, inst.clone());
            }
        }
    }

    for block in &mut func.blocks {
        let mut replacements: Vec<(usize, Vec<MInst>)> = Vec::new();

        for (inst_idx, inst) in block.insts.iter().enumerate() {
            let Some(dst) = inst.def() else { continue };
            if !matches!(inst, MInst::Or { .. } | MInst::OrImm { .. }) {
                continue;
            }

            let mut chunks: Vec<(u8, u8, u8)> = Vec::new();
            let mut source_reg: Option<VReg> = None;
            if !collect_deposit_chain_chunks(dst, &defs, &mut chunks, &mut source_reg) {
                continue;
            }

            let Some(src) = source_reg else { continue };
            let total_width: usize = chunks.iter().map(|(_, width, _)| *width as usize).sum();
            if !(8..=64).contains(&total_width) {
                continue;
            }
            chunks.sort_unstable();

            let mut mask_val = 0u64;
            let mut expected_src_lsb = 0u8;
            let mut prev_dst_end = 0u8;
            let mut valid = true;
            for &(src_lsb, width, dst_lsb) in &chunks {
                if width == 0
                    || src_lsb != expected_src_lsb
                    || src_lsb as u16 + width as u16 > 64
                    || dst_lsb as u16 + width as u16 > 64
                    || dst_lsb < prev_dst_end
                {
                    valid = false;
                    break;
                }
                for bit in dst_lsb..dst_lsb + width {
                    mask_val |= 1u64 << bit;
                }
                expected_src_lsb += width;
                prev_dst_end = dst_lsb + width;
            }
            if !valid || mask_val == 0 {
                continue;
            }

            let new_insts = if mask_width(mask_val) == Some(total_width) {
                if mask_val == u64::MAX {
                    vec![MInst::Mov { dst, src }]
                } else if u32::try_from(mask_val).is_ok() {
                    vec![MInst::AndImm {
                        dst,
                        src,
                        imm: mask_val,
                    }]
                } else {
                    let mask_vreg = func.vregs.alloc();
                    while func.spill_descs.len() <= mask_vreg.0 as usize {
                        func.spill_descs.push(SpillDesc::remat(mask_val));
                    }
                    vec![
                        MInst::LoadImm {
                            dst: mask_vreg,
                            value: mask_val,
                        },
                        MInst::And {
                            dst,
                            lhs: src,
                            rhs: mask_vreg,
                        },
                    ]
                }
            } else {
                let mask_vreg = func.vregs.alloc();
                while func.spill_descs.len() <= mask_vreg.0 as usize {
                    func.spill_descs.push(SpillDesc::remat(mask_val));
                }
                vec![
                    MInst::LoadImm {
                        dst: mask_vreg,
                        value: mask_val,
                    },
                    MInst::Pdep {
                        dst,
                        src,
                        mask: mask_vreg,
                    },
                ]
            };

            replacements.push((inst_idx, new_insts));
        }

        for (idx, new_insts) in replacements.into_iter().rev() {
            block.insts.splice(idx..=idx, new_insts);
        }
    }
}

fn collect_deposit_chain_chunks(
    reg: VReg,
    defs: &HashMap<VReg, MInst>,
    chunks: &mut Vec<(u8, u8, u8)>,
    source_reg: &mut Option<VReg>,
) -> bool {
    let Some(def) = defs.get(&reg) else {
        return false;
    };

    match def {
        MInst::Or { lhs, rhs, .. } => {
            collect_deposit_chain_chunks(*lhs, defs, chunks, source_reg)
                && collect_deposit_chain_chunks(*rhs, defs, chunks, source_reg)
        }
        MInst::OrImm { src, imm, .. } if *imm == 0 => {
            collect_deposit_chain_chunks(*src, defs, chunks, source_reg)
        }
        MInst::Mov { src, .. } => collect_deposit_chain_chunks(*src, defs, chunks, source_reg),
        _ => collect_deposit_term(reg, defs, chunks, source_reg),
    }
}

fn collect_deposit_term(
    reg: VReg,
    defs: &HashMap<VReg, MInst>,
    chunks: &mut Vec<(u8, u8, u8)>,
    source_reg: &mut Option<VReg>,
) -> bool {
    let Some((src, src_lsb, width, dst_lsb)) = trace_deposit_term(reg, defs) else {
        return false;
    };
    match source_reg {
        Some(existing) if *existing != src => return false,
        None => *source_reg = Some(src),
        _ => {}
    }
    chunks.push((src_lsb, width, dst_lsb));
    true
}

fn trace_deposit_term(reg: VReg, defs: &HashMap<VReg, MInst>) -> Option<(VReg, u8, u8, u8)> {
    trace_deposit_term_inner(reg, defs)
        .filter(|(_, _, width, dst_lsb)| *width > 0 && (*dst_lsb as u16 + *width as u16) <= 64)
}

fn trace_deposit_term_inner(reg: VReg, defs: &HashMap<VReg, MInst>) -> Option<(VReg, u8, u8, u8)> {
    let Some(def) = defs.get(&reg) else {
        return Some((reg, 0, 64, 0));
    };
    match def {
        MInst::Mov { src, .. } => trace_deposit_term_inner(*src, defs),
        MInst::ShlImm { src, imm, .. } if *imm < 64 => {
            let (base, src_lsb, width) = trace_value_window(*src, defs)?;
            Some((base, src_lsb, width.min(64 - *imm), *imm))
        }
        MInst::AndImm { src, imm, .. } => {
            let (base, src_lsb, width, dst_lsb) = trace_deposit_term_inner(*src, defs)?;
            let mask_w = mask_width(*imm)? as u8;
            Some((
                base,
                src_lsb,
                width.min(mask_w.saturating_sub(dst_lsb)),
                dst_lsb,
            ))
        }
        MInst::And { lhs, rhs, .. } => {
            if let Some(mask) = load_imm_value(*lhs, defs) {
                let (base, src_lsb, width, dst_lsb) = trace_deposit_term_inner(*rhs, defs)?;
                let mask_w = mask_width(mask)? as u8;
                Some((
                    base,
                    src_lsb,
                    width.min(mask_w.saturating_sub(dst_lsb)),
                    dst_lsb,
                ))
            } else if let Some(mask) = load_imm_value(*rhs, defs) {
                let (base, src_lsb, width, dst_lsb) = trace_deposit_term_inner(*lhs, defs)?;
                let mask_w = mask_width(mask)? as u8;
                Some((
                    base,
                    src_lsb,
                    width.min(mask_w.saturating_sub(dst_lsb)),
                    dst_lsb,
                ))
            } else {
                None
            }
        }
        _ => {
            let (base, src_lsb, width) = trace_value_window(reg, defs)?;
            Some((base, src_lsb, width, 0))
        }
    }
}

fn trace_value_window(reg: VReg, defs: &HashMap<VReg, MInst>) -> Option<(VReg, u8, u8)> {
    let Some(def) = defs.get(&reg) else {
        return Some((reg, 0, 64));
    };
    match def {
        MInst::Mov { src, .. } => trace_value_window(*src, defs),
        MInst::ShrImm { src, imm, .. } => {
            let (base, lsb, width) = trace_value_window(*src, defs).unwrap_or((*src, 0, 64));
            let new_lsb = lsb.checked_add(*imm)?;
            Some((base, new_lsb, width.saturating_sub(*imm)))
        }
        MInst::AndImm { src, imm, .. } => {
            let mask_w = mask_width(*imm)? as u8;
            if let Some((base, lsb, width)) = trace_value_window(*src, defs) {
                Some((base, lsb, width.min(mask_w)))
            } else {
                Some((reg, 0, mask_w))
            }
        }
        MInst::And { lhs, rhs, .. } => {
            if let Some(mask) = load_imm_value(*lhs, defs) {
                let mask_w = mask_width(mask)? as u8;
                if let Some((base, lsb, width)) = trace_value_window(*rhs, defs) {
                    Some((base, lsb, width.min(mask_w)))
                } else {
                    Some((reg, 0, mask_w))
                }
            } else if let Some(mask) = load_imm_value(*rhs, defs) {
                let mask_w = mask_width(mask)? as u8;
                if let Some((base, lsb, width)) = trace_value_window(*lhs, defs) {
                    Some((base, lsb, width.min(mask_w)))
                } else {
                    Some((reg, 0, mask_w))
                }
            } else {
                None
            }
        }
        MInst::LoadConstantTableAddr { .. }
        | MInst::Load { .. }
        | MInst::LoadIndexed { .. }
        | MInst::LoadPtr { .. } => Some((reg, 0, 64)),
        _ => None,
    }
}

fn load_imm_value(reg: VReg, defs: &HashMap<VReg, MInst>) -> Option<u64> {
    match defs.get(&reg)? {
        MInst::LoadImm { value, .. } => Some(*value),
        MInst::Mov { src, .. } => load_imm_value(*src, defs),
        _ => None,
    }
}

/// Fold a bit-extract OR chain into BMI2 PEXT.
///
/// Pattern:
///   `((src >> s0) & lowmask(w0)) << 0
///    | ((src >> s1) & lowmask(w1)) << w0 | ...`
/// where destination chunks are contiguous low bits and source chunks are
/// strictly increasing. This is `pext(src, mask)`.
fn fold_extract_chain_to_pext(func: &mut MFunction) {
    let mut defs: HashMap<VReg, MInst> = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            if let Some(d) = inst.def() {
                defs.insert(d, inst.clone());
            }
        }
    }

    for block in &mut func.blocks {
        let mut replacements: Vec<(usize, Vec<MInst>)> = Vec::new();

        for (inst_idx, inst) in block.insts.iter().enumerate() {
            let Some(dst) = inst.def() else { continue };
            if !matches!(inst, MInst::Or { .. } | MInst::OrImm { .. }) {
                continue;
            }

            let mut chunks: Vec<(u8, u8, u8)> = Vec::new();
            let mut source_reg: Option<VReg> = None;
            if !collect_deposit_chain_chunks(dst, &defs, &mut chunks, &mut source_reg) {
                continue;
            }

            let Some(src) = source_reg else { continue };
            let total_width: usize = chunks.iter().map(|(_, width, _)| *width as usize).sum();
            if !(8..=64).contains(&total_width) {
                continue;
            }
            chunks.sort_unstable_by_key(|(src_lsb, _, _)| *src_lsb);

            let mut mask_val = 0u64;
            let mut expected_dst_lsb = 0u8;
            let mut prev_src_end = 0u8;
            let mut valid = true;
            for &(src_lsb, width, dst_lsb) in &chunks {
                if width == 0
                    || dst_lsb != expected_dst_lsb
                    || src_lsb as u16 + width as u16 > 64
                    || dst_lsb as u16 + width as u16 > 64
                    || src_lsb < prev_src_end
                {
                    valid = false;
                    break;
                }
                for bit in src_lsb..src_lsb + width {
                    mask_val |= 1u64 << bit;
                }
                expected_dst_lsb += width;
                prev_src_end = src_lsb + width;
            }
            if !valid || mask_val == 0 {
                continue;
            }

            let new_insts = if mask_width(mask_val) == Some(total_width) {
                if mask_val == u64::MAX {
                    vec![MInst::Mov { dst, src }]
                } else if u32::try_from(mask_val).is_ok() {
                    vec![MInst::AndImm {
                        dst,
                        src,
                        imm: mask_val,
                    }]
                } else {
                    let mask_vreg = func.vregs.alloc();
                    while func.spill_descs.len() <= mask_vreg.0 as usize {
                        func.spill_descs.push(SpillDesc::remat(mask_val));
                    }
                    vec![
                        MInst::LoadImm {
                            dst: mask_vreg,
                            value: mask_val,
                        },
                        MInst::And {
                            dst,
                            lhs: src,
                            rhs: mask_vreg,
                        },
                    ]
                }
            } else {
                let mask_vreg = func.vregs.alloc();
                while func.spill_descs.len() <= mask_vreg.0 as usize {
                    func.spill_descs.push(SpillDesc::remat(mask_val));
                }
                vec![
                    MInst::LoadImm {
                        dst: mask_vreg,
                        value: mask_val,
                    },
                    MInst::Pext {
                        dst,
                        src,
                        mask: mask_vreg,
                    },
                ]
            };

            replacements.push((inst_idx, new_insts));
        }

        for (idx, new_insts) in replacements.into_iter().rev() {
            block.insts.splice(idx..=idx, new_insts);
        }
    }
}

/// Fold XOR chains of single-bit extractions from the same source into
/// PEXT + POPCNT + AND 1.
///
/// Pattern: `(src >> a) & 1 ^ (src >> b) & 1 ^ ...` where all extractions
/// come from the same source register.
///
/// Replacement: `pext(src, mask) → popcnt → and 1` where
/// `mask = (1 << a) | (1 << b) | ...`
fn fold_xor_chain_to_pext(func: &mut MFunction) {
    // Build def map: VReg → instruction (cloned to avoid borrowing func)
    let mut defs: HashMap<VReg, MInst> = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            if let Some(d) = inst.def() {
                defs.insert(d, inst.clone());
            }
        }
    }

    // For each block, scan for Xor instructions and try to fold
    for block in &mut func.blocks {
        let mut replacements: Vec<(usize, Vec<MInst>)> = Vec::new();

        for (inst_idx, inst) in block.insts.iter().enumerate() {
            // Look for: v = xor a, b  where result is 1-bit (used with and 1)
            let MInst::Xor { dst, lhs, rhs } = inst else {
                continue;
            };

            // Try to collect the full XOR chain and extract bit positions
            let mut bits: Vec<(VReg, u64)> = Vec::new();
            let mut source_reg: Option<VReg> = None;

            let ok = collect_xor_chain_bits(*dst, *lhs, *rhs, &defs, &mut bits, &mut source_reg);
            if !ok {
                continue;
            }

            // Need at least 3 bits to be worth the PEXT overhead
            let Some(src) = source_reg else { continue };
            if bits.len() < 3 {
                continue;
            }

            // Build mask from bit positions
            let mut mask_val: u64 = 0;
            for &(_, pos) in &bits {
                if pos >= 64 {
                    continue;
                } // skip wide
                mask_val |= 1u64 << pos;
            }
            if mask_val == 0 {
                continue;
            }

            // Generate: mask_vreg = imm mask_val
            //           pext_vreg = pext src, mask_vreg
            //           popcnt_vreg = popcnt pext_vreg
            //           dst = and popcnt_vreg, 1
            let mask_vreg = func.vregs.alloc();
            while func.spill_descs.len() <= mask_vreg.0 as usize {
                func.spill_descs.push(SpillDesc::remat(mask_val));
            }
            let pext_vreg = func.vregs.alloc();
            while func.spill_descs.len() <= pext_vreg.0 as usize {
                func.spill_descs.push(SpillDesc::transient());
            }
            let popcnt_vreg = func.vregs.alloc();
            while func.spill_descs.len() <= popcnt_vreg.0 as usize {
                func.spill_descs.push(SpillDesc::transient());
            }

            let new_insts = vec![
                MInst::LoadImm {
                    dst: mask_vreg,
                    value: mask_val,
                },
                MInst::Pext {
                    dst: pext_vreg,
                    src,
                    mask: mask_vreg,
                },
                MInst::Popcnt {
                    dst: popcnt_vreg,
                    src: pext_vreg,
                },
                MInst::AndImm {
                    dst: *dst,
                    src: popcnt_vreg,
                    imm: 1,
                },
            ];
            replacements.push((inst_idx, new_insts));
        }

        // Apply replacements in reverse order (to preserve indices)
        for (idx, new_insts) in replacements.into_iter().rev() {
            block.insts.splice(idx..=idx, new_insts);
        }
    }
}

/// Fold add trees of single-bit extractions from the same source into
/// `and mask` + `popcnt`.
///
/// Pattern: `(src >> a) & 1 + (src >> b) & 1 + ...`
/// Replacement:
///   if mask == all_ones: `popcnt src`
///   else: `masked = and src, mask; popcnt masked`
fn fold_add_chain_to_popcnt(func: &mut MFunction) {
    let mut defs: HashMap<VReg, MInst> = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            if let Some(d) = inst.def() {
                defs.insert(d, inst.clone());
            }
        }
    }

    for block in &mut func.blocks {
        let mut replacements: Vec<(usize, Vec<MInst>)> = Vec::new();

        for (inst_idx, inst) in block.insts.iter().enumerate() {
            let MInst::Add { dst, lhs, rhs } = inst else {
                continue;
            };

            let mut bits: Vec<(VReg, u64)> = Vec::new();
            let mut source_reg: Option<VReg> = None;

            if !collect_add_chain_bits(*lhs, &defs, &mut bits, &mut source_reg)
                || !collect_add_chain_bits(*rhs, &defs, &mut bits, &mut source_reg)
            {
                continue;
            }

            let Some(src) = source_reg else { continue };
            if bits.len() < 3 {
                continue;
            }

            let mut mask: u64 = 0;
            for &(_, bit) in &bits {
                if bit < 64 {
                    if (mask >> bit) & 1 == 1 {
                        mask = 0;
                        break;
                    }
                    mask |= 1u64 << bit;
                }
            }
            if mask == 0 {
                continue;
            }

            let all_bits_mask = if bits.len() >= 64 {
                u64::MAX
            } else {
                (1u64 << bits.len()) - 1
            };

            let new_insts = if mask == u64::MAX || mask == all_bits_mask {
                vec![MInst::Popcnt { dst: *dst, src }]
            } else {
                let masked_vreg = func.vregs.alloc();
                while func.spill_descs.len() <= masked_vreg.0 as usize {
                    func.spill_descs.push(SpillDesc::transient());
                }
                vec![
                    MInst::AndImm {
                        dst: masked_vreg,
                        src,
                        imm: mask,
                    },
                    MInst::Popcnt {
                        dst: *dst,
                        src: masked_vreg,
                    },
                ]
            };

            replacements.push((inst_idx, new_insts));
        }

        for (idx, new_insts) in replacements.into_iter().rev() {
            block.insts.splice(idx..=idx, new_insts);
        }
    }
}

/// Recursively collect single-bit extractions from a XOR chain.
/// Returns true if the entire chain consists of single-bit extractions
/// from the same source register.
fn collect_xor_chain_bits(
    _vreg: VReg,
    lhs: VReg,
    rhs: VReg,
    defs: &HashMap<VReg, MInst>,
    bits: &mut Vec<(VReg, u64)>,
    source_reg: &mut Option<VReg>,
) -> bool {
    // Try to extract a bit from each operand
    for &operand in &[lhs, rhs] {
        if let Some(def_inst) = defs.get(&operand) {
            match def_inst {
                // Pattern: v = xor a, b (recursive)
                MInst::Xor {
                    lhs: l2, rhs: r2, ..
                } => {
                    if !collect_xor_chain_bits(operand, *l2, *r2, defs, bits, source_reg) {
                        return false;
                    }
                }
                // Pattern: v = shr src, imm (bit extraction)
                MInst::ShrImm { src, imm, .. } => {
                    match source_reg {
                        Some(s) if *s != *src => return false, // different source
                        None => *source_reg = Some(*src),
                        _ => {}
                    }
                    bits.push((*src, *imm as u64));
                }
                // Pattern: v = and src, 1 (masked bit — look through)
                MInst::AndImm {
                    src: and_src,
                    imm: 1,
                    ..
                } => {
                    if let Some(inner) = defs.get(and_src) {
                        match inner {
                            MInst::ShrImm { src, imm, .. } => {
                                match source_reg {
                                    Some(s) if *s != *src => return false,
                                    None => *source_reg = Some(*src),
                                    _ => {}
                                }
                                bits.push((*src, *imm as u64));
                            }
                            MInst::Xor {
                                lhs: l2, rhs: r2, ..
                            } => {
                                if !collect_xor_chain_bits(
                                    *and_src, *l2, *r2, defs, bits, source_reg,
                                ) {
                                    return false;
                                }
                            }
                            _ => return false,
                        }
                    } else {
                        return false;
                    }
                }
                _ => return false,
            }
        } else {
            return false;
        }
    }
    true
}

/// Recursively collect single-bit extractions from an add tree.
/// Returns true if the tree contains only 0/1 bit extractions from one source.
fn collect_add_chain_bits(
    reg: VReg,
    defs: &HashMap<VReg, MInst>,
    bits: &mut Vec<(VReg, u64)>,
    source_reg: &mut Option<VReg>,
) -> bool {
    let Some(def) = defs.get(&reg) else {
        return false;
    };

    match def {
        MInst::Add { lhs, rhs, .. } => {
            collect_add_chain_bits(*lhs, defs, bits, source_reg)
                && collect_add_chain_bits(*rhs, defs, bits, source_reg)
        }
        MInst::Mov { src, .. } => collect_add_chain_bits(*src, defs, bits, source_reg),
        MInst::AddImm { src, imm, .. } if *imm == 0 => {
            collect_add_chain_bits(*src, defs, bits, source_reg)
        }
        MInst::AndImm { src, imm, .. } if *imm == 1 => {
            let Some(inner) = defs.get(src) else {
                return false;
            };
            match inner {
                MInst::ShrImm { src, imm, .. } => {
                    match source_reg {
                        Some(s) if *s != *src => return false,
                        None => *source_reg = Some(*src),
                        _ => {}
                    }
                    bits.push((*src, *imm as u64));
                    true
                }
                MInst::Mov { src, .. } => {
                    match source_reg {
                        Some(s) if *s != *src => return false,
                        None => *source_reg = Some(*src),
                        _ => {}
                    }
                    bits.push((*src, 0));
                    true
                }
                _ => false,
            }
        }
        _ => false,
    }
}

/// Constant deduplication: merge LoadImm instructions with the same value
/// into a single VReg. Reduces register pressure and instruction count.
fn constant_dedup(func: &mut MFunction) {
    let mut aliases: HashMap<VReg, VReg> = HashMap::new();
    // Map from constant value → canonical VReg
    let mut const_map: HashMap<u64, VReg> = HashMap::new();

    for block in &func.blocks {
        const_map.clear(); // per-block to avoid cross-block live range extension
        for inst in &block.insts {
            if let MInst::LoadImm { dst, value } = inst {
                if let Some(&canonical) = const_map.get(value) {
                    aliases.insert(*dst, canonical);
                } else {
                    const_map.insert(*value, *dst);
                }
            }
        }
    }

    if aliases.is_empty() {
        return;
    }

    // Apply aliases
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            let current_uses = inst.uses();
            for u in current_uses {
                if let Some(&target) = aliases.get(&u) {
                    inst.rewrite_use(u, target);
                }
            }
        }
        for phi in &mut block.phis {
            for (_, src) in &mut phi.sources {
                if let Some(&a) = aliases.get(src) {
                    *src = a;
                }
            }
        }
    }

    // Remove duplicated LoadImm
    for block in &mut func.blocks {
        block.insts.retain(|inst| {
            if let MInst::LoadImm { dst, .. } = inst {
                !aliases.contains_key(dst)
            } else {
                true
            }
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct MemorySlot {
    base: BaseReg,
    offset: i32,
    size: OpSize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct SelectTerm {
    cond: VReg,
    true_val: VReg,
    false_val: VReg,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LinearBitCopy {
    source: VReg,
    /// `output_bit = source_bit + shift`.
    shift: i16,
    /// Output bits which still copy the corresponding source bit.
    output_mask: u64,
}

const MAX_RECONSTRUCTION_TERMS: usize = 64;
const MAX_RECONSTRUCTION_DEPTH: usize = 64;

/// Collapse an OR tree which partitions one value into bit fields and deposits
/// every field back at its original position.
///
/// SIR struct projection followed by reconstruction commonly lowers to:
///
/// ```text
/// ((src >> 0) & m0) << 0 |
/// ((src >> n) & mn) << n | ...
/// ```
///
/// Ordinary GVN cannot see that the complete tree is just `src & union(mask)`,
/// because every field has a different SSA definition.  Track the affine bit
/// position through only the operations which copy bits (`mov`, logical shifts,
/// and immediate masks).  A rewrite is made only when every nonzero OR term
/// copies bits from one source with `output_bit == source_bit`; relocated bits,
/// constants, and mixed sources are rejected.
fn fold_reconstructed_bit_partitions(func: &mut MFunction) {
    for block in &mut func.blocks {
        let definitions = block
            .insts
            .iter()
            .enumerate()
            .filter_map(|(index, instruction)| instruction.def().map(|dst| (dst, index)))
            .collect::<HashMap<_, _>>();
        let mut replacements = Vec::new();

        for (index, instruction) in block.insts.iter().enumerate() {
            let dst = match instruction {
                MInst::Or { dst, .. } | MInst::Or32 { dst, .. } => *dst,
                _ => continue,
            };
            let mut terms = Vec::new();
            if !collect_reconstructed_terms(
                dst,
                u64::MAX,
                &block.insts,
                &definitions,
                &mut terms,
                0,
            ) || terms.len() < 2
            {
                continue;
            }

            let source = terms[0].source;
            if terms
                .iter()
                .any(|term| term.source != source || term.shift != 0)
            {
                continue;
            }
            let mask = terms
                .iter()
                .fold(0u64, |mask, term| mask | term.output_mask);
            let replacement = if mask == u64::MAX {
                MInst::Mov { dst, src: source }
            } else if mask == u64::from(u32::MAX) {
                MInst::Mov32 { dst, src: source }
            } else if let Ok(imm) = u32::try_from(mask) {
                MInst::AndImm32 {
                    dst,
                    src: source,
                    imm,
                }
            } else if and_imm_ok(mask) {
                MInst::AndImm {
                    dst,
                    src: source,
                    imm: mask,
                }
            } else {
                continue;
            };
            replacements.push((index, replacement));
        }

        for (index, replacement) in replacements {
            block.insts[index] = replacement;
        }
    }
}

fn collect_reconstructed_terms(
    value: VReg,
    output_mask: u64,
    instructions: &[MInst],
    definitions: &HashMap<VReg, usize>,
    terms: &mut Vec<LinearBitCopy>,
    depth: usize,
) -> bool {
    if depth >= MAX_RECONSTRUCTION_DEPTH || terms.len() >= MAX_RECONSTRUCTION_TERMS {
        return false;
    }
    let Some(&definition) = definitions.get(&value) else {
        terms.push(LinearBitCopy {
            source: value,
            shift: 0,
            output_mask,
        });
        return true;
    };

    match &instructions[definition] {
        MInst::Or { lhs, rhs, .. } => {
            collect_reconstructed_terms(
                *lhs,
                output_mask,
                instructions,
                definitions,
                terms,
                depth + 1,
            ) && collect_reconstructed_terms(
                *rhs,
                output_mask,
                instructions,
                definitions,
                terms,
                depth + 1,
            )
        }
        MInst::Or32 { lhs, rhs, .. } => {
            let output_mask = output_mask & u64::from(u32::MAX);
            collect_reconstructed_terms(
                *lhs,
                output_mask,
                instructions,
                definitions,
                terms,
                depth + 1,
            ) && collect_reconstructed_terms(
                *rhs,
                output_mask,
                instructions,
                definitions,
                terms,
                depth + 1,
            )
        }
        MInst::LoadImm { value: 0, .. } => true,
        _ => {
            let Some(mut term) = trace_linear_bit_copy(value, instructions, definitions, depth + 1)
            else {
                return false;
            };
            term.output_mask &= output_mask;
            if term.output_mask != 0 {
                terms.push(term);
            }
            true
        }
    }
}

fn trace_linear_bit_copy(
    value: VReg,
    instructions: &[MInst],
    definitions: &HashMap<VReg, usize>,
    depth: usize,
) -> Option<LinearBitCopy> {
    if depth >= MAX_RECONSTRUCTION_DEPTH {
        return None;
    }
    let Some(&definition) = definitions.get(&value) else {
        return Some(LinearBitCopy {
            source: value,
            shift: 0,
            output_mask: u64::MAX,
        });
    };

    let instruction = &instructions[definition];
    let (source, operation) = match instruction {
        MInst::Mov { src, .. } => (*src, LinearCopyOp::Mask(u64::MAX)),
        MInst::Mov32 { src, .. } => (*src, LinearCopyOp::Mask(u64::from(u32::MAX))),
        MInst::AndImm { src, imm, .. } => (*src, LinearCopyOp::Mask(*imm)),
        MInst::AndImm32 { src, imm, .. } => (*src, LinearCopyOp::Mask(u64::from(*imm))),
        MInst::ShrImm { src, imm, .. } => (*src, LinearCopyOp::ShiftRight(*imm)),
        MInst::ShlImm { src, imm, .. } => (*src, LinearCopyOp::ShiftLeft(*imm)),
        MInst::LoadImm { value: 0, .. } => {
            return Some(LinearBitCopy {
                source: value,
                shift: 0,
                output_mask: 0,
            });
        }
        // An OR below a field extraction is deliberately treated as one
        // opaque source. This lets a reconstructed value fold back to the
        // already-available packed value instead of duplicating its inputs.
        _ => {
            return Some(LinearBitCopy {
                source: value,
                shift: 0,
                output_mask: u64::MAX,
            });
        }
    };

    let mut copy = trace_linear_bit_copy(source, instructions, definitions, depth + 1)?;
    match operation {
        LinearCopyOp::Mask(mask) => copy.output_mask &= mask,
        LinearCopyOp::ShiftRight(shift) => {
            copy.output_mask >>= shift;
            copy.shift -= i16::from(shift);
        }
        LinearCopyOp::ShiftLeft(shift) => {
            copy.output_mask <<= shift;
            copy.shift += i16::from(shift);
        }
    }
    Some(copy)
}

#[derive(Clone, Copy)]
enum LinearCopyOp {
    Mask(u64),
    ShiftRight(u8),
    ShiftLeft(u8),
}

fn eliminate_redundant_or_terms(func: &mut MFunction) {
    for block in &mut func.blocks {
        let mut mov_aliases: HashMap<VReg, VReg> = HashMap::new();
        let mut rewrite_aliases: HashMap<VReg, VReg> = HashMap::new();
        let mut select_terms: HashMap<VReg, SelectTerm> = HashMap::new();
        let mut or_terms: HashMap<VReg, HashSet<SelectTerm>> = HashMap::new();

        for inst in &mut block.insts {
            if !rewrite_aliases.is_empty() {
                rewrite_uses(inst, &rewrite_aliases);
            }

            match inst {
                MInst::Mov { dst, src } => {
                    let canonical = resolve_alias(*src, &mov_aliases);
                    mov_aliases.insert(*dst, canonical);
                    if let Some(term) = select_terms.get(&canonical).copied() {
                        select_terms.insert(*dst, term);
                    }
                    if let Some(terms) = or_terms.get(&canonical).cloned() {
                        or_terms.insert(*dst, terms);
                    }
                }
                MInst::Select {
                    dst,
                    cond,
                    true_val,
                    false_val,
                } => {
                    let term = SelectTerm {
                        cond: resolve_alias(*cond, &mov_aliases),
                        true_val: resolve_alias(*true_val, &mov_aliases),
                        false_val: resolve_alias(*false_val, &mov_aliases),
                    };
                    select_terms.insert(*dst, term);
                    mov_aliases.remove(dst);
                    or_terms.remove(dst);
                }
                MInst::Or { dst, lhs, rhs } => {
                    let lhs = resolve_alias(*lhs, &rewrite_aliases);
                    let rhs = resolve_alias(*rhs, &rewrite_aliases);
                    let lhs_terms = or_terms.get(&lhs).cloned();
                    let rhs_terms = or_terms.get(&rhs).cloned();
                    let lhs_term = select_terms.get(&lhs).copied();
                    let rhs_term = select_terms.get(&rhs).copied();

                    let replacement = lhs_terms
                        .as_ref()
                        .and_then(|terms| rhs_term.filter(|term| terms.contains(term)).map(|_| lhs))
                        .or_else(|| {
                            rhs_terms.as_ref().and_then(|terms| {
                                lhs_term.filter(|term| terms.contains(term)).map(|_| rhs)
                            })
                        });

                    if let Some(src) = replacement {
                        let dst_vreg = *dst;
                        *inst = MInst::Mov { dst: dst_vreg, src };
                        rewrite_aliases.insert(dst_vreg, src);
                        mov_aliases.insert(dst_vreg, src);
                        if let Some(terms) = or_terms.get(&src).cloned() {
                            or_terms.insert(dst_vreg, terms);
                        }
                        continue;
                    }

                    let mut terms = lhs_terms.unwrap_or_default();
                    if let Some(rhs_terms) = rhs_terms {
                        terms.extend(rhs_terms);
                    }
                    if let Some(term) = lhs_term {
                        terms.insert(term);
                    }
                    if let Some(term) = rhs_term {
                        terms.insert(term);
                    }
                    if terms.is_empty() {
                        or_terms.remove(dst);
                    } else {
                        or_terms.insert(*dst, terms);
                    }
                    mov_aliases.remove(dst);
                    select_terms.remove(dst);
                }
                _ => {
                    if let Some(dst) = inst.def() {
                        mov_aliases.remove(&dst);
                        select_terms.remove(&dst);
                        or_terms.remove(&dst);
                    }
                }
            }
        }
    }
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

type LocalMemoryByte = (BaseReg, i64);

#[derive(Clone, Copy, PartialEq, Eq)]
enum LocalMemoryEvent {
    Load(usize),
    Store(usize),
}

#[derive(Clone)]
struct PartialStoreEvent {
    slot: MemorySlot,
    source: VReg,
    insert: Option<StateInsertDesc>,
    prior_events: [Option<LocalMemoryEvent>; 8],
    prior_writes: [Option<usize>; 8],
}

struct PartialRoundTripPlan {
    load_instruction: usize,
    insertion_instruction: usize,
    load_slot: MemorySlot,
    destination: VReg,
    stores: Vec<usize>,
}

fn slot_bytes(slot: MemorySlot) -> impl Iterator<Item = LocalMemoryByte> {
    let start = i64::from(slot.offset);
    (0..slot.size.bytes() as usize).map(move |byte| (slot.base, start + byte as i64))
}

fn direct_memory_barrier(inst: &MInst) -> bool {
    let reads = memory_effect::reads(inst);
    let writes = memory_effect::writes(inst);
    [reads, writes].into_iter().any(|effects| {
        matches!(
            effects.unknown_memory(),
            Some(memory_effect::UnknownMemory::Direct(_))
        ) || effects.ranges().next().is_some()
    })
}

fn contained_slot(inner: MemorySlot, outer: MemorySlot) -> bool {
    if inner.base != outer.base {
        return false;
    }
    let inner_start = i64::from(inner.offset);
    let outer_start = i64::from(outer.offset);
    let inner_end = inner_start + i64::from(inner.size.bytes());
    let outer_end = outer_start + i64::from(outer.size.bytes());
    outer_start <= inner_start && inner_end <= outer_end
}

fn recover_partial_store_insert(
    definitions: &HashMap<VReg, &MInst>,
    slot: MemorySlot,
    source: VReg,
    possible_ones: &[u64],
    constants: &HashMap<VReg, u64>,
) -> Option<StateInsertDesc> {
    let MInst::Or { lhs, rhs, .. } = definitions.get(&source)? else {
        return None;
    };
    let storage_mask = machine_width_mask(slot.size);

    let cleared = |value: VReg| -> Option<(VReg, u64)> {
        match definitions.get(&value)? {
            MInst::AndImm { src, imm, .. } => Some((*src, *imm & storage_mask)),
            MInst::AndImm32 { src, imm, .. } => Some((*src, u64::from(*imm) & storage_mask)),
            MInst::And { lhs, rhs, .. } | MInst::And32 { lhs, rhs, .. } => constants
                .get(rhs)
                .map(|mask| (*lhs, *mask & storage_mask))
                .or_else(|| constants.get(lhs).map(|mask| (*rhs, *mask & storage_mask))),
            _ => None,
        }
    };
    let ((old, clear_mask), inserted) = cleared(*lhs)
        .map(|clear| (clear, *rhs))
        .or_else(|| cleared(*rhs).map(|clear| (clear, *lhs)))?;
    if !matches!(
        definitions.get(&old),
        Some(MInst::Load {
            base,
            offset,
            size,
            ..
        }) if *base == slot.base && *offset == slot.offset && *size == slot.size
    ) {
        return None;
    }

    let changed_mask = storage_mask & !clear_mask;
    let bit_offset = changed_mask.trailing_zeros() as usize;
    let width_bits = changed_mask.count_ones() as usize;
    let field_mask = if width_bits == 64 {
        u64::MAX
    } else {
        (1_u64 << width_bits) - 1
    };
    if width_bits == 0
        || changed_mask != field_mask.checked_shl(bit_offset as u32).unwrap_or(0)
        || possible_bits(inserted, possible_ones, constants) & !changed_mask != 0
    {
        return None;
    }

    let (value, value_bit_offset) = match definitions.get(&inserted) {
        Some(MInst::ShlImm { src, imm, .. })
            if usize::from(*imm) == bit_offset
                && possible_bits(*src, possible_ones, constants) & !field_mask == 0 =>
        {
            (*src, 0)
        }
        _ => (inserted, bit_offset),
    };
    Some(StateInsertDesc {
        value,
        value_bit_offset,
        bit_offset,
        width_bits,
        complete_value: false,
    })
}

fn discover_partial_round_trips(
    block: &MBlock,
    spill_descs: &[SpillDesc],
    defined_values: &HashSet<VReg>,
    possible_ones: &[u64],
    constants: &HashMap<VReg, u64>,
) -> (Vec<PartialRoundTripPlan>, Vec<Option<PartialStoreEvent>>) {
    let mut last_events = HashMap::<LocalMemoryByte, LocalMemoryEvent>::new();
    let mut last_writes = HashMap::<LocalMemoryByte, usize>::new();
    let mut stores = vec![None; block.insts.len()];
    let mut plans = Vec::new();
    let mut region_start = 0usize;
    let mut definitions = HashMap::<VReg, &MInst>::new();

    for (instruction, inst) in block.insts.iter().enumerate() {
        match *inst {
            MInst::Store {
                base,
                offset,
                src,
                size,
            } => {
                let slot = MemorySlot { base, offset, size };
                let mut prior_events = [None; 8];
                let mut prior_writes = [None; 8];
                for (byte, key) in slot_bytes(slot).enumerate() {
                    prior_events[byte] = last_events.get(&key).copied();
                    prior_writes[byte] = last_writes.get(&key).copied();
                    last_events.insert(key, LocalMemoryEvent::Store(instruction));
                    last_writes.insert(key, instruction);
                }
                let insert = spill_descs
                    .get(src.0 as usize)
                    .and_then(|descriptor| descriptor.state_insert)
                    .filter(|insert| {
                        defined_values.contains(&insert.value)
                            && insert.width_bits != 0
                            && insert
                                .bit_offset
                                .checked_add(insert.width_bits)
                                .is_some_and(|end| end <= size.bytes() as usize * 8)
                            && insert
                                .value_bit_offset
                                .checked_add(insert.width_bits)
                                .is_some_and(|end| end <= 64)
                    })
                    .or_else(|| {
                        recover_partial_store_insert(
                            &definitions,
                            slot,
                            src,
                            possible_ones,
                            constants,
                        )
                        .filter(|insert| defined_values.contains(&insert.value))
                    });
                stores[instruction] = Some(PartialStoreEvent {
                    slot,
                    source: src,
                    insert,
                    prior_events,
                    prior_writes,
                });
            }
            MInst::Load {
                dst,
                base,
                offset,
                size,
            } => {
                let load_slot = MemorySlot { base, offset, size };
                let mut candidates = slot_bytes(load_slot)
                    .filter_map(|byte| match last_events.get(&byte) {
                        Some(LocalMemoryEvent::Store(store)) => Some(*store),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                candidates.sort_unstable();
                candidates.dedup();

                let valid = !candidates.is_empty()
                    && candidates.iter().all(|&store| {
                        let Some(event) = stores[store].as_ref() else {
                            return false;
                        };
                        event.slot.size.bytes() < size.bytes()
                            && contained_slot(event.slot, load_slot)
                            && slot_bytes(event.slot).all(|byte| {
                                last_events.get(&byte) == Some(&LocalMemoryEvent::Store(store))
                            })
                    });
                if valid {
                    let mut barrier = region_start;
                    for byte in slot_bytes(load_slot) {
                        let mut write = last_writes.get(&byte).copied();
                        while let Some(candidate) = write.filter(|write| candidates.contains(write))
                        {
                            let event = stores[candidate]
                                .as_ref()
                                .expect("candidate store has an event");
                            let byte_index = usize::try_from(byte.1 - i64::from(event.slot.offset))
                                .expect("candidate contains queried byte");
                            write = event.prior_writes[byte_index];
                        }
                        if let Some(write) = write {
                            barrier = barrier.max(write.saturating_add(1));
                        }
                    }

                    let first_store = candidates[0];
                    let mut insertion = first_store;
                    for &store in &candidates {
                        let event = stores[store]
                            .as_ref()
                            .expect("candidate store has an event");
                        for prior in event.prior_events.into_iter().flatten() {
                            if let LocalMemoryEvent::Load(load) = prior
                                && load >= barrier
                            {
                                insertion = insertion.min(load);
                            }
                        }
                    }
                    insertion = insertion.max(barrier);
                    if insertion <= first_store {
                        plans.push(PartialRoundTripPlan {
                            load_instruction: instruction,
                            insertion_instruction: insertion,
                            load_slot,
                            destination: dst,
                            stores: candidates,
                        });
                    }
                }

                for byte in slot_bytes(load_slot) {
                    last_events.insert(byte, LocalMemoryEvent::Load(instruction));
                }
            }
            _ if direct_memory_barrier(inst) => {
                last_events.clear();
                last_writes.clear();
                region_start = instruction.saturating_add(1);
            }
            _ => {}
        }
        if let Some(destination) = inst.def() {
            definitions.insert(destination, inst);
        }
    }

    (plans, stores)
}

fn retain_dead_partial_store_plans(
    block: &MBlock,
    plans: Vec<PartialRoundTripPlan>,
) -> Vec<PartialRoundTripPlan> {
    let mut plans_by_load = plans
        .iter()
        .enumerate()
        .map(|(plan, candidate)| (candidate.load_instruction, plan))
        .collect::<HashMap<_, _>>();
    let mut accepted = vec![false; plans.len()];
    let mut removed_stores = HashSet::<usize>::new();
    let mut next_events = HashMap::<LocalMemoryByte, LocalMemoryEvent>::new();

    for (instruction, inst) in block.insts.iter().enumerate().rev() {
        if let Some(plan) = plans_by_load.remove(&instruction) {
            let candidate = &plans[plan];
            let all_overwritten = candidate.stores.iter().all(|&store| {
                let MInst::Store {
                    base, offset, size, ..
                } = block.insts[store]
                else {
                    return false;
                };
                slot_bytes(MemorySlot { base, offset, size }).all(|byte| {
                    matches!(
                        next_events.get(&byte),
                        Some(LocalMemoryEvent::Store(next)) if !removed_stores.contains(next)
                    )
                })
            });
            if all_overwritten {
                accepted[plan] = true;
                removed_stores.extend(candidate.stores.iter().copied());
            }
        }

        match *inst {
            MInst::Store {
                base, offset, size, ..
            } if !removed_stores.contains(&instruction) => {
                for byte in slot_bytes(MemorySlot { base, offset, size }) {
                    next_events.insert(byte, LocalMemoryEvent::Store(instruction));
                }
            }
            MInst::Store { .. } => {}
            MInst::Load {
                base, offset, size, ..
            } => {
                for byte in slot_bytes(MemorySlot { base, offset, size }) {
                    next_events.insert(byte, LocalMemoryEvent::Load(instruction));
                }
            }
            _ if direct_memory_barrier(inst) => next_events.clear(),
            _ => {}
        }
    }

    plans
        .into_iter()
        .zip(accepted)
        .filter_map(|(plan, accepted)| accepted.then_some(plan))
        .collect()
}

fn emit_partial_store_overlay(
    instructions: &mut Vec<MInst>,
    vregs: &mut VRegAllocator,
    spill_descs: &mut Vec<SpillDesc>,
    mut current: VReg,
    destination: VReg,
    load_slot: MemorySlot,
    stores: &[PartialStoreEvent],
) {
    let load_mask = match load_slot.size {
        OpSize::S8 => u8::MAX as u64,
        OpSize::S16 => u16::MAX as u64,
        OpSize::S32 => u32::MAX as u64,
        OpSize::S64 => u64::MAX,
    };

    for (index, store) in stores.iter().enumerate() {
        let (source, source_bit_offset, width_bits, store_bit_offset) =
            if let Some(insert) = store.insert {
                (
                    insert.value,
                    insert.value_bit_offset,
                    insert.width_bits,
                    insert.bit_offset,
                )
            } else {
                (store.source, 0, store.slot.size.bytes() as usize * 8, 0)
            };
        let stored_mask = match width_bits {
            64 => u64::MAX,
            width => (1_u64 << width) - 1,
        };
        let shift =
            u8::try_from((store.slot.offset - load_slot.offset) * 8 + store_bit_offset as i32)
                .expect("contained scalar store shift fits one word");
        let source = if source_bit_offset == 0 {
            source
        } else {
            let shifted = alloc_transient_vreg(vregs, spill_descs);
            instructions.push(MInst::ShrImm {
                dst: shifted,
                src: source,
                imm: source_bit_offset as u8,
            });
            shifted
        };
        let normalized = if width_bits == 64 {
            source
        } else {
            let normalized = alloc_transient_vreg(vregs, spill_descs);
            if width_bits <= 32 {
                instructions.push(MInst::AndImm32 {
                    dst: normalized,
                    src: source,
                    imm: stored_mask as u32,
                });
            } else if and_imm_ok(stored_mask) {
                instructions.push(MInst::AndImm {
                    dst: normalized,
                    src: source,
                    imm: stored_mask,
                });
            } else {
                let mask = alloc_transient_vreg(vregs, spill_descs);
                instructions.push(MInst::LoadImm {
                    dst: mask,
                    value: stored_mask,
                });
                instructions.push(MInst::And {
                    dst: normalized,
                    lhs: source,
                    rhs: mask,
                });
            }
            normalized
        };

        let shifted = if shift == 0 {
            normalized
        } else {
            let shifted = alloc_transient_vreg(vregs, spill_descs);
            instructions.push(MInst::ShlImm {
                dst: shifted,
                src: normalized,
                imm: shift,
            });
            shifted
        };

        let clear_mask = load_mask & !(stored_mask << shift);
        let cleared = alloc_transient_vreg(vregs, spill_descs);
        if load_slot.size != OpSize::S64 {
            instructions.push(MInst::AndImm32 {
                dst: cleared,
                src: current,
                imm: clear_mask as u32,
            });
        } else if and_imm_ok(clear_mask) {
            instructions.push(MInst::AndImm {
                dst: cleared,
                src: current,
                imm: clear_mask,
            });
        } else {
            let mask = alloc_transient_vreg(vregs, spill_descs);
            instructions.push(MInst::LoadImm {
                dst: mask,
                value: clear_mask,
            });
            instructions.push(MInst::And {
                dst: cleared,
                lhs: current,
                rhs: mask,
            });
        }

        let merged = if index + 1 == stores.len() {
            destination
        } else {
            alloc_transient_vreg(vregs, spill_descs)
        };
        instructions.push(MInst::Or {
            dst: merged,
            lhs: cleared,
            rhs: shifted,
        });
        current = merged;
    }
}

/// Promote a local sequence of partial state writes through its sole wide
/// observation.
///
/// For every physical byte, the forward scan proves that the wide load is the
/// first observer of each selected store. The reverse scan proves that every
/// selected byte is overwritten before another observer. The load can
/// therefore move to the preceding memory version, while the removed stores
/// are represented as ordinary SSA inserts at the original load point.
///
/// Direct scalar accesses touch at most eight byte facts. Both scans and the
/// rewrite are linear in block instructions and use storage proportional to
/// the direct bytes referenced by one block. Insert recovery additionally
/// reuses one sparse whole-function possible-bit solve.
fn promote_partial_store_round_trips(func: &mut MFunction) {
    let constants = func
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .filter_map(|inst| match inst {
            MInst::LoadImm { dst, value } => Some((*dst, *value)),
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    let possible_ones = global_possible_one_bits(func, &constants);
    let defined_values = func
        .blocks
        .iter()
        .flat_map(|block| {
            block
                .phis
                .iter()
                .map(|phi| phi.dst)
                .chain(block.insts.iter().filter_map(MInst::def))
        })
        .collect::<HashSet<_>>();
    let (vregs, spill_descs, blocks) = (&mut func.vregs, &mut func.spill_descs, &mut func.blocks);
    for block in blocks {
        let (plans, store_events) = discover_partial_round_trips(
            block,
            spill_descs,
            &defined_values,
            &possible_ones,
            &constants,
        );
        let plans = retain_dead_partial_store_plans(block, plans);
        if plans.is_empty() {
            continue;
        }

        let mut insertions = HashMap::<usize, Vec<MInst>>::new();
        let mut replacements = HashMap::<usize, Vec<MInst>>::new();
        let mut removals = HashSet::<usize>::new();
        for plan in plans {
            let old = alloc_transient_vreg(vregs, spill_descs);
            insertions
                .entry(plan.insertion_instruction)
                .or_default()
                .push(MInst::Load {
                    dst: old,
                    base: plan.load_slot.base,
                    offset: plan.load_slot.offset,
                    size: plan.load_slot.size,
                });
            let stores = plan
                .stores
                .iter()
                .map(|&store| {
                    store_events[store]
                        .as_ref()
                        .expect("planned store retains its event")
                        .clone()
                })
                .collect::<Vec<_>>();
            let mut replacement = Vec::new();
            emit_partial_store_overlay(
                &mut replacement,
                vregs,
                spill_descs,
                old,
                plan.destination,
                plan.load_slot,
                &stores,
            );
            replacements.insert(plan.load_instruction, replacement);
            removals.extend(plan.stores);
            spill_descs[plan.destination.0 as usize] = SpillDesc::transient();
        }

        let original = std::mem::take(&mut block.insts);
        let mut rewritten = Vec::with_capacity(original.len());
        for (instruction, inst) in original.into_iter().enumerate() {
            if let Some(mut inserted) = insertions.remove(&instruction) {
                rewritten.append(&mut inserted);
            }
            if removals.contains(&instruction) {
                continue;
            }
            if let Some(mut replacement) = replacements.remove(&instruction) {
                rewritten.append(&mut replacement);
            } else {
                rewritten.push(inst);
            }
        }
        block.insts = rewritten;
    }
}

fn forward_local_store_loads(func: &mut MFunction) {
    let (vregs, spill_descs, blocks) = (&mut func.vregs, &mut func.spill_descs, &mut func.blocks);
    for block in blocks {
        let mut available: HashMap<MemorySlot, VReg> = HashMap::new();
        let mut rewritten = Vec::with_capacity(block.insts.len());

        for inst in block.insts.drain(..) {
            match inst {
                MInst::Store {
                    base,
                    offset,
                    src,
                    size,
                } => {
                    invalidate_overlapping_slots(&mut available, base, offset, size);
                    available.insert(MemorySlot { base, offset, size }, src);
                    rewritten.push(MInst::Store {
                        base,
                        offset,
                        src,
                        size,
                    });
                }
                MInst::Load {
                    dst,
                    base,
                    offset,
                    size,
                } => {
                    let key = MemorySlot { base, offset, size };
                    if let Some(&src) = available.get(&key) {
                        rewritten.push(MInst::Mov { dst, src });
                        continue;
                    }
                    if let Some((covering_slot, src)) =
                        find_best_covering_value(&available, base, offset, size)
                    {
                        emit_partial_load_forward(
                            &mut rewritten,
                            vregs,
                            spill_descs,
                            dst,
                            src,
                            covering_slot.offset,
                            covering_slot.size,
                            offset,
                            size,
                        );
                        continue;
                    }
                    available.insert(MemorySlot { base, offset, size }, dst);
                    rewritten.push(MInst::Load {
                        dst,
                        base,
                        offset,
                        size,
                    });
                }
                MInst::LoadIndexed { .. }
                | MInst::LoadPtrIndexed { .. }
                | MInst::StoreIndexed { .. }
                | MInst::OrStoreIndexed { .. }
                | MInst::StorePtrIndexed { .. }
                | MInst::ReleaseStorePtrIndexed { .. } => {
                    available.clear();
                    rewritten.push(inst);
                }
                MInst::MemCopy {
                    src_offset,
                    dst_offset,
                    byte_len,
                } => {
                    // The source is read but unchanged. Only values cached for
                    // the written destination range become stale.
                    invalidate_overlapping_byte_range(
                        &mut available,
                        BaseReg::SimState,
                        dst_offset,
                        byte_len,
                    );
                    rewritten.push(MInst::MemCopy {
                        src_offset,
                        dst_offset,
                        byte_len,
                    });
                }
                other => rewritten.push(other),
            }
        }

        block.insts = rewritten;
    }
}

fn find_best_covering_value(
    available: &HashMap<MemorySlot, VReg>,
    base: BaseReg,
    offset: i32,
    size: OpSize,
) -> Option<(MemorySlot, VReg)> {
    let load_start = offset as i64;
    let load_end = load_start + i64::from(size.bytes());
    available
        .iter()
        .filter_map(|(slot, &src)| {
            if slot.base != base {
                return None;
            }
            let value_start = slot.offset as i64;
            let value_end = value_start + i64::from(slot.size.bytes());
            (value_start <= load_start && load_end <= value_end).then_some((*slot, src))
        })
        // Several earlier loads/stores can cover the same narrow load.  Hash
        // iteration order is randomized, so select the cheapest extraction
        // explicitly: least over-read first, then least right shift.  The
        // final fields make equal-cost selection reproducible as well.
        .min_by_key(|(slot, src)| {
            (
                slot.size.bytes(),
                load_start - i64::from(slot.offset),
                slot.offset,
                src.0,
            )
        })
}

fn emit_partial_load_forward(
    rewritten: &mut Vec<MInst>,
    vregs: &mut VRegAllocator,
    spill_descs: &mut Vec<SpillDesc>,
    dst: VReg,
    src: VReg,
    store_offset: i32,
    _store_size: OpSize,
    load_offset: i32,
    load_size: OpSize,
) {
    let shift_bytes = (load_offset - store_offset) as u8;
    let shift_bits = shift_bytes * 8;
    let mut current = src;

    if shift_bits != 0 {
        let shifted = alloc_transient_vreg(vregs, spill_descs);
        rewritten.push(MInst::ShrImm {
            dst: shifted,
            src: current,
            imm: shift_bits,
        });
        current = shifted;
    }

    let mask = match load_size {
        OpSize::S8 => Some(0xff),
        OpSize::S16 => Some(0xffff),
        OpSize::S32 => Some(0xffff_ffff),
        OpSize::S64 => None,
    };

    if let Some(mask) = mask {
        rewritten.push(MInst::AndImm {
            dst,
            src: current,
            imm: mask,
        });
    } else {
        rewritten.push(MInst::Mov { dst, src: current });
    }
}

fn alloc_transient_vreg(vregs: &mut VRegAllocator, spill_descs: &mut Vec<SpillDesc>) -> VReg {
    let vreg = vregs.alloc();
    while spill_descs.len() <= vreg.0 as usize {
        spill_descs.push(SpillDesc::transient());
    }
    vreg
}

#[derive(Default)]
struct LaterDirectStores {
    sim_state: BTreeMap<i32, u8>,
    stack_frame: BTreeMap<i32, u8>,
}

impl LaterDirectStores {
    const SIZES: [(OpSize, u8); 4] = [
        (OpSize::S8, 1 << 0),
        (OpSize::S16, 1 << 1),
        (OpSize::S32, 1 << 2),
        (OpSize::S64, 1 << 3),
    ];

    fn slots(&self, base: BaseReg) -> &BTreeMap<i32, u8> {
        match base {
            BaseReg::SimState => &self.sim_state,
            BaseReg::StackFrame => &self.stack_frame,
        }
    }

    fn slots_mut(&mut self, base: BaseReg) -> &mut BTreeMap<i32, u8> {
        match base {
            BaseReg::SimState => &mut self.sim_state,
            BaseReg::StackFrame => &mut self.stack_frame,
        }
    }

    fn size_bit(size: OpSize) -> u8 {
        match size {
            OpSize::S8 => 1 << 0,
            OpSize::S16 => 1 << 1,
            OpSize::S32 => 1 << 2,
            OpSize::S64 => 1 << 3,
        }
    }

    fn contains(&self, slot: MemorySlot) -> bool {
        self.slots(slot.base)
            .get(&slot.offset)
            .is_some_and(|sizes| sizes & Self::size_bit(slot.size) != 0)
    }

    fn insert(&mut self, slot: MemorySlot) {
        *self.slots_mut(slot.base).entry(slot.offset).or_default() |= Self::size_bit(slot.size);
    }

    fn clear(&mut self, base: BaseReg) {
        self.slots_mut(base).clear();
    }

    /// Forget later stores whose values can be observed by `range`.
    ///
    /// Direct stores are at most eight bytes wide, so an overlapping store
    /// starts no earlier than seven bytes before the read.  Indexing by start
    /// offset finds narrow-read candidates in O(log n + overlap) instead of
    /// retaining over every store in the block; removing those candidates is
    /// O(overlap * log n).  A wide bounded indexed read visits only the
    /// tracked starts in its alias envelope.
    fn invalidate_range(
        &mut self,
        range: memory_effect::MemoryRange,
        scratch: &mut Vec<(i32, u8)>,
    ) {
        let Some(read_end) = range.end() else {
            self.clear(range.base);
            return;
        };
        let read_start = range.offset;
        if read_end <= read_start {
            return;
        }

        const MAX_STORE_BYTES: i64 = 8;
        let first_candidate = read_start.saturating_sub(MAX_STORE_BYTES - 1);
        let last_candidate = read_end - 1;
        if last_candidate < i64::from(i32::MIN) || first_candidate > i64::from(i32::MAX) {
            return;
        }
        let first_candidate =
            first_candidate.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
        let last_candidate = last_candidate.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
        if first_candidate > last_candidate {
            return;
        }

        scratch.clear();
        scratch.extend(
            self.slots(range.base)
                .range(first_candidate..=last_candidate)
                .filter_map(|(&offset, &sizes)| {
                    let mut retained = 0u8;
                    for (size, bit) in Self::SIZES {
                        if sizes & bit == 0 {
                            continue;
                        }
                        let slot_start = i64::from(offset);
                        let slot_end = slot_start + i64::from(size.bytes());
                        if slot_end <= read_start || read_end <= slot_start {
                            retained |= bit;
                        }
                    }
                    (retained != sizes).then_some((offset, retained))
                }),
        );
        let slots = self.slots_mut(range.base);
        for &(offset, retained) in scratch.iter() {
            if retained == 0 {
                slots.remove(&offset);
            } else {
                slots.insert(offset, retained);
            }
        }
    }
}

fn invalidate_stores_observed_by(
    later_stores: &mut LaterDirectStores,
    inst: &MInst,
    scratch: &mut Vec<(i32, u8)>,
) {
    let reads = memory_effect::reads(inst);
    if let Some(memory) = reads.unknown_memory() {
        match memory {
            memory_effect::UnknownMemory::Direct(base) => later_stores.clear(base),
            // Runtime-owned pointer memory is explicitly disjoint from both
            // direct-addressed bases tracked by this local DSE.
            memory_effect::UnknownMemory::Indirect => {}
        }
    }
    for range in reads.ranges() {
        later_stores.invalidate_range(range, scratch);
    }
}

pub(super) fn eliminate_redundant_local_stores(func: &mut MFunction) {
    for block in &mut func.blocks {
        let mut later_stores = LaterDirectStores::default();
        let mut invalidation_scratch = Vec::new();
        let mut reversed = Vec::with_capacity(block.insts.len());

        for inst in block.insts.drain(..).rev() {
            invalidate_stores_observed_by(&mut later_stores, &inst, &mut invalidation_scratch);
            if let MInst::Store {
                base, offset, size, ..
            } = &inst
            {
                let slot = MemorySlot {
                    base: *base,
                    offset: *offset,
                    size: *size,
                };
                if later_stores.contains(slot) {
                    continue;
                }
                // Writes do not observe an earlier value. Keep every exact
                // later overwrite candidate, including overlapping widths;
                // intervening reads invalidate precisely the candidates they
                // can observe.
                later_stores.insert(slot);
            }
            reversed.push(inst);
        }

        reversed.reverse();
        block.insts = reversed;
    }
}

fn invalidate_overlapping_slots<T>(
    available: &mut HashMap<MemorySlot, T>,
    base: BaseReg,
    offset: i32,
    size: OpSize,
) {
    invalidate_overlapping_byte_range(available, base, offset, size.bytes() as usize);
}

fn invalidate_overlapping_byte_range<T>(
    available: &mut HashMap<MemorySlot, T>,
    base: BaseReg,
    offset: i32,
    byte_len: usize,
) {
    let Some((start, end)) = byte_range(offset, byte_len) else {
        available.retain(|slot, _| slot.base != base);
        return;
    };
    available.retain(|slot, _| {
        if slot.base != base {
            return true;
        }
        let slot_start = slot.offset as i64;
        let slot_end = slot_start + i64::from(slot.size.bytes());
        slot_end <= start || end <= slot_start
    });
}

/// Copy propagation: replace every full-word copy, and every `Mov32` whose
/// source is already structurally proven zero-extended to 32 bits, with its
/// source throughout the function. A `Mov32` from an arbitrary 64-bit source
/// remains a real truncating definition.
fn copy_propagate(func: &mut MFunction) {
    // Build alias map: dst → src (transitively resolved)
    let mut aliases: HashMap<VReg, VReg> = HashMap::new();
    let definitions = func
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .filter_map(|inst| inst.def().map(|dst| (dst, inst.clone())))
        .collect::<HashMap<_, _>>();
    let mut upper_bounds = HashMap::new();

    for block in &func.blocks {
        for inst in &block.insts {
            let copy = match inst {
                MInst::Mov { dst, src } => Some((*dst, *src)),
                MInst::Mov32 { dst, src }
                    if unsigned_upper_bound(
                        *src,
                        &definitions,
                        &mut upper_bounds,
                        &mut HashSet::new(),
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

/// Dead code elimination: remove instructions whose defs are never used.
fn dead_code_eliminate(func: &mut MFunction) {
    dead_code_eliminate_impl(func, true);
}

/// Post-allocation DCE must preserve the phi rows used to construct the
/// already-verified parallel-copy plan.
fn dead_code_eliminate_preserving_phis(func: &mut MFunction) {
    dead_code_eliminate_impl(func, false);
}

fn dead_code_eliminate_impl(func: &mut MFunction, remove_unused_phis: bool) {
    // Iterate until no more dead code is removed (cascading DCE).
    loop {
        let mut used: std::collections::HashSet<VReg> = std::collections::HashSet::new();
        for block in &func.blocks {
            for inst in &block.insts {
                for u in inst.uses() {
                    used.insert(u);
                }
            }
            for phi in &block.phis {
                for (_, src) in &phi.sources {
                    used.insert(*src);
                }
            }
        }

        let mut removed = false;
        for block in &mut func.blocks {
            let before = block.insts.len();
            block.insts.retain(|inst| {
                if let Some(def) = inst.def() {
                    if !used.contains(&def) {
                        return matches!(
                            inst,
                            MInst::Store { .. }
                                | MInst::StorePtr { .. }
                                | MInst::ReleaseStorePtr { .. }
                                | MInst::StoreIndexed { .. }
                                | MInst::OrStoreIndexed { .. }
                                | MInst::StorePtrIndexed { .. }
                                | MInst::ReleaseStorePtrIndexed { .. }
                                | MInst::Branch { .. }
                                | MInst::Jump { .. }
                                | MInst::Return
                                | MInst::ReturnError { .. }
                        );
                    }
                }
                true
            });
            if block.insts.len() < before {
                removed = true;
            }

            if remove_unused_phis {
                let phi_before = block.phis.len();
                block.phis.retain(|phi| used.contains(&phi.dst));
                if block.phis.len() < phi_before {
                    removed = true;
                }
            }
        }

        if !removed {
            break;
        }
    }
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
mod tests {
    use super::*;

    fn make_func(insts: Vec<MInst>, vreg_count: u32) -> MFunction {
        let mut vregs = VRegAllocator::new();
        for _ in 0..vreg_count {
            vregs.alloc();
        }
        let spill_descs = (0..vreg_count).map(|_| SpillDesc::transient()).collect();
        let mut func = MFunction::new(vregs, spill_descs);
        let mut block = MBlock::new(BlockId(0));
        block.insts = insts;
        func.push_block(block);
        func
    }

    fn byte_load_pack(with_write_barrier: bool) -> MFunction {
        let mut insts = (0..8)
            .map(|byte| MInst::Load {
                dst: VReg(byte),
                base: BaseReg::SimState,
                offset: 32 + byte as i32,
                size: OpSize::S8,
            })
            .collect::<Vec<_>>();
        if with_write_barrier {
            insts.push(MInst::Store {
                base: BaseReg::SimState,
                offset: 200,
                src: VReg(0),
                size: OpSize::S8,
            });
        }
        let mut accumulated = VReg(0);
        for byte in 1..8 {
            let shifted = VReg(7 + byte);
            insts.push(MInst::ShlImm {
                dst: shifted,
                src: VReg(byte),
                imm: byte as u8 * 8,
            });
            let merged = VReg(14 + byte);
            insts.push(MInst::Or {
                dst: merged,
                lhs: accumulated,
                rhs: shifted,
            });
            accumulated = merged;
        }
        insts.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 100,
            src: accumulated,
            size: OpSize::S64,
        });
        make_func(insts, 22)
    }

    #[test]
    fn folds_little_endian_byte_load_pack_into_one_native_load() {
        let mut func = byte_load_pack(false);
        fold_contiguous_load_packs(&mut func);
        dead_code_eliminate(&mut func);

        assert!(matches!(
            func.blocks[0].insts.as_slice(),
            [
                MInst::Load {
                    dst: VReg(21),
                    base: BaseReg::SimState,
                    offset: 32,
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 100,
                    src: VReg(21),
                    size: OpSize::S64,
                }
            ]
        ));
    }

    #[test]
    fn contiguous_load_pack_does_not_cross_a_memory_write() {
        let mut func = byte_load_pack(true);
        fold_contiguous_load_packs(&mut func);
        dead_code_eliminate(&mut func);

        assert!(!func.blocks[0].insts.iter().any(|instruction| matches!(
            instruction,
            MInst::Load {
                offset: 32,
                size: OpSize::S64,
                ..
            }
        )));
        assert!(
            func.blocks[0]
                .insts
                .iter()
                .any(|instruction| matches!(instruction, MInst::Or { dst: VReg(21), .. }))
        );
    }

    fn partial_store_round_trip(extra_read: bool, final_store: bool) -> MFunction {
        let mut insts = vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 100,
                size: OpSize::S8,
            },
            MInst::AndImm {
                dst: VReg(1),
                src: VReg(0),
                imm: !7,
            },
            MInst::LoadImm {
                dst: VReg(2),
                value: 5,
            },
            MInst::Or {
                dst: VReg(3),
                lhs: VReg(1),
                rhs: VReg(2),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 100,
                src: VReg(3),
                size: OpSize::S8,
            },
            MInst::Load {
                dst: VReg(4),
                base: BaseReg::SimState,
                offset: 100,
                size: OpSize::S64,
            },
        ];
        if extra_read {
            insts.push(MInst::Load {
                dst: VReg(5),
                base: BaseReg::SimState,
                offset: 100,
                size: OpSize::S8,
            });
        }
        if final_store {
            insts.push(MInst::Store {
                base: BaseReg::SimState,
                offset: 100,
                src: VReg(4),
                size: OpSize::S64,
            });
        }
        let mut func = make_func(insts, 6);
        func.spill_descs[3] = SpillDesc::transient().with_state_insert(VReg(2), 0, 3);
        func
    }

    #[test]
    fn promotes_dead_partial_store_through_its_only_wide_observer() {
        let mut func = partial_store_round_trip(false, true);
        promote_partial_store_round_trips(&mut func);

        assert!(matches!(
            func.blocks[0].insts[0],
            MInst::Load {
                base: BaseReg::SimState,
                offset: 100,
                size: OpSize::S64,
                ..
            }
        ));
        assert_eq!(
            func.blocks[0]
                .insts
                .iter()
                .filter(|inst| matches!(
                    inst,
                    MInst::Store {
                        size: OpSize::S8,
                        ..
                    }
                ))
                .count(),
            0
        );
        assert_eq!(
            func.blocks[0]
                .insts
                .iter()
                .filter(|inst| matches!(
                    inst,
                    MInst::Load {
                        size: OpSize::S64,
                        ..
                    }
                ))
                .count(),
            1
        );
        assert!(func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::AndImm32 {
                src: VReg(2),
                imm: 7,
                ..
            }
        )));
        assert!(!func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::AndImm32 {
                src: VReg(3),
                imm: 0xff,
                ..
            }
        )));
        assert!(matches!(
            func.blocks[0].insts.last(),
            Some(MInst::Store {
                base: BaseReg::SimState,
                offset: 100,
                src: VReg(4),
                size: OpSize::S64,
            })
        ));
    }

    #[test]
    fn recovers_partial_insert_when_store_provenance_is_stale() {
        let mut func = partial_store_round_trip(false, true);
        func.spill_descs[3].state_insert = None;
        promote_partial_store_round_trips(&mut func);

        assert!(func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::AndImm32 {
                src: VReg(2),
                imm: 7,
                ..
            }
        )));
        assert!(!func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::AndImm32 {
                src: VReg(3),
                imm: 0xff,
                ..
            }
        )));
    }

    #[test]
    fn keeps_partial_store_observed_again_before_covering_overwrite() {
        let mut func = partial_store_round_trip(true, true);
        promote_partial_store_round_trips(&mut func);

        assert!(func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::Store {
                base: BaseReg::SimState,
                offset: 100,
                size: OpSize::S8,
                ..
            }
        )));
    }

    #[test]
    fn keeps_partial_store_live_at_block_exit() {
        let mut func = partial_store_round_trip(false, false);
        promote_partial_store_round_trips(&mut func);

        assert!(func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::Store {
                base: BaseReg::SimState,
                offset: 100,
                size: OpSize::S8,
                ..
            }
        )));
    }

    #[test]
    fn promotes_disjoint_partial_stores_in_program_order() {
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 0x12,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 100,
                    src: VReg(0),
                    size: OpSize::S8,
                },
                MInst::LoadImm {
                    dst: VReg(1),
                    value: 0x34,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 101,
                    src: VReg(1),
                    size: OpSize::S8,
                },
                MInst::Load {
                    dst: VReg(2),
                    base: BaseReg::SimState,
                    offset: 100,
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 100,
                    src: VReg(2),
                    size: OpSize::S64,
                },
            ],
            3,
        );
        promote_partial_store_round_trips(&mut func);

        assert_eq!(
            func.blocks[0]
                .insts
                .iter()
                .filter(|inst| matches!(
                    inst,
                    MInst::Store {
                        size: OpSize::S8,
                        ..
                    }
                ))
                .count(),
            0
        );
        assert!(
            func.blocks[0]
                .insts
                .iter()
                .any(|inst| matches!(inst, MInst::ShlImm { imm: 8, .. }))
        );
    }

    #[test]
    fn recovers_nonzero_bit_insert_without_assuming_padding_is_zero() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 100,
                    size: OpSize::S8,
                },
                MInst::AndImm {
                    dst: VReg(1),
                    src: VReg(0),
                    imm: !0x38,
                },
                MInst::LoadImm {
                    dst: VReg(2),
                    value: 5,
                },
                MInst::ShlImm {
                    dst: VReg(3),
                    src: VReg(2),
                    imm: 3,
                },
                MInst::Or {
                    dst: VReg(4),
                    lhs: VReg(1),
                    rhs: VReg(3),
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 100,
                    src: VReg(4),
                    size: OpSize::S8,
                },
                MInst::Load {
                    dst: VReg(5),
                    base: BaseReg::SimState,
                    offset: 100,
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 100,
                    src: VReg(5),
                    size: OpSize::S64,
                },
            ],
            6,
        );
        promote_partial_store_round_trips(&mut func);

        assert!(func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::ShlImm {
                src: VReg(2),
                imm: 3,
                ..
            }
        )));
        assert!(!func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::AndImm32 {
                src: VReg(4),
                imm: 0xff,
                ..
            }
        )));
    }

    #[test]
    fn keeps_partial_store_across_unknown_direct_alias() {
        let mut func = partial_store_round_trip(false, true);
        func.blocks[0].insts.insert(
            5,
            MInst::StoreIndexed {
                base: BaseReg::SimState,
                offset: 0,
                index: VReg(2),
                src: VReg(2),
                size: OpSize::S8,
                alias_range: None,
            },
        );
        promote_partial_store_round_trips(&mut func);

        assert!(func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::Store {
                base: BaseReg::SimState,
                offset: 100,
                size: OpSize::S8,
                ..
            }
        )));
    }

    #[test]
    fn sinks_selected_indexed_loads_to_one_selected_address() {
        let alias = |offset| MemoryAliasRange::new(offset, 8);
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 16,
                },
                MInst::LoadImm {
                    dst: VReg(1),
                    value: 1,
                },
                MInst::LoadIndexed {
                    dst: VReg(2),
                    base: BaseReg::SimState,
                    offset: 100,
                    index: VReg(0),
                    scale: 1,
                    size: OpSize::S64,
                    alias_range: alias(100),
                },
                MInst::LoadIndexed {
                    dst: VReg(3),
                    base: BaseReg::SimState,
                    offset: 200,
                    index: VReg(0),
                    scale: 1,
                    size: OpSize::S64,
                    alias_range: alias(200),
                },
                MInst::LoadIndexed {
                    dst: VReg(4),
                    base: BaseReg::SimState,
                    offset: 300,
                    index: VReg(0),
                    scale: 1,
                    size: OpSize::S64,
                    alias_range: alias(300),
                },
                MInst::LoadIndexed {
                    dst: VReg(5),
                    base: BaseReg::SimState,
                    offset: 400,
                    index: VReg(0),
                    scale: 1,
                    size: OpSize::S64,
                    alias_range: alias(400),
                },
                MInst::CmpImmSelect {
                    dst: VReg(6),
                    lhs: VReg(1),
                    imm: 0,
                    kind: CmpKind::Eq,
                    true_val: VReg(2),
                    false_val: VReg(3),
                },
                MInst::GuardedCmpSelect {
                    dst: VReg(7),
                    guard: VReg(1),
                    lhs: VReg(0),
                    rhs: VReg(1),
                    kind: CmpKind::Eq,
                    true_val: VReg(4),
                    false_val: VReg(5),
                },
                MInst::Select {
                    dst: VReg(8),
                    cond: VReg(1),
                    true_val: VReg(6),
                    false_val: VReg(7),
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 500,
                    src: VReg(8),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            9,
        );

        sink_selected_indexed_loads(&mut func);

        let insts = &func.blocks[0].insts;
        let loads = insts
            .iter()
            .filter_map(|inst| match inst {
                MInst::LoadIndexed {
                    dst,
                    base,
                    offset,
                    size,
                    alias_range,
                    ..
                } => Some((*dst, *base, *offset, *size, *alias_range)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            loads,
            vec![(
                VReg(8),
                BaseReg::SimState,
                100,
                OpSize::S64,
                MemoryAliasRange::new(100, 308),
            )],
            "{insts:#?}"
        );
        assert_eq!(
            insts
                .iter()
                .filter(|inst| select_values(inst).is_some())
                .count(),
            3,
            "{insts:#?}"
        );
        assert!(
            insts.iter().any(|inst| matches!(inst, MInst::Add { .. })),
            "{insts:#?}"
        );
        assert_eq!(func.verify_result(), Ok(()));
    }

    #[test]
    fn selected_indexed_load_sinking_does_not_cross_a_write() {
        let mut func = make_func(
            vec![
                MInst::LoadIndexed {
                    dst: VReg(2),
                    base: BaseReg::SimState,
                    offset: 100,
                    index: VReg(0),
                    scale: 1,
                    size: OpSize::S64,
                    alias_range: MemoryAliasRange::new(100, 8),
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 104,
                    src: VReg(1),
                    size: OpSize::S64,
                },
                MInst::LoadIndexed {
                    dst: VReg(3),
                    base: BaseReg::SimState,
                    offset: 200,
                    index: VReg(0),
                    scale: 1,
                    size: OpSize::S64,
                    alias_range: MemoryAliasRange::new(200, 8),
                },
                MInst::Select {
                    dst: VReg(4),
                    cond: VReg(1),
                    true_val: VReg(2),
                    false_val: VReg(3),
                },
                MInst::Return,
            ],
            5,
        );

        sink_selected_indexed_loads(&mut func);

        assert_eq!(
            func.blocks[0]
                .insts
                .iter()
                .filter(|inst| matches!(inst, MInst::LoadIndexed { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn selected_indexed_load_sinking_requires_one_shared_index() {
        let mut func = make_func(
            vec![
                MInst::LoadIndexed {
                    dst: VReg(3),
                    base: BaseReg::SimState,
                    offset: 100,
                    index: VReg(0),
                    scale: 1,
                    size: OpSize::S64,
                    alias_range: MemoryAliasRange::new(100, 8),
                },
                MInst::LoadIndexed {
                    dst: VReg(4),
                    base: BaseReg::SimState,
                    offset: 200,
                    index: VReg(1),
                    scale: 1,
                    size: OpSize::S64,
                    alias_range: MemoryAliasRange::new(200, 8),
                },
                MInst::Select {
                    dst: VReg(5),
                    cond: VReg(2),
                    true_val: VReg(3),
                    false_val: VReg(4),
                },
                MInst::Return,
            ],
            6,
        );

        sink_selected_indexed_loads(&mut func);

        assert_eq!(
            func.blocks[0]
                .insts
                .iter()
                .filter(|inst| matches!(inst, MInst::LoadIndexed { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn algebraic_simplify_sinks_repeated_leaf_masks_to_and_root() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S8,
                },
                MInst::Load {
                    dst: VReg(1),
                    base: BaseReg::SimState,
                    offset: 1,
                    size: OpSize::S8,
                },
                MInst::Load {
                    dst: VReg(2),
                    base: BaseReg::SimState,
                    offset: 2,
                    size: OpSize::S8,
                },
                MInst::AndImm32 {
                    dst: VReg(3),
                    src: VReg(0),
                    imm: 1,
                },
                MInst::AndImm32 {
                    dst: VReg(4),
                    src: VReg(1),
                    imm: 1,
                },
                MInst::AndImm32 {
                    dst: VReg(5),
                    src: VReg(2),
                    imm: 1,
                },
                MInst::And {
                    dst: VReg(6),
                    lhs: VReg(3),
                    rhs: VReg(4),
                },
                MInst::And {
                    dst: VReg(7),
                    lhs: VReg(6),
                    rhs: VReg(5),
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 8,
                    src: VReg(7),
                    size: OpSize::S8,
                },
                MInst::Return,
            ],
            8,
        );

        algebraic_simplify(&mut func);

        assert_eq!(
            func.blocks[0]
                .insts
                .iter()
                .filter(|instruction| matches!(
                    instruction,
                    MInst::AndImm { .. } | MInst::AndImm32 { .. }
                ))
                .count(),
            1
        );
        assert_eq!(
            func.blocks[0]
                .insts
                .iter()
                .filter(|instruction| matches!(
                    instruction,
                    MInst::And { .. } | MInst::And32 { .. }
                ))
                .count(),
            2
        );
        assert!(func.blocks[0].insts.iter().any(|instruction| matches!(
            instruction,
            MInst::AndImm32 {
                dst: VReg(7),
                imm: 1,
                ..
            }
        )));
        func.verify();
    }

    #[test]
    fn algebraic_simplify_combines_different_and_masks() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S8,
                },
                MInst::Load {
                    dst: VReg(1),
                    base: BaseReg::SimState,
                    offset: 1,
                    size: OpSize::S8,
                },
                MInst::AndImm32 {
                    dst: VReg(2),
                    src: VReg(0),
                    imm: 0xf,
                },
                MInst::AndImm32 {
                    dst: VReg(3),
                    src: VReg(1),
                    imm: 3,
                },
                MInst::And {
                    dst: VReg(4),
                    lhs: VReg(2),
                    rhs: VReg(3),
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 8,
                    src: VReg(4),
                    size: OpSize::S8,
                },
                MInst::Return,
            ],
            5,
        );

        algebraic_simplify(&mut func);

        assert!(func.blocks[0].insts.iter().any(|instruction| matches!(
            instruction,
            MInst::AndImm32 {
                dst: VReg(4),
                imm: 3,
                ..
            }
        )));
        assert_eq!(
            func.blocks[0]
                .insts
                .iter()
                .filter(|instruction| matches!(
                    instruction,
                    MInst::AndImm { .. } | MInst::AndImm32 { .. }
                ))
                .count(),
            1
        );
        func.verify();
    }

    #[test]
    fn algebraic_simplify_keeps_shared_leaf_masks() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S8,
                },
                MInst::Load {
                    dst: VReg(1),
                    base: BaseReg::SimState,
                    offset: 1,
                    size: OpSize::S8,
                },
                MInst::Load {
                    dst: VReg(2),
                    base: BaseReg::SimState,
                    offset: 2,
                    size: OpSize::S8,
                },
                MInst::AndImm32 {
                    dst: VReg(3),
                    src: VReg(0),
                    imm: 1,
                },
                MInst::AndImm32 {
                    dst: VReg(4),
                    src: VReg(1),
                    imm: 1,
                },
                MInst::AndImm32 {
                    dst: VReg(5),
                    src: VReg(2),
                    imm: 1,
                },
                MInst::And {
                    dst: VReg(6),
                    lhs: VReg(3),
                    rhs: VReg(4),
                },
                MInst::And {
                    dst: VReg(7),
                    lhs: VReg(3),
                    rhs: VReg(5),
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 8,
                    src: VReg(6),
                    size: OpSize::S8,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 9,
                    src: VReg(7),
                    size: OpSize::S8,
                },
                MInst::Return,
            ],
            8,
        );
        let original = func.blocks[0].insts.clone();

        algebraic_simplify(&mut func);

        assert_eq!(func.blocks[0].insts, original);
        func.verify();
    }

    #[test]
    fn folds_complete_bit_partition_reconstruction_to_original_word() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::Mov32 {
                    dst: VReg(1),
                    src: VReg(0),
                },
                MInst::AndImm32 {
                    dst: VReg(2),
                    src: VReg(1),
                    imm: 0x1,
                },
                MInst::ShrImm {
                    dst: VReg(3),
                    src: VReg(1),
                    imm: 1,
                },
                MInst::AndImm32 {
                    dst: VReg(4),
                    src: VReg(3),
                    imm: 0x7f,
                },
                MInst::ShlImm {
                    dst: VReg(5),
                    src: VReg(4),
                    imm: 1,
                },
                MInst::Or {
                    dst: VReg(6),
                    lhs: VReg(2),
                    rhs: VReg(5),
                },
                MInst::ShrImm {
                    dst: VReg(7),
                    src: VReg(1),
                    imm: 8,
                },
                MInst::AndImm32 {
                    dst: VReg(8),
                    src: VReg(7),
                    imm: 0xff,
                },
                MInst::ShlImm {
                    dst: VReg(9),
                    src: VReg(8),
                    imm: 8,
                },
                MInst::Or {
                    dst: VReg(10),
                    lhs: VReg(6),
                    rhs: VReg(9),
                },
                MInst::ShrImm {
                    dst: VReg(11),
                    src: VReg(1),
                    imm: 16,
                },
                MInst::ShlImm {
                    dst: VReg(12),
                    src: VReg(11),
                    imm: 16,
                },
                MInst::Or {
                    dst: VReg(13),
                    lhs: VReg(10),
                    rhs: VReg(12),
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 8,
                    src: VReg(13),
                    size: OpSize::S32,
                },
                MInst::Return,
            ],
            14,
        );

        fold_reconstructed_bit_partitions(&mut func);

        assert!(matches!(
            func.blocks[0].insts[13],
            MInst::Mov32 {
                dst: VReg(13),
                src: VReg(0)
            }
        ));
    }

    #[test]
    fn folds_single_use_compare_branch_before_allocation() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::Load {
                    dst: VReg(1),
                    base: BaseReg::SimState,
                    offset: 8,
                    size: OpSize::S64,
                },
                MInst::Cmp {
                    dst: VReg(2),
                    lhs: VReg(0),
                    rhs: VReg(1),
                    kind: CmpKind::LtU,
                },
                MInst::Branch {
                    cond: VReg(2),
                    true_bb: BlockId(1),
                    false_bb: BlockId(2),
                },
            ],
            3,
        );

        assert_eq!(fold_register_branch_predicates(&mut func), 1);
        assert!(matches!(
            func.blocks[0].insts.last(),
            Some(MInst::BranchPred {
                predicate: BranchPredicate::Compare {
                    lhs: VReg(0),
                    rhs: VReg(1),
                    kind: CmpKind::LtU,
                },
                ..
            })
        ));
        assert!(
            !func.blocks[0]
                .insts
                .iter()
                .any(|instruction| instruction.def() == Some(VReg(2)))
        );
    }

    #[test]
    fn delays_direct_memory_branch_until_after_state_forwarding() {
        let original = vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 4,
                size: OpSize::S16,
            },
            MInst::Branch {
                cond: VReg(0),
                true_bb: BlockId(1),
                false_bb: BlockId(2),
            },
        ];
        let mut early = make_func(original.clone(), 1);
        assert_eq!(fold_register_branch_predicates(&mut early), 0);
        assert_eq!(early.blocks[0].insts, original);

        let mut late = make_func(original, 1);
        assert_eq!(fold_memory_branch_predicates(&mut late), 1);
        assert!(matches!(
            late.blocks[0].insts.as_slice(),
            [MInst::BranchPred {
                predicate: BranchPredicate::MemoryNonZero {
                    base: BaseReg::SimState,
                    offset: 4,
                    size: OpSize::S16,
                },
                ..
            }]
        ));
    }

    #[test]
    fn branch_predicate_keeps_a_compare_result_used_on_an_edge() {
        let mut vregs = VRegAllocator::new();
        let input = vregs.alloc();
        let alternative = vregs.alloc();
        let condition = vregs.alloc();
        let merged = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);

        let mut entry = MBlock::new(BlockId(0));
        entry.insts = vec![
            MInst::CmpImm {
                dst: condition,
                lhs: input,
                imm: 0,
                kind: CmpKind::Ne,
            },
            MInst::Branch {
                cond: condition,
                true_bb: BlockId(1),
                false_bb: BlockId(2),
            },
        ];
        let mut true_block = MBlock::new(BlockId(1));
        true_block.push(MInst::Jump { target: BlockId(3) });
        let mut false_block = MBlock::new(BlockId(2));
        false_block.push(MInst::Jump { target: BlockId(3) });
        let mut join = MBlock::new(BlockId(3));
        join.phis.push(PhiNode {
            dst: merged,
            sources: vec![(BlockId(1), condition), (BlockId(2), alternative)],
        });
        join.push(MInst::Return);
        func.blocks = vec![entry, true_block, false_block, join];

        assert_eq!(fold_register_branch_predicates(&mut func), 0);
        assert!(matches!(
            func.blocks[0].insts.last(),
            Some(MInst::Branch { cond, .. }) if *cond == condition
        ));
    }

    #[test]
    fn branch_predicate_does_not_consume_an_unrelated_adjacent_compare() {
        let original = vec![
            MInst::LoadImm {
                dst: VReg(2),
                value: 7,
            },
            MInst::LoadImm {
                dst: VReg(0),
                value: 1,
            },
            MInst::CmpImm {
                dst: VReg(1),
                lhs: VReg(2),
                imm: 0,
                kind: CmpKind::Eq,
            },
            MInst::Branch {
                cond: VReg(0),
                true_bb: BlockId(1),
                false_bb: BlockId(2),
            },
        ];
        let mut func = make_func(original.clone(), 3);

        assert_eq!(fold_register_branch_predicates(&mut func), 0);
        assert_eq!(func.blocks[0].insts, original);
    }

    #[test]
    fn bit_partition_reconstruction_rejects_mixed_sources() {
        let mut func = make_func(
            vec![
                MInst::AndImm32 {
                    dst: VReg(2),
                    src: VReg(0),
                    imm: 0xff,
                },
                MInst::AndImm32 {
                    dst: VReg(3),
                    src: VReg(1),
                    imm: 0xff00,
                },
                MInst::Or {
                    dst: VReg(4),
                    lhs: VReg(2),
                    rhs: VReg(3),
                },
                MInst::Return,
            ],
            5,
        );

        fold_reconstructed_bit_partitions(&mut func);

        assert!(matches!(func.blocks[0].insts[2], MInst::Or { .. }));
    }

    #[test]
    fn bit_partition_reconstruction_rejects_relocated_bits() {
        let mut func = make_func(
            vec![
                MInst::AndImm32 {
                    dst: VReg(1),
                    src: VReg(0),
                    imm: 0xff,
                },
                MInst::ShlImm {
                    dst: VReg(2),
                    src: VReg(1),
                    imm: 8,
                },
                MInst::AndImm32 {
                    dst: VReg(3),
                    src: VReg(0),
                    imm: 0xff00,
                },
                MInst::Or {
                    dst: VReg(4),
                    lhs: VReg(2),
                    rhs: VReg(3),
                },
                MInst::Return,
            ],
            5,
        );

        fold_reconstructed_bit_partitions(&mut func);

        assert!(matches!(func.blocks[0].insts[3], MInst::Or { .. }));
    }

    #[test]
    fn full_word_masked_merge_collapses_to_a_direct_store() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::Load {
                    dst: VReg(1),
                    base: BaseReg::SimState,
                    offset: 8,
                    size: OpSize::S64,
                },
                MInst::AndImm32 {
                    dst: VReg(2),
                    src: VReg(1),
                    imm: 0,
                },
                MInst::Or {
                    dst: VReg(3),
                    lhs: VReg(2),
                    rhs: VReg(0),
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 8,
                    src: VReg(3),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            4,
        );

        optimize(&mut func);

        assert!(func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::Load {
                offset: 0,
                size: OpSize::S64,
                ..
            }
        )));
        assert!(!func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::Load {
                offset: 8,
                size: OpSize::S64,
                ..
            }
        )));
        assert!(func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::Store {
                offset: 8,
                src: VReg(0),
                size: OpSize::S64,
                ..
            }
        )));
        assert!(!func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::And { .. }
                | MInst::And32 { .. }
                | MInst::AndImm { .. }
                | MInst::AndImm32 { .. }
                | MInst::Or { .. }
                | MInst::Or32 { .. }
                | MInst::OrImm { .. }
        )));
    }

    #[test]
    fn word32_algebraic_identities_keep_their_zero_extension() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::LoadImm {
                    dst: VReg(1),
                    value: 0,
                },
                MInst::LoadImm {
                    dst: VReg(2),
                    value: 1,
                },
                MInst::LoadImm {
                    dst: VReg(3),
                    value: u32::MAX as u64,
                },
                MInst::Add32 {
                    dst: VReg(4),
                    lhs: VReg(0),
                    rhs: VReg(1),
                },
                MInst::Sub32 {
                    dst: VReg(5),
                    lhs: VReg(0),
                    rhs: VReg(1),
                },
                MInst::Mul32 {
                    dst: VReg(6),
                    lhs: VReg(0),
                    rhs: VReg(2),
                },
                MInst::And32 {
                    dst: VReg(7),
                    lhs: VReg(0),
                    rhs: VReg(3),
                },
                MInst::Or32 {
                    dst: VReg(8),
                    lhs: VReg(0),
                    rhs: VReg(1),
                },
                MInst::Xor32 {
                    dst: VReg(9),
                    lhs: VReg(0),
                    rhs: VReg(1),
                },
                MInst::AndImm32 {
                    dst: VReg(10),
                    src: VReg(0),
                    imm: u32::MAX,
                },
                MInst::Return,
            ],
            11,
        );

        algebraic_simplify(&mut func);

        for (index, dst) in (4..=10).enumerate() {
            assert!(
                matches!(
                    func.blocks[0].insts[index + 4],
                    MInst::Mov32 {
                        dst: actual_dst,
                        src: VReg(0)
                    } if actual_dst == VReg(dst)
                ),
                "word32 identity at v{dst} lost its zero extension"
            );
        }
    }

    #[test]
    fn word32_constant_fold_truncates_inputs_and_zero_extends_results() {
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: u64::MAX,
                },
                MInst::LoadImm {
                    dst: VReg(1),
                    value: 2,
                },
                MInst::Add32 {
                    dst: VReg(2),
                    lhs: VReg(0),
                    rhs: VReg(1),
                },
                MInst::Sub32 {
                    dst: VReg(3),
                    lhs: VReg(0),
                    rhs: VReg(1),
                },
                MInst::Mul32 {
                    dst: VReg(4),
                    lhs: VReg(0),
                    rhs: VReg(1),
                },
                MInst::And32 {
                    dst: VReg(5),
                    lhs: VReg(0),
                    rhs: VReg(1),
                },
                MInst::Or32 {
                    dst: VReg(6),
                    lhs: VReg(0),
                    rhs: VReg(1),
                },
                MInst::Xor32 {
                    dst: VReg(7),
                    lhs: VReg(0),
                    rhs: VReg(1),
                },
                MInst::AndImm32 {
                    dst: VReg(8),
                    src: VReg(0),
                    imm: 0x8000_0000,
                },
                MInst::Return,
            ],
            9,
        );

        constant_fold(&mut func);

        for (index, expected) in [
            1,
            0xffff_fffd,
            0xffff_fffe,
            2,
            0xffff_ffff,
            0xffff_fffd,
            0x8000_0000,
        ]
        .into_iter()
        .enumerate()
        {
            let dst = VReg(index as u32 + 2);
            assert!(
                matches!(
                    func.blocks[0].insts[index + 2],
                    MInst::LoadImm {
                        dst: actual_dst,
                        value
                    } if actual_dst == dst && value == expected
                ),
                "word32 constant fold for {dst} produced the wrong value"
            );
        }
    }

    #[test]
    fn copy_propagates_only_redundant_word32_snapshots() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::Mov32 {
                    dst: VReg(1),
                    src: VReg(0),
                },
                MInst::Mov32 {
                    dst: VReg(2),
                    src: VReg(1),
                },
                MInst::Mov32 {
                    dst: VReg(3),
                    src: VReg(1),
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 8,
                    src: VReg(2),
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(3),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            4,
        );

        copy_propagate(&mut func);

        assert_eq!(
            func.blocks[0]
                .insts
                .iter()
                .filter(|inst| matches!(inst, MInst::Mov32 { .. }))
                .count(),
            1,
            "the first Mov32 is a real 64-to-32 truncation"
        );
        let stored = func.blocks[0]
            .insts
            .iter()
            .filter_map(|inst| match inst {
                MInst::Store { src, .. } => Some(*src),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(stored, vec![VReg(1), VReg(1)]);
    }

    #[test]
    fn folded_constants_are_marked_rematerializable() {
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 3,
                },
                MInst::ShlImm {
                    dst: VReg(1),
                    src: VReg(0),
                    imm: 2,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: VReg(1),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            2,
        );

        optimize(&mut func);

        let constant = func.blocks[0]
            .insts
            .iter()
            .find_map(|inst| match inst {
                MInst::LoadImm { dst, value: 12 } => Some(*dst),
                _ => None,
            })
            .expect("the shift of a known constant must fold");
        assert!(matches!(
            func.spill_desc(constant).map(|desc| &desc.kind),
            Some(SpillKind::Remat { value: 12 })
        ));
    }

    #[test]
    fn redundant_mask_elimination_keeps_mask_after_subtraction() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S8,
                },
                MInst::LoadImm {
                    dst: VReg(1),
                    value: 0,
                },
                MInst::Sub {
                    dst: VReg(2),
                    lhs: VReg(1),
                    rhs: VReg(0),
                },
                MInst::AndImm {
                    dst: VReg(3),
                    src: VReg(2),
                    imm: 0x1ff,
                },
                MInst::Return,
            ],
            4,
        );

        redundant_mask_eliminate(&mut func);

        // `0 - 1` is all ones, not a nine-bit result.  The mask is needed.
        assert!(matches!(
            func.blocks[0].insts[3],
            MInst::AndImm {
                dst: VReg(3),
                src: VReg(2),
                imm: 0x1ff,
            }
        ));
    }

    #[test]
    fn redundant_mask_elimination_keeps_mask_after_unchecked_bsr() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::Bsr {
                    dst: VReg(1),
                    src: VReg(0),
                },
                MInst::AndImm {
                    dst: VReg(2),
                    src: VReg(1),
                    imm: 0x3f,
                },
                MInst::Return,
            ],
            3,
        );

        redundant_mask_eliminate(&mut func);

        assert!(matches!(
            func.blocks[0].insts[2],
            MInst::AndImm {
                dst: VReg(2),
                src: VReg(1),
                imm: 0x3f,
            }
        ));
    }

    #[test]
    fn redundant_word32_register_mask_is_eliminated() {
        let mask = 0x3fff_ffff;
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::AndImm32 {
                    dst: VReg(1),
                    src: VReg(0),
                    imm: mask,
                },
                MInst::LoadImm {
                    dst: VReg(2),
                    value: u64::from(mask),
                },
                MInst::And32 {
                    dst: VReg(3),
                    lhs: VReg(1),
                    rhs: VReg(2),
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 8,
                    src: VReg(3),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            4,
        );

        redundant_mask_eliminate(&mut func);

        assert!(matches!(
            func.blocks[0].insts[3],
            MInst::Mov {
                dst: VReg(3),
                src: VReg(1),
            }
        ));
    }

    #[test]
    fn redundant_word32_mask_preserves_required_zero_extension() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S8,
                },
                MInst::ShlImm {
                    dst: VReg(1),
                    src: VReg(0),
                    imm: 40,
                },
                MInst::AndImm32 {
                    dst: VReg(2),
                    src: VReg(1),
                    imm: 0xff,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 8,
                    src: VReg(2),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            3,
        );

        redundant_mask_eliminate(&mut func);

        assert!(matches!(
            func.blocks[0].insts[2],
            MInst::Mov32 {
                dst: VReg(2),
                src: VReg(1),
            }
        ));
    }

    #[test]
    fn redundant_mask_elimination_follows_phi_across_blocks() {
        let mut func = make_func(Vec::new(), 4);
        func.blocks.clear();

        let mut left = MBlock::new(BlockId(0));
        left.insts = vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S8,
            },
            MInst::Jump { target: BlockId(2) },
        ];
        let mut right = MBlock::new(BlockId(1));
        right.insts = vec![
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 8,
                size: OpSize::S16,
            },
            MInst::Jump { target: BlockId(2) },
        ];
        let mut join = MBlock::new(BlockId(2));
        join.phis.push(PhiNode {
            dst: VReg(2),
            sources: vec![(BlockId(0), VReg(0)), (BlockId(1), VReg(1))],
        });
        join.insts = vec![
            MInst::AndImm {
                dst: VReg(3),
                src: VReg(2),
                imm: u64::from(u16::MAX),
            },
            MInst::Return,
        ];
        func.push_block(left);
        func.push_block(right);
        func.push_block(join);

        redundant_mask_eliminate(&mut func);

        assert!(matches!(
            func.blocks[2].insts[0],
            MInst::Mov {
                dst: VReg(3),
                src: VReg(2),
            }
        ));
    }

    #[test]
    fn redundant_mask_elimination_keeps_mask_for_wide_phi_arm() {
        let mut func = make_func(Vec::new(), 4);
        func.blocks.clear();

        let mut left = MBlock::new(BlockId(0));
        left.insts = vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S8,
            },
            MInst::Jump { target: BlockId(2) },
        ];
        let mut right = MBlock::new(BlockId(1));
        right.insts = vec![
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 8,
                size: OpSize::S64,
            },
            MInst::Jump { target: BlockId(2) },
        ];
        let mut join = MBlock::new(BlockId(2));
        join.phis.push(PhiNode {
            dst: VReg(2),
            sources: vec![(BlockId(0), VReg(0)), (BlockId(1), VReg(1))],
        });
        join.insts = vec![
            MInst::AndImm {
                dst: VReg(3),
                src: VReg(2),
                imm: u64::from(u16::MAX),
            },
            MInst::Return,
        ];
        func.push_block(left);
        func.push_block(right);
        func.push_block(join);

        redundant_mask_eliminate(&mut func);

        assert!(matches!(
            func.blocks[2].insts[0],
            MInst::AndImm {
                dst: VReg(3),
                src: VReg(2),
                imm,
            } if imm == u64::from(u16::MAX)
        ));
    }

    #[test]
    fn repeated_large_register_mask_is_eliminated() {
        let mask = 0x00ff_00ff_00ff_00ff;
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::LoadImm {
                    dst: VReg(1),
                    value: mask,
                },
                MInst::And {
                    dst: VReg(2),
                    lhs: VReg(0),
                    rhs: VReg(1),
                },
                MInst::And {
                    dst: VReg(3),
                    lhs: VReg(2),
                    rhs: VReg(1),
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 8,
                    src: VReg(3),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            4,
        );

        redundant_mask_eliminate(&mut func);

        assert!(matches!(
            func.blocks[0].insts[3],
            MInst::Mov {
                dst: VReg(3),
                src: VReg(2),
            }
        ));
    }

    #[test]
    fn dominators_do_not_depend_on_block_storage_order() {
        // Storage order is entry, join, left, right; reverse postorder is
        // entry, right, left, join.
        let preds = vec![vec![], vec![2, 3], vec![0], vec![0]];
        let succs = vec![vec![2, 3], vec![], vec![1], vec![1]];
        assert_eq!(
            compute_dominators(4, &preds, &succs),
            vec![None, Some(0), Some(0), Some(0)]
        );
    }

    #[test]
    fn folds_only_proven_in_range_variable_shift_guards() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::AndImm {
                    dst: VReg(1),
                    src: VReg(0),
                    imm: 7,
                },
                MInst::Shr {
                    dst: VReg(2),
                    lhs: VReg(0),
                    rhs: VReg(1),
                },
                MInst::LoadImm {
                    dst: VReg(3),
                    value: 0,
                },
                MInst::CmpImmSelect {
                    dst: VReg(4),
                    lhs: VReg(1),
                    imm: 64,
                    kind: CmpKind::LtU,
                    true_val: VReg(2),
                    false_val: VReg(3),
                },
                MInst::AndImm {
                    dst: VReg(5),
                    src: VReg(0),
                    imm: 127,
                },
                MInst::CmpImmSelect {
                    dst: VReg(6),
                    lhs: VReg(5),
                    imm: 64,
                    kind: CmpKind::LtU,
                    true_val: VReg(2),
                    false_val: VReg(3),
                },
                MInst::Return,
            ],
            7,
        );

        fold_proven_comparisons(&mut func);

        assert!(matches!(
            func.blocks[0].insts[4],
            MInst::Mov { src: VReg(2), .. }
        ));
        assert!(matches!(
            func.blocks[0].insts[6],
            MInst::CmpImmSelect { .. }
        ));
    }

    #[test]
    fn folds_repeated_boolean_normalization_after_immediate_lowering() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::CmpImm {
                    dst: VReg(1),
                    lhs: VReg(0),
                    imm: 7,
                    kind: CmpKind::LtU,
                },
                MInst::CmpImm {
                    dst: VReg(2),
                    lhs: VReg(1),
                    imm: 0,
                    kind: CmpKind::Ne,
                },
                MInst::Return,
            ],
            3,
        );

        fold_boolean_normalizations(&mut func);

        assert!(matches!(
            func.blocks[0].insts[2],
            MInst::Mov {
                dst: VReg(2),
                src: VReg(1)
            }
        ));
    }

    #[test]
    fn folds_exclusive_boolean_negation_into_register_compare() {
        let mut func = make_func(
            vec![
                MInst::Cmp {
                    dst: VReg(2),
                    lhs: VReg(0),
                    rhs: VReg(1),
                    kind: CmpKind::LtU,
                },
                MInst::CmpImm {
                    dst: VReg(3),
                    lhs: VReg(2),
                    imm: 0,
                    kind: CmpKind::Eq,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: VReg(3),
                    size: OpSize::S8,
                },
                MInst::Return,
            ],
            4,
        );

        fold_boolean_normalizations(&mut func);
        dead_code_eliminate(&mut func);

        assert_eq!(
            func.blocks[0].insts,
            vec![
                MInst::Cmp {
                    dst: VReg(3),
                    lhs: VReg(0),
                    rhs: VReg(1),
                    kind: CmpKind::GeU,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: VReg(3),
                    size: OpSize::S8,
                },
                MInst::Return,
            ]
        );
    }

    #[test]
    fn folds_exclusive_boolean_negation_into_immediate_compare() {
        let mut func = make_func(
            vec![
                MInst::CmpImm {
                    dst: VReg(1),
                    lhs: VReg(0),
                    imm: 7,
                    kind: CmpKind::Eq,
                },
                MInst::CmpImm {
                    dst: VReg(2),
                    lhs: VReg(1),
                    imm: 0,
                    kind: CmpKind::Eq,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: VReg(2),
                    size: OpSize::S8,
                },
                MInst::Return,
            ],
            3,
        );

        fold_boolean_normalizations(&mut func);
        dead_code_eliminate(&mut func);

        assert_eq!(
            func.blocks[0].insts,
            vec![
                MInst::CmpImm {
                    dst: VReg(2),
                    lhs: VReg(0),
                    imm: 7,
                    kind: CmpKind::Ne,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: VReg(2),
                    size: OpSize::S8,
                },
                MInst::Return,
            ]
        );
    }

    #[test]
    fn keeps_shared_boolean_comparison_without_duplication() {
        let mut func = make_func(
            vec![
                MInst::CmpImm {
                    dst: VReg(1),
                    lhs: VReg(0),
                    imm: 7,
                    kind: CmpKind::Eq,
                },
                MInst::CmpImm {
                    dst: VReg(2),
                    lhs: VReg(1),
                    imm: 0,
                    kind: CmpKind::Eq,
                },
                MInst::And {
                    dst: VReg(3),
                    lhs: VReg(1),
                    rhs: VReg(0),
                },
                MInst::Return,
            ],
            4,
        );

        fold_boolean_normalizations(&mut func);

        assert!(matches!(
            func.blocks[0].insts[1],
            MInst::CmpImm {
                lhs: VReg(1),
                imm: 0,
                kind: CmpKind::Eq,
                ..
            }
        ));
    }

    #[test]
    fn fuses_single_use_cmp_select() {
        let mut func = make_func(
            vec![
                MInst::Cmp {
                    dst: VReg(2),
                    lhs: VReg(0),
                    rhs: VReg(1),
                    kind: CmpKind::GtU,
                },
                MInst::Select {
                    dst: VReg(5),
                    cond: VReg(2),
                    true_val: VReg(3),
                    false_val: VReg(4),
                },
                MInst::Return,
            ],
            6,
        );

        fuse_compare_selects(&mut func);

        assert!(matches!(
            func.blocks[0].insts.as_slice(),
            [
                MInst::CmpSelect {
                    dst: VReg(5),
                    lhs: VReg(0),
                    rhs: VReg(1),
                    kind: CmpKind::GtU,
                    true_val: VReg(3),
                    false_val: VReg(4),
                },
                MInst::Return
            ]
        ));
    }

    #[test]
    fn equal_value_selects_remove_their_complete_predicate_graph() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::Load {
                    dst: VReg(1),
                    base: BaseReg::SimState,
                    offset: 8,
                    size: OpSize::S64,
                },
                MInst::CmpImm {
                    dst: VReg(2),
                    lhs: VReg(1),
                    imm: 0,
                    kind: CmpKind::Ne,
                },
                MInst::Select {
                    dst: VReg(3),
                    cond: VReg(2),
                    true_val: VReg(0),
                    false_val: VReg(0),
                },
                MInst::CmpSelect {
                    dst: VReg(4),
                    lhs: VReg(1),
                    rhs: VReg(2),
                    kind: CmpKind::GtU,
                    true_val: VReg(3),
                    false_val: VReg(3),
                },
                MInst::CmpImmSelect {
                    dst: VReg(5),
                    lhs: VReg(2),
                    imm: 1,
                    kind: CmpKind::Eq,
                    true_val: VReg(4),
                    false_val: VReg(4),
                },
                MInst::GuardedCmpSelect {
                    dst: VReg(6),
                    guard: VReg(2),
                    lhs: VReg(1),
                    rhs: VReg(0),
                    kind: CmpKind::LeU,
                    true_val: VReg(5),
                    false_val: VReg(5),
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(6),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            7,
        );

        optimize(&mut func);

        assert!(matches!(
            func.blocks[0].insts.as_slice(),
            [
                MInst::Load {
                    dst: VReg(0),
                    offset: 0,
                    ..
                },
                MInst::Store {
                    src: VReg(0),
                    offset: 16,
                    ..
                },
                MInst::Return
            ]
        ));
    }

    #[test]
    fn dead_code_elimination_removes_unused_phi_chains() {
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 1,
                },
                MInst::LoadImm {
                    dst: VReg(1),
                    value: 2,
                },
                MInst::Return,
            ],
            4,
        );
        func.blocks[0].phis.extend([
            PhiNode {
                dst: VReg(2),
                sources: vec![(BlockId(1), VReg(0)), (BlockId(2), VReg(1))],
            },
            PhiNode {
                dst: VReg(3),
                sources: vec![(BlockId(1), VReg(2)), (BlockId(2), VReg(2))],
            },
        ]);

        dead_code_eliminate(&mut func);

        assert!(func.blocks[0].phis.is_empty());
        assert!(matches!(func.blocks[0].insts.as_slice(), [MInst::Return]));
    }

    #[test]
    fn keeps_multi_use_cmp_select_condition() {
        let mut func = make_func(
            vec![
                MInst::CmpImm {
                    dst: VReg(1),
                    lhs: VReg(0),
                    imm: 0,
                    kind: CmpKind::Ne,
                },
                MInst::Select {
                    dst: VReg(4),
                    cond: VReg(1),
                    true_val: VReg(2),
                    false_val: VReg(3),
                },
                MInst::Branch {
                    cond: VReg(1),
                    true_bb: BlockId(1),
                    false_bb: BlockId(2),
                },
            ],
            5,
        );

        fuse_compare_selects(&mut func);

        assert!(matches!(func.blocks[0].insts[0], MInst::CmpImm { .. }));
        assert!(matches!(func.blocks[0].insts[1], MInst::Select { .. }));
    }

    #[test]
    fn post_regalloc_peephole_folds_adjacent_single_use_cmp() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S8,
                },
                MInst::LoadImm {
                    dst: VReg(1),
                    value: 0,
                },
                MInst::Cmp {
                    dst: VReg(2),
                    lhs: VReg(0),
                    rhs: VReg(1),
                    kind: CmpKind::Ne,
                },
                MInst::Return,
            ],
            3,
        );

        post_regalloc_peephole(&mut func);

        assert!(matches!(
            func.blocks[0].insts[1],
            MInst::CmpImm {
                lhs: VReg(0),
                imm: 0,
                kind: CmpKind::Ne,
                ..
            }
        ));
        assert_eq!(func.blocks[0].insts.len(), 3);
    }

    #[test]
    fn post_regalloc_peephole_folds_width_normalization_into_unsigned_load() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 1,
                    size: OpSize::S8,
                },
                MInst::AndImm32 {
                    dst: VReg(1),
                    src: VReg(0),
                    imm: 0xff,
                },
                MInst::Load {
                    dst: VReg(2),
                    base: BaseReg::SimState,
                    offset: 2,
                    size: OpSize::S16,
                },
                MInst::AndImm32 {
                    dst: VReg(3),
                    src: VReg(2),
                    imm: 0xffff,
                },
                MInst::Load {
                    dst: VReg(4),
                    base: BaseReg::SimState,
                    offset: 4,
                    size: OpSize::S32,
                },
                MInst::Mov32 {
                    dst: VReg(5),
                    src: VReg(4),
                },
                MInst::Load {
                    dst: VReg(6),
                    base: BaseReg::SimState,
                    offset: 8,
                    size: OpSize::S64,
                },
                MInst::Mov {
                    dst: VReg(7),
                    src: VReg(6),
                },
                MInst::Return,
            ],
            8,
        );

        post_regalloc_peephole(&mut func);

        let expected = [
            (VReg(1), 1, OpSize::S8),
            (VReg(3), 2, OpSize::S16),
            (VReg(5), 4, OpSize::S32),
            (VReg(7), 8, OpSize::S64),
        ];
        assert_eq!(func.blocks[0].insts.len(), expected.len() + 1);
        for (inst, (dst, offset, size)) in func.blocks[0].insts.iter().zip(expected) {
            assert!(matches!(
                inst,
                MInst::Load {
                    dst: actual_dst,
                    base: BaseReg::SimState,
                    offset: actual_offset,
                    size: actual_size,
                } if *actual_dst == dst && *actual_offset == offset && *actual_size == size
            ));
        }
    }

    #[test]
    fn late_state_load_cse_repairs_copy_folding_without_changing_assignment() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::Load {
                    dst: VReg(1),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::Load {
                    dst: VReg(2),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 8,
                    src: VReg(0),
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(1),
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 24,
                    src: VReg(2),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            3,
        );
        let mut assignment = AssignmentMap::default();
        assignment.set(VReg(0), PhysReg::R10);
        assignment.set(VReg(1), PhysReg::R9);
        assignment.set(VReg(2), PhysReg::R8);

        assert_eq!(post_regalloc_direct_load_cse(&mut func, &assignment), 2);
        post_regalloc_peephole(&mut func);
        post_regalloc_cleanup(&mut func);
        post_regalloc_direct_load_cse(&mut func, &assignment);

        assert_eq!(
            func.blocks[0]
                .insts
                .iter()
                .filter(|instruction| matches!(instruction, MInst::Load { .. }))
                .count(),
            1
        );
        assert_eq!(
            func.blocks[0]
                .insts
                .iter()
                .filter(|instruction| matches!(instruction, MInst::Mov { .. }))
                .count(),
            2
        );
        assert!(func.blocks[0].insts.iter().all(|instruction| {
            !matches!(instruction, MInst::Mov { src, .. } if *src != VReg(0))
        }));
        super::super::regalloc::verify_assignment(&func, &assignment).unwrap();
    }

    #[test]
    fn late_state_load_cse_prefers_value_already_in_destination_register() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 8,
                    src: VReg(0),
                    size: OpSize::S64,
                },
                MInst::Load {
                    dst: VReg(1),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(1),
                    size: OpSize::S64,
                },
                MInst::Load {
                    dst: VReg(2),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 24,
                    src: VReg(2),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            3,
        );
        let mut assignment = AssignmentMap::default();
        assignment.set(VReg(0), PhysReg::R9);
        assignment.set(VReg(1), PhysReg::R10);
        assignment.set(VReg(2), PhysReg::R10);

        assert_eq!(post_regalloc_direct_load_cse(&mut func, &assignment), 2);
        assert!(matches!(
            func.blocks[0].insts[2],
            MInst::Mov {
                dst: VReg(1),
                src: VReg(0)
            }
        ));
        assert!(matches!(
            func.blocks[0].insts[4],
            MInst::Mov {
                dst: VReg(2),
                src: VReg(1)
            }
        ));
        super::super::regalloc::verify_assignment(&func, &assignment).unwrap();
    }

    #[test]
    fn late_direct_load_cse_reuses_stack_home_until_overlapping_store() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::StackFrame,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: VReg(0),
                    size: OpSize::S64,
                },
                MInst::Load {
                    dst: VReg(1),
                    base: BaseReg::StackFrame,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::StackFrame,
                    offset: 0,
                    src: VReg(1),
                    size: OpSize::S64,
                },
                MInst::Load {
                    dst: VReg(2),
                    base: BaseReg::StackFrame,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            3,
        );
        let mut assignment = AssignmentMap::default();
        assignment.set(VReg(0), PhysReg::R9);
        assignment.set(VReg(1), PhysReg::R10);
        assignment.set(VReg(2), PhysReg::R8);

        assert_eq!(post_regalloc_direct_load_cse(&mut func, &assignment), 1);
        assert!(matches!(
            func.blocks[0].insts[2],
            MInst::Mov {
                dst: VReg(1),
                src: VReg(0)
            }
        ));
        assert!(matches!(
            func.blocks[0].insts[4],
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::StackFrame,
                offset: 0,
                size: OpSize::S64
            }
        ));
        super::super::regalloc::verify_assignment(&func, &assignment).unwrap();
    }

    #[test]
    fn post_regalloc_peephole_keeps_a_multi_use_loaded_value() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 1,
                    size: OpSize::S8,
                },
                MInst::AndImm32 {
                    dst: VReg(1),
                    src: VReg(0),
                    imm: 0xff,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 2,
                    src: VReg(0),
                    size: OpSize::S8,
                },
                MInst::Return,
            ],
            2,
        );

        post_regalloc_peephole(&mut func);

        assert!(matches!(
            func.blocks[0].insts.as_slice(),
            [
                MInst::Load { dst: VReg(0), .. },
                MInst::AndImm32 {
                    dst: VReg(1),
                    src: VReg(0),
                    ..
                },
                MInst::Store { src: VReg(0), .. },
                MInst::Return
            ]
        ));
    }

    #[test]
    fn post_regalloc_cleanup_removes_dead_remats_and_equal_select_predicates() {
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 0x100,
                },
                MInst::Load {
                    dst: VReg(1),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::Load {
                    dst: VReg(2),
                    base: BaseReg::SimState,
                    offset: 8,
                    size: OpSize::S64,
                },
                MInst::Select {
                    dst: VReg(3),
                    cond: VReg(2),
                    true_val: VReg(1),
                    false_val: VReg(1),
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(3),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            4,
        );

        post_regalloc_peephole(&mut func);
        post_regalloc_cleanup(&mut func);

        assert!(matches!(
            func.blocks[0].insts.as_slice(),
            [
                MInst::Load {
                    dst: VReg(1),
                    offset: 0,
                    ..
                },
                MInst::Mov {
                    dst: VReg(3),
                    src: VReg(1)
                },
                MInst::Store {
                    src: VReg(3),
                    offset: 16,
                    ..
                },
                MInst::Return
            ]
        ));
    }

    #[test]
    fn post_regalloc_peephole_folds_unsigned_load_copies_at_every_machine_width() {
        let mut instructions = Vec::new();
        for (index, size) in [OpSize::S8, OpSize::S16, OpSize::S32, OpSize::S64]
            .into_iter()
            .enumerate()
        {
            let loaded = VReg((index * 2) as u32);
            let destination = VReg((index * 2 + 1) as u32);
            instructions.push(MInst::Load {
                dst: loaded,
                base: BaseReg::SimState,
                offset: index as i32 * 8,
                size,
            });
            instructions.push(MInst::Mov {
                dst: destination,
                src: loaded,
            });
        }
        instructions.push(MInst::Return);
        let mut func = make_func(instructions, 8);

        post_regalloc_peephole(&mut func);

        assert_eq!(func.blocks[0].insts.len(), 5);
        for (index, (inst, size)) in func.blocks[0]
            .insts
            .iter()
            .zip([OpSize::S8, OpSize::S16, OpSize::S32, OpSize::S64])
            .enumerate()
        {
            assert!(matches!(
                inst,
                MInst::Load {
                    dst,
                    base: BaseReg::SimState,
                    offset,
                    size: actual_size,
                } if *dst == VReg((index * 2 + 1) as u32)
                    && *offset == index as i32 * 8
                    && *actual_size == size
            ));
        }
    }

    #[test]
    fn post_regalloc_peephole_keeps_multi_use_constant() {
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 1,
                },
                MInst::Add {
                    dst: VReg(1),
                    lhs: VReg(2),
                    rhs: VReg(0),
                },
                MInst::Or {
                    dst: VReg(3),
                    lhs: VReg(1),
                    rhs: VReg(0),
                },
                MInst::Return,
            ],
            4,
        );

        post_regalloc_peephole(&mut func);

        assert!(matches!(func.blocks[0].insts[0], MInst::LoadImm { .. }));
        assert_eq!(func.blocks[0].insts.len(), 4);
    }

    #[test]
    fn post_regalloc_peephole_folds_nearby_single_use_imm() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::LoadImm {
                    dst: VReg(1),
                    value: 7,
                },
                MInst::Store {
                    base: BaseReg::StackFrame,
                    offset: 0,
                    src: VReg(0),
                    size: OpSize::S64,
                },
                MInst::ShrImm {
                    dst: VReg(2),
                    src: VReg(0),
                    imm: 3,
                },
                MInst::And {
                    dst: VReg(3),
                    lhs: VReg(2),
                    rhs: VReg(1),
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 8,
                    src: VReg(3),
                    size: OpSize::S8,
                },
                MInst::Return,
            ],
            4,
        );

        post_regalloc_peephole(&mut func);

        assert!(
            !func.blocks[0]
                .insts
                .iter()
                .any(|inst| matches!(inst, MInst::LoadImm { dst: VReg(1), .. })),
            "{:#?}",
            func.blocks[0].insts
        );
        assert!(func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::AndImm {
                dst: VReg(3),
                src: VReg(2),
                imm: 7
            }
        )));
    }

    #[test]
    fn post_regalloc_peephole_folds_adjacent_alu_immediates() {
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 5,
                },
                MInst::Add {
                    dst: VReg(1),
                    lhs: VReg(0),
                    rhs: VReg(2),
                },
                MInst::LoadImm {
                    dst: VReg(3),
                    value: 0xffff_ffff,
                },
                MInst::And {
                    dst: VReg(4),
                    lhs: VReg(5),
                    rhs: VReg(3),
                },
                MInst::LoadImm {
                    dst: VReg(6),
                    value: 31,
                },
                MInst::Shr {
                    dst: VReg(7),
                    lhs: VReg(8),
                    rhs: VReg(6),
                },
                MInst::Return,
            ],
            9,
        );

        post_regalloc_peephole(&mut func);

        assert!(matches!(
            func.blocks[0].insts[0],
            MInst::AddImm {
                dst: VReg(1),
                src: VReg(2),
                imm: 5,
            }
        ));
        assert!(matches!(
            func.blocks[0].insts[1],
            MInst::AndImm {
                dst: VReg(4),
                src: VReg(5),
                imm: 0xffff_ffff,
            }
        ));
        assert!(matches!(
            func.blocks[0].insts[2],
            MInst::ShrImm {
                dst: VReg(7),
                src: VReg(8),
                imm: 31,
            }
        ));
        assert_eq!(func.blocks[0].insts.len(), 4);
    }

    #[test]
    fn post_regalloc_peephole_rejects_unsupported_immediates() {
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: i32::MAX as u64 + 1,
                },
                MInst::Or {
                    dst: VReg(1),
                    lhs: VReg(2),
                    rhs: VReg(0),
                },
                MInst::LoadImm {
                    dst: VReg(3),
                    value: 64,
                },
                MInst::Shl {
                    dst: VReg(4),
                    lhs: VReg(5),
                    rhs: VReg(3),
                },
                MInst::Return,
            ],
            6,
        );

        post_regalloc_peephole(&mut func);

        assert!(matches!(func.blocks[0].insts[0], MInst::LoadImm { .. }));
        assert!(matches!(func.blocks[0].insts[1], MInst::Or { .. }));
        assert!(matches!(func.blocks[0].insts[2], MInst::LoadImm { .. }));
        assert!(matches!(func.blocks[0].insts[3], MInst::Shl { .. }));
        assert_eq!(func.blocks[0].insts.len(), 5);
    }

    #[test]
    fn post_regalloc_peephole_folds_sign_extended_immediates() {
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: u64::MAX - 1,
                },
                MInst::And {
                    dst: VReg(1),
                    lhs: VReg(2),
                    rhs: VReg(0),
                },
                MInst::LoadImm {
                    dst: VReg(3),
                    value: u64::MAX,
                },
                MInst::Sub {
                    dst: VReg(4),
                    lhs: VReg(5),
                    rhs: VReg(3),
                },
                MInst::LoadImm {
                    dst: VReg(6),
                    value: u64::MAX,
                },
                MInst::Cmp {
                    dst: VReg(7),
                    lhs: VReg(8),
                    rhs: VReg(6),
                    kind: CmpKind::Eq,
                },
                MInst::Return,
            ],
            9,
        );

        post_regalloc_peephole(&mut func);

        assert!(matches!(
            func.blocks[0].insts[0],
            MInst::AndImm {
                dst: VReg(1),
                src: VReg(2),
                imm: 0xffff_ffff_ffff_fffe,
            }
        ));
        assert!(matches!(
            func.blocks[0].insts[1],
            MInst::SubImm {
                dst: VReg(4),
                src: VReg(5),
                imm: -1,
            }
        ));
        assert!(matches!(
            func.blocks[0].insts[2],
            MInst::CmpImm {
                dst: VReg(7),
                lhs: VReg(8),
                imm: -1,
                kind: CmpKind::Eq,
            }
        ));
        assert_eq!(func.blocks[0].insts.len(), 4);
    }

    #[test]
    fn lower_to_imm_forms_uses_sign_extended_immediates() {
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: u64::MAX,
                },
                MInst::Add {
                    dst: VReg(1),
                    lhs: VReg(2),
                    rhs: VReg(0),
                },
                MInst::LoadImm {
                    dst: VReg(3),
                    value: 0x8000_0000,
                },
                MInst::Sub {
                    dst: VReg(4),
                    lhs: VReg(5),
                    rhs: VReg(3),
                },
                MInst::Return,
            ],
            6,
        );

        lower_to_imm_forms(&mut func);

        assert!(matches!(
            func.blocks[0].insts[1],
            MInst::AddImm {
                dst: VReg(1),
                src: VReg(2),
                imm: -1,
            }
        ));
        assert!(matches!(func.blocks[0].insts[3], MInst::Sub { .. }));
    }

    #[test]
    fn lower_to_imm_forms_folds_multi_use_and_constants() {
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 7,
                },
                MInst::And {
                    dst: VReg(1),
                    lhs: VReg(2),
                    rhs: VReg(0),
                },
                MInst::And {
                    dst: VReg(3),
                    lhs: VReg(4),
                    rhs: VReg(0),
                },
                MInst::Return,
            ],
            5,
        );

        lower_to_imm_forms(&mut func);

        assert!(matches!(
            func.blocks[0].insts[1],
            MInst::AndImm {
                dst: VReg(1),
                src: VReg(2),
                imm: 7,
            }
        ));
        assert!(matches!(
            func.blocks[0].insts[2],
            MInst::AndImm {
                dst: VReg(3),
                src: VReg(4),
                imm: 7,
            }
        ));
    }

    #[test]
    fn lower_to_imm_forms_folds_word32_and_constant_low_word() {
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 0xfeed_face_3fff_ffff,
                },
                MInst::And32 {
                    dst: VReg(1),
                    lhs: VReg(2),
                    rhs: VReg(0),
                },
                MInst::Return,
            ],
            3,
        );

        lower_to_imm_forms(&mut func);

        assert!(matches!(
            func.blocks[0].insts[1],
            MInst::AndImm32 {
                dst: VReg(1),
                src: VReg(2),
                imm: 0x3fff_ffff,
            }
        ));
    }

    #[test]
    fn lower_to_imm_forms_folds_constant_memory_indices() {
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 24,
                },
                MInst::LoadIndexed {
                    dst: VReg(1),
                    base: BaseReg::SimState,
                    offset: 16,
                    index: VReg(0),
                    scale: 1,
                    size: OpSize::S64,
                    alias_range: None,
                },
                MInst::StoreIndexed {
                    base: BaseReg::SimState,
                    offset: 32,
                    index: VReg(0),
                    src: VReg(2),
                    size: OpSize::S64,
                    alias_range: None,
                },
                MInst::Return,
            ],
            3,
        );

        lower_to_imm_forms(&mut func);

        assert!(matches!(
            func.blocks[0].insts[1],
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 40,
                size: OpSize::S64,
            }
        ));
        assert!(matches!(
            func.blocks[0].insts[2],
            MInst::Store {
                base: BaseReg::SimState,
                offset: 56,
                src: VReg(2),
                size: OpSize::S64,
            }
        ));
    }

    #[test]
    fn folds_add_tree_of_bit_extracts_to_popcnt() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 8,
                    size: OpSize::S64,
                },
                MInst::ShrImm {
                    dst: VReg(1),
                    src: VReg(0),
                    imm: 0,
                },
                MInst::AndImm {
                    dst: VReg(2),
                    src: VReg(1),
                    imm: 1,
                },
                MInst::ShrImm {
                    dst: VReg(3),
                    src: VReg(0),
                    imm: 1,
                },
                MInst::AndImm {
                    dst: VReg(4),
                    src: VReg(3),
                    imm: 1,
                },
                MInst::ShrImm {
                    dst: VReg(5),
                    src: VReg(0),
                    imm: 2,
                },
                MInst::AndImm {
                    dst: VReg(6),
                    src: VReg(5),
                    imm: 1,
                },
                MInst::Add {
                    dst: VReg(7),
                    lhs: VReg(2),
                    rhs: VReg(4),
                },
                MInst::Add {
                    dst: VReg(8),
                    lhs: VReg(7),
                    rhs: VReg(6),
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: VReg(8),
                    size: OpSize::S8,
                },
                MInst::Return,
            ],
            9,
        );

        optimize(&mut func);

        let insts = &func.blocks[0].insts;
        assert!(
            insts.iter().any(|inst| matches!(
                inst,
                MInst::Popcnt {
                    dst: VReg(8),
                    src: _
                }
            )),
            "{insts:#?}"
        );
    }

    #[test]
    fn does_not_fold_add_tree_with_duplicate_bit() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 8,
                    size: OpSize::S64,
                },
                MInst::ShrImm {
                    dst: VReg(1),
                    src: VReg(0),
                    imm: 0,
                },
                MInst::AndImm {
                    dst: VReg(2),
                    src: VReg(1),
                    imm: 1,
                },
                MInst::ShrImm {
                    dst: VReg(3),
                    src: VReg(0),
                    imm: 0,
                },
                MInst::AndImm {
                    dst: VReg(4),
                    src: VReg(3),
                    imm: 1,
                },
                MInst::ShrImm {
                    dst: VReg(5),
                    src: VReg(0),
                    imm: 2,
                },
                MInst::AndImm {
                    dst: VReg(6),
                    src: VReg(5),
                    imm: 1,
                },
                MInst::Add {
                    dst: VReg(7),
                    lhs: VReg(2),
                    rhs: VReg(4),
                },
                MInst::Add {
                    dst: VReg(8),
                    lhs: VReg(7),
                    rhs: VReg(6),
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: VReg(8),
                    size: OpSize::S8,
                },
                MInst::Return,
            ],
            9,
        );

        optimize(&mut func);

        let insts = &func.blocks[0].insts;
        assert!(!insts.iter().any(|inst| matches!(
            inst,
            MInst::Popcnt {
                dst: VReg(8),
                src: _
            }
        )));
    }

    #[test]
    fn folds_chunk_deposit_chain_to_pdep() {
        if !crate::backend::native::features::X86Features::detect().bmi2() {
            return;
        }

        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 8,
                    size: OpSize::S64,
                },
                MInst::AndImm {
                    dst: VReg(1),
                    src: VReg(0),
                    imm: 0xf,
                },
                MInst::ShlImm {
                    dst: VReg(2),
                    src: VReg(1),
                    imm: 2,
                },
                MInst::ShrImm {
                    dst: VReg(3),
                    src: VReg(0),
                    imm: 4,
                },
                MInst::AndImm {
                    dst: VReg(4),
                    src: VReg(3),
                    imm: 0xf,
                },
                MInst::ShlImm {
                    dst: VReg(5),
                    src: VReg(4),
                    imm: 8,
                },
                MInst::Or {
                    dst: VReg(6),
                    lhs: VReg(2),
                    rhs: VReg(5),
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: VReg(6),
                    size: OpSize::S16,
                },
                MInst::Return,
            ],
            7,
        );

        optimize(&mut func);

        let insts = &func.blocks[0].insts;
        assert!(
            insts.iter().any(|inst| matches!(
                inst,
                MInst::Pdep {
                    dst: VReg(6),
                    src: VReg(0),
                    ..
                }
            )),
            "{insts:#?}"
        );
    }

    #[test]
    fn folds_chunk_extract_chain_to_pext() {
        if !crate::backend::native::features::X86Features::detect().bmi2() {
            return;
        }

        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 8,
                    size: OpSize::S64,
                },
                MInst::ShrImm {
                    dst: VReg(1),
                    src: VReg(0),
                    imm: 2,
                },
                MInst::AndImm {
                    dst: VReg(2),
                    src: VReg(1),
                    imm: 0xf,
                },
                MInst::ShrImm {
                    dst: VReg(3),
                    src: VReg(0),
                    imm: 8,
                },
                MInst::AndImm {
                    dst: VReg(4),
                    src: VReg(3),
                    imm: 0xf,
                },
                MInst::ShlImm {
                    dst: VReg(5),
                    src: VReg(4),
                    imm: 4,
                },
                MInst::Or {
                    dst: VReg(6),
                    lhs: VReg(2),
                    rhs: VReg(5),
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: VReg(6),
                    size: OpSize::S8,
                },
                MInst::Return,
            ],
            7,
        );

        optimize(&mut func);

        let insts = &func.blocks[0].insts;
        assert!(
            insts.iter().any(|inst| matches!(
                inst,
                MInst::Pext {
                    dst: VReg(6),
                    src: VReg(0),
                    ..
                }
            )),
            "{insts:#?}"
        );
    }

    #[test]
    fn folds_dynamic_bit_toggle_insert_to_xor() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 8,
                    size: OpSize::S64,
                },
                MInst::LoadImm {
                    dst: VReg(1),
                    value: 1,
                },
                MInst::Load {
                    dst: VReg(2),
                    base: BaseReg::SimState,
                    offset: 16,
                    size: OpSize::S8,
                },
                MInst::Shl {
                    dst: VReg(3),
                    lhs: VReg(1),
                    rhs: VReg(2),
                },
                MInst::BitNot {
                    dst: VReg(4),
                    src: VReg(3),
                },
                MInst::And {
                    dst: VReg(5),
                    lhs: VReg(0),
                    rhs: VReg(4),
                },
                MInst::Shr {
                    dst: VReg(6),
                    lhs: VReg(0),
                    rhs: VReg(2),
                },
                MInst::And {
                    dst: VReg(7),
                    lhs: VReg(6),
                    rhs: VReg(1),
                },
                MInst::Xor {
                    dst: VReg(8),
                    lhs: VReg(7),
                    rhs: VReg(1),
                },
                MInst::Shl {
                    dst: VReg(9),
                    lhs: VReg(8),
                    rhs: VReg(2),
                },
                MInst::Or {
                    dst: VReg(10),
                    lhs: VReg(5),
                    rhs: VReg(9),
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: VReg(10),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            11,
        );

        optimize(&mut func);

        let insts = &func.blocks[0].insts;
        assert!(
            insts.iter().any(|inst| matches!(
                inst,
                MInst::Xor {
                    dst: VReg(10),
                    lhs: VReg(0),
                    rhs: VReg(3),
                } | MInst::Xor {
                    dst: VReg(10),
                    lhs: VReg(3),
                    rhs: VReg(0),
                }
            )),
            "{insts:#?}"
        );
    }

    #[test]
    fn forwards_exact_store_to_load_in_block() {
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 0x55,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(0),
                    size: OpSize::S8,
                },
                MInst::Load {
                    dst: VReg(1),
                    base: BaseReg::SimState,
                    offset: 16,
                    size: OpSize::S8,
                },
                MInst::AddImm {
                    dst: VReg(2),
                    src: VReg(1),
                    imm: 1,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 24,
                    src: VReg(2),
                    size: OpSize::S8,
                },
                MInst::Return,
            ],
            3,
        );

        optimize(&mut func);

        let insts = &func.blocks[0].insts;
        assert!(
            insts.iter().any(|inst| matches!(
                inst,
                MInst::LoadImm {
                    dst: VReg(1),
                    value: 85,
                }
            )),
            "{insts:#?}"
        );
        assert!(
            insts.iter().any(|inst| matches!(
                inst,
                MInst::AddImm {
                    dst: VReg(2),
                    src: VReg(1),
                    imm: 1,
                }
            )),
            "{insts:#?}"
        );
        assert!(
            insts.iter().any(|inst| matches!(
                inst,
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 24,
                    src: VReg(2),
                    size: OpSize::S8,
                }
            )),
            "{insts:#?}"
        );
        assert!(!insts.iter().any(|inst| matches!(
            inst,
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S8,
            }
        )));
    }

    #[test]
    fn does_not_forward_across_overlapping_store() {
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 0x1122,
                },
                MInst::LoadImm {
                    dst: VReg(1),
                    value: 0x33,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(0),
                    size: OpSize::S16,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 17,
                    src: VReg(1),
                    size: OpSize::S8,
                },
                MInst::Load {
                    dst: VReg(2),
                    base: BaseReg::SimState,
                    offset: 16,
                    size: OpSize::S16,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 32,
                    src: VReg(2),
                    size: OpSize::S16,
                },
                MInst::Return,
            ],
            3,
        );

        optimize(&mut func);

        let insts = &func.blocks[0].insts;
        assert!(
            insts.iter().any(|inst| matches!(
                inst,
                MInst::Load {
                    dst: VReg(2),
                    base: BaseReg::SimState,
                    offset: 16,
                    size: OpSize::S16,
                }
            )),
            "{insts:#?}"
        );
        assert!(
            insts.iter().any(|inst| matches!(
                inst,
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 32,
                    src: VReg(2),
                    size: OpSize::S16,
                }
            )),
            "{insts:#?}"
        );
    }

    #[test]
    fn eliminates_redundant_same_slot_store() {
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 1,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(0),
                    size: OpSize::S8,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(0),
                    size: OpSize::S8,
                },
                MInst::Return,
            ],
            1,
        );

        optimize(&mut func);

        let store_count = func.blocks[0]
            .insts
            .iter()
            .filter(|inst| {
                matches!(
                    inst,
                    MInst::Store {
                        base: BaseReg::SimState,
                        offset: 16,
                        src: VReg(0),
                        size: OpSize::S8,
                    }
                )
            })
            .count();
        assert_eq!(store_count, 1, "{:#?}", func.blocks[0].insts);
    }

    #[test]
    fn eliminates_dead_store_overwritten_before_any_load() {
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 1,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(0),
                    size: OpSize::S8,
                },
                MInst::LoadImm {
                    dst: VReg(1),
                    value: 2,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(1),
                    size: OpSize::S8,
                },
                MInst::Return,
            ],
            2,
        );

        optimize(&mut func);

        assert!(
            !func.blocks[0].insts.iter().any(|inst| matches!(
                inst,
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(0),
                    size: OpSize::S8,
                }
            )),
            "{:#?}",
            func.blocks[0].insts
        );
        assert!(func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(1),
                size: OpSize::S8,
            }
        )));
    }

    #[test]
    fn keeps_store_before_unknown_memory_access() {
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 1,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(0),
                    size: OpSize::S8,
                },
                MInst::LoadImm {
                    dst: VReg(1),
                    value: 0,
                },
                MInst::LoadIndexed {
                    dst: VReg(2),
                    base: BaseReg::SimState,
                    offset: 0,
                    index: VReg(1),
                    scale: 1,
                    size: OpSize::S8,
                    alias_range: None,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 24,
                    src: VReg(2),
                    size: OpSize::S8,
                },
                MInst::LoadImm {
                    dst: VReg(3),
                    value: 2,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(3),
                    size: OpSize::S8,
                },
                MInst::Return,
            ],
            4,
        );

        optimize(&mut func);

        assert!(func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(0),
                size: OpSize::S8,
            }
        )));
        assert!(func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(3),
                size: OpSize::S8,
            }
        )));
    }

    #[test]
    fn bounded_disjoint_indexed_read_does_not_block_dead_store_elimination() {
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 1,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(0),
                    size: OpSize::S8,
                },
                MInst::LoadImm {
                    dst: VReg(1),
                    value: 0,
                },
                MInst::LoadIndexed {
                    dst: VReg(2),
                    base: BaseReg::SimState,
                    offset: 64,
                    index: VReg(1),
                    scale: 1,
                    size: OpSize::S8,
                    alias_range: MemoryAliasRange::new(64, 8),
                },
                MInst::LoadImm {
                    dst: VReg(3),
                    value: 2,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(3),
                    size: OpSize::S8,
                },
                MInst::Return,
            ],
            4,
        );

        eliminate_redundant_local_stores(&mut func);

        assert!(!func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(0),
                size: OpSize::S8,
            }
        )));
        assert!(func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(3),
                size: OpSize::S8,
            }
        )));
    }

    #[test]
    fn bounded_overlapping_indexed_read_keeps_preceding_store() {
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 1,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(0),
                    size: OpSize::S8,
                },
                MInst::LoadImm {
                    dst: VReg(1),
                    value: 0,
                },
                MInst::LoadIndexed {
                    dst: VReg(2),
                    base: BaseReg::SimState,
                    offset: 16,
                    index: VReg(1),
                    scale: 1,
                    size: OpSize::S8,
                    alias_range: MemoryAliasRange::new(16, 8),
                },
                MInst::LoadImm {
                    dst: VReg(3),
                    value: 2,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(3),
                    size: OpSize::S8,
                },
                MInst::Return,
            ],
            4,
        );

        eliminate_redundant_local_stores(&mut func);

        assert!(func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(0),
                size: OpSize::S8,
            }
        )));
        assert!(func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(3),
                size: OpSize::S8,
            }
        )));
    }

    #[test]
    fn indirect_read_does_not_block_direct_state_dead_store_elimination() {
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 1,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(0),
                    size: OpSize::S8,
                },
                MInst::LoadImm {
                    dst: VReg(1),
                    value: 0,
                },
                MInst::LoadPtr {
                    dst: VReg(2),
                    ptr: VReg(1),
                    offset: 0,
                    size: OpSize::S8,
                },
                MInst::LoadImm {
                    dst: VReg(3),
                    value: 2,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(3),
                    size: OpSize::S8,
                },
                MInst::Return,
            ],
            4,
        );

        eliminate_redundant_local_stores(&mut func);

        assert!(!func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(0),
                size: OpSize::S8,
            }
        )));
        assert!(func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(3),
                size: OpSize::S8,
            }
        )));
    }

    #[test]
    fn eliminates_redundant_or_of_same_select_term() {
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 0,
                },
                MInst::LoadImm {
                    dst: VReg(1),
                    value: 2,
                },
                MInst::Load {
                    dst: VReg(2),
                    base: BaseReg::SimState,
                    offset: 16,
                    size: OpSize::S8,
                },
                MInst::Cmp {
                    dst: VReg(3),
                    lhs: VReg(2),
                    rhs: VReg(0),
                    kind: CmpKind::Ne,
                },
                MInst::Select {
                    dst: VReg(4),
                    cond: VReg(3),
                    true_val: VReg(1),
                    false_val: VReg(0),
                },
                MInst::Or {
                    dst: VReg(5),
                    lhs: VReg(2),
                    rhs: VReg(4),
                },
                MInst::Mov {
                    dst: VReg(6),
                    src: VReg(3),
                },
                MInst::Select {
                    dst: VReg(7),
                    cond: VReg(6),
                    true_val: VReg(1),
                    false_val: VReg(0),
                },
                MInst::Or {
                    dst: VReg(8),
                    lhs: VReg(5),
                    rhs: VReg(7),
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 24,
                    src: VReg(8),
                    size: OpSize::S8,
                },
                MInst::Return,
            ],
            9,
        );

        optimize(&mut func);

        assert!(
            !func.blocks[0]
                .insts
                .iter()
                .any(|inst| matches!(inst, MInst::Or { dst: VReg(8), .. })),
            "{:#?}",
            func.blocks[0].insts
        );
        assert!(func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::Store {
                base: BaseReg::SimState,
                offset: 24,
                src: VReg(5),
                size: OpSize::S8,
            }
        )));
    }

    #[test]
    fn keeps_or_of_different_select_terms() {
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 0,
                },
                MInst::LoadImm {
                    dst: VReg(1),
                    value: 2,
                },
                MInst::Load {
                    dst: VReg(2),
                    base: BaseReg::SimState,
                    offset: 16,
                    size: OpSize::S8,
                },
                MInst::Load {
                    dst: VReg(3),
                    base: BaseReg::SimState,
                    offset: 17,
                    size: OpSize::S8,
                },
                MInst::Cmp {
                    dst: VReg(4),
                    lhs: VReg(2),
                    rhs: VReg(0),
                    kind: CmpKind::Ne,
                },
                MInst::Cmp {
                    dst: VReg(5),
                    lhs: VReg(3),
                    rhs: VReg(0),
                    kind: CmpKind::Ne,
                },
                MInst::Select {
                    dst: VReg(6),
                    cond: VReg(4),
                    true_val: VReg(1),
                    false_val: VReg(0),
                },
                MInst::Or {
                    dst: VReg(7),
                    lhs: VReg(2),
                    rhs: VReg(6),
                },
                MInst::Select {
                    dst: VReg(8),
                    cond: VReg(5),
                    true_val: VReg(1),
                    false_val: VReg(0),
                },
                MInst::Or {
                    dst: VReg(9),
                    lhs: VReg(7),
                    rhs: VReg(8),
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 24,
                    src: VReg(9),
                    size: OpSize::S8,
                },
                MInst::Return,
            ],
            10,
        );

        optimize(&mut func);

        assert!(
            func.blocks[0]
                .insts
                .iter()
                .any(|inst| matches!(inst, MInst::Or { dst: VReg(9), .. })),
            "{:#?}",
            func.blocks[0].insts
        );
    }

    #[test]
    fn forwards_partial_load_from_recent_store() {
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 0x3412,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(0),
                    size: OpSize::S16,
                },
                MInst::Load {
                    dst: VReg(1),
                    base: BaseReg::SimState,
                    offset: 17,
                    size: OpSize::S8,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 24,
                    src: VReg(1),
                    size: OpSize::S8,
                },
                MInst::Return,
            ],
            2,
        );

        optimize(&mut func);

        let insts = &func.blocks[0].insts;
        assert!(
            !insts.iter().any(|inst| matches!(
                inst,
                MInst::Load {
                    dst: VReg(1),
                    base: BaseReg::SimState,
                    offset: 17,
                    size: OpSize::S8,
                }
            )),
            "{insts:#?}"
        );
        assert!(
            insts.iter().any(|inst| matches!(
                inst,
                MInst::LoadImm {
                    dst: _,
                    value: 0x34,
                }
            )),
            "{insts:#?}"
        );
    }

    #[test]
    fn partial_load_uses_smallest_covering_value() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 6,
                    size: OpSize::S16,
                },
                MInst::Load {
                    dst: VReg(1),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::Load {
                    dst: VReg(2),
                    base: BaseReg::SimState,
                    offset: 7,
                    size: OpSize::S8,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(2),
                    size: OpSize::S8,
                },
                MInst::Return,
            ],
            3,
        );

        forward_local_store_loads(&mut func);

        let insts = &func.blocks[0].insts;
        assert!(insts.iter().any(|inst| matches!(
            inst,
            MInst::ShrImm {
                src: VReg(0),
                imm: 8,
                ..
            }
        )));
        assert!(!insts.iter().any(|inst| matches!(
            inst,
            MInst::ShrImm {
                src: VReg(1),
                imm: 56,
                ..
            }
        )));
    }

    #[test]
    fn sink_loads_keeps_each_definition_before_its_use() {
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 10,
                },
                MInst::LoadImm {
                    dst: VReg(1),
                    value: 20,
                },
                MInst::LoadImm {
                    dst: VReg(2),
                    value: 30,
                },
                MInst::LoadImm {
                    dst: VReg(3),
                    value: 40,
                },
                MInst::LoadImm {
                    dst: VReg(4),
                    value: 50,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: VReg(0),
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 8,
                    src: VReg(1),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            5,
        );

        sink_loads(&mut func);

        assert_eq!(func.verify_result(), Ok(()));
    }

    #[test]
    fn memcopy_destination_invalidates_local_load_forwarding() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 16,
                    size: OpSize::S64,
                },
                MInst::MemCopy {
                    src_offset: 64,
                    dst_offset: 16,
                    byte_len: 8,
                },
                MInst::Load {
                    dst: VReg(1),
                    base: BaseReg::SimState,
                    offset: 16,
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 96,
                    src: VReg(1),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            2,
        );

        forward_local_store_loads(&mut func);

        assert!(matches!(
            func.blocks[0].insts[2],
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            }
        ));
    }

    #[test]
    fn memcopy_preserves_nonoverlapping_local_load_forwarding() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 80,
                    size: OpSize::S64,
                },
                MInst::MemCopy {
                    src_offset: 64,
                    dst_offset: 16,
                    byte_len: 8,
                },
                MInst::Load {
                    dst: VReg(1),
                    base: BaseReg::SimState,
                    offset: 80,
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 96,
                    src: VReg(1),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            2,
        );

        forward_local_store_loads(&mut func);

        assert!(matches!(
            func.blocks[0].insts[2],
            MInst::Mov {
                dst: VReg(1),
                src: VReg(0),
            }
        ));
    }

    #[test]
    fn memcopy_destination_invalidates_global_load_gvn() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 16,
                    size: OpSize::S64,
                },
                MInst::MemCopy {
                    src_offset: 64,
                    dst_offset: 16,
                    byte_len: 8,
                },
                MInst::Load {
                    dst: VReg(1),
                    base: BaseReg::SimState,
                    offset: 16,
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 96,
                    src: VReg(1),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            2,
        );

        global_gvn(&mut func);

        assert!(matches!(
            func.blocks[0].insts[2],
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            }
        ));
    }

    #[test]
    fn global_gvn_numbers_values_instead_of_raw_vregs() {
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 7,
                },
                MInst::LoadImm {
                    dst: VReg(1),
                    value: 7,
                },
                MInst::Add {
                    dst: VReg(2),
                    lhs: VReg(0),
                    rhs: VReg(0),
                },
                // v1 and v0 have the same value number.  This expression must
                // therefore match v2 in this GVN invocation, without relying
                // on a later copy-propagation pass.
                MInst::Add {
                    dst: VReg(3),
                    lhs: VReg(1),
                    rhs: VReg(0),
                },
                // Keep the first leader naturally live at the repeated
                // expression, so reusing it does not lengthen its lifetime.
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 8,
                    src: VReg(2),
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: VReg(3),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            4,
        );

        global_gvn(&mut func);

        assert_eq!(
            func.blocks[0].insts[1],
            MInst::Mov {
                dst: VReg(1),
                src: VReg(0),
            }
        );
        assert_eq!(
            func.blocks[0].insts[3],
            MInst::Mov {
                dst: VReg(3),
                src: VReg(2),
            }
        );
    }

    #[test]
    fn global_gvn_recomputes_a_dead_same_block_leader() {
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 3,
                },
                MInst::LoadImm {
                    dst: VReg(1),
                    value: 5,
                },
                MInst::Add {
                    dst: VReg(2),
                    lhs: VReg(0),
                    rhs: VReg(1),
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: VReg(0),
                    size: OpSize::S64,
                },
                MInst::Add {
                    dst: VReg(3),
                    lhs: VReg(0),
                    rhs: VReg(1),
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 8,
                    src: VReg(3),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            4,
        );

        global_gvn(&mut func);

        assert!(matches!(
            func.blocks[0].insts[4],
            MInst::Add { dst: VReg(3), .. }
        ));
    }

    #[test]
    fn global_gvn_reuses_a_dead_same_block_rematerializable_leader() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::ShrImm {
                    dst: VReg(1),
                    src: VReg(0),
                    imm: 3,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(1),
                    size: OpSize::S64,
                },
                MInst::ShrImm {
                    dst: VReg(2),
                    src: VReg(0),
                    imm: 3,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 24,
                    src: VReg(2),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            3,
        );

        global_gvn(&mut func);

        assert_eq!(
            func.blocks[0].insts[3],
            MInst::Mov {
                dst: VReg(2),
                src: VReg(1),
            }
        );
    }

    #[test]
    fn global_gvn_reuses_a_dead_same_block_versioned_load_leader() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(0),
                    size: OpSize::S64,
                },
                MInst::Load {
                    dst: VReg(1),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 24,
                    src: VReg(1),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            2,
        );

        global_gvn(&mut func);

        assert_eq!(
            func.blocks[0].insts[2],
            MInst::Mov {
                dst: VReg(1),
                src: VReg(0),
            }
        );
    }

    #[test]
    fn optimize_shares_repeated_immediate_index_calculation() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::LoadImm {
                    dst: VReg(1),
                    value: 3,
                },
                MInst::LoadImm {
                    dst: VReg(2),
                    value: 7,
                },
                MInst::Shr {
                    dst: VReg(3),
                    lhs: VReg(0),
                    rhs: VReg(1),
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(3),
                    size: OpSize::S64,
                },
                MInst::And32 {
                    dst: VReg(4),
                    lhs: VReg(0),
                    rhs: VReg(2),
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 24,
                    src: VReg(4),
                    size: OpSize::S64,
                },
                MInst::Shr {
                    dst: VReg(5),
                    lhs: VReg(0),
                    rhs: VReg(1),
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 32,
                    src: VReg(5),
                    size: OpSize::S64,
                },
                MInst::And32 {
                    dst: VReg(6),
                    lhs: VReg(0),
                    rhs: VReg(2),
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 40,
                    src: VReg(6),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            41,
        );

        optimize(&mut func);

        assert_eq!(
            func.blocks[0]
                .insts
                .iter()
                .filter(|inst| matches!(inst, MInst::ShrImm { imm: 3, .. }))
                .count(),
            1
        );
        let stored = func.blocks[0]
            .insts
            .iter()
            .filter_map(|inst| match inst {
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16 | 32,
                    src,
                    size: OpSize::S64,
                } => Some(*src),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0], stored[1]);

        assert_eq!(
            func.blocks[0]
                .insts
                .iter()
                .filter(|inst| matches!(inst, MInst::AndImm32 { imm: 7, .. }))
                .count(),
            1
        );
        let stored = func.blocks[0]
            .insts
            .iter()
            .filter_map(|inst| match inst {
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 24 | 40,
                    src,
                    size: OpSize::S64,
                } => Some(*src),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0], stored[1]);
    }

    #[test]
    fn global_gvn_does_not_extend_a_leader_only_for_cross_block_cse() {
        let mut vregs = VRegAllocator::new();
        for _ in 0..4 {
            vregs.alloc();
        }
        let spill_descs = (0..4).map(|_| SpillDesc::transient()).collect();
        let mut func = MFunction::new(vregs, spill_descs);

        let mut entry = MBlock::new(BlockId(0));
        entry.insts = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 3,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 5,
            },
            MInst::Add {
                dst: VReg(2),
                lhs: VReg(0),
                rhs: VReg(1),
            },
            MInst::Jump { target: BlockId(1) },
        ];
        func.push_block(entry);

        let mut successor = MBlock::new(BlockId(1));
        successor.insts = vec![
            MInst::Add {
                dst: VReg(3),
                lhs: VReg(0),
                rhs: VReg(1),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: VReg(3),
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        func.push_block(successor);

        global_gvn(&mut func);

        assert!(matches!(
            func.blocks[1].insts[0],
            MInst::Add { dst: VReg(3), .. }
        ));
    }

    #[test]
    fn global_gvn_does_not_extend_state_load_leader_across_blocks() {
        let mut vregs = VRegAllocator::new();
        for _ in 0..2 {
            vregs.alloc();
        }
        let spill_descs = (0..2).map(|_| SpillDesc::transient()).collect();
        let mut func = MFunction::new(vregs, spill_descs);

        let mut entry = MBlock::new(BlockId(0));
        entry.insts = vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            MInst::Jump { target: BlockId(1) },
        ];
        func.push_block(entry);

        let mut successor = MBlock::new(BlockId(1));
        successor.insts = vec![
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 80,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        func.push_block(successor);

        global_gvn(&mut func);

        assert!(matches!(
            func.blocks[1].insts[0],
            MInst::Load { dst: VReg(1), .. }
        ));
    }

    #[test]
    fn global_gvn_reuses_state_load_leader_that_is_already_live() {
        let mut vregs = VRegAllocator::new();
        for _ in 0..2 {
            vregs.alloc();
        }
        let spill_descs = (0..2).map(|_| SpillDesc::transient()).collect();
        let mut func = MFunction::new(vregs, spill_descs);

        let mut entry = MBlock::new(BlockId(0));
        entry.insts = vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            MInst::Jump { target: BlockId(1) },
        ];
        func.push_block(entry);

        let mut successor = MBlock::new(BlockId(1));
        successor.insts = vec![
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 80,
                src: VReg(0),
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 88,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        func.push_block(successor);

        global_gvn(&mut func);

        assert_eq!(
            func.blocks[1].insts[0],
            MInst::Mov {
                dst: VReg(1),
                src: VReg(0),
            }
        );
    }

    #[test]
    fn global_gvn_reuses_a_cross_block_leader_that_is_already_live() {
        let mut vregs = VRegAllocator::new();
        for _ in 0..4 {
            vregs.alloc();
        }
        let spill_descs = (0..4).map(|_| SpillDesc::transient()).collect();
        let mut func = MFunction::new(vregs, spill_descs);

        let mut entry = MBlock::new(BlockId(0));
        entry.insts = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 3,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 5,
            },
            MInst::Add {
                dst: VReg(2),
                lhs: VReg(0),
                rhs: VReg(1),
            },
            MInst::Jump { target: BlockId(1) },
        ];
        func.push_block(entry);

        let mut successor = MBlock::new(BlockId(1));
        successor.insts = vec![
            MInst::Add {
                dst: VReg(3),
                lhs: VReg(0),
                rhs: VReg(1),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: VReg(2),
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 8,
                src: VReg(3),
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        func.push_block(successor);

        global_gvn(&mut func);

        assert_eq!(
            func.blocks[1].insts[0],
            MInst::Mov {
                dst: VReg(3),
                src: VReg(2),
            }
        );
    }

    #[test]
    fn global_gvn_does_not_reuse_a_sibling_expression() {
        let mut vregs = VRegAllocator::new();
        for _ in 0..3 {
            vregs.alloc();
        }
        let spill_descs = (0..3).map(|_| SpillDesc::transient()).collect();
        let mut func = MFunction::new(vregs, spill_descs);

        let mut entry = MBlock::new(BlockId(0));
        entry.insts = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 1,
            },
            MInst::Branch {
                cond: VReg(0),
                true_bb: BlockId(1),
                false_bb: BlockId(2),
            },
        ];
        func.push_block(entry);

        let mut left = MBlock::new(BlockId(1));
        left.insts = vec![
            MInst::Add {
                dst: VReg(1),
                lhs: VReg(0),
                rhs: VReg(0),
            },
            MInst::Return,
        ];
        func.push_block(left);

        let mut right = MBlock::new(BlockId(2));
        right.insts = vec![
            MInst::Add {
                dst: VReg(2),
                lhs: VReg(0),
                rhs: VReg(0),
            },
            MInst::Return,
        ];
        func.push_block(right);

        global_gvn(&mut func);

        assert!(matches!(func.blocks[1].insts[0], MInst::Add { .. }));
        assert!(matches!(func.blocks[2].insts[0], MInst::Add { .. }));
    }

    #[test]
    fn global_gvn_does_not_reuse_bsr_with_unspecified_zero_result() {
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 0,
                },
                MInst::Bsr {
                    dst: VReg(1),
                    src: VReg(0),
                },
                MInst::Bsr {
                    dst: VReg(2),
                    src: VReg(0),
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: VReg(2),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            3,
        );

        global_gvn(&mut func);

        assert!(matches!(func.blocks[0].insts[1], MInst::Bsr { .. }));
        assert!(matches!(func.blocks[0].insts[2], MInst::Bsr { .. }));
    }

    #[test]
    fn global_gvn_invalidates_loads_at_exact_byte_boundaries() {
        let mut overlapping = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 16,
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 23,
                    src: VReg(0),
                    size: OpSize::S8,
                },
                MInst::Load {
                    dst: VReg(1),
                    base: BaseReg::SimState,
                    offset: 16,
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 32,
                    src: VReg(0),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            2,
        );
        global_gvn(&mut overlapping);
        assert!(matches!(overlapping.blocks[0].insts[2], MInst::Load { .. }));

        let mut adjacent = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 16,
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 24,
                    src: VReg(0),
                    size: OpSize::S8,
                },
                MInst::Load {
                    dst: VReg(1),
                    base: BaseReg::SimState,
                    offset: 16,
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 32,
                    src: VReg(0),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            2,
        );
        global_gvn(&mut adjacent);
        assert_eq!(
            adjacent.blocks[0].insts[2],
            MInst::Mov {
                dst: VReg(1),
                src: VReg(0),
            }
        );
    }

    #[test]
    fn memcopy_preserves_nonoverlapping_global_load_gvn() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 80,
                    size: OpSize::S64,
                },
                MInst::MemCopy {
                    src_offset: 64,
                    dst_offset: 16,
                    byte_len: 8,
                },
                MInst::Load {
                    dst: VReg(1),
                    base: BaseReg::SimState,
                    offset: 80,
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 96,
                    src: VReg(1),
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 104,
                    src: VReg(0),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            2,
        );

        global_gvn(&mut func);

        assert!(matches!(
            func.blocks[0].insts[2],
            MInst::Mov {
                dst: VReg(1),
                src: VReg(0),
            }
        ));
    }

    #[test]
    fn global_gvn_restores_load_scope_for_sibling_subtrees() {
        let mut vregs = VRegAllocator::new();
        for _ in 0..4 {
            vregs.alloc();
        }
        let spill_descs = (0..4).map(|_| SpillDesc::transient()).collect();
        let mut func = MFunction::new(vregs, spill_descs);

        let mut entry = MBlock::new(BlockId(0));
        entry.insts = vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 1,
            },
            MInst::Branch {
                cond: VReg(1),
                true_bb: BlockId(1),
                false_bb: BlockId(2),
            },
        ];
        func.push_block(entry);

        let mut writing_child = MBlock::new(BlockId(1));
        writing_child.insts = vec![
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 80,
                src: VReg(2),
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        func.push_block(writing_child);

        let mut sibling = MBlock::new(BlockId(2));
        sibling.insts = vec![
            MInst::Load {
                dst: VReg(3),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 88,
                src: VReg(3),
                size: OpSize::S64,
            },
            // Keep the entry load independently live in this child. This
            // isolates the scoped-memory assertion from GVN's rule against
            // extending a leader solely for cross-block CSE.
            MInst::Store {
                base: BaseReg::SimState,
                offset: 96,
                src: VReg(0),
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        func.push_block(sibling);

        global_gvn(&mut func);

        assert!(matches!(func.blocks[1].insts[1], MInst::Load { .. }));
        assert!(matches!(
            func.blocks[2].insts[0],
            MInst::Mov {
                dst: VReg(3),
                src: VReg(0),
            }
        ));
    }

    #[test]
    fn global_gvn_does_not_reuse_load_across_a_joining_write() {
        let mut vregs = VRegAllocator::new();
        for _ in 0..4 {
            vregs.alloc();
        }
        let spill_descs = (0..4).map(|_| SpillDesc::transient()).collect();
        let mut func = MFunction::new(vregs, spill_descs);

        let mut entry = MBlock::new(BlockId(0));
        entry.insts = vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 1,
            },
            MInst::LoadImm {
                dst: VReg(2),
                value: 9,
            },
            MInst::Branch {
                cond: VReg(1),
                true_bb: BlockId(1),
                false_bb: BlockId(2),
            },
        ];
        func.push_block(entry);

        let mut writing_arm = MBlock::new(BlockId(1));
        writing_arm.insts = vec![
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(2),
                size: OpSize::S64,
            },
            MInst::Jump { target: BlockId(3) },
        ];
        func.push_block(writing_arm);

        let mut unchanged_arm = MBlock::new(BlockId(2));
        unchanged_arm.insts = vec![MInst::Jump { target: BlockId(3) }];
        func.push_block(unchanged_arm);

        let mut join = MBlock::new(BlockId(3));
        join.insts = vec![
            MInst::Load {
                dst: VReg(3),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            // Keep the entry value live at the repeated load. The memory
            // version, not register liveness, must reject this replacement.
            MInst::Store {
                base: BaseReg::SimState,
                offset: 80,
                src: VReg(0),
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 88,
                src: VReg(3),
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        func.push_block(join);

        global_gvn(&mut func);

        assert!(matches!(func.blocks[3].insts[0], MInst::Load { .. }));
    }

    #[test]
    fn global_gvn_does_not_reuse_load_across_a_loop_carried_write() {
        let mut vregs = VRegAllocator::new();
        for _ in 0..3 {
            vregs.alloc();
        }
        let spill_descs = (0..3).map(|_| SpillDesc::transient()).collect();
        let mut func = MFunction::new(vregs, spill_descs);

        let mut entry = MBlock::new(BlockId(0));
        entry.insts = vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 1,
            },
            MInst::Jump { target: BlockId(1) },
        ];
        func.push_block(entry);

        let mut header = MBlock::new(BlockId(1));
        header.insts = vec![MInst::Branch {
            cond: VReg(1),
            true_bb: BlockId(2),
            false_bb: BlockId(3),
        }];
        func.push_block(header);

        let mut body = MBlock::new(BlockId(2));
        body.insts = vec![
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Jump { target: BlockId(1) },
        ];
        func.push_block(body);

        let mut exit = MBlock::new(BlockId(3));
        exit.insts = vec![
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 80,
                src: VReg(0),
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 88,
                src: VReg(2),
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        func.push_block(exit);

        global_gvn(&mut func);

        assert!(matches!(func.blocks[3].insts[0], MInst::Load { .. }));
    }

    #[test]
    fn global_gvn_sparse_mark_invalidates_only_its_metadata_ranges() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 16,
                    size: OpSize::S64,
                },
                MInst::Load {
                    dst: VReg(1),
                    base: BaseReg::SimState,
                    offset: 200,
                    size: OpSize::S64,
                },
                MInst::SparseMarkActive {
                    active_index: 3,
                    active_bits_offset: 200,
                    active_capacity: 16,
                },
                MInst::Load {
                    dst: VReg(2),
                    base: BaseReg::SimState,
                    offset: 16,
                    size: OpSize::S64,
                },
                MInst::Load {
                    dst: VReg(3),
                    base: BaseReg::SimState,
                    offset: 200,
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 400,
                    src: VReg(0),
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 408,
                    src: VReg(1),
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 416,
                    src: VReg(2),
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 424,
                    src: VReg(3),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            4,
        );

        global_gvn(&mut func);

        assert_eq!(
            func.blocks[0].insts[3],
            MInst::Mov {
                dst: VReg(2),
                src: VReg(0),
            }
        );
        assert!(matches!(func.blocks[0].insts[4], MInst::Load { .. }));
    }

    #[test]
    fn global_gvn_bounded_indexed_store_invalidates_only_its_alias_range() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 16,
                    size: OpSize::S64,
                },
                MInst::Load {
                    dst: VReg(1),
                    base: BaseReg::SimState,
                    offset: 128,
                    size: OpSize::S64,
                },
                MInst::LoadImm {
                    dst: VReg(2),
                    value: 0,
                },
                MInst::LoadImm {
                    dst: VReg(3),
                    value: 1,
                },
                MInst::StoreIndexed {
                    base: BaseReg::SimState,
                    offset: 16,
                    index: VReg(2),
                    src: VReg(3),
                    size: OpSize::S8,
                    alias_range: MemoryAliasRange::new(16, 64),
                },
                MInst::Load {
                    dst: VReg(4),
                    base: BaseReg::SimState,
                    offset: 16,
                    size: OpSize::S64,
                },
                MInst::Load {
                    dst: VReg(5),
                    base: BaseReg::SimState,
                    offset: 128,
                    size: OpSize::S64,
                },
                // Keep the original nonoverlapping leader live independently
                // of the candidate CSE. The test is about the bounded alias
                // envelope, not permission to lengthen a state-load range.
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 400,
                    src: VReg(1),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            6,
        );

        global_gvn(&mut func);

        assert!(matches!(func.blocks[0].insts[5], MInst::Load { .. }));
        assert_eq!(
            func.blocks[0].insts[6],
            MInst::Mov {
                dst: VReg(5),
                src: VReg(1),
            }
        );
    }

    #[test]
    fn memcopy_source_read_keeps_preceding_store_live() {
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 1,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 64,
                    src: VReg(0),
                    size: OpSize::S64,
                },
                MInst::MemCopy {
                    src_offset: 64,
                    dst_offset: 16,
                    byte_len: 8,
                },
                MInst::LoadImm {
                    dst: VReg(1),
                    value: 2,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 64,
                    src: VReg(1),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            2,
        );

        eliminate_redundant_local_stores(&mut func);

        assert!(func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::Store {
                base: BaseReg::SimState,
                offset: 64,
                src: VReg(0),
                size: OpSize::S64,
            }
        )));
    }

    #[test]
    fn memcopy_preserves_nonoverlapping_dead_store_elimination() {
        let mut func = make_func(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 1,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 80,
                    src: VReg(0),
                    size: OpSize::S64,
                },
                MInst::MemCopy {
                    src_offset: 64,
                    dst_offset: 16,
                    byte_len: 8,
                },
                MInst::LoadImm {
                    dst: VReg(1),
                    value: 2,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 80,
                    src: VReg(1),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            2,
        );

        eliminate_redundant_local_stores(&mut func);

        assert!(!func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::Store {
                base: BaseReg::SimState,
                offset: 80,
                src: VReg(0),
                size: OpSize::S64,
            }
        )));
    }

    #[test]
    fn folds_exact_narrow_load_and_store_into_direct_memory_and() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 37,
                    size: OpSize::S8,
                },
                MInst::AndImm {
                    dst: VReg(1),
                    src: VReg(0),
                    imm: 0xffff_ffff_ffff_fffcu64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 37,
                    src: VReg(1),
                    size: OpSize::S8,
                },
                MInst::Return,
            ],
            2,
        );

        algebraic_simplify(&mut func);
        dead_code_eliminate(&mut func);
        assert_eq!(fold_direct_immediate_stores(&mut func), 1);
        assert_eq!(
            func.blocks[0].insts,
            vec![
                MInst::AndStoreImm {
                    base: BaseReg::SimState,
                    offset: 37,
                    size: OpSize::S8,
                    imm: 0xfc,
                },
                MInst::Return,
            ]
        );
        assert_eq!(func.verify_result(), Ok(()));
    }

    #[test]
    fn folds_encodable_qword_load_and_store_into_direct_memory_and() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 40,
                    size: OpSize::S64,
                },
                MInst::AndImm32 {
                    dst: VReg(1),
                    src: VReg(0),
                    imm: 0x3f,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 40,
                    src: VReg(1),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            2,
        );

        assert_eq!(fold_direct_immediate_stores(&mut func), 1);
        assert_eq!(
            func.blocks[0].insts,
            vec![
                MInst::AndStoreImm {
                    base: BaseReg::SimState,
                    offset: 40,
                    size: OpSize::S64,
                    imm: 0x3f,
                },
                MInst::Return,
            ]
        );
        assert_eq!(func.verify_result(), Ok(()));
    }

    #[test]
    fn qword_direct_memory_and_preserves_word32_zero_extension() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 40,
                    size: OpSize::S64,
                },
                MInst::AndImm32 {
                    dst: VReg(1),
                    src: VReg(0),
                    imm: 0xfc00_000f,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 40,
                    src: VReg(1),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
            2,
        );

        assert_eq!(fold_direct_immediate_stores(&mut func), 0);
    }

    #[test]
    fn folds_clear_then_set_of_same_narrow_bits_into_direct_memory_or() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 37,
                    size: OpSize::S8,
                },
                MInst::AndImm {
                    dst: VReg(1),
                    src: VReg(0),
                    imm: !0x40,
                },
                MInst::OrImm {
                    dst: VReg(2),
                    src: VReg(1),
                    imm: 0x40,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 37,
                    src: VReg(2),
                    size: OpSize::S8,
                },
                MInst::Return,
            ],
            3,
        );

        algebraic_simplify(&mut func);
        dead_code_eliminate(&mut func);
        assert_eq!(fold_direct_immediate_stores(&mut func), 1);
        assert_eq!(
            func.blocks[0].insts,
            vec![
                MInst::OrStoreImm {
                    base: BaseReg::SimState,
                    offset: 37,
                    size: OpSize::S8,
                    imm: 0x40,
                },
                MInst::Return,
            ]
        );
        assert_eq!(func.verify_result(), Ok(()));
    }

    #[test]
    fn keeps_clear_then_set_when_the_or_does_not_restore_every_cleared_bit() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 37,
                    size: OpSize::S8,
                },
                MInst::AndImm {
                    dst: VReg(1),
                    src: VReg(0),
                    imm: !0x60,
                },
                MInst::OrImm {
                    dst: VReg(2),
                    src: VReg(1),
                    imm: 0x40,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 37,
                    src: VReg(2),
                    size: OpSize::S8,
                },
                MInst::Return,
            ],
            3,
        );

        algebraic_simplify(&mut func);
        dead_code_eliminate(&mut func);
        assert_eq!(fold_direct_immediate_stores(&mut func), 0);
    }

    #[test]
    fn direct_memory_and_requires_exclusive_ssa_temporaries() {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 37,
                    size: OpSize::S8,
                },
                MInst::AndImm {
                    dst: VReg(1),
                    src: VReg(0),
                    imm: 0xfc,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 37,
                    src: VReg(1),
                    size: OpSize::S8,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 38,
                    src: VReg(0),
                    size: OpSize::S8,
                },
                MInst::Return,
            ],
            2,
        );

        assert_eq!(fold_direct_immediate_stores(&mut func), 0);
    }

    #[test]
    fn post_regalloc_cleanup_threads_empty_edge_blocks() {
        let mut func = make_func(Vec::new(), 1);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: VReg(0),
            value: 1,
        });
        entry.push(MInst::BranchPred {
            predicate: BranchPredicate::CompareImm {
                lhs: VReg(0),
                imm: 0,
                kind: CmpKind::Ne,
            },
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });

        let mut true_edge = MBlock::new(BlockId(1));
        true_edge.push(MInst::Jump { target: BlockId(3) });

        let mut false_edge = MBlock::new(BlockId(2));
        false_edge.push(MInst::Jump { target: BlockId(4) });

        let mut true_target = MBlock::new(BlockId(3));
        true_target.push(MInst::Return);

        let mut false_target = MBlock::new(BlockId(4));
        false_target.push(MInst::Return);

        func.blocks = vec![entry, true_edge, false_edge, true_target, false_target];

        post_regalloc_cleanup(&mut func);

        assert_eq!(func.verify_result(), Ok(()));
        assert!(matches!(
            func.blocks[0].insts.last(),
            Some(MInst::BranchPred {
                true_bb: BlockId(3),
                false_bb: BlockId(4),
                ..
            })
        ));
        assert!(!func.blocks.iter().any(|block| block.id == BlockId(1)));
        assert!(!func.blocks.iter().any(|block| block.id == BlockId(2)));
    }

    #[test]
    fn simplify_cfg_does_not_collapse_distinct_phi_edges() {
        let mut func = make_func(Vec::new(), 3);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: VReg(0),
            value: 1,
        });
        entry.push(MInst::LoadImm {
            dst: VReg(1),
            value: 10,
        });
        entry.push(MInst::LoadImm {
            dst: VReg(2),
            value: 20,
        });
        entry.push(MInst::Branch {
            cond: VReg(0),
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::Jump { target: BlockId(3) });
        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::Jump { target: BlockId(3) });
        let mut merge = MBlock::new(BlockId(3));
        merge.phis.push(PhiNode {
            dst: VReg(3),
            sources: vec![(BlockId(1), VReg(1)), (BlockId(2), VReg(2))],
        });
        merge.push(MInst::Return);
        func.vregs.alloc();
        func.spill_descs.push(SpillDesc::transient());
        func.blocks = vec![entry, left, right, merge];

        simplify_cfg(&mut func);

        assert_eq!(func.verify_result(), Ok(()));
        assert_eq!(func.blocks.len(), 4);
    }
}
