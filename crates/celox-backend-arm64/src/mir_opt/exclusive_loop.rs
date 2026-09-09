//! Dispatch mutually exclusive loop predicates before computing their operands.
//!
//! `(key == A && p) || (key == B && q)` needs only the predicate selected by
//! `key`. A result phi preserves every use of the original condition.

use super::*;
use crate::HashSet;
use crate::mir::{BaseReg, BlockId, MBlock, PhiNode, SpillDesc};

pub(super) fn movable(inst: &MInst) -> bool {
    matches!(
        inst,
        MInst::LoadImm { .. }
            | MInst::Mov { .. }
            | MInst::Mov32 { .. }
            | MInst::Load {
                base: BaseReg::SimState,
                ..
            }
            | MInst::LoadIndexed {
                base: BaseReg::SimState,
                ..
            }
            | MInst::Add { .. }
            | MInst::Add32 { .. }
            | MInst::AddImm { .. }
            | MInst::Sub { .. }
            | MInst::Sub32 { .. }
            | MInst::SubImm { .. }
            | MInst::Mul { .. }
            | MInst::Mul32 { .. }
            | MInst::And { .. }
            | MInst::And32 { .. }
            | MInst::AndImm { .. }
            | MInst::AndImm32 { .. }
            | MInst::Or { .. }
            | MInst::Or32 { .. }
            | MInst::OrImm { .. }
            | MInst::Xor { .. }
            | MInst::Xor32 { .. }
            | MInst::BitNot { .. }
            | MInst::Neg { .. }
            | MInst::Shl { .. }
            | MInst::ShlImm { .. }
            | MInst::Shr { .. }
            | MInst::ShrImm { .. }
            | MInst::Sar { .. }
            | MInst::SarImm { .. }
            | MInst::BitExtract { .. }
            | MInst::BitInsert { .. }
            | MInst::OrShifted { .. }
            | MInst::Cmp { .. }
            | MInst::CmpImm { .. }
            | MInst::Select { .. }
            | MInst::CmpSelect { .. }
            | MInst::CmpImmSelect { .. }
    )
}

fn private_operand(block: &MBlock, operand: VReg, uses: &[usize]) -> Vec<usize> {
    let mut needed = HashMap::<VReg, usize>::default();
    needed.insert(operand, 1);
    let mut selected = Vec::new();
    for (position, inst) in block.insts.iter().enumerate().rev() {
        let Some(dst) = inst.def() else { continue };
        if needed.get(&dst).copied() != Some(uses[dst.0 as usize]) || !movable(inst) {
            continue;
        }
        selected.push(position);
        for source in inst.uses() {
            *needed.entry(source).or_default() += 1;
        }
    }
    selected.reverse();
    selected
}

struct Case {
    comparison: MInst,
    predicate: VReg,
    instructions: Vec<usize>,
}

struct Plan {
    guard: Option<VReg>,
    cases: Vec<Case>,
    removed: HashSet<usize>,
}

fn and_operands(inst: &MInst) -> Option<(VReg, VReg)> {
    match *inst {
        MInst::And { lhs, rhs, .. } | MInst::And32 { lhs, rhs, .. } => Some((lhs, rhs)),
        _ => None,
    }
}

fn or_operands(inst: &MInst) -> Option<(VReg, VReg)> {
    match *inst {
        MInst::Or { lhs, rhs, .. } | MInst::Or32 { lhs, rhs, .. } => Some((lhs, rhs)),
        _ => None,
    }
}

