//! MIR optimization passes: run between ISel and regalloc.
//!
//! - Copy propagation: `v2 = mov v1` → replace all uses of v2 with v1
//! - Dead code elimination: remove instructions whose defs are unused

use std::collections::{HashMap, HashSet};

use super::mir::*;

/// Run all MIR optimization passes.
pub fn optimize(func: &mut MFunction) {
    let verify = std::env::var_os("CELOX_MIR_VERIFY_PASSES").is_some();
    macro_rules! pass {
        ($name:literal, $call:expr) => {{
            $call;
            if verify {
                if let Err(error) = func.verify_result() {
                    panic!("after MIR pass {}: {error}", $name);
                }
            }
        }};
    }
    if func.vregs.count() > 40 {
        // High-pressure: full pipeline
        pass!("fold_proven_comparisons", fold_proven_comparisons(func));
        for _ in 0..2 {
            pass!("constant_fold", constant_fold(func));
            pass!("constant_dedup", constant_dedup(func));
            pass!("copy_propagate", copy_propagate(func));
            pass!("forward_local_store_loads", forward_local_store_loads(func));
            pass!(
                "eliminate_redundant_local_stores",
                eliminate_redundant_local_stores(func)
            );
            pass!("algebraic_simplify", algebraic_simplify(func));
            pass!("redundant_mask_eliminate", redundant_mask_eliminate(func));
            pass!("fold_bit_toggle_insert", fold_bit_toggle_insert(func));
            pass!("global_gvn", global_gvn(func));
            pass!("dead_code_eliminate", dead_code_eliminate(func));
        }
        pass!("lower_to_imm_forms", lower_to_imm_forms(func));
        pass!(
            "fold_boolean_normalizations",
            fold_boolean_normalizations(func)
        );
        pass!("redundant_mask_eliminate", redundant_mask_eliminate(func));
        pass!("copy_propagate", copy_propagate(func));
        pass!("dead_code_eliminate", dead_code_eliminate(func));
        pass!("fuse_compare_selects", fuse_compare_selects(func));
        pass!("dead_code_eliminate", dead_code_eliminate(func));
        pass!("sink_loads", sink_loads(func));
        pass!("split_live_ranges", split_live_ranges(func));
        pass!(
            "eliminate_redundant_or_terms",
            eliminate_redundant_or_terms(func)
        );
        pass!("dead_code_eliminate", dead_code_eliminate(func));
        if func.target_features.bmi2() {
            pass!(
                "fold_deposit_chain_to_pdep",
                fold_deposit_chain_to_pdep(func)
            );
            pass!(
                "fold_extract_chain_to_pext",
                fold_extract_chain_to_pext(func)
            );
            pass!("fold_xor_chain_to_pext", fold_xor_chain_to_pext(func));
        }
        pass!("fold_add_chain_to_popcnt", fold_add_chain_to_popcnt(func));
        pass!("dead_code_eliminate", dead_code_eliminate(func));
    } else {
        // Low-pressure: lightweight but complete pipeline
        pass!("fold_proven_comparisons", fold_proven_comparisons(func));
        pass!("constant_fold", constant_fold(func));
        pass!("constant_dedup", constant_dedup(func));
        pass!("copy_propagate", copy_propagate(func));
        pass!("forward_local_store_loads", forward_local_store_loads(func));
        pass!(
            "eliminate_redundant_local_stores",
            eliminate_redundant_local_stores(func)
        );
        pass!("algebraic_simplify", algebraic_simplify(func));
        pass!("redundant_mask_eliminate", redundant_mask_eliminate(func));
        pass!("fold_bit_toggle_insert", fold_bit_toggle_insert(func));
        pass!(
            "eliminate_redundant_or_terms",
            eliminate_redundant_or_terms(func)
        );
        if func.target_features.bmi2() {
            pass!(
                "fold_deposit_chain_to_pdep",
                fold_deposit_chain_to_pdep(func)
            );
            pass!(
                "fold_extract_chain_to_pext",
                fold_extract_chain_to_pext(func)
            );
            pass!("fold_xor_chain_to_pext", fold_xor_chain_to_pext(func));
        }
        pass!("fold_add_chain_to_popcnt", fold_add_chain_to_popcnt(func));
        pass!("dead_code_eliminate", dead_code_eliminate(func));
        pass!("lower_to_imm_forms", lower_to_imm_forms(func));
        pass!(
            "fold_boolean_normalizations",
            fold_boolean_normalizations(func)
        );
        pass!("redundant_mask_eliminate", redundant_mask_eliminate(func));
        pass!("copy_propagate", copy_propagate(func));
        pass!("dead_code_eliminate", dead_code_eliminate(func));
        pass!("fuse_compare_selects", fuse_compare_selects(func));
        pass!("dead_code_eliminate", dead_code_eliminate(func));
    }
    // The last algebraic/GVN iteration can expose a constant through a copy or
    // create a constant-valued immediate operation.  Close that pipeline here
    // instead of handing the resulting artificial long live ranges to
    // register allocation.
    pass!("final_copy_propagate", copy_propagate(func));
    pass!("final_constant_fold", constant_fold(func));
    pass!("final_lower_to_imm_forms", lower_to_imm_forms(func));
    pass!("final_dead_code_eliminate", dead_code_eliminate(func));
    pass!("simplify_cfg", simplify_cfg(func));
    // CFG simplification concatenates linear blocks.  Re-place constants only
    // after that concatenation, otherwise a block-local constant can acquire a
    // very long artificial live range in the merged block.
    pass!("final_sink_loads", sink_loads(func));
    pass!(
        "refresh_constant_spill_descs",
        refresh_constant_spill_descs(func)
    );
    if cfg!(debug_assertions) || std::env::var_os("CELOX_MIR_VERIFY").is_some() {
        if let Err(error) = func.verify_result() {
            panic!("after MIR optimizer: {error}");
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

/// `ToTwoState` and boolean lowering can leave `cmp.ne boolean, 0` after the
/// comparison itself has already normalized the value to zero or one. These
/// become visible especially after immediate-form lowering, so remove them
/// late and let copy propagation collapse the resulting aliases.
fn fold_boolean_normalizations(func: &mut MFunction) {
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
                _ => None,
            };
            if let Some(replacement) = replacement {
                *inst = replacement;
            }
        }
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
                    MInst::Sub { dst, lhs, rhs } => {
                        fold_bin(&consts, *dst, *lhs, *rhs, u64::wrapping_sub)
                    }
                    MInst::Mul { dst, lhs, rhs } => {
                        fold_bin(&consts, *dst, *lhs, *rhs, u64::wrapping_mul)
                    }
                    MInst::And { dst, lhs, rhs } => {
                        fold_bin(&consts, *dst, *lhs, *rhs, |a, b| a & b)
                    }
                    MInst::Or { dst, lhs, rhs } => {
                        fold_bin(&consts, *dst, *lhs, *rhs, |a, b| a | b)
                    }
                    MInst::Xor { dst, lhs, rhs } => {
                        fold_bin(&consts, *dst, *lhs, *rhs, |a, b| a ^ b)
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

// ────────────────────────────────────────────────────────────────
// Phase 1B: Redundant mask elimination
// ────────────────────────────────────────────────────────────────

/// Helper: compute the width of a mask that is `(1 << w) - 1`.
fn mask_width(imm: u64) -> Option<usize> {
    if imm == 0 {
        return Some(0);
    }
    if imm == u64::MAX {
        return Some(64);
    }
    // Check if imm is of the form (1 << w) - 1: all lower bits set
    let w = imm.trailing_ones() as usize;
    if imm == (1u64 << w) - 1 {
        Some(w)
    } else {
        None
    }
}

/// Redundant mask elimination: track known bit widths and remove unnecessary
/// AND masks when the source is already narrow enough.
fn redundant_mask_eliminate(func: &mut MFunction) {
    // Build def-map for AND chain folding
    let mut def_map: HashMap<VReg, MInst> = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            if let Some(d) = inst.def() {
                def_map.insert(d, inst.clone());
            }
        }
    }

    for block in &mut func.blocks {
        let mut known: HashMap<VReg, usize> = HashMap::new();

        for inst in &mut block.insts {
            let known_width = compute_known_width(inst, &known);

            let should_replace = if let MInst::AndImm { dst, src, imm } = inst {
                // Check 1: redundant mask (source already narrow enough)
                if let Some(mw) = mask_width(*imm) {
                    if let Some(&src_w) = known.get(src) {
                        if src_w <= mw {
                            Some(MaskElimAction::Mov(*dst, *src))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                // Check 2: AND chain folding — AndImm(AndImm(x, m1), m2) → AndImm(x, m1 & m2)
                } else {
                    None
                }
                .or_else(|| {
                    // AND chain: if src was defined by AndImm(inner, m1), fold to AndImm(inner, m1 & imm)
                    if let Some(MInst::AndImm {
                        src: inner,
                        imm: m1,
                        ..
                    }) = def_map.get(src)
                    {
                        let folded = *m1 & *imm;
                        Some(MaskElimAction::FoldAnd(*dst, *inner, folded))
                    } else {
                        None
                    }
                })
            } else {
                None
            };

            if let Some(action) = should_replace {
                match action {
                    MaskElimAction::Mov(dst, src) => {
                        *inst = MInst::Mov { dst, src };
                        if let Some(&src_w) = known.get(&src) {
                            known.insert(dst, src_w);
                        }
                    }
                    MaskElimAction::FoldAnd(dst, inner, folded_mask) => {
                        *inst = MInst::AndImm {
                            dst,
                            src: inner,
                            imm: folded_mask,
                        };
                        let w = if folded_mask == 0 {
                            0
                        } else {
                            64 - folded_mask.leading_zeros() as usize
                        };
                        known.insert(dst, w);
                    }
                }
                continue;
            }

            if let Some(w) = known_width {
                if let Some(d) = inst.def() {
                    known.insert(d, w);
                }
            }
        }
    }
}

enum MaskElimAction {
    Mov(VReg, VReg),
    FoldAnd(VReg, VReg, u64),
}

/// Compute the known bit width of an instruction's result.
fn compute_known_width(inst: &MInst, known: &HashMap<VReg, usize>) -> Option<usize> {
    match inst {
        MInst::LoadImm { value, .. } => {
            if *value == 0 {
                Some(0)
            } else {
                Some(64 - value.leading_zeros() as usize)
            }
        }
        MInst::LoadConstantTableAddr { .. } => Some(64),
        MInst::Load { size, .. } | MInst::LoadIndexed { size, .. } => {
            Some(size.bytes() as usize * 8)
        }
        MInst::Cmp { .. } | MInst::CmpImm { .. } => Some(1),
        MInst::Popcnt { .. } => Some(7), // max popcnt(u64) = 64, fits in 7 bits
        MInst::Bsr { .. } => Some(6),    // max bsr(u64) = 63
        MInst::BsrOr { .. } => Some(6),  // max bsr(u64) = 63
        MInst::Mov { src, .. } => known.get(src).copied(),
        MInst::AndImm { src, imm, .. } => {
            let imm_w = if *imm == 0 {
                0
            } else {
                64 - imm.leading_zeros() as usize
            };
            let src_w = known.get(src).copied().unwrap_or(64);
            Some(src_w.min(imm_w))
        }
        MInst::OrImm { src, imm, .. } => {
            let imm_w = if *imm == 0 {
                0
            } else {
                64 - imm.leading_zeros() as usize
            };
            let src_w = known.get(src).copied().unwrap_or(64);
            Some(src_w.max(imm_w))
        }
        MInst::ShrImm { src, imm, .. } => known.get(src).map(|&w| w.saturating_sub(*imm as usize)),
        MInst::ShlImm { src, imm, .. } => known.get(src).map(|&w| (w + *imm as usize).min(64)),
        MInst::And { lhs, rhs, .. } => match (known.get(lhs), known.get(rhs)) {
            (Some(&l), Some(&r)) => Some(l.min(r)),
            (Some(&l), None) => Some(l),
            (None, Some(&r)) => Some(r),
            _ => None,
        },
        MInst::Or { lhs, rhs, .. } | MInst::Xor { lhs, rhs, .. } => {
            match (known.get(lhs), known.get(rhs)) {
                (Some(&l), Some(&r)) => Some(l.max(r)),
                _ => None,
            }
        }
        MInst::Add { lhs, rhs, .. } => match (known.get(lhs), known.get(rhs)) {
            (Some(&l), Some(&r)) => Some((l.max(r) + 1).min(64)),
            _ => None,
        },
        // Bit widths alone do not bound a subtraction result: `0 - 1`
        // produces all one bits.  In particular, do not use an unsigned
        // input-width estimate to remove a following mask.
        MInst::Sub { .. } => None,
        MInst::Mul { lhs, rhs, .. } => match (known.get(lhs), known.get(rhs)) {
            (Some(&l), Some(&r)) => Some((l + r).min(64)),
            _ => None,
        },
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
        } => match (known.get(true_val), known.get(false_val)) {
            (Some(&t), Some(&f)) => Some(t.max(f)),
            _ => None,
        },
        MInst::Pext { .. } => Some(64), // conservative
        MInst::Pdep { .. } => Some(64), // conservative
        _ => None,
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
    Load(BaseReg, i32, OpSize),
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

fn gvn_value(value_numbers: &[ValueNumber], vreg: VReg) -> ValueNumber {
    value_numbers[vreg.0 as usize]
}

fn gvn_key(inst: &MInst, value_numbers: &[ValueNumber]) -> Option<GvnKey> {
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
        // Bsr has an unspecified result for zero, so it has no reusable value.
        MInst::Bsr { .. } => None,
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
        } => Some(GvnKey::Load(*base, *offset, *size)),
        _ => None,
    }
}

fn gvn_binary(op: GvnOpcode, mut lhs: ValueNumber, mut rhs: ValueNumber) -> GvnKey {
    if gvn_is_commutative(op) && rhs < lhs {
        std::mem::swap(&mut lhs, &mut rhs);
    }
    GvnKey::Binary(op, lhs, rhs)
}

#[derive(Clone, Copy)]
enum MemoryWrite {
    None,
    Static {
        base: BaseReg,
        offset: i32,
        byte_len: usize,
    },
    Unknown {
        base: Option<BaseReg>,
    },
}

/// Describe the memory written by an instruction. `MemCopy` reads its source
/// range and writes its destination range; only the latter invalidates an
/// already-loaded value.
fn memory_write(inst: &MInst) -> MemoryWrite {
    match inst {
        MInst::Store {
            base, offset, size, ..
        } => MemoryWrite::Static {
            base: *base,
            offset: *offset,
            byte_len: size.bytes() as usize,
        },
        MInst::MemCopy {
            dst_offset,
            byte_len,
            ..
        } => MemoryWrite::Static {
            base: BaseReg::SimState,
            offset: *dst_offset,
            byte_len: *byte_len,
        },
        MInst::SparseCommit { .. }
        | MInst::SparseMarkActive { .. }
        | MInst::SparseCommitWorklist { .. } => MemoryWrite::Unknown {
            base: Some(BaseReg::SimState),
        },
        MInst::StoreIndexed { base, .. } => MemoryWrite::Unknown { base: Some(*base) },
        MInst::StorePtr { .. }
        | MInst::ReleaseStorePtr { .. }
        | MInst::StorePtrIndexed { .. }
        | MInst::ReleaseStorePtrIndexed { .. } => MemoryWrite::Unknown { base: None },
        _ => MemoryWrite::None,
    }
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
    let mut active_load_keys: HashSet<GvnKey> = HashSet::new();
    let mut table_changes: Vec<(GvnKey, Option<ValueNumber>)> = Vec::new();
    let mut leader_changes: Vec<(ValueNumber, VReg, Option<usize>)> = Vec::new();
    let mut replacements: Vec<(usize, usize, MInst)> = Vec::new(); // (block_idx, inst_idx, new_inst)

    // Dominator-scoped GVN. Every table mutation, including load invalidation
    // by a store, is undo-logged so sibling subtrees see exactly the state at
    // their common dominator.
    fn gvn_dfs(
        node: usize,
        dom_children: &[Vec<usize>],
        func: &MFunction,
        value_numbers: &mut [ValueNumber],
        value_leaders: &mut [VReg],
        leader_blocks: &mut [Option<usize>],
        live_out: &[HashSet<VReg>],
        last_uses: &[HashMap<VReg, usize>],
        value_table: &mut HashMap<GvnKey, ValueNumber>,
        active_load_keys: &mut HashSet<GvnKey>,
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
            value_table,
            active_load_keys,
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
                value_table,
                active_load_keys,
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
                value_table.insert(key.clone(), previous);
                if matches!(key, GvnKey::Load(..)) {
                    active_load_keys.insert(key);
                }
            } else {
                value_table.remove(&key);
                if matches!(key, GvnKey::Load(..)) {
                    active_load_keys.remove(&key);
                }
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
        value_table: &mut HashMap<GvnKey, ValueNumber>,
        active_load_keys: &mut HashSet<GvnKey>,
        table_changes: &mut Vec<(GvnKey, Option<ValueNumber>)>,
        leader_changes: &mut Vec<(ValueNumber, VReg, Option<usize>)>,
        replacements: &mut Vec<(usize, usize, MInst)>,
    ) {
        for inst_idx in 0..block.insts.len() {
            let inst = &block.insts[inst_idx];

            let write = memory_write(inst);
            if !matches!(write, MemoryWrite::None) {
                // Only invalidate Load CSE entries that overlap a known write.
                // Dynamic/pointer writes remain conservative full clobbers.
                let load_keys = active_load_keys
                    .iter()
                    .filter(|k| {
                        let GvnKey::Load(load_base, load_offset, load_size) = k else {
                            return false;
                        };
                        match write {
                            MemoryWrite::None => false,
                            MemoryWrite::Unknown { base } => {
                                base.is_none_or(|base| *load_base == base)
                            }
                            MemoryWrite::Static {
                                base,
                                offset,
                                byte_len,
                            } => {
                                if *load_base != base {
                                    return false;
                                }
                                byte_ranges_may_overlap(
                                    *load_offset,
                                    load_size.bytes() as usize,
                                    offset,
                                    byte_len,
                                )
                            }
                        }
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                for key in load_keys {
                    let previous = value_table
                        .remove(&key)
                        .expect("active load key must exist in the GVN table");
                    active_load_keys.remove(&key);
                    table_changes.push((key, Some(previous)));
                }
            }

            if let MInst::Mov { dst, src } = inst {
                let number = gvn_value(value_numbers, *src);
                value_numbers[dst.0 as usize] = number;
                continue;
            }

            if let Some(key) = gvn_key(inst, value_numbers) {
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
                    if dst != leader && reuse_does_not_extend_live_range {
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
                    if matches!(key, GvnKey::Load(..)) {
                        active_load_keys.insert(key.clone());
                    }
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
        &mut value_table,
        &mut active_load_keys,
        &mut table_changes,
        &mut leader_changes,
        &mut replacements,
    );
    debug_assert!(value_table.is_empty());
    debug_assert!(active_load_keys.is_empty());
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

/// Algebraic simplification: identity, annihilation, self-inverse, and
/// strength reduction rules.
fn algebraic_simplify(func: &mut MFunction) {
    // Build def map for constant lookups
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
                // Identity: mul x, 1 → x; annihilation: mul x, 0 → 0
                // Strength reduction: mul x, 2^n → shl x, n
                MInst::Mul { dst, lhs, rhs } => try_simplify_mul(*dst, *lhs, *rhs, &consts),
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
                // OrImm identity
                MInst::OrImm { dst, src, imm: 0 } => Some(Simplification::Mov(*dst, *src)),
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
                    Simplification::Const(dst, value) => {
                        *inst = MInst::LoadImm { dst, value };
                        consts.insert(dst, value);
                    }
                    Simplification::Shl(dst, src, imm) => {
                        *inst = MInst::ShlImm { dst, src, imm };
                    }
                }
            }
        }
    }
}

enum Simplification {
    Mov(VReg, VReg),
    Const(VReg, u64),
    Shl(VReg, VReg, u8),
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
            match inst {
                MInst::Jump { target } => {
                    if let Some(&new_target) = resolved.get(target) {
                        *target = new_target;
                    }
                }
                MInst::Branch {
                    true_bb, false_bb, ..
                } => {
                    if let Some(&new_t) = resolved.get(true_bb) {
                        *true_bb = new_t;
                    }
                    if let Some(&new_f) = resolved.get(false_bb) {
                        *false_bb = new_f;
                    }
                }
                _ => {}
            }
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

// ────────────────────────────────────────────────────────────────
// Live range splitting
// ────────────────────────────────────────────────────────────────

/// Split long live ranges by re-materializing values close to their use.
///
/// For VRegs with a long gap between definition and use, insert a
/// re-materialization instruction (duplicate Load/LoadImm) just before
/// the use and rewrite the use to the new VReg. The original VReg's
/// live range shortens, freeing a register during the gap.
///
/// Handles:
/// - LoadImm: re-emit the same constant (free, already handled by sink_loads for most cases)
/// - SimState Load: re-load from the same memory address (1 instruction)
/// - Transient values: insert Store to stack after def + Load before use (2+1 instructions)
fn split_live_ranges(func: &mut MFunction) {
    // Only worth splitting if register pressure is high.
    // 13 registers, so only activate when VRegs significantly exceed registers.
    let num_regs = 13usize;
    if (func.vregs.count() as usize) < num_regs * 4 {
        return;
    }

    // Build def-map: VReg → (block_idx, inst_idx, defining instruction)
    let mut def_info: HashMap<VReg, (usize, usize, MInst)> = HashMap::new();
    for (bi, block) in func.blocks.iter().enumerate() {
        for (ii, inst) in block.insts.iter().enumerate() {
            if let Some(d) = inst.def() {
                def_info.insert(d, (bi, ii, inst.clone()));
            }
        }
    }

    // For each block, find VRegs with long gaps and split them.
    // Process per-block: find uses where the def is far away (in the same block).
    const SPLIT_THRESHOLD: usize = 20;

    // Collect all splits to apply
    let mut splits: Vec<SplitAction> = Vec::new();

    for (bi, block) in func.blocks.iter().enumerate() {
        // Find first use position of each VReg in this block
        let mut first_use_in_block: HashMap<VReg, usize> = HashMap::new();
        for (ii, inst) in block.insts.iter().enumerate() {
            for u in inst.uses() {
                first_use_in_block.entry(u).or_insert(ii);
            }
        }

        // Check each used VReg: if defined much earlier in the same block, split
        for (&vreg, &use_pos) in &first_use_in_block {
            let Some(&(def_bi, def_ii, ref def_inst)) = def_info.get(&vreg) else {
                continue;
            };

            // Only handle same-block for now (cross-block is complex)
            if def_bi != bi {
                continue;
            }

            let gap = use_pos.saturating_sub(def_ii);
            if gap < SPLIT_THRESHOLD {
                continue;
            }

            // Determine re-materialization strategy
            let remat = match def_inst {
                MInst::LoadImm { .. } => {
                    // Already handled by sink_loads — skip
                    None
                }
                MInst::Load {
                    base: BaseReg::SimState,
                    offset,
                    size,
                    ..
                } => {
                    let has_store = block.insts[def_ii + 1..use_pos]
                        .iter()
                        .any(|i| may_clobber_static_load(i, BaseReg::SimState, *offset, *size));
                    if !has_store {
                        Some(RematKind::SimLoad(*offset, *size))
                    } else {
                        None
                    }
                }
                _ => {
                    // Transient value: use Store + Load via stack
                    Some(RematKind::StackSpill)
                }
            };

            if let Some(kind) = remat {
                splits.push(SplitAction {
                    block_idx: bi,
                    use_pos,
                    vreg,
                    kind,
                });
            }
        }
    }

    // Sort splits by (block_idx, use_pos) descending to apply from end to start
    splits.sort_by_key(|split| std::cmp::Reverse((split.block_idx, split.use_pos)));

    // Apply splits
    for split in splits {
        let block = &mut func.blocks[split.block_idx];
        let (reload_inst, spill_desc, new_vreg) = match split.kind {
            RematKind::SimLoad(offset, size) => {
                let new_vreg = func.vregs.alloc();
                let inst = MInst::Load {
                    dst: new_vreg,
                    base: BaseReg::SimState,
                    offset,
                    size,
                };
                // Use transient SpillDesc — the regalloc will handle
                // further spilling if needed. The key benefit is that
                // the new VReg has a short live range.
                (inst, SpillDesc::transient(), new_vreg)
            }
            RematKind::StackSpill => {
                // For transient values: allocate a stack slot in the MIR,
                // insert Store after def and Load before use.
                // This is more complex — for now, skip transient values.
                // The regalloc handles them with its own spilling.
                continue;
            }
        };

        // Ensure spill_descs is large enough
        while func.spill_descs.len() <= new_vreg.0 as usize {
            func.spill_descs.push(spill_desc.clone());
        }

        // Insert reload instruction just before the use
        block.insts.insert(split.use_pos, reload_inst);

        // Rewrite the use (and all subsequent uses of this VReg in this block)
        // to use new_vreg instead
        for inst in &mut block.insts[split.use_pos + 1..] {
            if inst.uses().contains(&split.vreg) {
                inst.rewrite_use(split.vreg, new_vreg);
            }
        }
    }
}

struct SplitAction {
    block_idx: usize,
    use_pos: usize,
    vreg: VReg,
    kind: RematKind,
}

enum RematKind {
    SimLoad(i32, OpSize),
    StackSpill,
}

fn may_clobber_static_load(inst: &MInst, base: BaseReg, offset: i32, size: OpSize) -> bool {
    match memory_write(inst) {
        MemoryWrite::None => false,
        MemoryWrite::Unknown { base: write_base } => write_base.is_none_or(|other| other == base),
        MemoryWrite::Static {
            base: write_base,
            offset: write_offset,
            byte_len,
        } => {
            write_base == base
                && byte_ranges_may_overlap(offset, size.bytes() as usize, write_offset, byte_len)
        }
    }
}

fn byte_range(offset: i32, byte_len: usize) -> Option<(i64, i64)> {
    let start = i64::from(offset);
    let byte_len = i64::try_from(byte_len).ok()?;
    Some((start, start.checked_add(byte_len)?))
}

fn byte_ranges_may_overlap(
    lhs_offset: i32,
    lhs_byte_len: usize,
    rhs_offset: i32,
    rhs_byte_len: usize,
) -> bool {
    match (
        byte_range(lhs_offset, lhs_byte_len),
        byte_range(rhs_offset, rhs_byte_len),
    ) {
        (Some((lhs_start, lhs_end)), Some((rhs_start, rhs_end))) => {
            lhs_start < rhs_end && rhs_start < lhs_end
        }
        _ => true,
    }
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
                        find_covering_store(&available, base, offset, size)
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

fn find_covering_store(
    available: &HashMap<MemorySlot, VReg>,
    base: BaseReg,
    offset: i32,
    size: OpSize,
) -> Option<(MemorySlot, VReg)> {
    let load_start = offset as i64;
    let load_end = load_start + i64::from(size.bytes());
    available.iter().find_map(|(slot, &src)| {
        if slot.base != base {
            return None;
        }
        let store_start = slot.offset as i64;
        let store_end = store_start + i64::from(slot.size.bytes());
        if store_start <= load_start && load_end <= store_end {
            Some((*slot, src))
        } else {
            None
        }
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

fn eliminate_redundant_local_stores(func: &mut MFunction) {
    for block in &mut func.blocks {
        let mut later_stores: HashMap<MemorySlot, ()> = HashMap::new();
        let mut reversed = Vec::with_capacity(block.insts.len());

        for inst in block.insts.drain(..).rev() {
            match inst {
                MInst::Store {
                    base,
                    offset,
                    src,
                    size,
                } => {
                    let key = MemorySlot { base, offset, size };
                    if later_stores.contains_key(&key) {
                        continue;
                    }
                    invalidate_overlapping_slots(&mut later_stores, base, offset, size);
                    later_stores.insert(key, ());
                    reversed.push(MInst::Store {
                        base,
                        offset,
                        src,
                        size,
                    });
                }
                MInst::MemCopy {
                    src_offset,
                    dst_offset,
                    byte_len,
                } => {
                    // In reverse order, both ranges are barriers: the source
                    // is observed by the copy and the destination is changed
                    // by it. Preserve tracking for unrelated addresses.
                    invalidate_overlapping_byte_range(
                        &mut later_stores,
                        BaseReg::SimState,
                        src_offset,
                        byte_len,
                    );
                    invalidate_overlapping_byte_range(
                        &mut later_stores,
                        BaseReg::SimState,
                        dst_offset,
                        byte_len,
                    );
                    reversed.push(MInst::MemCopy {
                        src_offset,
                        dst_offset,
                        byte_len,
                    });
                }
                MInst::LoadIndexed { .. }
                | MInst::LoadPtrIndexed { .. }
                | MInst::StoreIndexed { .. }
                | MInst::StorePtrIndexed { .. }
                | MInst::ReleaseStorePtrIndexed { .. }
                | MInst::LoadPtr { .. }
                | MInst::StorePtr { .. }
                | MInst::ReleaseStorePtr { .. } => {
                    later_stores.clear();
                    reversed.push(inst);
                }
                MInst::Load {
                    dst,
                    base,
                    offset,
                    size,
                } => {
                    invalidate_overlapping_slots(&mut later_stores, base, offset, size);
                    reversed.push(MInst::Load {
                        dst,
                        base,
                        offset,
                        size,
                    });
                }
                other => reversed.push(other),
            }
        }

        reversed.reverse();
        let rewritten = reversed;
        block.insts = rewritten;
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

/// Copy propagation: for each `Mov { dst, src }`, replace all uses of dst
/// with src throughout the function. Then remove the Mov.
fn copy_propagate(func: &mut MFunction) {
    // Build alias map: dst → src (transitively resolved)
    let mut aliases: HashMap<VReg, VReg> = HashMap::new();

    for block in &func.blocks {
        for inst in &block.insts {
            if let MInst::Mov { dst, src } = inst {
                // Resolve transitively: if src is itself an alias, follow the chain
                let mut target = *src;
                while let Some(&next) = aliases.get(&target) {
                    target = next;
                }
                aliases.insert(*dst, target);
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
            if let MInst::Mov { dst, src } = inst {
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
                    size: OpSize::S64,
                },
                MInst::StoreIndexed {
                    base: BaseReg::SimState,
                    offset: 32,
                    index: VReg(0),
                    src: VReg(2),
                    size: OpSize::S64,
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
                    size: OpSize::S8,
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
    fn memcopy_destination_blocks_static_load_rematerialization() {
        let mut insts = vec![
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
        ];
        insts.extend((1..=24).map(|register| MInst::LoadImm {
            dst: VReg(register),
            value: register as u64,
        }));
        insts.extend([
            MInst::Store {
                base: BaseReg::SimState,
                offset: 96,
                src: VReg(0),
                size: OpSize::S64,
            },
            MInst::Return,
        ]);
        let mut func = make_func(insts, 60);

        split_live_ranges(&mut func);

        assert_eq!(
            func.blocks[0]
                .insts
                .iter()
                .filter(|inst| matches!(
                    inst,
                    MInst::Load {
                        base: BaseReg::SimState,
                        offset: 16,
                        size: OpSize::S64,
                        ..
                    }
                ))
                .count(),
            1,
            "{:#?}",
            func.blocks[0].insts
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
