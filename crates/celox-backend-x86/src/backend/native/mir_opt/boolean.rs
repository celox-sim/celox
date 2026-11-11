//! Share the zero test across a conjunction of zero comparisons.

use super::*;

/// Shorten private chains without duplicating any shared subexpression. Fold
/// complemented leaves with De Morgan's law so an ANDN chain becomes an OR
/// tree followed by one ANDN, instead of losing the target's NOT/AND fusion.
pub(super) fn balance_private_bitwise_trees(func: &mut MFunction) {
    #[derive(Clone, Copy, PartialEq, Eq)]
    struct Operation {
        and: bool,
        word32: bool,
    }
    #[derive(Clone, Copy)]
    enum Definition {
        Binary(Operation, VReg, VReg),
        Not(VReg),
    }
    #[derive(Clone, Copy)]
    enum User {
        Unused,
        One(Option<VReg>),
        Multiple,
    }
    impl User {
        fn record(&mut self, user: Option<VReg>) {
            *self = match *self {
                Self::Unused => Self::One(user),
                Self::One(_) | Self::Multiple => Self::Multiple,
            };
        }
    }
    fn operands(inst: &MInst) -> Option<(Operation, VReg, VReg)> {
        let (and, word32, lhs, rhs) = match *inst {
            MInst::And { lhs, rhs, .. } => (true, false, lhs, rhs),
            MInst::And32 { lhs, rhs, .. } => (true, true, lhs, rhs),
            MInst::Or { lhs, rhs, .. } => (false, false, lhs, rhs),
            MInst::Or32 { lhs, rhs, .. } => (false, true, lhs, rhs),
            _ => return None,
        };
        Some((Operation { and, word32 }, lhs, rhs))
    }
    fn instruction(op: Operation, dst: VReg, lhs: VReg, rhs: VReg) -> MInst {
        match (op.and, op.word32) {
            (true, false) => MInst::And { dst, lhs, rhs },
            (true, true) => MInst::And32 { dst, lhs, rhs },
            (false, false) => MInst::Or { dst, lhs, rhs },
            (false, true) => MInst::Or32 { dst, lhs, rhs },
        }
    }
    fn tree(
        op: Operation,
        values: &[(VReg, usize)],
        vregs: &mut VRegAllocator,
        descs: &mut Vec<SpillDesc>,
        scheduled: &mut Vec<(usize, MInst)>,
    ) -> (VReg, usize) {
        if values.len() == 1 {
            return values[0];
        }
        let middle = values.len() / 2;
        let (lhs, lhs_ready) = tree(op, &values[..middle], vregs, descs, scheduled);
        let (rhs, rhs_ready) = tree(op, &values[middle..], vregs, descs, scheduled);
        let dst = alloc_transient_vreg(vregs, descs);
        let ready = lhs_ready.max(rhs_ready);
        scheduled.push((ready, instruction(op, dst, lhs, rhs)));
        (dst, ready)
    }
    let mut definitions = vec![None; func.vregs.count() as usize];
    let mut available = vec![None; definitions.len()];
    let mut users = vec![User::Unused; definitions.len()];
    for (bi, block) in func.blocks.iter().enumerate() {
        for phi in &block.phis {
            for &(_, source) in &phi.sources {
                users[source.0 as usize].record(None);
            }
        }
        for (index, inst) in block.insts.iter().enumerate() {
            if let Some(dst) = inst.def() {
                available[dst.0 as usize] = Some((bi, index + 1));
                // Only these facts participate in tree matching. Retaining
                // every full instruction (and every user list) is unnecessary.
                let definition = if let Some((op, lhs, rhs)) = operands(inst) {
                    Some(Definition::Binary(op, lhs, rhs))
                } else if let MInst::BitNot { src, .. } = inst {
                    Some(Definition::Not(*src))
                } else {
                    None
                };
                definitions[dst.0 as usize] = definition.map(|definition| (bi, definition));
            }
            for source in inst.uses() {
                users[source.0 as usize].record(inst.def());
            }
        }
    }
    for (bi, block) in func.blocks.iter_mut().enumerate() {
        let original = std::mem::take(&mut block.insts);
        let mut scheduled = Vec::new();
        let ready = |value: VReg| {
            let position = available[value.0 as usize]
                .filter(|&(owner, _)| owner == bi)
                .map_or(0, |(_, position)| position);
            (value, position)
        };
        for inst in original {
            let Some((op, lhs, rhs)) = operands(&inst) else {
                block.push(inst);
                continue;
            };
            let dst = inst.def().unwrap();
            if let User::One(Some(user)) = users[dst.0 as usize]
                && let Some((owner, parent)) = &definitions[user.0 as usize]
                && *owner == bi
                && matches!(parent, Definition::Binary(parent_op, _, _) if *parent_op == op)
            {
                block.push(inst);
                continue;
            }
            let mut stack = vec![(rhs, 1usize), (lhs, 1usize)];
            let mut positive = Vec::new();
            let mut negative = Vec::new();
            let mut depth = 0;
            while let Some((value, level)) = stack.pop() {
                depth = depth.max(level);
                if matches!(users[value.0 as usize], User::One(_))
                    && let Some((owner, definition)) = &definitions[value.0 as usize]
                    && *owner == bi
                {
                    if let Definition::Binary(child_op, a, b) = *definition
                        && child_op == op
                    {
                        stack.extend([(b, level + 1), (a, level + 1)]);
                        continue;
                    }
                    if let Definition::Not(src) = *definition {
                        negative.push(ready(src));
                        continue;
                    }
                }
                positive.push(ready(value));
            }
            let count = positive.len() + negative.len();
            if count < 6
                || (negative.len() < 2 && depth <= count.next_power_of_two().ilog2() as usize)
            {
                block.push(inst);
                continue;
            }
            let vregs = &mut func.vregs;
            let descs = &mut func.spill_descs;
            let positive =
                (!positive.is_empty()).then(|| tree(op, &positive, vregs, descs, &mut scheduled));
            let result = if negative.is_empty() {
                positive.unwrap().0
            } else {
                let (negative, position) = tree(
                    Operation { and: !op.and, ..op },
                    &negative,
                    vregs,
                    descs,
                    &mut scheduled,
                );
                let inverted = alloc_transient_vreg(vregs, descs);
                scheduled.push((
                    position,
                    MInst::BitNot {
                        dst: inverted,
                        src: negative,
                    },
                ));
                if let Some((positive, _)) = positive {
                    block.push(instruction(op, dst, inverted, positive));
                    continue;
                }
                inverted
            };
            block.push(if op.word32 {
                MInst::Mov32 { dst, src: result }
            } else {
                MInst::Mov { dst, src: result }
            });
        }
        if !scheduled.is_empty() {
            // Rebuilding at the root would keep every leaf live until then.
            // Reduce each subtree as soon as its captured SSA operands exist;
            // only pure arithmetic moves, never loads or observable effects.
            // Stable order also keeps same-position children before parents.
            scheduled.sort_by_key(|&(position, _)| position);
            let mut scheduled = scheduled.into_iter().peekable();
            let rewritten = std::mem::take(&mut block.insts);
            for (position, inst) in rewritten.into_iter().enumerate() {
                while scheduled.peek().is_some_and(|&(at, _)| at == position) {
                    block.push(scheduled.next().unwrap().1);
                }
                block.push(inst);
            }
            debug_assert!(scheduled.next().is_none());
        }
    }
}

