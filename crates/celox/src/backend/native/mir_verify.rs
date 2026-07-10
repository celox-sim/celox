use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use super::mir::{BlockId, MFunction, MInst, VReg};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirVerifyError {
    pub invariant: &'static str,
    pub block: Option<BlockId>,
    pub instruction: Option<usize>,
    pub message: String,
}

impl MirVerifyError {
    fn function(invariant: &'static str, message: impl Into<String>) -> Self {
        Self {
            invariant,
            block: None,
            instruction: None,
            message: message.into(),
        }
    }

    fn block(invariant: &'static str, block: BlockId, message: impl Into<String>) -> Self {
        Self {
            invariant,
            block: Some(block),
            instruction: None,
            message: message.into(),
        }
    }

    fn instruction(
        invariant: &'static str,
        block: BlockId,
        instruction: usize,
        message: impl Into<String>,
    ) -> Self {
        Self {
            invariant,
            block: Some(block),
            instruction: Some(instruction),
            message: message.into(),
        }
    }
}

impl fmt::Display for MirVerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MIR verify [{}]", self.invariant)?;
        if let Some(block) = self.block {
            write!(f, " at {block}")?;
        }
        if let Some(instruction) = self.instruction {
            write!(f, "/i{instruction}")?;
        }
        write!(f, ": {}", self.message)
    }
}

impl std::error::Error for MirVerifyError {}

#[derive(Clone, Copy)]
enum DefSite {
    Phi(BlockId),
    Inst(BlockId, usize),
}

