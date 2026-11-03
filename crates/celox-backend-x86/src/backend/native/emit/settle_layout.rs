//! Keep selective-settle guards near their clean continuations. Allocation
//! order remains unchanged; only physical code placement follows these hints.
use super::*;

// The value when all settle flags are zero, and whether it depends on a flag.
// Unknown state inputs stop the analysis. This is a layout hint, never a proof
// permitting removal or movement of an instruction across a CFG edge.
type CleanValue = Option<(u64, bool)>;

fn compare(kind: CmpKind, lhs: u64, rhs: u64) -> u64 {
    u64::from(match kind {
        CmpKind::Eq => lhs == rhs,
        CmpKind::Ne => lhs != rhs,
        CmpKind::LtU => lhs < rhs,
        CmpKind::LeU => lhs <= rhs,
        CmpKind::GtU => lhs > rhs,
        CmpKind::GeU => lhs >= rhs,
        CmpKind::LtS => (lhs as i64) < rhs as i64,
        CmpKind::LeS => (lhs as i64) <= rhs as i64,
        CmpKind::GtS => (lhs as i64) > rhs as i64,
        CmpKind::GeS => (lhs as i64) >= rhs as i64,
    })
}

pub(super) fn cluster_clean_guards(func: &MFunction, order: Vec<usize>) -> Vec<usize> {
    let Some(flags) = &func.settle_flags else {
        return order;
    };
    let is_flag = |base: BaseReg, offset: i32, size: OpSize| {
        base == BaseReg::SimState
            && offset >= flags.start
            && i64::from(offset) + i64::from(size.bytes()) <= i64::from(flags.end)
    };
    let mut facts: Vec<CleanValue> = vec![None; func.vregs.count() as usize];
    for block in &func.blocks {
        for inst in &block.insts {
            let Some(dst) = inst.def() else {
                continue;
            };
            let fact = (|| {
                let get = |value: VReg| facts[value.0 as usize];
                Some(match *inst {
                    MInst::LoadImm { value, .. } => (value, false),
                    MInst::Load {
                        base, offset, size, ..
                    } if is_flag(base, offset, size) => (0, true),
                    MInst::Mov { src, .. } => get(src)?,
                    MInst::Mov32 { src, .. } => {
                        let (v, d) = get(src)?;
                        (u64::from(v as u32), d)
                    }
                    MInst::Or { lhs, rhs, .. }
                    | MInst::Or32 { lhs, rhs, .. }
                    | MInst::And { lhs, rhs, .. }
                    | MInst::And32 { lhs, rhs, .. }
                    | MInst::Xor { lhs, rhs, .. }
                    | MInst::Xor32 { lhs, rhs, .. } => {
                        let (a, ad) = get(lhs)?;
                        let (b, bd) = get(rhs)?;
                        let value = match inst {
                            MInst::Or { .. } | MInst::Or32 { .. } => a | b,
                            MInst::And { .. } | MInst::And32 { .. } => a & b,
                            _ => a ^ b,
                        };
                        let value = if matches!(
                            inst,
                            MInst::Or32 { .. } | MInst::And32 { .. } | MInst::Xor32 { .. }
                        ) {
                            u64::from(value as u32)
                        } else {
                            value
                        };
                        (value, ad || bd)
                    }
                    MInst::AndImm { src, imm, .. } => {
                        let (v, d) = get(src)?;
                        (v & imm, d)
                    }
                    MInst::AndImm32 { src, imm, .. } => {
                        let (v, d) = get(src)?;
                        (v & u64::from(imm), d)
                    }
                    MInst::Popcnt { src, .. } => {
                        let (v, d) = get(src)?;
                        (u64::from(v.count_ones()), d)
                    }
                    MInst::Cmp { lhs, rhs, kind, .. } => {
                        let (a, ad) = get(lhs)?;
                        let (b, bd) = get(rhs)?;
                        (compare(kind, a, b), ad || bd)
                    }
                    MInst::CmpImm { lhs, imm, kind, .. } => {
                        let (a, ad) = get(lhs)?;
                        (compare(kind, a, imm as i64 as u64), ad)
                    }
                    MInst::Select {
                        cond,
                        true_val,
                        false_val,
                        ..
                    } => {
                        let (c, cd) = get(cond)?;
                        let (v, d) = get(if c != 0 { true_val } else { false_val })?;
                        (v, cd || d)
                    }
                    _ => return None,
                })
            })();
            facts[dst.0 as usize] = fact;
        }
    }
    let indices = func
        .blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.id, i))
        .collect::<HashMap<_, _>>();
    let mut preferred = vec![None; func.blocks.len()];
    for (index, block) in func.blocks.iter().enumerate() {
        let fact = (|| {
            Some(match block.terminator()? {
                MInst::Branch { cond, .. } => facts[cond.0 as usize]?,
                MInst::BranchPred { predicate, .. } => match *predicate {
                    BranchPredicate::MemoryNonZero { base, offset, size }
                        if is_flag(base, offset, size) =>
                    {
                        (0, true)
                    }
                    BranchPredicate::CompareImm { lhs, imm, kind } => {
                        let (v, d) = facts[lhs.0 as usize]?;
                        (compare(kind, v, imm as i64 as u64), d)
                    }
                    BranchPredicate::Compare { lhs, rhs, kind } => {
                        let (a, ad) = facts[lhs.0 as usize]?;
                        let (b, bd) = facts[rhs.0 as usize]?;
                        (compare(kind, a, b), ad || bd)
                    }
                    _ => return None,
                },
                _ => return None,
            })
        })();
        if let Some((value, true)) = fact {
            let (yes, no) = block.terminator().unwrap().branch_targets().unwrap();
            preferred[index] = indices.get(&if value != 0 { yes } else { no }).copied();
        }
    }
    // Publication diamonds are also usually clean. Their taken arm only
    // writes notification flags and rejoins the unchanged continuation.
    // This changes placement alone; every branch and store is retained.
    for (index, block) in func.blocks.iter().enumerate() {
        if preferred[index].is_some() {
            continue;
        }
        let Some((yes, no)) = block.terminator().and_then(MInst::branch_targets) else {
            continue;
        };
        let publish = &func.blocks[indices[&yes]];
        if !matches!(publish.terminator(), Some(MInst::Jump { target }) if *target == no) {
            continue;
        }
        let mut stores = false;
        let only_flags = publish.insts.iter().all(|inst| match *inst {
            MInst::Store {
                base, offset, size, ..
            }
            | MInst::OrStoreImm {
                base, offset, size, ..
            } => {
                stores = true;
                is_flag(base, offset, size)
            }
            MInst::Load {
                base, offset, size, ..
            } => is_flag(base, offset, size),
            MInst::LoadImm { .. }
            | MInst::Mov { .. }
            | MInst::Mov32 { .. }
            | MInst::Or { .. }
            | MInst::Or32 { .. }
            | MInst::OrImm { .. }
            | MInst::AndImm { .. }
            | MInst::AndImm32 { .. }
            | MInst::Jump { .. } => true,
            _ => false,
        });
        if only_flags && stores {
            preferred[index] = indices.get(&no).copied();
        }
    }
    let mut placed = vec![false; func.blocks.len()];
    let mut result = Vec::with_capacity(order.len());
    for first in order {
        let mut current = first;
        let mut following = false;
        while !placed[current] {
            placed[current] = true;
            result.push(current);
            if let Some(next) = preferred[current] {
                current = next;
                following = true;
            } else if following
                && let Some(MInst::Jump { target }) = func.blocks[current].terminator()
            {
                current = indices[target];
            } else {
                break;
            }
        }
    }
    result
}