fn plan(block: &MBlock, cond: VReg, uses: &[usize]) -> Option<Plan> {
    // The result moves to the join. A use earlier in this block would then
    // precede its definition, even if that use has no memory side effects.
    if block.insts[..block.insts.len() - 1]
        .iter()
        .any(|inst| inst.uses().contains(&cond))
    {
        return None;
    }
    let definitions = block
        .insts
        .iter()
        .enumerate()
        .filter_map(|(position, inst)| inst.def().map(|dst| (dst, position)))
        .collect::<HashMap<_, _>>();
    let definition = |value: VReg| {
        definitions
            .get(&value)
            .map(|&position| &block.insts[position])
    };
    let root = definition(cond)?;
    let (guard, disjunction) = if or_operands(root).is_some() {
        (None, cond)
    } else {
        let (lhs, rhs) = and_operands(root)?;
        [(lhs, rhs), (rhs, lhs)]
            .into_iter()
            .find_map(|(guard, disjunction)| {
                or_operands(definition(disjunction)?)
                    .is_some()
                    .then_some((Some(guard), disjunction))
            })?
    };
    let mut removed = HashSet::default();
    removed.insert(definitions[&cond]);
    let mut pending = vec![disjunction];
    let mut terms = Vec::new();
    while let Some(value) = pending.pop() {
        if value != cond && uses[value.0 as usize] != 1 {
            return None;
        }
        let position = *definitions.get(&value)?;
        if let Some((lhs, rhs)) = or_operands(&block.insts[position]) {
            removed.insert(position);
            pending.extend([rhs, lhs]);
        } else {
            let (lhs, rhs) = and_operands(&block.insts[position])?;
            removed.insert(position);
            terms.push((lhs, rhs));
        }
        if pending.len() + terms.len() > 8 {
            return None;
        }
    }
    if terms.len() < 2 {
        return None;
    }
    let equality = |value: VReg| {
        if uses[value.0 as usize] != 1 {
            return None;
        }
        match *definition(value)? {
            MInst::CmpImm {
                lhs,
                imm,
                kind: CmpKind::Eq,
                ..
            } => Some((lhs, imm)),
            _ => None,
        }
    };
    // An AND can have an equality on both sides. Try either selector from the
    // first term, then require that selector and distinct constants throughout.
    for first_guard in [terms[0].0, terms[0].1] {
        let Some((selector, _)) = equality(first_guard) else {
            continue;
        };
        let mut values = HashSet::default();
        let mut cases = Vec::new();
        let mut selected = removed.clone();
        let mut cost = 0usize;
        for &(lhs, rhs) in &terms {
            let pair = [(lhs, rhs), (rhs, lhs)]
                .into_iter()
                .find_map(|(comparison, predicate)| {
                    let (key, value) = equality(comparison)?;
                    (key == selector).then_some((comparison, predicate, value))
                });
            let Some((comparison, predicate, value)) = pair else {
                break;
            };
            if !values.insert(value) {
                break;
            }
            let instructions = private_operand(block, predicate, uses);
            let Some(&predicate_position) = definitions.get(&predicate) else {
                break;
            };
            if !instructions.contains(&predicate_position) {
                break;
            }
            cost += instructions
                .iter()
                .filter(|&&position| {
                    !matches!(
                        block.insts[position],
                        MInst::LoadImm { .. } | MInst::Mov { .. } | MInst::Mov32 { .. }
                    )
                })
                .count();
            if instructions
                .iter()
                .any(|position| selected.contains(position))
            {
                break;
            }
            selected.extend(instructions.iter().copied());
            selected.insert(definitions[&comparison]);
            cases.push(Case {
                comparison: definition(comparison)?.clone(),
                predicate,
                instructions,
            });
        }
        if cases.len() != terms.len() || cost < 10 {
            continue;
        }
        let first = cases
            .iter()
            .flat_map(|case| case.instructions.iter().copied())
            .min()?;
        if block.insts[first..block.insts.len() - 1]
            .iter()
            .any(|inst| !movable(inst))
        {
            continue;
        }
        return Some(Plan {
            guard,
            cases,
            removed: selected,
        });
    }
    None
}

