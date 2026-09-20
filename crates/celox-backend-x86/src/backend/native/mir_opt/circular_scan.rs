//! Replace a pure circular minimum-distance bitmap scan with a rotated BSF.

use super::*;

#[cfg(test)]
pub(super) mod tests;

enum Bitmap {
    Word(VReg),
    Broadcast(VReg),
    Constant(bool),
    And(Box<Self>, Box<Self>),
    Or(Box<Self>, Box<Self>),
    Xor(Box<Self>, Box<Self>),
    Not(Box<Self>),
}

struct Scan {
    entry: BlockId,
    latch: BlockId,
    exit: BlockId,
    blocks: HashSet<BlockId>,
    found: VReg,
    distance: VReg,
    origin: VReg,
    width: u32,
    bitmap: Bitmap,
}

struct Matcher<'a> {
    func: &'a MFunction,
    positions: HashMap<BlockId, usize>,
    definitions: Vec<Option<&'a MInst>>,
    definition_blocks: Vec<Option<BlockId>>,
    zeros: Vec<u64>,
}

impl Matcher<'_> {
    fn definition(&self, value: VReg) -> Option<&MInst> {
        self.definitions[value.0 as usize]
    }
    fn constant(&self, value: VReg) -> Option<u64> {
        match self.definition(value)? {
            MInst::LoadImm { value, .. } => Some(*value),
            _ => None,
        }
    }
    fn block(&self, id: BlockId) -> &MBlock {
        &self.func.blocks[self.positions[&id]]
    }
    fn branch(&self, id: BlockId) -> Option<(VReg, BlockId, BlockId)> {
        match *self.block(id).terminator()? {
            MInst::Branch {
                cond,
                true_bb,
                false_bb,
            } => Some((cond, true_bb, false_bb)),
            _ => None,
        }
    }
    fn invariant(&self, value: VReg, blocks: &HashSet<BlockId>) -> bool {
        self.definition_blocks[value.0 as usize].is_some_and(|block| !blocks.contains(&block))
    }
    fn bitmap(
        &self,
        value: VReg,
        index: VReg,
        blocks: &HashSet<BlockId>,
        budget: &mut usize,
    ) -> Option<Bitmap> {
        *budget = budget.checked_sub(1)?;
        if self.invariant(value, blocks) {
            return Some(
                self.constant(value)
                    .map_or(Bitmap::Broadcast(value), |constant| {
                        Bitmap::Constant(constant & 1 != 0)
                    }),
            );
        }
        let recurse =
            |value, budget: &mut usize| self.bitmap(value, index, blocks, budget).map(Box::new);
        Some(match *self.definition(value)? {
            MInst::Shr { lhs, rhs, .. } if rhs == index && self.invariant(lhs, blocks) => {
                Bitmap::Word(lhs)
            }
            MInst::AndImm { src, imm, .. } => {
                if imm & 1 == 0 {
                    Bitmap::Constant(false)
                } else {
                    *recurse(src, budget)?
                }
            }
            MInst::AndImm32 { src, imm, .. } => {
                if imm & 1 == 0 {
                    Bitmap::Constant(false)
                } else {
                    *recurse(src, budget)?
                }
            }
            MInst::And { lhs, rhs, .. } | MInst::And32 { lhs, rhs, .. } => {
                Bitmap::And(recurse(lhs, budget)?, recurse(rhs, budget)?)
            }
            MInst::Or { lhs, rhs, .. } | MInst::Or32 { lhs, rhs, .. } => {
                Bitmap::Or(recurse(lhs, budget)?, recurse(rhs, budget)?)
            }
            MInst::Xor { lhs, rhs, .. } | MInst::Xor32 { lhs, rhs, .. } => {
                Bitmap::Xor(recurse(lhs, budget)?, recurse(rhs, budget)?)
            }
            MInst::BitNot { src, .. } => Bitmap::Not(recurse(src, budget)?),
            MInst::CmpImm {
                lhs, imm: 0, kind, ..
            } if self.zeros[lhs.0 as usize] & !1 == !1 => match kind {
                CmpKind::Eq => Bitmap::Not(recurse(lhs, budget)?),
                CmpKind::Ne => *recurse(lhs, budget)?,
                _ => return None,
            },
            _ => return None,
        })
    }
    fn incoming(&self, phi: &PhiNode, predecessor: BlockId) -> Option<VReg> {
        (phi.sources.len() == 2).then_some(())?;
        phi.sources
            .iter()
            .find_map(|&(pred, value)| (pred == predecessor).then_some(value))
    }
    fn header_phi(
        &self,
        header: &MBlock,
        entry: BlockId,
        latch: BlockId,
        next: VReg,
    ) -> Option<VReg> {
        header.phis.iter().find_map(|phi| {
            (self.incoming(phi, latch) == Some(next)
                && self
                    .incoming(phi, entry)
                    .and_then(|value| self.constant(value))
                    == Some(0))
            .then_some(phi.dst)
        })
    }
    fn match_scan(&self, region: &celox_analysis::cfg::NaturalLoop) -> Option<Scan> {
        // Require a header, candidate comparison, update/skip edges and latch.
        if region.blocks.len() != 5 {
            return None;
        }
        let header = &self.func.blocks[region.header];
        let blocks = region
            .blocks
            .iter()
            .map(|&position| self.func.blocks[position].id)
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
        let (condition, taken, untaken) = self.branch(latch)?;
        let MInst::CmpImm {
            lhs: next_index,
            imm,
            kind,
            ..
        } = *self.definition(condition)?
        else {
            return None;
        };
        if !(8..=32).contains(&imm)
            || !(imm as u32).is_power_of_two()
            || !matches!(
                (kind, taken == header.id, untaken == header.id),
                (CmpKind::Ne, true, false) | (CmpKind::Eq, false, true)
            )
        {
            return None;
        }
        let width = imm as u32;
        let index_phi = header
            .phis
            .iter()
            .find(|phi| self.incoming(phi, latch) == Some(next_index))?;
        let &(entry, initial_index) = index_phi
            .sources
            .iter()
            .find(|(pred, _)| !blocks.contains(pred))?;
        if self.constant(initial_index) != Some(0)
            || !matches!(self.block(entry).terminator(), Some(MInst::Jump { target }) if *target == header.id)
        {
            return None;
        }
        let (index_mask, _) =
            counted_loop::step(next_index, index_phi.dst, true, &self.definitions)?;
        if u64::from(width) > index_mask {
            return None;
        }
        let (predicate, choose, skip) = self.branch(header.id)?;
        let (improves, update, alternate_skip) = self.branch(choose)?;
        if alternate_skip != skip
            || [choose, skip, update, latch]
                .into_iter()
                .collect::<HashSet<_>>()
                .len()
                != 4
            || [choose, skip, update, latch]
                .iter()
                .any(|block| !blocks.contains(block))
        {
            return None;
        }
        for edge in [skip, update] {
            let block = self.block(edge);
            if !block.phis.is_empty()
                || !matches!(block.insts.as_slice(), [MInst::Jump { target }] if *target == latch)
            {
                return None;
            }
        }
        let latch_block = self.block(latch);
        let found_phi = latch_block.phis.iter().find(|phi| {
            self.incoming(phi, update)
                .and_then(|value| self.constant(value))
                == Some(1)
                && self.incoming(phi, skip) == self.header_phi(header, entry, latch, phi.dst)
        })?;
        let found = self.incoming(found_phi, skip)?;
        let (empty, better) = match *self.definition(improves)? {
            MInst::Or { lhs, rhs, .. } | MInst::Or32 { lhs, rhs, .. } => (lhs, rhs),
            _ => return None,
        };
        let (delta, old_distance) = [(empty, better), (better, empty)].into_iter().find_map(|(empty, better)| {
            if !matches!(self.definition(empty), Some(MInst::CmpImm { lhs, imm: 0, kind: CmpKind::Eq, .. }) if *lhs == found) { return None; }
            match self.definition(better)? { MInst::Cmp { lhs, rhs, kind: CmpKind::LtU, .. } => Some((*lhs, *rhs)), _ => None }
        })?;
        let distance_phi = latch_block.phis.iter().find(|phi| {
            self.incoming(phi, update) == Some(delta)
                && self.incoming(phi, skip) == Some(old_distance)
                && self.header_phi(header, entry, latch, phi.dst) == Some(old_distance)
        })?;
        let difference = match *self.definition(delta)? {
            MInst::AndImm { src, imm, .. } if imm == u64::from(width - 1) => src,
            MInst::AndImm32 { src, imm, .. } if imm == width - 1 => src,
            _ => return None,
        };
        let origin = match *self.definition(difference)? {
            MInst::Sub { lhs, rhs, .. } | MInst::Sub32 { lhs, rhs, .. }
                if lhs == index_phi.dst && self.invariant(rhs, &blocks) =>
            {
                rhs
            }
            _ => return None,
        };
        if self.zeros[predicate.0 as usize] & !1 != !1 {
            return None;
        }
        let bitmap = self.bitmap(predicate, index_phi.dst, &blocks, &mut 128)?;
        let outputs = [found_phi.dst, distance_phi.dst];
        for block in &self.func.blocks {
            if blocks.contains(&block.id) {
                if block.insts.iter().any(|inst| {
                    !matches!(inst, MInst::Branch { .. } | MInst::Jump { .. })
                        && (!loop_guard::movable(inst)
                            || memory_effect::reads(inst).has_effect()
                            || memory_effect::writes(inst).has_effect())
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
                        && !outputs.contains(&value)
                })
            {
                return None;
            }
        }
        Some(Scan {
            entry,
            latch,
            exit,
            blocks,
            found: found_phi.dst,
            distance: distance_phi.dst,
            origin,
            width,
            bitmap,
        })
    }
}

