//! Reuse a conditional increment's predicate when testing whether it changed.

use super::*;

/// A nonzero modular increment always changes the observed bits, including
/// on wraparound. Keep the selected value itself intact: a 32-bit increment
/// clears upper bits, whereas the unselected arm may retain them.
pub(super) fn fold(func: &mut MFunction) {
    let mut defs = vec![None; func.vregs.count() as usize];
    for inst in func.blocks.iter().flat_map(|block| &block.insts) {
        if let Some(dst) = inst.def() {
            defs[dst.0 as usize] = Some(inst);
        }
    }
    let mut replacements = Vec::new();
    for (bi, block) in func.blocks.iter().enumerate() {
        for (ii, inst) in block.insts.iter().enumerate() {
            let (dst, lhs, rhs, kind) = match *inst {
                MInst::Cmp {
                    dst,
                    lhs,
                    rhs,
                    kind,
                } => (dst, lhs, Some(rhs), kind),
                MInst::CmpImm {
                    dst,
                    lhs,
                    imm: 0,
                    kind,
                } => (dst, lhs, None, kind),
                _ => continue,
            };
            if !matches!(kind, CmpKind::Eq | CmpKind::Ne) {
                continue;
            }
            let is_zero =
                |reg: VReg| matches!(defs[reg.0 as usize], Some(MInst::LoadImm { value: 0, .. }));
            let xor = match rhs {
                None => Some(lhs),
                Some(rhs) if is_zero(rhs) => Some(lhs),
                Some(rhs) if is_zero(lhs) => Some(rhs),
                _ => None,
            };
            let operands = match xor.and_then(|reg| defs[reg.0 as usize]) {
                Some(MInst::Xor { lhs, rhs, .. }) => Some((*lhs, *rhs, false)),
                Some(MInst::Xor32 { lhs, rhs, .. }) => Some((*lhs, *rhs, true)),
                _ => rhs.map(|rhs| (lhs, rhs, false)),
            };
            let Some((lhs, rhs, word32)) = operands else {
                continue;
            };
            let replacement = change_predicate(&defs, dst, lhs, rhs, word32, kind)
                .or_else(|| change_predicate(&defs, dst, rhs, lhs, word32, kind));
            if let Some(replacement) = replacement {
                replacements.push((bi, ii, replacement));
            }
        }
    }
    drop(defs);
    for (bi, ii, replacement) in replacements {
        func.blocks[bi].insts[ii] = replacement;
    }
}

fn change_predicate(
    defs: &[Option<&MInst>],
    dst: VReg,
    base: VReg,
    selected: VReg,
    word32: bool,
    comparison: CmpKind,
) -> Option<MInst> {
    let select = defs[selected.0 as usize]?;
    let (true_val, false_val) = match *select {
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
        } => (true_val, false_val),
        _ => return None,
    };
    let changed_when_true = if false_val == base && is_increment(defs, true_val, base, word32) {
        true
    } else if true_val == base && is_increment(defs, false_val, base, word32) {
        false
    } else {
        return None;
    };
    let invert = changed_when_true == (comparison == CmpKind::Eq);
    let predicate_kind = |kind| {
        if invert {
            invert_compare_kind(kind)
        } else {
            kind
        }
    };
    Some(match *select {
        MInst::Select { cond, .. }
            if !invert
                && matches!(
                    defs[cond.0 as usize],
                    Some(MInst::Cmp { .. } | MInst::CmpImm { .. })
                ) =>
        {
            MInst::Mov { dst, src: cond }
        }
        MInst::Select { cond, .. } => MInst::CmpImm {
            dst,
            lhs: cond,
            imm: 0,
            kind: predicate_kind(CmpKind::Ne),
        },
        MInst::CmpSelect { lhs, rhs, kind, .. } => MInst::Cmp {
            dst,
            lhs,
            rhs,
            kind: predicate_kind(kind),
        },
        MInst::CmpImmSelect { lhs, imm, kind, .. } => MInst::CmpImm {
            dst,
            lhs,
            imm,
            kind: predicate_kind(kind),
        },
        _ => unreachable!(),
    })
}

fn is_increment(defs: &[Option<&MInst>], value: VReg, base: VReg, word32: bool) -> bool {
    increment_delta(defs, value, base).is_some_and(|(delta, _)| {
        if word32 {
            delta as u32 != 0
        } else {
            delta != 0
        }
    })
}

fn increment_delta(defs: &[Option<&MInst>], value: VReg, base: VReg) -> Option<(u64, bool)> {
    let inst = defs[value.0 as usize]?;
    let constant = |reg: VReg| match defs[reg.0 as usize] {
        Some(MInst::LoadImm { value, .. }) => Some(*value),
        _ => None,
    };
    let (amount, arithmetic32) = match *inst {
        MInst::Add { lhs, rhs, .. } | MInst::Add32 { lhs, rhs, .. } => {
            let amount = if lhs == base {
                constant(rhs)
            } else if rhs == base {
                constant(lhs)
            } else {
                None
            };
            (amount, matches!(inst, MInst::Add32 { .. }))
        }
        MInst::Sub { lhs, rhs, .. } | MInst::Sub32 { lhs, rhs, .. } if lhs == base => (
            constant(rhs).map(u64::wrapping_neg),
            matches!(inst, MInst::Sub32 { .. }),
        ),
        MInst::AddImm { src, imm, .. } | MInst::SubImm { src, imm, .. } if src == base => {
            let delta = imm as i64 as u64;
            (
                Some(if matches!(inst, MInst::SubImm { .. }) {
                    delta.wrapping_neg()
                } else {
                    delta
                }),
                false,
            )
        }
        _ => return None,
    };
    let delta = amount?;
    Some((
        if arithmetic32 {
            delta & u64::from(u32::MAX)
        } else {
            delta
        },
        arithmetic32,
    ))
}

