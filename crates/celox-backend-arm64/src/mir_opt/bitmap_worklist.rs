//! Skip empty lanes in a counted loop guarded by an invariant bitmap.

use super::*;
use crate::HashSet;
use crate::mir::{BlockId, MBlock, PhiNode, SpillDesc};

/// SIR may put a loop's final branch in a separate, private block after its
/// conditional selections. Join that tail so loop analysis sees the guard.
pub(crate) fn merge_header_tails(function: &mut MFunction) {
    let positions = function
        .blocks
        .iter()
        .enumerate()
        .map(|(at, block)| (block.id, at))
        .collect::<HashMap<_, _>>();
    let Some(successors) = function
        .blocks
        .iter()
        .map(|block| {
            block
                .successors()
                .into_iter()
                .map(|id| positions.get(&id).copied())
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
        .map(|region| function.blocks[region.header].id)
        .collect::<Vec<_>>();
    for header in headers {
        for _ in 0..8 {
            let Some(head_at) = function.blocks.iter().position(|block| block.id == header) else {
                break;
            };
            let Some(&MInst::Jump { target }) = function.blocks[head_at].terminator() else {
                break;
            };
            let Some(tail_at) = function.blocks.iter().position(|block| block.id == target) else {
                break;
            };
            if tail_at == 0 || head_at == tail_at || !function.blocks[tail_at].phis.is_empty() {
                break;
            }
            if function
                .blocks
                .iter()
                .any(|block| block.id != header && block.successors().contains(&target))
            {
                break;
            }
            let tail = std::mem::take(&mut function.blocks[tail_at].insts);
            function.blocks[head_at].insts.pop();
            function.blocks[head_at].insts.extend(tail);
            for block in &mut function.blocks {
                for phi in &mut block.phis {
                    for (predecessor, _) in &mut phi.sources {
                        if *predecessor == target {
                            *predecessor = header;
                        }
                    }
                }
            }
            function.blocks.remove(tail_at);
        }
    }
}

struct Plan {
    header: BlockId,
    entry: BlockId,
    latch: BlockId,
    exit: BlockId,
    index: VReg,
    width: u32,
    words: Vec<VReg>,
    blocks: HashSet<BlockId>,
    outputs: HashMap<VReg, VReg>,
}

struct Analysis<'a> {
    func: &'a MFunction,
    positions: HashMap<BlockId, usize>,
    definitions: Vec<Option<&'a MInst>>,
    definition_blocks: Vec<Option<BlockId>>,
    zeros: Vec<u64>,
}

impl Analysis<'_> {
    fn block(&self, id: BlockId) -> &MBlock {
        &self.func.blocks[self.positions[&id]]
    }
    fn definition(&self, value: VReg) -> Option<&MInst> {
        self.definitions[value.0 as usize]
    }
    fn constant(&self, value: VReg) -> Option<u64> {
        match self.definition(value)? {
            MInst::LoadImm { value, .. } => Some(*value),
            _ => None,
        }
    }
    fn words(&self, condition: VReg, index: VReg, blocks: &HashSet<BlockId>) -> Vec<VReg> {
        let mut pending = vec![condition];
        let mut visited = HashSet::default();
        let mut words = BTreeSet::new();
        while let Some(value) = pending.pop() {
            if !visited.insert(value) {
                continue;
            }
            match self.definition(value) {
                Some(MInst::And { lhs, rhs, .. } | MInst::And32 { lhs, rhs, .. }) => {
                    pending.extend([*lhs, *rhs])
                }
                Some(MInst::AndImm { src, imm, .. }) if imm & 1 != 0 => pending.push(*src),
                Some(MInst::AndImm32 { src, imm, .. }) if imm & 1 != 0 => pending.push(*src),
                Some(MInst::Mov { src, .. } | MInst::Mov32 { src, .. }) => pending.push(*src),
                Some(MInst::Shr { lhs, rhs, .. })
                    if *rhs == index
                        && self.definition_blocks[lhs.0 as usize]
                            .is_some_and(|block| !blocks.contains(&block)) =>
                {
                    words.insert(*lhs);
                }
                _ => {}
            }
        }
        words.into_iter().collect()
    }
    fn skip_aliases(
        &self,
        header: BlockId,
        first: BlockId,
        latch: BlockId,
        condition: VReg,
        blocks: &HashSet<BlockId>,
    ) -> Option<HashMap<VReg, VReg>> {
        let mut aliases = HashMap::default();
        for inst in &self.block(header).insts {
            if let MInst::Select {
                dst,
                cond,
                false_val,
                ..
            } = *inst
                && cond == condition
            {
                aliases.insert(dst, false_val);
            }
        }
        let mut visited = HashSet::default();
        let mut predecessor = header;
        let mut next = first;
        loop {
            if !blocks.contains(&next) || next == header || !visited.insert(next) {
                return None;
            }
            let block = self.block(next);
            for phi in &block.phis {
                let source = phi
                    .sources
                    .iter()
                    .find_map(|&(pred, source)| (pred == predecessor).then_some(source))?;
                aliases.insert(phi.dst, resolve(source, &aliases));
            }
            if next == latch {
                return Some(aliases);
            }
            for inst in &block.insts[..block.insts.len() - 1] {
                if !exclusive_loop::movable(inst)
                    || matches!(inst, MInst::Load { .. } | MInst::LoadIndexed { .. })
                {
                    return None;
                }
                if let MInst::Mov { dst, src } = *inst {
                    aliases.insert(dst, resolve(src, &aliases));
                }
            }
            let target = match *block.terminator()? {
                MInst::Jump { target } => target,
                MInst::Branch {
                    cond,
                    true_bb,
                    false_bb,
                } => {
                    let cond = resolve(cond, &aliases);
                    let value = if cond == condition {
                        0
                    } else {
                        self.constant(cond)?
                    };
                    if value != 0 { true_bb } else { false_bb }
                }
                _ => return None,
            };
            predecessor = next;
            next = target;
        }
    }
    fn plan(&self, region: &celox_analysis::cfg::NaturalLoop) -> Option<Plan> {
        let header = &self.func.blocks[region.header];
        let blocks = region
            .blocks
            .iter()
            .map(|&block| self.func.blocks[block].id)
            .collect::<HashSet<_>>();
        let exits = region
            .blocks
            .iter()
            .flat_map(|&position| {
                self.func.blocks[position]
                    .successors()
                    .into_iter()
                    .filter(|target| !blocks.contains(target))
                    .map(move |target| (self.func.blocks[position].id, target))
            })
            .collect::<Vec<_>>();
        let &[(latch, exit)] = exits.as_slice() else {
            return None;
        };
        let &MInst::Branch {
            cond: loop_condition,
            true_bb,
            false_bb,
        } = self.block(latch).terminator()?
        else {
            return None;
        };
        let &MInst::CmpImm {
            lhs: next_index,
            imm,
            kind,
            ..
        } = self.definition(loop_condition)?
        else {
            return None;
        };
        if !(8..=64).contains(&imm)
            || !matches!(
                (kind, true_bb == header.id, false_bb == header.id),
                (CmpKind::Ne, true, false) | (CmpKind::Eq, false, true)
            )
        {
            return None;
        }
        let index_phi = header
            .phis
            .iter()
            .find(|phi| phi.sources.len() == 2 && phi.sources.contains(&(latch, next_index)))?;
        let &(entry, initial) = index_phi
            .sources
            .iter()
            .find(|(pred, _)| !blocks.contains(pred))?;
        if self.constant(initial) != Some(0)
            || !matches!(self.block(entry).terminator(), Some(MInst::Jump { target }) if *target == header.id)
        {
            return None;
        }
        let (mask, _) = counted_loop::step(next_index, index_phi.dst, true, &self.definitions)?;
        if imm as u64 > mask {
            return None;
        }
        let &MInst::Branch {
            cond: predicate,
            false_bb: skip,
            ..
        } = header.terminator()?
        else {
            return None;
        };
        if self.zeros[predicate.0 as usize] & !1 != !1 {
            return None;
        }
        let words = self.words(predicate, index_phi.dst, &blocks);
        if words.is_empty() {
            return None;
        }
        let aliases = self.skip_aliases(header.id, skip, latch, predicate, &blocks)?;
        let mut outputs = HashMap::default();
        for phi in &header.phis {
            if phi.dst == index_phi.dst {
                continue;
            }
            if phi.sources.len() != 2 || !phi.sources.iter().any(|&(pred, _)| pred == entry) {
                return None;
            }
            let source = phi
                .sources
                .iter()
                .find_map(|&(pred, source)| (pred == latch).then_some(source))?;
            if resolve(source, &aliases) != phi.dst || source == phi.dst {
                return None;
            }
            outputs.insert(source, phi.dst);
        }
        for block in &self.func.blocks {
            if blocks.contains(&block.id) {
                if block.insts.iter().any(|inst| {
                    !matches!(inst, MInst::Branch { .. } | MInst::Jump { .. })
                        && !exclusive_loop::movable(inst)
                }) {
                    return None;
                }
            } else if block
                .insts
                .iter()
                .flat_map(MInst::uses)
                .chain(
                    block
                        .phis
                        .iter()
                        .flat_map(|phi| phi.sources.iter().map(|&(_, source)| source)),
                )
                .any(|value| {
                    self.definition_blocks[value.0 as usize]
                        .is_some_and(|block| blocks.contains(&block))
                        && !outputs.contains_key(&value)
                })
            {
                return None;
            }
        }
        Some(Plan {
            header: header.id,
            entry,
            latch,
            exit,
            index: index_phi.dst,
            width: imm as u32,
            words,
            blocks,
            outputs,
        })
    }
}