/// With normalized booleans, `other & (condition == 0)` is an ANDN. Keep
/// comparisons which have other users, or whose inputs are wider truth values.
pub(super) fn fold_inverted_conjunctions(func: &mut MFunction) {
    if !func.target_features.bmi1() {
        return;
    }
    let zeros = known_bits::known_zeros(func);
    let mut definitions = vec![None; func.vregs.count() as usize];
    let mut uses = vec![0usize; definitions.len()];
    for block in &func.blocks {
        for phi in &block.phis {
            for &(_, source) in &phi.sources {
                uses[source.0 as usize] += 1;
            }
        }
        for inst in &block.insts {
            if let Some(dst) = inst.def() {
                definitions[dst.0 as usize] = Some(inst.clone());
            }
            for source in inst.uses() {
                uses[source.0 as usize] += 1;
            }
        }
    }
    let is_boolean = |value: VReg| zeros[value.0 as usize] & !1 == !1;
    for block in &mut func.blocks {
        let original = std::mem::take(&mut block.insts);
        for mut inst in original {
            if let MInst::And { dst, lhs, rhs } | MInst::And32 { dst, lhs, rhs } = inst {
                for (comparison, other) in [(lhs, rhs), (rhs, lhs)] {
                    if uses[comparison.0 as usize] != 1 || !is_boolean(other) {
                        continue;
                    }
                    if let Some(MInst::CmpImm {
                        lhs: condition,
                        imm: 0,
                        kind: CmpKind::Eq,
                        ..
                    }) = definitions[comparison.0 as usize]
                        && is_boolean(condition)
                    {
                        let inverted = func.vregs.alloc();
                        func.spill_descs.push(SpillDesc::transient());
                        block.push(MInst::BitNot {
                            dst: inverted,
                            src: condition,
                        });
                        inst = MInst::And32 {
                            dst,
                            lhs: inverted,
                            rhs: other,
                        };
                        break;
                    }
                }
            }
            block.push(inst);
        }
    }
}