pub fn verify_function(func: &MFunction) -> Result<(), MirVerifyError> {
    if func.blocks.is_empty() {
        return Err(MirVerifyError::function(
            "CFG.NON_EMPTY",
            "function has no blocks",
        ));
    }
    let vreg_count = func.vregs.count() as usize;
    if func.spill_descs.len() != vreg_count {
        return Err(MirVerifyError::function(
            "SIDETABLE.SPILL_DESCS_COMPLETE",
            format!(
                "{} VRegs were allocated but spill_descs has {} entries",
                vreg_count,
                func.spill_descs.len()
            ),
        ));
    }
    if !func.value_widths.is_empty() && func.value_widths.len() != vreg_count {
        return Err(MirVerifyError::function(
            "SIDETABLE.VALUE_WIDTHS_COMPLETE",
            format!(
                "{} VRegs were allocated but value_widths has {} entries",
                vreg_count,
                func.value_widths.len()
            ),
        ));
    }
    for (index, width) in func.value_widths.iter().enumerate() {
        if matches!(width, Some(65..)) {
            return Err(MirVerifyError::function(
                "SIDETABLE.VALUE_WIDTH_VALID",
                format!("v{index} has invalid known width {width:?}"),
            ));
        }
    }

    let entry = func.blocks[0].id;
    let mut block_indices = BTreeMap::new();
    for (index, block) in func.blocks.iter().enumerate() {
        if block_indices.insert(block.id, index).is_some() {
            return Err(MirVerifyError::block(
                "CFG.UNIQUE_BLOCK_ID",
                block.id,
                "block id occurs more than once",
            ));
        }
        if block.insts.is_empty() || !block.insts.last().is_some_and(MInst::is_terminator) {
            return Err(MirVerifyError::block(
                "CFG.BLOCK_HAS_TERMINATOR",
                block.id,
                "block does not end in a terminator",
            ));
        }
        if let Some((position, _)) = block
            .insts
            .iter()
            .enumerate()
            .take(block.insts.len() - 1)
            .find(|(_, inst)| inst.is_terminator())
        {
            return Err(MirVerifyError::instruction(
                "CFG.TERMINATOR_IS_LAST",
                block.id,
                position,
                "terminator appears before the end of the block",
            ));
        }
    }

    let mut predecessors: BTreeMap<BlockId, BTreeSet<BlockId>> = block_indices
        .keys()
        .copied()
        .map(|id| (id, BTreeSet::new()))
        .collect();
    for block in &func.blocks {
        for target in block.successors() {
            let Some(preds) = predecessors.get_mut(&target) else {
                return Err(MirVerifyError::block(
                    "CFG.TARGET_EXISTS",
                    block.id,
                    format!("terminator targets missing block {target}"),
                ));
            };
            preds.insert(block.id);
        }
    }
    let reachable = reachable_blocks(func, entry, &block_indices);
    if reachable.len() != func.blocks.len() {
        let unreachable = block_indices
            .keys()
            .filter(|id| !reachable.contains(id))
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        return Err(MirVerifyError::function(
            "CFG.ALL_BLOCKS_REACHABLE",
            format!("unreachable blocks: {unreachable}"),
        ));
    }

    let mut defs = BTreeMap::new();
    for block in &func.blocks {
        if block.id == entry && !block.phis.is_empty() {
            return Err(MirVerifyError::block(
                "SSA.ENTRY_HAS_NO_PHIS",
                block.id,
                "entry block contains phi nodes",
            ));
        }
        for (phi_index, phi) in block.phis.iter().enumerate() {
            require_vreg(func, block.id, None, phi.dst)?;
            insert_def(&mut defs, phi.dst, DefSite::Phi(block.id), block.id, None)?;
            let expected = &predecessors[&block.id];
            let mut actual = BTreeSet::new();
            for &(pred, src) in &phi.sources {
                if !expected.contains(&pred) {
                    return Err(MirVerifyError::block(
                        "SSA.PHI_SOURCE_IS_PREDECESSOR",
                        block.id,
                        format!(
                            "phi {phi_index} has source from {pred}, which is not a predecessor"
                        ),
                    ));
                }
                if !actual.insert(pred) {
                    return Err(MirVerifyError::block(
                        "SSA.PHI_ONE_SOURCE_PER_PREDECESSOR",
                        block.id,
                        format!("phi {phi_index} has duplicate source for {pred}"),
                    ));
                }
                require_vreg(func, block.id, None, src)?;
            }
            if &actual != expected {
                return Err(MirVerifyError::block(
                    "SSA.PHI_COVERS_PREDECESSORS",
                    block.id,
                    format!(
                        "phi {phi_index} sources are {actual:?}, predecessors are {expected:?}"
                    ),
                ));
            }
        }
        for (index, inst) in block.insts.iter().enumerate() {
            verify_instruction_constraints(block.id, index, inst)?;
            for reg in inst.uses() {
                require_vreg(func, block.id, Some(index), reg)?;
            }
            if let Some(dst) = inst.def() {
                require_vreg(func, block.id, Some(index), dst)?;
                insert_def(
                    &mut defs,
                    dst,
                    DefSite::Inst(block.id, index),
                    block.id,
                    Some(index),
                )?;
            }
        }
    }

    let dominators = compute_dominators(entry, &predecessors, &reachable);
    for block in &func.blocks {
        for phi in &block.phis {
            for &(pred, src) in &phi.sources {
                let predecessor_end = func.blocks[block_indices[&pred]].insts.len();
                verify_use(
                    &defs,
                    &dominators,
                    pred,
                    predecessor_end,
                    src,
                    Some(block.id),
                )?;
            }
        }
        for (index, inst) in block.insts.iter().enumerate() {
            for reg in inst.uses() {
                verify_use(&defs, &dominators, block.id, index, reg, None)?;
            }
        }
    }
    Ok(())
}

fn verify_instruction_constraints(
    block: BlockId,
    index: usize,
    inst: &MInst,
) -> Result<(), MirVerifyError> {
    match inst {
        MInst::ShrImm { imm, .. } | MInst::ShlImm { imm, .. } | MInst::SarImm { imm, .. }
            if *imm >= 64 =>
        {
            Err(MirVerifyError::instruction(
                "OPCODE.SHIFT_IMMEDIATE_RANGE",
                block,
                index,
                format!("shift immediate {imm} is outside 0..64"),
            ))
        }
        MInst::AndImm { imm, .. }
            if !((*imm as i64) >= i32::MIN as i64 && (*imm as i64) <= i32::MAX as i64)
                && *imm > u32::MAX as u64 =>
        {
            Err(MirVerifyError::instruction(
                "OPCODE.AND_IMMEDIATE_ENCODABLE",
                block,
                index,
                format!("and immediate {imm:#x} is not encodable as imm32"),
            ))
        }
        MInst::OrImm { imm, .. }
            if (*imm as i64) < i32::MIN as i64 || (*imm as i64) > i32::MAX as i64 =>
        {
            Err(MirVerifyError::instruction(
                "OPCODE.OR_IMMEDIATE_ENCODABLE",
                block,
                index,
                format!("or immediate {imm:#x} is not encodable as sign-extended imm32"),
            ))
        }
        MInst::MemCopy { byte_len: 0, .. } => Err(MirVerifyError::instruction(
            "OPCODE.MEMCOPY_NON_ZERO",
            block,
            index,
            "zero-length memcpy must be eliminated before MIR",
        )),
        _ => Ok(()),
    }
}