fn temporary(func: &mut MFunction) -> VReg {
    let value = func.vregs.alloc();
    func.spill_descs.push(SpillDesc::transient());
    value
}

fn constant(func: &mut MFunction, instructions: &mut Vec<MInst>, value: u64) -> VReg {
    let dst = temporary(func);
    func.spill_descs[dst.0 as usize] = SpillDesc::remat(value);
    instructions.push(MInst::LoadImm { dst, value });
    dst
}

fn emit_bitmap(bitmap: &Bitmap, func: &mut MFunction, instructions: &mut Vec<MInst>) -> VReg {
    if let Bitmap::Word(value) = *bitmap {
        return value;
    }
    if let Bitmap::Constant(value) = *bitmap {
        return constant(func, instructions, if value { u64::MAX } else { 0 });
    }
    let dst = temporary(func);
    let instruction = match bitmap {
        Bitmap::Word(_) | Bitmap::Constant(_) => unreachable!(),
        Bitmap::Broadcast(value) => {
            let bit = temporary(func);
            instructions.push(MInst::AndImm32 {
                dst: bit,
                src: *value,
                imm: 1,
            });
            MInst::Neg { dst, src: bit }
        }
        Bitmap::Not(value) => MInst::BitNot {
            dst,
            src: emit_bitmap(value, func, instructions),
        },
        Bitmap::And(lhs, rhs) | Bitmap::Or(lhs, rhs) | Bitmap::Xor(lhs, rhs) => {
            let lhs = emit_bitmap(lhs, func, instructions);
            let rhs = emit_bitmap(rhs, func, instructions);
            match bitmap {
                Bitmap::And(..) => MInst::And { dst, lhs, rhs },
                Bitmap::Or(..) => MInst::Or { dst, lhs, rhs },
                _ => MInst::Xor { dst, lhs, rhs },
            }
        }
    };
    instructions.push(instruction);
    dst
}