/// Recover a scalar choice from `b ^ ((a ^ b) & -condition)`. The mask may
/// truncate a packed field only when the arms cannot differ outside that field.
pub(super) fn fold_masked_selects(func: &mut MFunction) {
    let zeros = known_bits::known_zeros(func);
    let mut definitions = vec![None; func.vregs.count() as usize];
    for block in &func.blocks {
        for inst in &block.insts {
            if let Some(dst) = inst.def() {
                definitions[dst.0 as usize] = Some(inst.clone());
            }
        }
    }
    let definition = |value: VReg| definitions[value.0 as usize].as_ref();
    let constant = |value: VReg| match definition(value) {
        Some(MInst::LoadImm { value, .. }) => Some(*value),
        _ => None,
    };
    for block in &mut func.blocks {
        let original = std::mem::take(&mut block.insts);
        for inst in original {
            let (dst, lhs, rhs, word32) = match inst {
                MInst::Xor { dst, lhs, rhs } => (dst, lhs, rhs, false),
                MInst::Xor32 { dst, lhs, rhs } => (dst, lhs, rhs, true),
                _ => {
                    block.push(inst);
                    continue;
                }
            };
            let mut replacement = None;
            for (false_val, masked) in [(lhs, rhs), (rhs, lhs)] {
                let (first, second, and_mask) = match definition(masked) {
                    Some(MInst::And { lhs, rhs, .. }) => (*lhs, *rhs, u64::MAX),
                    Some(MInst::And32 { lhs, rhs, .. }) => (*lhs, *rhs, u64::from(u32::MAX)),
                    _ => continue,
                };
                for (difference, mut mask_value) in [(first, second), (second, first)] {
                    let (a, b, xor_mask) = match definition(difference) {
                        Some(MInst::Xor { lhs, rhs, .. }) => (*lhs, *rhs, u64::MAX),
                        Some(MInst::Xor32 { lhs, rhs, .. }) => (*lhs, *rhs, u64::from(u32::MAX)),
                        _ => continue,
                    };
                    let true_val = if a == false_val {
                        b
                    } else if b == false_val {
                        a
                    } else {
                        continue;
                    };
                    let mut mask = and_mask & xor_mask;
                    let condition = loop {
                        match definition(mask_value) {
                            Some(MInst::AndImm { src, imm, .. }) => {
                                mask &= imm;
                                mask_value = *src;
                            }
                            Some(MInst::AndImm32 { src, imm, .. }) => {
                                mask &= u64::from(*imm);
                                mask_value = *src;
                            }
                            Some(MInst::And { lhs, rhs, .. } | MInst::And32 { lhs, rhs, .. }) => {
                                if matches!(definition(mask_value), Some(MInst::And32 { .. })) {
                                    mask &= u64::from(u32::MAX);
                                }
                                if let Some(value) = constant(*lhs) {
                                    mask &= value;
                                    mask_value = *rhs;
                                } else if let Some(value) = constant(*rhs) {
                                    mask &= value;
                                    mask_value = *lhs;
                                } else {
                                    break None;
                                }
                            }
                            Some(MInst::Neg { src, .. }) => break Some(*src),
                            Some(MInst::Sub { lhs, rhs, .. } | MInst::Sub32 { lhs, rhs, .. })
                                if constant(*lhs) == Some(0) =>
                            {
                                if matches!(definition(mask_value), Some(MInst::Sub32 { .. })) {
                                    mask &= u64::from(u32::MAX);
                                }
                                break Some(*rhs);
                            }
                            _ => break None,
                        }
                    };
                    let Some(cond) = condition else {
                        continue;
                    };
                    let demanded = if word32 {
                        u64::from(u32::MAX)
                    } else {
                        u64::MAX
                    };
                    if zeros[cond.0 as usize] & !1 == !1
                        && demanded & !mask & !(zeros[a.0 as usize] & zeros[b.0 as usize]) == 0
                    {
                        replacement = Some((cond, true_val, false_val));
                        break;
                    }
                }
                if replacement.is_some() {
                    break;
                }
            }
            if let Some((cond, true_val, false_val)) = replacement {
                let result = if word32 {
                    let value = func.vregs.alloc();
                    func.spill_descs.push(SpillDesc::transient());
                    value
                } else {
                    dst
                };
                block.push(MInst::Select {
                    dst: result,
                    cond,
                    true_val,
                    false_val,
                });
                if word32 {
                    block.push(MInst::Mov32 { dst, src: result });
                }
            } else {
                block.push(inst);
            }
        }
    }
}