fn require_vreg(
    func: &MFunction,
    block: BlockId,
    instruction: Option<usize>,
    reg: VReg,
) -> Result<(), MirVerifyError> {
    if reg.0 < func.vregs.count() {
        return Ok(());
    }
    Err(match instruction {
        Some(index) => MirVerifyError::instruction(
            "VREG.ALLOCATED",
            block,
            index,
            format!("{reg} is outside allocated range 0..{}", func.vregs.count()),
        ),
        None => MirVerifyError::block(
            "VREG.ALLOCATED",
            block,
            format!("{reg} is outside allocated range 0..{}", func.vregs.count()),
        ),
    })
}

fn insert_def(
    defs: &mut BTreeMap<VReg, DefSite>,
    reg: VReg,
    site: DefSite,
    block: BlockId,
    instruction: Option<usize>,
) -> Result<(), MirVerifyError> {
    if defs.insert(reg, site).is_some() {
        return Err(match instruction {
            Some(index) => MirVerifyError::instruction(
                "SSA.SINGLE_DEFINITION",
                block,
                index,
                format!("{reg} is defined more than once"),
            ),
            None => MirVerifyError::block(
                "SSA.SINGLE_DEFINITION",
                block,
                format!("{reg} is defined more than once"),
            ),
        });
    }
    Ok(())
}

fn verify_use(
    defs: &BTreeMap<VReg, DefSite>,
    dominators: &Dominators,
    block: BlockId,
    instruction: usize,
    reg: VReg,
    phi_target: Option<BlockId>,
) -> Result<(), MirVerifyError> {
    let context_block = phi_target.unwrap_or(block);
    let Some(site) = defs.get(&reg).copied() else {
        return Err(MirVerifyError::instruction(
            "SSA.USE_HAS_DEFINITION",
            context_block,
            instruction,
            format!("{reg} is used but never defined"),
        ));
    };
    let valid = match site {
        DefSite::Phi(def_block) => dominators.dominates(def_block, block),
        DefSite::Inst(def_block, def_index) => {
            (def_block == block && def_index < instruction)
                || (def_block != block && dominators.dominates(def_block, block))
        }
    };
    if !valid {
        return Err(MirVerifyError::instruction(
            "SSA.DEFINITION_DOMINATES_USE",
            context_block,
            instruction,
            format!("definition of {reg} does not dominate this use"),
        ));
    }
    Ok(())
}

fn reachable_blocks(
    func: &MFunction,
    entry: BlockId,
    block_indices: &BTreeMap<BlockId, usize>,
) -> BTreeSet<BlockId> {
    let mut reachable = BTreeSet::new();
    let mut queue = VecDeque::from([entry]);
    while let Some(block) = queue.pop_front() {
        if !reachable.insert(block) {
            continue;
        }
        for successor in func.blocks[block_indices[&block]].successors() {
            if block_indices.contains_key(&successor) {
                queue.push_back(successor);
            }
        }
    }
    reachable
}

fn compute_dominators(
    entry: BlockId,
    predecessors: &BTreeMap<BlockId, BTreeSet<BlockId>>,
    reachable: &BTreeSet<BlockId>,
) -> Dominators {
    let ids = reachable.iter().copied().collect::<Vec<_>>();
    let index = ids
        .iter()
        .enumerate()
        .map(|(index, &id)| (id, index))
        .collect::<BTreeMap<_, _>>();
    let words = ids.len().div_ceil(64);
    let mut all = vec![u64::MAX; words];
    if let Some(last) = all.last_mut() {
        *last &= u64::MAX >> (words * 64 - ids.len());
    }
    let mut bits = vec![all; ids.len()];
    let entry_index = index[&entry];
    bits[entry_index].fill(0);
    bits[entry_index][entry_index / 64] |= 1 << (entry_index % 64);
    loop {
        let mut changed = false;
        for &block in &ids {
            if block == entry {
                continue;
            }
            let block_index = index[&block];
            let mut next = vec![u64::MAX; words];
            for pred in &predecessors[&block] {
                let pred_index = index[pred];
                for (word, pred_word) in next.iter_mut().zip(&bits[pred_index]) {
                    *word &= pred_word;
                }
            }
            next[block_index / 64] |= 1 << (block_index % 64);
            if next != bits[block_index] {
                bits[block_index] = next;
                changed = true;
            }
        }
        if !changed {
            return Dominators { index, bits };
        }
    }
}