/// `c ? x + 1 : x` is `x + (c != 0)`. For narrow arithmetic the unchanged
/// arm must also be zero extended; otherwise this would clear its upper bits.
pub(super) fn lower_unit_steps(func: &mut MFunction) {
    #[derive(Clone, Copy, PartialEq, Eq, Hash)]
    enum PredicateKey {
        Compare(VReg, VReg, CmpKind),
        Immediate(VReg, i32, CmpKind),
    }
    fn key(inst: &MInst) -> Option<PredicateKey> {
        match *inst {
            MInst::Cmp {
                mut lhs,
                mut rhs,
                kind,
                ..
            } => {
                if matches!(kind, CmpKind::Eq | CmpKind::Ne) && rhs < lhs {
                    std::mem::swap(&mut lhs, &mut rhs);
                }
                Some(PredicateKey::Compare(lhs, rhs, kind))
            }
            MInst::CmpImm { lhs, imm, kind, .. } => Some(PredicateKey::Immediate(lhs, imm, kind)),
            _ => None,
        }
    }
    let mut defs = vec![None; func.vregs.count() as usize];
    for inst in func.blocks.iter().flat_map(|block| &block.insts) {
        if let Some(dst) = inst.def() {
            defs[dst.0 as usize] = Some(inst);
        }
    }
    let mut replacements = Vec::new();
    let mut predicates = Vec::new();
    let mut moved = HashSet::default();
    for (bi, block) in func.blocks.iter().enumerate() {
        let mut conditions = HashMap::default();
        for (ii, inst) in block.insts.iter().enumerate() {
            if let Some(key) = key(inst) {
                conditions.entry(key).or_insert((ii, inst));
            }
        }
        for (ii, inst) in block.insts.iter().enumerate() {
            let (dst, true_val, false_val) = match *inst {
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
                } => (dst, true_val, false_val),
                _ => continue,
            };
            let step = increment_delta(&defs, true_val, false_val)
                .map(|delta| (false_val, delta))
                .or_else(|| {
                    increment_delta(&defs, false_val, true_val).map(|delta| (true_val, delta))
                });
            let Some((base, (delta, word32))) = step else {
                continue;
            };
            let mask = if word32 {
                u64::from(u32::MAX)
            } else {
                u64::MAX
            };
            if delta != 1 && delta != mask {
                continue;
            }
            if word32
                && !matches!(
                    defs[base.0 as usize],
                    Some(
                        MInst::Load {
                            size: OpSize::S8 | OpSize::S16 | OpSize::S32,
                            ..
                        } | MInst::Mov32 { .. }
                            | MInst::Add32 { .. }
                            | MInst::Sub32 { .. }
                            | MInst::AndImm32 { .. }
                    )
                )
            {
                continue;
            }
            let predicate = change_predicate(&defs, dst, base, dst, false, CmpKind::Ne)
                .expect("a unit step always changes its base");
            let rhs = if let MInst::Mov { src, .. } = predicate {
                src
            } else {
                // Do not introduce a SETcc just to replace CMOV. Reuse a
                // predicate already materialized for another consumer, often
                // the change test folded above. Its operands are precisely
                // the select's condition inputs, so they already dominate
                // this position even if the comparison originally came later.
                let Some(&(from, comparison)) =
                    key(&predicate).and_then(|key| conditions.get(&key))
                else {
                    continue;
                };
                if from > ii && moved.insert((bi, from)) {
                    predicates.push((bi, ii, comparison.clone()));
                }
                comparison.def().unwrap()
            };
            let lhs = base;
            let replacement = match (delta == 1, word32) {
                (true, false) => MInst::Add { dst, lhs, rhs },
                (true, true) => MInst::Add32 { dst, lhs, rhs },
                (false, false) => MInst::Sub { dst, lhs, rhs },
                (false, true) => MInst::Sub32 { dst, lhs, rhs },
            };
            replacements.push((bi, ii, replacement));
        }
    }
    drop(defs);
    for (bi, ii, replacement) in replacements {
        func.blocks[bi].insts[ii] = replacement;
    }
    let mut predicates = predicates.into_iter().peekable();
    for (bi, block) in func.blocks.iter_mut().enumerate() {
        if !predicates.peek().is_some_and(|&(at, _, _)| at == bi) {
            continue;
        }
        for (ii, inst) in std::mem::take(&mut block.insts).into_iter().enumerate() {
            if predicates
                .peek()
                .is_some_and(|&(owner, at, _)| owner == bi && at == ii)
            {
                block.push(predicates.next().unwrap().2);
            }
            if !moved.contains(&(bi, ii)) {
                block.push(inst);
            }
        }
    }
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests;