fn tested_bits(mut value: VReg, definitions: &[Option<MInst>]) -> (VReg, u64) {
    let mut mask = u64::MAX;
    loop {
        match &definitions[value.0 as usize] {
            Some(MInst::AndImm { src, imm, .. }) => {
                mask &= imm;
                value = *src;
            }
            Some(MInst::AndImm32 { src, imm, .. }) => {
                mask &= u64::from(*imm);
                value = *src;
            }
            Some(MInst::And { lhs, rhs, .. } | MInst::And32 { lhs, rhs, .. }) => {
                let pair =
                    [(*lhs, *rhs), (*rhs, *lhs)]
                        .into_iter()
                        .find_map(|(source, constant)| {
                            if let Some(MInst::LoadImm { value, .. }) =
                                definitions[constant.0 as usize]
                            {
                                Some((source, value))
                            } else {
                                None
                            }
                        });
                let Some((source, constant)) = pair else {
                    return (value, mask);
                };
                if matches!(definitions[value.0 as usize], Some(MInst::And32 { .. })) {
                    mask &= u64::from(u32::MAX);
                }
                mask &= constant;
                value = source;
            }
            Some(MInst::ShrImm { src, imm, .. }) => {
                mask = mask.checked_shl(u32::from(*imm)).unwrap_or(0);
                value = *src;
            }
            Some(MInst::ShlImm { src, imm, .. }) => {
                mask = mask.checked_shr(u32::from(*imm)).unwrap_or(0);
                value = *src;
            }
            Some(MInst::Mov { src, .. }) => value = *src,
            Some(MInst::Mov32 { src, .. }) => {
                mask &= u64::from(u32::MAX);
                value = *src;
            }
            _ => return (value, mask),
        }
    }
}