struct Dominators {
    index: BTreeMap<BlockId, usize>,
    bits: Vec<Vec<u64>>,
}

impl Dominators {
    fn dominates(&self, dominator: BlockId, block: BlockId) -> bool {
        let dominator = self.index[&dominator];
        let block = self.index[&block];
        self.bits[block][dominator / 64] & (1 << (dominator % 64)) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::super::mir::{MBlock, PhiNode, SpillDesc, VRegAllocator};
    use super::*;

    fn function(vreg_count: u32, blocks: Vec<MBlock>) -> MFunction {
        let mut vregs = VRegAllocator::new();
        for _ in 0..vreg_count {
            vregs.alloc();
        }
        let spill_descs = (0..vreg_count).map(|_| SpillDesc::transient()).collect();
        let mut func = MFunction::new(vregs, spill_descs);
        func.blocks = blocks;
        func
    }

    #[test]
    fn accepts_well_formed_phi() {
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: VReg(0),
            value: 1,
        });
        entry.push(MInst::Branch {
            cond: VReg(0),
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::LoadImm {
            dst: VReg(1),
            value: 2,
        });
        left.push(MInst::Jump { target: BlockId(3) });
        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::LoadImm {
            dst: VReg(2),
            value: 3,
        });
        right.push(MInst::Jump { target: BlockId(3) });
        let mut merge = MBlock::new(BlockId(3));
        merge.phis.push(PhiNode {
            dst: VReg(3),
            sources: vec![(BlockId(1), VReg(1)), (BlockId(2), VReg(2))],
        });
        merge.push(MInst::Mov {
            dst: VReg(4),
            src: VReg(3),
        });
        merge.push(MInst::Return);
        assert_eq!(
            function(5, vec![entry, left, right, merge]).verify_result(),
            Ok(())
        );
    }

    #[test]
    fn rejects_phi_missing_predecessor() {
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: VReg(0),
            value: 1,
        });
        entry.push(MInst::Branch {
            cond: VReg(0),
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::LoadImm {
            dst: VReg(1),
            value: 2,
        });
        left.push(MInst::Jump { target: BlockId(2) });
        let mut merge = MBlock::new(BlockId(2));
        merge.phis.push(PhiNode {
            dst: VReg(2),
            sources: vec![(BlockId(1), VReg(1))],
        });
        merge.push(MInst::Return);
        assert_eq!(
            function(3, vec![entry, left, merge])
                .verify_result()
                .unwrap_err()
                .invariant,
            "SSA.PHI_COVERS_PREDECESSORS"
        );
    }

    #[test]
    fn rejects_same_block_use_before_definition() {
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Mov {
            dst: VReg(1),
            src: VReg(0),
        });
        block.push(MInst::LoadImm {
            dst: VReg(0),
            value: 1,
        });
        block.push(MInst::Return);
        assert_eq!(
            function(2, vec![block])
                .verify_result()
                .unwrap_err()
                .invariant,
            "SSA.DEFINITION_DOMINATES_USE"
        );
    }

    #[test]
    fn rejects_unencodable_and_immediate() {
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm {
            dst: VReg(0),
            value: 1,
        });
        block.push(MInst::AndImm {
            dst: VReg(1),
            src: VReg(0),
            imm: 0x0000_0001_0000_0000,
        });
        block.push(MInst::Return);
        assert_eq!(
            function(2, vec![block])
                .verify_result()
                .unwrap_err()
                .invariant,
            "OPCODE.AND_IMMEDIATE_ENCODABLE"
        );
    }

    #[test]
    fn accepts_sign_extended_and_immediate() {
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm {
            dst: VReg(0),
            value: 1,
        });
        block.push(MInst::AndImm {
            dst: VReg(1),
            src: VReg(0),
            imm: 0xffff_ffff_ffff_ff00,
        });
        block.push(MInst::Return);
        assert_eq!(function(2, vec![block]).verify_result(), Ok(()));
    }
}