fn resolve(mut value: VReg, aliases: &HashMap<VReg, VReg>) -> VReg {
    while let Some(&next) = aliases.get(&value) {
        if value == next {
            break;
        }
        value = next;
    }
    value
}

fn temporary(func: &mut MFunction) -> VReg {
    let dst = func.vregs.alloc();
    func.spill_descs.push(SpillDesc::transient());
    dst
}

fn find_plan(func: &MFunction) -> Option<Plan> {
    let positions = func
        .blocks
        .iter()
        .enumerate()
        .map(|(position, block)| (block.id, position))
        .collect::<HashMap<_, _>>();
    let successors = func
        .blocks
        .iter()
        .map(|block| {
            block
                .successors()
                .into_iter()
                .map(|target| positions.get(&target).copied())
                .collect::<Option<Vec<_>>>()
        })
        .collect::<Option<Vec<_>>>()?;
    let cfg =
        celox_analysis::cfg::ForwardControlFlowGraph::analyze_structure(successors, 0).ok()?;
    let mut definitions = vec![None; func.value_count()];
    let mut definition_blocks = vec![None; definitions.len()];
    for block in &func.blocks {
        for phi in &block.phis {
            definition_blocks[phi.dst.0 as usize] = Some(block.id);
        }
        for inst in &block.insts {
            if let Some(dst) = inst.def() {
                definitions[dst.0 as usize] = Some(inst);
                definition_blocks[dst.0 as usize] = Some(block.id);
            }
        }
    }
    let analysis = Analysis {
        func,
        positions,
        definitions,
        definition_blocks,
        zeros: known_bits::known_zeros(func),
    };
    cfg.loops.iter().find_map(|region| analysis.plan(region))
}