pub(crate) fn run(func: &mut MFunction) {
    let positions = func
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.id, index))
        .collect::<HashMap<_, _>>();
    let Some(successors) = func
        .blocks
        .iter()
        .map(|block| {
            block
                .successors()
                .into_iter()
                .map(|target| positions.get(&target).copied())
                .collect::<Option<Vec<_>>>()
        })
        .collect::<Option<Vec<_>>>()
    else {
        return;
    };
    let Ok(cfg) = celox_analysis::cfg::ForwardControlFlowGraph::analyze_structure(successors, 0)
    else {
        return;
    };
    let headers = cfg
        .loops
        .iter()
        .map(|region| func.blocks[region.header].id)
        .collect::<HashSet<_>>();
    let zeros = known_bits::known_zeros(func);
    let mut uses = vec![0usize; func.value_count()];
    for block in &func.blocks {
        for phi in &block.phis {
            for &(_, source) in &phi.sources {
                uses[source.0 as usize] += 1;
            }
        }
        for inst in &block.insts {
            for source in inst.uses() {
                uses[source.0 as usize] += 1;
            }
        }
    }
    let Some(mut next_id) = func
        .blocks
        .iter()
        .map(|block| block.id.0)
        .max()
        .and_then(|id| id.checked_add(1))
    else {
        return;
    };
    let mut inserted = HashMap::<BlockId, Vec<MBlock>>::default();
    let original_count = func.blocks.len();
    for position in 0..original_count {
        let block = &func.blocks[position];
        if !headers.contains(&block.id) {
            continue;
        }
        let Some(&MInst::Branch {
            cond,
            true_bb,
            false_bb,
        }) = block.terminator()
        else {
            continue;
        };
        if true_bb == false_bb {
            continue;
        }
        let Some(plan) = plan(block, cond, &uses) else {
            continue;
        };
        let count = plan.cases.len() as u32;
        let Some(end_id) = next_id.checked_add(count * 2 + 1) else {
            break;
        };
        let dispatch = (0..count)
            .map(|offset| BlockId(next_id + offset))
            .collect::<Vec<_>>();
        let bodies = (0..count)
            .map(|offset| BlockId(next_id + count + offset))
            .collect::<Vec<_>>();
        let join_id = BlockId(next_id + count * 2);
        next_id = end_id;
        let original_id = block.id;
        let original = std::mem::take(&mut func.blocks[position].insts);
        while (func.vregs.count() as usize) < uses.len() {
            func.vregs.alloc();
        }
        func.spill_descs
            .resize_with(func.vregs.count() as usize, SpillDesc::transient);
        let zero = func.vregs.alloc();
        func.spill_descs.push(SpillDesc::remat(0));
        let header = &mut func.blocks[position];
        for (index, inst) in original.iter().enumerate().take(original.len() - 1) {
            if !plan.removed.contains(&index) {
                header.push(inst.clone());
            }
        }
        header.push(MInst::LoadImm {
            dst: zero,
            value: 0,
        });
        if let Some(mut guard) = plan.guard {
            // Each equality masks its predicate to bit zero. The outer
            // bitwise guard also observes only that bit, not nonzero truth.
            if zeros[guard.0 as usize] & !1 != !1 {
                let normalized = func.vregs.alloc();
                func.spill_descs.push(SpillDesc::transient());
                header.push(MInst::AndImm32 {
                    dst: normalized,
                    src: guard,
                    imm: 1,
                });
                guard = normalized;
            }
            header.push(MInst::Branch {
                cond: guard,
                true_bb: dispatch[0],
                false_bb: join_id,
            });
        } else {
            header.push(MInst::Jump {
                target: dispatch[0],
            });
        }
        let mut sources = vec![(*dispatch.last().unwrap(), zero)];
        if plan.guard.is_some() {
            sources.push((original_id, zero));
        }
        let mut generated = Vec::new();
        for (index, case) in plan.cases.into_iter().enumerate() {
            let mut decision = MBlock::new(dispatch[index]);
            let condition = case.comparison.def().unwrap();
            decision.push(case.comparison);
            decision.push(MInst::Branch {
                cond: condition,
                true_bb: bodies[index],
                false_bb: dispatch.get(index + 1).copied().unwrap_or(join_id),
            });
            let mut body = MBlock::new(bodies[index]);
            for instruction in case.instructions {
                body.push(original[instruction].clone());
            }
            let predicate = if zeros[case.predicate.0 as usize] & !1 == !1 {
                case.predicate
            } else {
                let normalized = func.vregs.alloc();
                func.spill_descs.push(SpillDesc::transient());
                body.push(MInst::AndImm32 {
                    dst: normalized,
                    src: case.predicate,
                    imm: 1,
                });
                normalized
            };
            body.push(MInst::Jump { target: join_id });
            sources.push((body.id, predicate));
            generated.push(decision);
            generated.push(body);
        }
        let mut join = MBlock::new(join_id);
        join.phis.push(PhiNode { dst: cond, sources });
        join.push(MInst::Branch {
            cond,
            true_bb,
            false_bb,
        });
        for successor in &mut func.blocks {
            if successor.id == true_bb || successor.id == false_bb {
                for phi in &mut successor.phis {
                    for (predecessor, _) in &mut phi.sources {
                        if *predecessor == original_id {
                            *predecessor = join_id;
                        }
                    }
                }
            }
        }
        generated.push(join);
        inserted.insert(original_id, generated);
    }
    // Keep the new dispatch close to its loop header. Every successful case
    // falls through to its private operands, then rejoins the original branch.
    if !inserted.is_empty() {
        func.blocks = std::mem::take(&mut func.blocks)
            .into_iter()
            .flat_map(|block| {
                let following = inserted.remove(&block.id).unwrap_or_default();
                std::iter::once(block).chain(following)
            })
            .collect();
    }
}
