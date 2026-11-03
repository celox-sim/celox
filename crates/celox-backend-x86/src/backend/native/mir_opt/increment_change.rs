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
mod tests {
    use super::*;
    use crate::native::{emit, jit_mem::JitCode, mir_legalize, regalloc};

    fn compile(mut function: MFunction) -> (JitCode, usize) {
        mir_legalize::legalize(&mut function);
        let allocation = regalloc::run_regalloc(&mut function).unwrap();
        let code = emit::emit(
            &function,
            &allocation.assignment,
            allocation.spill_frame_size,
        )
        .unwrap();
        (
            JitCode::new(&code.code).unwrap(),
            code.required_state_size.max(48) as usize,
        )
    }

    #[test]
    fn pipeline_reuses_comparisons_exposed_by_tree_scheduling() {
        let mut vregs = VRegAllocator::new();
        for _ in 0..18 {
            vregs.alloc();
        }
        let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 18]);
        let mut block = MBlock::new(BlockId(0));
        for i in 0..3 {
            block.push(MInst::Load {
                dst: VReg(i),
                base: BaseReg::SimState,
                offset: i as i32 * 8,
                size: OpSize::S32,
            });
        }
        block.push(MInst::LoadImm {
            dst: VReg(3),
            value: 1,
        });
        block.push(MInst::Cmp {
            dst: VReg(4),
            lhs: VReg(1),
            rhs: VReg(2),
            kind: CmpKind::Ne,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 24,
            src: VReg(4),
            size: OpSize::S64,
        });
        for i in 9..13 {
            block.push(MInst::Load {
                dst: VReg(i),
                base: BaseReg::SimState,
                offset: 40 + (i - 9) as i32,
                size: OpSize::S8,
            });
        }
        block.push(MInst::Or32 {
            dst: VReg(13),
            lhs: VReg(4),
            rhs: VReg(9),
        });
        block.push(MInst::Add32 {
            dst: VReg(5),
            lhs: VReg(0),
            rhs: VReg(3),
        });
        block.push(MInst::CmpSelect {
            dst: VReg(6),
            lhs: VReg(1),
            rhs: VReg(2),
            kind: CmpKind::Ne,
            true_val: VReg(5),
            false_val: VReg(0),
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: VReg(6),
            size: OpSize::S32,
        });
        block.push(MInst::Xor32 {
            dst: VReg(7),
            lhs: VReg(0),
            rhs: VReg(6),
        });
        block.push(MInst::CmpImm {
            dst: VReg(8),
            lhs: VReg(7),
            imm: 0,
            kind: CmpKind::Ne,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 32,
            src: VReg(8),
            size: OpSize::S64,
        });
        for (i, rhs) in [VReg(8), VReg(10), VReg(11), VReg(12)]
            .into_iter()
            .enumerate()
        {
            block.push(MInst::Or32 {
                dst: VReg(14 + i as u32),
                lhs: VReg(13 + i as u32),
                rhs,
            });
        }
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 48,
            src: VReg(17),
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        function.push_block(block);
        pipeline::optimize(&mut function);
        function.verify();
        let stored = |offset| {
            function
                .blocks
                .iter()
                .flat_map(|block| &block.insts)
                .find_map(|inst| match inst {
                    MInst::Store {
                        offset: at, src, ..
                    } if *at == offset => Some(*src),
                    _ => None,
                })
                .unwrap()
        };
        assert_eq!(stored(24), stored(32), "{:?}", function.blocks);
        let (code, size) = compile(function);
        for count in [0u32, u32::MAX] {
            for changed in [false, true] {
                let mut memory = vec![0; size];
                memory[..4].copy_from_slice(&count.to_le_bytes());
                memory[8..12].copy_from_slice(&u32::from(changed).to_le_bytes());
                assert_eq!(unsafe { code.call(&mut memory) }, 0);
                assert_eq!(
                    u32::from_le_bytes(memory[..4].try_into().unwrap()),
                    count.wrapping_add(u32::from(changed))
                );
                assert_eq!(
                    u64::from_le_bytes(memory[32..40].try_into().unwrap()),
                    u64::from(changed)
                );
            }
        }
    }

    #[test]
    fn increment_changes_preserve_wraparound_widths_and_shared_xor_values() {
        for bounded in [false, true] {
            for arithmetic in 0..6 {
                for observation in 0..3 {
                    for selector in 0..3 {
                        for reversed in [false, true] {
                            for equal in [false, true] {
                                for amount in [0, 1, u32::MAX as u64, 1 << 32, u64::MAX] {
                                    if arithmetic >= 4 && amount as i64 != amount as i32 as i64 {
                                        continue;
                                    }
                                    let mut vregs = VRegAllocator::new();
                                    for _ in 0..8 {
                                        vregs.alloc();
                                    }
                                    let mut original =
                                        MFunction::new(vregs, vec![SpillDesc::transient(); 8]);
                                    let mut block = MBlock::new(BlockId(0));
                                    for i in 0..2 {
                                        block.push(MInst::Load {
                                            dst: VReg(i),
                                            base: BaseReg::SimState,
                                            offset: i as i32 * 8,
                                            size: if bounded && i == 0 {
                                                OpSize::S32
                                            } else {
                                                OpSize::S64
                                            },
                                        });
                                    }
                                    block.push(MInst::LoadImm {
                                        dst: VReg(2),
                                        value: amount,
                                    });
                                    let (dst, lhs, rhs) = (VReg(3), VReg(0), VReg(2));
                                    block.push(match arithmetic {
                                        0 => MInst::Add {
                                            dst,
                                            lhs: rhs,
                                            rhs: lhs,
                                        },
                                        1 => MInst::Add32 { dst, lhs, rhs },
                                        2 => MInst::Sub { dst, lhs, rhs },
                                        3 => MInst::Sub32 { dst, lhs, rhs },
                                        4 => MInst::AddImm {
                                            dst,
                                            src: lhs,
                                            imm: amount as i32,
                                        },
                                        _ => MInst::SubImm {
                                            dst,
                                            src: lhs,
                                            imm: amount as i32,
                                        },
                                    });
                                    let (true_val, false_val) = if reversed {
                                        (VReg(0), VReg(3))
                                    } else {
                                        (VReg(3), VReg(0))
                                    };
                                    block.push(match selector {
                                        0 => MInst::Select {
                                            dst: VReg(4),
                                            cond: VReg(1),
                                            true_val,
                                            false_val,
                                        },
                                        1 => MInst::CmpSelect {
                                            dst: VReg(4),
                                            lhs: VReg(1),
                                            rhs: VReg(2),
                                            kind: CmpKind::LtU,
                                            true_val,
                                            false_val,
                                        },
                                        _ => MInst::CmpImmSelect {
                                            dst: VReg(4),
                                            lhs: VReg(1),
                                            imm: -1,
                                            kind: CmpKind::GtS,
                                            true_val,
                                            false_val,
                                        },
                                    });
                                    let (dst, lhs, rhs) = (VReg(5), VReg(4), VReg(0));
                                    block.push(if observation == 2 {
                                        MInst::Xor32 { dst, lhs, rhs }
                                    } else {
                                        MInst::Xor { dst, lhs, rhs }
                                    });
                                    let kind = if equal { CmpKind::Eq } else { CmpKind::Ne };
                                    block.push(if observation == 0 {
                                        MInst::Cmp {
                                            dst: VReg(6),
                                            lhs: VReg(4),
                                            rhs: VReg(0),
                                            kind,
                                        }
                                    } else {
                                        MInst::CmpImm {
                                            dst: VReg(6),
                                            lhs: VReg(5),
                                            imm: 0,
                                            kind,
                                        }
                                    });
                                    for (offset, src) in
                                        [(16, VReg(4)), (24, VReg(5)), (32, VReg(6))]
                                    {
                                        block.push(MInst::Store {
                                            base: BaseReg::SimState,
                                            offset,
                                            src,
                                            size: OpSize::S64,
                                        });
                                    }
                                    block.push(MInst::Return);
                                    original.push_block(block);
                                    let mut optimized = original.clone();
                                    fold(&mut optimized);
                                    lower_unit_steps(&mut optimized);
                                    let narrow = matches!(arithmetic, 1 | 3);
                                    let mask = if narrow {
                                        u64::from(u32::MAX)
                                    } else {
                                        u64::MAX
                                    };
                                    let unit = amount & mask;
                                    let should_lower = !equal
                                        && (!narrow || bounded)
                                        && (unit == 1 || unit == mask);
                                    let selected = optimized.blocks[0]
                                        .insts
                                        .iter()
                                        .find(|inst| inst.def() == Some(VReg(4)))
                                        .unwrap();
                                    assert_eq!(
                                        matches!(
                                            selected,
                                            MInst::Add { .. }
                                                | MInst::Add32 { .. }
                                                | MInst::Sub { .. }
                                                | MInst::Sub32 { .. }
                                        ),
                                        should_lower
                                    );
                                    optimized.verify();
                                    let should_fold =
                                        if observation == 2 || matches!(arithmetic, 1 | 3) {
                                            amount as u32 != 0
                                        } else {
                                            amount != 0
                                        };
                                    let result = optimized.blocks[0]
                                        .insts
                                        .iter()
                                        .find(|inst| inst.def() == Some(VReg(6)))
                                        .unwrap();
                                    assert_eq!(result.uses().contains(&VReg(1)), should_fold);
                                    let (before, before_size) = compile(original);
                                    let (after, after_size) = compile(optimized);
                                    for base in
                                        [0u64, 1, u32::MAX as u64, 1 << 32, 1 << 63, u64::MAX]
                                    {
                                        for condition in [0u64, 1, 2, 1 << 32, 1 << 63, u64::MAX] {
                                            let mut left = vec![0xa5; before_size.max(after_size)];
                                            left[..8].copy_from_slice(&base.to_le_bytes());
                                            left[8..16].copy_from_slice(&condition.to_le_bytes());
                                            let mut right = left.clone();
                                            assert_eq!(unsafe { before.call(&mut left) }, 0);
                                            assert_eq!(unsafe { after.call(&mut right) }, 0);
                                            assert_eq!(
                                                &left[..40],
                                                &right[..40],
                                                "bounded={bounded} arithmetic={arithmetic} observation={observation} selector={selector} reversed={reversed} equal={equal} amount={amount:#x} base={base:#x} condition={condition:#x}"
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