pub(super) fn run(func: &mut MFunction) {
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
                .map(|target| positions[&target])
                .collect()
        })
        .collect();
    let Ok(cfg) = celox_analysis::cfg::ForwardControlFlowGraph::analyze_structure(successors, 0)
    else {
        return;
    };
    let mut definitions = vec![None; func.vregs.count() as usize];
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
    let matcher = Matcher {
        func,
        positions,
        definitions,
        definition_blocks,
        zeros: known_bits::known_zeros(func),
    };
    let mut claimed = HashSet::default();
    let plans = cfg
        .loops
        .iter()
        .filter_map(|region| matcher.match_scan(region))
        .filter(|plan| {
            if plan.blocks.iter().any(|block| claimed.contains(block))
                || claimed.contains(&plan.entry)
            {
                return false;
            }
            claimed.extend(plan.blocks.iter().copied());
            true
        })
        .collect::<Vec<_>>();
    for plan in plans {
        let mut instructions = Vec::new();
        let bitmap = emit_bitmap(&plan.bitmap, func, &mut instructions);
        let bounded = temporary(func);
        let mask = ((1u64 << plan.width) - 1) as u32;
        instructions.push(MInst::AndImm32 {
            dst: bounded,
            src: bitmap,
            imm: mask,
        });
        let origin = temporary(func);
        instructions.push(MInst::AndImm32 {
            dst: origin,
            src: plan.origin,
            imm: plan.width - 1,
        });
        let width = constant(func, &mut instructions, u64::from(plan.width));
        let complement = temporary(func);
        instructions.push(MInst::Sub {
            dst: complement,
            lhs: width,
            rhs: origin,
        });
        let right = temporary(func);
        instructions.push(MInst::Shr {
            dst: right,
            lhs: bounded,
            rhs: origin,
        });
        let left = temporary(func);
        instructions.push(MInst::Shl {
            dst: left,
            lhs: bounded,
            rhs: complement,
        });
        let combined = temporary(func);
        instructions.push(MInst::Or {
            dst: combined,
            lhs: right,
            rhs: left,
        });
        let rotated = temporary(func);
        instructions.push(MInst::AndImm32 {
            dst: rotated,
            src: combined,
            imm: mask,
        });
        let sentinel = constant(func, &mut instructions, 1u64 << plan.width);
        let nonzero = temporary(func);
        instructions.push(MInst::Or {
            dst: nonzero,
            lhs: rotated,
            rhs: sentinel,
        });
        let distance = temporary(func);
        instructions.push(MInst::Bsf {
            dst: distance,
            src: nonzero,
        });
        instructions.push(MInst::CmpImm {
            dst: plan.found,
            lhs: rotated,
            imm: 0,
            kind: CmpKind::Ne,
        });
        let zero = constant(func, &mut instructions, 0);
        instructions.push(MInst::Select {
            dst: plan.distance,
            cond: plan.found,
            true_val: distance,
            false_val: zero,
        });
        func.spill_descs[plan.found.0 as usize] = SpillDesc::transient();
        func.spill_descs[plan.distance.0 as usize] = SpillDesc::transient();
        let entry = func
            .blocks
            .iter_mut()
            .find(|block| block.id == plan.entry)
            .unwrap();
        entry.insts.pop();
        entry.insts.extend(instructions);
        entry.push(MInst::Jump { target: plan.exit });
        func.blocks.retain(|block| !plan.blocks.contains(&block.id));
        for phi in &mut func
            .blocks
            .iter_mut()
            .find(|block| block.id == plan.exit)
            .unwrap()
            .phis
        {
            for (predecessor, _) in &mut phi.sources {
                if *predecessor == plan.latch {
                    *predecessor = plan.entry;
                }
            }
        }
    }
}