pub(crate) fn run(func: &mut MFunction) {
    // Reanalyze after each rewrite so adjacent or nested loops cannot retain
    // stale phi sources from an earlier rewrite.
    for _ in 0..64 {
        let Some(plan) = find_plan(func) else { break };
        let Some(id) = func
            .blocks
            .iter()
            .map(|block| block.id.0)
            .max()
            .and_then(|id| id.checked_add(1))
            .map(BlockId)
        else {
            break;
        };
        let value_count = func.value_count();
        while (func.vregs.count() as usize) < value_count {
            func.vregs.alloc();
        }
        func.spill_descs
            .resize_with(value_count, SpillDesc::transient);
        let mut setup = Vec::new();
        let mut bitmap = plan.words[0];
        for &word in &plan.words[1..] {
            let dst = temporary(func);
            setup.push(MInst::And {
                dst,
                lhs: bitmap,
                rhs: word,
            });
            bitmap = dst;
        }
        if plan.width < 64 {
            let mask = (1u64 << plan.width) - 1;
            let dst = temporary(func);
            if plan.width <= 32 {
                setup.push(MInst::AndImm32 {
                    dst,
                    src: bitmap,
                    imm: mask as u32,
                });
            } else {
                let constant = temporary(func);
                setup.push(MInst::LoadImm {
                    dst: constant,
                    value: mask,
                });
                setup.push(MInst::And {
                    dst,
                    lhs: bitmap,
                    rhs: constant,
                });
            }
            bitmap = dst;
        }
        let pending = temporary(func);
        let decremented = temporary(func);
        let remaining = temporary(func);
        let nonzero = temporary(func);
        let mut guard = MBlock::new(id);
        let header = func
            .blocks
            .iter_mut()
            .find(|block| block.id == plan.header)
            .unwrap();
        guard.phis = std::mem::take(&mut header.phis)
            .into_iter()
            .filter(|phi| phi.dst != plan.index)
            .collect();
        guard.phis.push(PhiNode {
            dst: pending,
            sources: vec![(plan.entry, bitmap), (plan.latch, remaining)],
        });
        guard.push(MInst::CmpImm {
            dst: nonzero,
            lhs: pending,
            imm: 0,
            kind: CmpKind::Ne,
        });
        guard.push(MInst::Branch {
            cond: nonzero,
            true_bb: plan.header,
            false_bb: plan.exit,
        });
        header.insts.insert(
            0,
            MInst::Bsf {
                dst: plan.index,
                src: pending,
            },
        );
        func.spill_descs[plan.index.0 as usize] = SpillDesc::transient();
        let entry = func
            .blocks
            .iter_mut()
            .find(|block| block.id == plan.entry)
            .unwrap();
        entry.insts.pop();
        entry.insts.extend(setup);
        entry.push(MInst::Jump { target: id });
        let latch = func
            .blocks
            .iter_mut()
            .find(|block| block.id == plan.latch)
            .unwrap();
        latch.insts.pop();
        latch.push(MInst::SubImm {
            dst: decremented,
            src: pending,
            imm: 1,
        });
        latch.push(MInst::And {
            dst: remaining,
            lhs: pending,
            rhs: decremented,
        });
        latch.push(MInst::Jump { target: id });
        for block in &mut func.blocks {
            if plan.blocks.contains(&block.id) {
                continue;
            }
            for inst in &mut block.insts {
                for source in inst.uses() {
                    if let Some(&replacement) = plan.outputs.get(&source) {
                        inst.rewrite_use(source, replacement);
                    }
                }
            }
            for phi in &mut block.phis {
                for (pred, source) in &mut phi.sources {
                    if let Some(&replacement) = plan.outputs.get(source) {
                        *source = replacement;
                    }
                    if block.id == plan.exit && *pred == plan.latch {
                        *pred = id;
                    }
                }
            }
        }
        let position = func
            .blocks
            .iter()
            .position(|block| block.id == plan.header)
            .unwrap();
        func.blocks.insert(position, guard);
    }
}