pub(super) fn fold_zero_conjunctions(func: &mut MFunction) {
    let mut definitions = vec![None; func.vregs.count() as usize];
    let mut users = vec![Vec::new(); func.vregs.count() as usize];
    for block in &func.blocks {
        for phi in &block.phis {
            for &(_, value) in &phi.sources {
                users[value.0 as usize].push(None);
            }
        }
        for inst in &block.insts {
            if let Some(dst) = inst.def() {
                definitions[dst.0 as usize] = Some(inst.clone());
            }
            for value in inst.uses() {
                users[value.0 as usize].push(inst.def());
            }
        }
    }
    let and_operands = |value: VReg| match &definitions[value.0 as usize] {
        Some(MInst::And { lhs, rhs, .. } | MInst::And32 { lhs, rhs, .. }) => Some((*lhs, *rhs)),
        _ => None,
    };
    for block in &mut func.blocks {
        let original = std::mem::take(&mut block.insts);
        for inst in original {
            let Some(dst) = inst.def() else {
                block.push(inst);
                continue;
            };
            let Some((lhs, rhs)) = and_operands(dst) else {
                block.push(inst);
                continue;
            };
            // Visit only roots of private AND trees. Shared subtrees remain
            // leaves, bounding the total walk by the original instruction count.
            if let [Some(user)] = users[dst.0 as usize].as_slice()
                && and_operands(*user).is_some()
            {
                block.push(inst);
                continue;
            }
            let mut stack = vec![rhs, lhs];
            let mut tested = Vec::new();
            let mut other = Vec::new();
            while let Some(value) = stack.pop() {
                if users[value.0 as usize].len() == 1 {
                    if let Some((lhs, rhs)) = and_operands(value) {
                        stack.extend([rhs, lhs]);
                        continue;
                    }
                    if let Some(MInst::CmpImm {
                        lhs,
                        imm: 0,
                        kind: CmpKind::Eq,
                        ..
                    }) = definitions[value.0 as usize]
                    {
                        tested.push(lhs);
                        continue;
                    }
                }
                other.push(value);
            }
            if tested.len() < 2 {
                block.push(inst);
                continue;
            }
            let mut groups = Vec::<(VReg, u64)>::new();
            let mut group_indices = HashMap::<VReg, usize>::default();
            for value in tested {
                let (source, mask) = tested_bits(value, &definitions);
                if let Some(&index) = group_indices.get(&source) {
                    groups[index].1 |= mask;
                } else {
                    group_indices.insert(source, groups.len());
                    groups.push((source, mask));
                }
            }
            let mut tested = Vec::new();
            for (source, mask) in groups {
                let value = if mask == u64::MAX {
                    source
                } else {
                    let value = func.vregs.alloc();
                    func.spill_descs.push(SpillDesc::transient());
                    if i32::try_from(mask as i64).is_ok() {
                        block.push(MInst::AndImm {
                            dst: value,
                            src: source,
                            imm: mask,
                        });
                    } else if let Ok(mask) = u32::try_from(mask) {
                        block.push(MInst::AndImm32 {
                            dst: value,
                            src: source,
                            imm: mask,
                        });
                    } else {
                        let constant = func.vregs.alloc();
                        func.spill_descs.push(SpillDesc::remat(mask));
                        block.push(MInst::LoadImm {
                            dst: constant,
                            value: mask,
                        });
                        block.push(MInst::And {
                            dst: value,
                            lhs: source,
                            rhs: constant,
                        });
                    }
                    value
                };
                tested.push(value);
            }
            let mut combined = tested[0];
            for value in tested.into_iter().skip(1) {
                let next = func.vregs.alloc();
                func.spill_descs.push(SpillDesc::transient());
                block.push(MInst::Or {
                    dst: next,
                    lhs: combined,
                    rhs: value,
                });
                combined = next;
            }
            let comparison = if other.is_empty() {
                dst
            } else {
                let value = func.vregs.alloc();
                func.spill_descs.push(SpillDesc::transient());
                value
            };
            block.push(MInst::CmpImm {
                dst: comparison,
                lhs: combined,
                imm: 0,
                kind: CmpKind::Eq,
            });
            let mut combined = comparison;
            let count = other.len();
            for (position, value) in other.into_iter().enumerate() {
                let next = if position + 1 == count {
                    dst
                } else {
                    let value = func.vregs.alloc();
                    func.spill_descs.push(SpillDesc::transient());
                    value
                };
                // One conjunct is a normalized comparison, so only bit zero
                // of every other conjunct can influence the result.
                block.push(MInst::And32 {
                    dst: next,
                    lhs: combined,
                    rhs: value,
                });
                combined = next;
            }
        }
    }
}

#[cfg(test)]
mod tests;
