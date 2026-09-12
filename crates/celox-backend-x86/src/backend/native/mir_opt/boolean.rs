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
        block: &mut MBlock,
        op: Operation,
        values: &[VReg],
        vregs: &mut VRegAllocator,
        descs: &mut Vec<SpillDesc>,
    ) -> VReg {
        if values.len() == 1 {
            return values[0];
        }
        let middle = values.len() / 2;
        let lhs = tree(block, op, &values[..middle], vregs, descs);
        let rhs = tree(block, op, &values[middle..], vregs, descs);
        let dst = alloc_transient_vreg(vregs, descs);
        block.push(instruction(op, dst, lhs, rhs));
        dst
    }
    let mut definitions = vec![None; func.vregs.count() as usize];
    let mut users = vec![User::Unused; definitions.len()];
    for (bi, block) in func.blocks.iter().enumerate() {
        for phi in &block.phis {
            for &(_, source) in &phi.sources {
                users[source.0 as usize].record(None);
            }
        }
        for inst in &block.insts {
            if let Some(dst) = inst.def() {
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
                        negative.push(src);
                        continue;
                    }
                }
                positive.push(value);
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
            let positive = (!positive.is_empty()).then(|| tree(block, op, &positive, vregs, descs));
            let result = if negative.is_empty() {
                positive.unwrap()
            } else {
                let negative = tree(
                    block,
                    Operation { and: !op.and, ..op },
                    &negative,
                    vregs,
                    descs,
                );
                let inverted = alloc_transient_vreg(vregs, descs);
                block.push(MInst::BitNot {
                    dst: inverted,
                    src: negative,
                });
                if let Some(positive) = positive {
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
mod tests {
    use super::*;
    use crate::native::{emit, jit_mem::JitCode, mir_legalize, regalloc};

    fn conjunction(shared: bool) -> MFunction {
        let mut vregs = VRegAllocator::new();
        for _ in 0..12 {
            vregs.alloc();
        }
        let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 12]);
        let mut block = MBlock::new(BlockId(0));
        for index in 0..4 {
            block.push(MInst::Load {
                dst: VReg(index),
                base: BaseReg::SimState,
                offset: index as i32 * 8,
                size: OpSize::S64,
            });
        }
        for index in 0..3 {
            block.push(MInst::CmpImm {
                dst: VReg(4 + index),
                lhs: VReg(index),
                imm: 0,
                kind: CmpKind::Eq,
            });
        }
        block.push(MInst::And {
            dst: VReg(7),
            lhs: VReg(4),
            rhs: VReg(3),
        });
        block.push(MInst::And32 {
            dst: VReg(8),
            lhs: VReg(7),
            rhs: VReg(5),
        });
        block.push(MInst::And {
            dst: VReg(9),
            lhs: VReg(8),
            rhs: VReg(6),
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 32,
            src: VReg(9),
            size: OpSize::S64,
        });
        if shared {
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset: 40,
                src: VReg(5),
                size: OpSize::S64,
            });
        }
        block.push(MInst::Return);
        function.push_block(block);
        function
    }

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
    fn balanced_bitwise_trees_preserve_width_complements_and_shared_values() {
        for count in [5usize, 6, 7, 12, 33] {
            for is_and in [false, true] {
                for word32 in [false, true] {
                    for complement in 0..3 {
                        for shared in [false, true] {
                            let mut vregs = VRegAllocator::new();
                            for _ in 0..count * 3 {
                                vregs.alloc();
                            }
                            let mut function =
                                MFunction::new(vregs, vec![SpillDesc::transient(); count * 3]);
                            let mut block = MBlock::new(BlockId(0));
                            let negated = |i: usize| {
                                complement == 2 || (complement == 1 && !i.is_multiple_of(3))
                            };
                            let mut leaves = Vec::new();
                            for index in 0..count {
                                let input = VReg(index as u32);
                                block.push(MInst::Load {
                                    dst: input,
                                    base: BaseReg::SimState,
                                    offset: index as i32 * 8,
                                    size: OpSize::S64,
                                });
                                leaves.push(if negated(index) {
                                    let dst = VReg((count + index) as u32);
                                    block.push(MInst::BitNot { dst, src: input });
                                    dst
                                } else {
                                    input
                                });
                            }
                            let mut result = leaves[0];
                            for (index, &rhs) in leaves.iter().enumerate().skip(1) {
                                let dst = VReg((count * 2 + index) as u32);
                                let lhs = result;
                                block.push(match (is_and, word32) {
                                    (false, false) => MInst::Or { dst, lhs, rhs },
                                    (false, true) => MInst::Or32 { dst, lhs, rhs },
                                    (true, false) => MInst::And { dst, lhs, rhs },
                                    (true, true) => MInst::And32 { dst, lhs, rhs },
                                });
                                if shared && index == 2 {
                                    block.push(MInst::Store {
                                        base: BaseReg::SimState,
                                        offset: (count * 8 + 8) as i32,
                                        src: dst,
                                        size: OpSize::S64,
                                    });
                                }
                                result = dst;
                            }
                            block.push(MInst::Store {
                                base: BaseReg::SimState,
                                offset: count as i32 * 8,
                                src: result,
                                size: OpSize::S64,
                            });
                            block.push(MInst::Return);
                            function.push_block(block);
                            let mut optimized = function.clone();
                            balance_private_bitwise_trees(&mut optimized);
                            copy_propagate(&mut optimized);
                            dead_code_eliminate(&mut optimized);
                            optimized.verify();
                            let (before, before_size) = compile(function);
                            let (after, after_size) = compile(optimized);
                            for sample in 0..128 {
                                let mut left = vec![0xa5u8; before_size.max(after_size)];
                                let mut expected = if is_and { u64::MAX } else { 0 };
                                for lane in 0..count {
                                    let value = if lane == sample % count {
                                        if sample < 64 {
                                            1u64 << sample
                                        } else {
                                            !(1u64 << (sample - 64))
                                        }
                                    } else if is_and {
                                        u64::MAX
                                    } else {
                                        0
                                    };
                                    expected = if is_and {
                                        expected & value
                                    } else {
                                        expected | value
                                    };
                                    let input = if negated(lane) { !value } else { value };
                                    left[lane * 8..lane * 8 + 8]
                                        .copy_from_slice(&input.to_le_bytes());
                                }
                                if word32 {
                                    expected &= u64::from(u32::MAX);
                                }
                                let mut right = left.clone();
                                assert_eq!(unsafe { before.call(&mut left) }, 0);
                                assert_eq!(unsafe { after.call(&mut right) }, 0);
                                assert_eq!(
                                    &left[..count * 8 + 16],
                                    &right[..count * 8 + 16],
                                    "count={count} and={is_and} word32={word32} complement={complement} shared={shared} sample={sample}"
                                );
                                assert_eq!(
                                    u64::from_le_bytes(
                                        right[count * 8..count * 8 + 8].try_into().unwrap()
                                    ),
                                    expected
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn zero_conjunctions_preserve_arbitrary_bits_and_shared_comparisons() {
        for shared in [false, true] {
            let original = conjunction(shared);
            let mut optimized = original.clone();
            fold_zero_conjunctions(&mut optimized);
            dead_code_eliminate(&mut optimized);
            optimized.verify();
            let compares = optimized
                .blocks
                .iter()
                .flat_map(|block| &block.insts)
                .filter(|inst| matches!(inst, MInst::CmpImm { .. }))
                .count();
            assert_eq!(compares, if shared { 2 } else { 1 });
            let (before, before_size) = compile(original);
            let (after, after_size) = compile(optimized);
            let values = [0u64, 1, 2, 3, 1 << 32, 1 << 63, u64::MAX];
            for a in values {
                for b in values {
                    for c in values {
                        for guard in values {
                            let mut left = vec![0u8; before_size.max(after_size)];
                            for (index, value) in [a, b, c, guard].into_iter().enumerate() {
                                left[index * 8..index * 8 + 8]
                                    .copy_from_slice(&value.to_le_bytes());
                            }
                            let mut right = left.clone();
                            assert_eq!(unsafe { before.call(&mut left) }, 0);
                            assert_eq!(unsafe { after.call(&mut right) }, 0);
                            assert_eq!(
                                &left[32..48],
                                &right[32..48],
                                "{a:#x}, {b:#x}, {c:#x}, {guard:#x}"
                            );
                            assert_eq!(
                                u64::from_le_bytes(right[32..40].try_into().unwrap()),
                                u64::from(a == 0 && b == 0 && c == 0) & guard
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn masked_selects_preserve_condition_truth_and_truncated_masks() {
        for word32 in [false, true] {
            for normalized in [false, true] {
                for mask in [0x3ff, 0x3fff_ffff_ffff_ffff, u64::MAX] {
                    for bounded in [false, true] {
                        let mut vregs = VRegAllocator::new();
                        for _ in 0..14 {
                            vregs.alloc();
                        }
                        let mut original = MFunction::new(vregs, vec![SpillDesc::transient(); 14]);
                        let mut block = MBlock::new(BlockId(0));
                        for index in 0..3 {
                            block.push(MInst::Load {
                                dst: VReg(index),
                                base: BaseReg::SimState,
                                offset: index as i32 * 8,
                                size: OpSize::S64,
                            });
                        }
                        block.push(MInst::LoadImm {
                            dst: VReg(3),
                            value: 0,
                        });
                        block.push(MInst::LoadImm {
                            dst: VReg(4),
                            value: mask,
                        });
                        block.push(if normalized {
                            MInst::AndImm {
                                dst: VReg(5),
                                src: VReg(0),
                                imm: 1,
                            }
                        } else {
                            MInst::Mov {
                                dst: VReg(5),
                                src: VReg(0),
                            }
                        });
                        for (dst, src) in [(VReg(6), VReg(1)), (VReg(7), VReg(2))] {
                            block.push(if bounded {
                                MInst::And {
                                    dst,
                                    lhs: src,
                                    rhs: VReg(4),
                                }
                            } else {
                                MInst::Mov { dst, src }
                            });
                        }
                        block.push(MInst::Sub {
                            dst: VReg(8),
                            lhs: VReg(3),
                            rhs: VReg(5),
                        });
                        block.push(MInst::And {
                            dst: VReg(9),
                            lhs: VReg(8),
                            rhs: VReg(4),
                        });
                        block.push(MInst::Xor {
                            dst: VReg(10),
                            lhs: VReg(6),
                            rhs: VReg(7),
                        });
                        block.push(MInst::And {
                            dst: VReg(11),
                            lhs: VReg(10),
                            rhs: VReg(9),
                        });
                        block.push(if word32 {
                            MInst::Xor32 {
                                dst: VReg(12),
                                lhs: VReg(7),
                                rhs: VReg(11),
                            }
                        } else {
                            MInst::Xor {
                                dst: VReg(12),
                                lhs: VReg(7),
                                rhs: VReg(11),
                            }
                        });
                        block.push(MInst::Store {
                            base: BaseReg::SimState,
                            offset: 32,
                            src: VReg(12),
                            size: OpSize::S64,
                        });
                        // The mask is also observed: folding must preserve shared producers.
                        block.push(MInst::Store {
                            base: BaseReg::SimState,
                            offset: 40,
                            src: VReg(9),
                            size: OpSize::S64,
                        });
                        block.push(MInst::Return);
                        original.push_block(block);
                        let mut optimized = original.clone();
                        fold_masked_selects(&mut optimized);
                        dead_code_eliminate(&mut optimized);
                        optimized.verify();
                        let expected_fold = normalized
                            && (bounded
                                || mask == u64::MAX
                                || (word32 && mask & u64::from(u32::MAX) == u64::from(u32::MAX)));
                        assert_eq!(
                            optimized.blocks[0]
                                .insts
                                .iter()
                                .any(|inst| matches!(inst, MInst::Select { .. })),
                            expected_fold
                        );
                        let (before, before_size) = compile(original);
                        let (after, after_size) = compile(optimized);
                        let values = [0u64, 1, 2, 3, 1 << 31, 1 << 32, 1 << 63, u64::MAX];
                        for a in values {
                            for b in values {
                                for condition in values {
                                    let mut left = vec![0u8; before_size.max(after_size)];
                                    for (index, value) in [condition, a, b].into_iter().enumerate()
                                    {
                                        left[index * 8..index * 8 + 8]
                                            .copy_from_slice(&value.to_le_bytes());
                                    }
                                    let mut right = left.clone();
                                    assert_eq!(unsafe { before.call(&mut left) }, 0);
                                    assert_eq!(unsafe { after.call(&mut right) }, 0);
                                    assert_eq!(
                                        &left[32..48],
                                        &right[32..48],
                                        "word32={word32}, normalized={normalized}, mask={mask:#x}, bounded={bounded}, condition={condition:#x}, a={a:#x}, b={b:#x}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn zero_conjunctions_preserve_shifted_and_truncated_fields() {
        for (left_shift, right_shift, mask) in [
            (0, 0, u64::MAX),
            (3, 9, 0x1fff_ffff),
            (31, 32, 0x8000_0000_ffff_0000),
            (32, 1, 0xffff_ffff),
            (63, 63, u64::MAX),
        ] {
            let mut vregs = VRegAllocator::new();
            for _ in 0..15 {
                vregs.alloc();
            }
            let mut original = MFunction::new(vregs, vec![SpillDesc::transient(); 15]);
            let mut block = MBlock::new(BlockId(0));
            block.push(MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            });
            block.push(MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 8,
                size: OpSize::S64,
            });
            block.push(MInst::LoadImm {
                dst: VReg(2),
                value: mask,
            });
            block.push(MInst::ShlImm {
                dst: VReg(3),
                src: VReg(0),
                imm: left_shift,
            });
            block.push(MInst::And32 {
                dst: VReg(4),
                lhs: VReg(3),
                rhs: VReg(2),
            });
            block.push(MInst::ShrImm {
                dst: VReg(5),
                src: VReg(0),
                imm: right_shift,
            });
            block.push(MInst::And {
                dst: VReg(6),
                lhs: VReg(5),
                rhs: VReg(2),
            });
            block.push(MInst::Mov32 {
                dst: VReg(7),
                src: VReg(0),
            });
            block.push(MInst::AndImm32 {
                dst: VReg(8),
                src: VReg(7),
                imm: 0x8000_0000,
            });
            for (dst, lhs) in [(9, 4), (10, 6), (11, 8)] {
                block.push(MInst::CmpImm {
                    dst: VReg(dst),
                    lhs: VReg(lhs),
                    imm: 0,
                    kind: CmpKind::Eq,
                });
            }
            block.push(MInst::And {
                dst: VReg(12),
                lhs: VReg(9),
                rhs: VReg(10),
            });
            block.push(MInst::And32 {
                dst: VReg(13),
                lhs: VReg(12),
                rhs: VReg(11),
            });
            block.push(MInst::And {
                dst: VReg(14),
                lhs: VReg(13),
                rhs: VReg(1),
            });
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset: 32,
                src: VReg(14),
                size: OpSize::S64,
            });
            block.push(MInst::Return);
            original.push_block(block);
            let mut optimized = original.clone();
            fold_zero_conjunctions(&mut optimized);
            dead_code_eliminate(&mut optimized);
            optimized.verify();
            let (before, before_size) = compile(original);
            let (after, after_size) = compile(optimized);
            let inputs = [0u64, u64::MAX]
                .into_iter()
                .chain((0..64).flat_map(|bit| [1 << bit, !(1 << bit)]));
            for value in inputs {
                for guard in [0u64, 1, 2, u64::MAX] {
                    let mut state = vec![0u8; before_size.max(after_size)];
                    state[..8].copy_from_slice(&value.to_le_bytes());
                    state[8..16].copy_from_slice(&guard.to_le_bytes());
                    let mut actual = state.clone();
                    assert_eq!(unsafe { before.call(&mut state) }, 0);
                    assert_eq!(unsafe { after.call(&mut actual) }, 0);
                    assert_eq!(
                        &state[32..40],
                        &actual[32..40],
                        "left={left_shift}, right={right_shift}, mask={mask:#x}, value={value:#x}, guard={guard:#x}"
                    );
                    let expected = u64::from(
                        ((value << left_shift) as u32 & mask as u32) == 0
                            && ((value >> right_shift) & mask) == 0
                            && value & 0x8000_0000 == 0,
                    ) & guard;
                    assert_eq!(
                        u64::from_le_bytes(actual[32..40].try_into().unwrap()),
                        expected
                    );
                }
            }
        }
    }

    #[test]
    fn inverted_conjunctions_preserve_boolean_and_wide_truth_values() {
        use crate::native::features::X86Features;
        if !std::arch::is_x86_feature_detected!("bmi1") {
            return;
        }
        for bounded in [false, true] {
            for shared in [false, true] {
                for bmi1 in [false, true] {
                    let mut vregs = VRegAllocator::new();
                    for _ in 0..7 {
                        vregs.alloc();
                    }
                    let mut original = MFunction::new(vregs, vec![SpillDesc::transient(); 7]);
                    original.target_features = X86Features::for_test(false).with_bmi1(bmi1);
                    let mut block = MBlock::new(BlockId(0));
                    block.push(MInst::Load {
                        dst: VReg(0),
                        base: BaseReg::SimState,
                        offset: 0,
                        size: OpSize::S64,
                    });
                    block.push(MInst::Load {
                        dst: VReg(1),
                        base: BaseReg::SimState,
                        offset: 8,
                        size: OpSize::S64,
                    });
                    block.push(if bounded {
                        MInst::AndImm {
                            dst: VReg(2),
                            src: VReg(0),
                            imm: 1,
                        }
                    } else {
                        MInst::Mov {
                            dst: VReg(2),
                            src: VReg(0),
                        }
                    });
                    block.push(MInst::AndImm {
                        dst: VReg(3),
                        src: VReg(1),
                        imm: 1,
                    });
                    block.push(MInst::CmpImm {
                        dst: VReg(4),
                        lhs: VReg(2),
                        imm: 0,
                        kind: CmpKind::Eq,
                    });
                    block.push(MInst::And {
                        dst: VReg(5),
                        lhs: VReg(3),
                        rhs: VReg(4),
                    });
                    block.push(MInst::Store {
                        base: BaseReg::SimState,
                        offset: 32,
                        src: VReg(5),
                        size: OpSize::S64,
                    });
                    if shared {
                        block.push(MInst::Store {
                            base: BaseReg::SimState,
                            offset: 40,
                            src: VReg(4),
                            size: OpSize::S64,
                        });
                    }
                    block.push(MInst::Return);
                    original.push_block(block);
                    let mut optimized = original.clone();
                    fold_inverted_conjunctions(&mut optimized);
                    dead_code_eliminate(&mut optimized);
                    optimized.verify();
                    assert_eq!(
                        optimized.blocks[0]
                            .insts
                            .iter()
                            .any(|inst| matches!(inst, MInst::BitNot { .. })),
                        bounded && !shared && bmi1
                    );
                    let (before, before_size) = compile(original);
                    let (after, after_size) = compile(optimized);
                    for a in [0u64, 1, 2, 3, 1 << 32, 1 << 63, u64::MAX] {
                        for b in [0u64, 1, 2, 3, 1 << 63, u64::MAX] {
                            let mut left = vec![0u8; before_size.max(after_size)];
                            left[..8].copy_from_slice(&a.to_le_bytes());
                            left[8..16].copy_from_slice(&b.to_le_bytes());
                            let mut right = left.clone();
                            assert_eq!(unsafe { before.call(&mut left) }, 0);
                            assert_eq!(unsafe { after.call(&mut right) }, 0);
                            assert_eq!(
                                &left[32..48],
                                &right[32..48],
                                "bounded={bounded}, shared={shared}, bmi1={bmi1}, a={a:#x}, b={b:#x}"
                            );
                        }
                    }
                }
            }
        }
    }
}
