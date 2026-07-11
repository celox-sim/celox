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
    for (index, table) in func.constant_tables().iter().enumerate() {
        if table.is_empty() {
            return Err(MirVerifyError::function(
                "SIDETABLE.CONSTANT_TABLE_NON_EMPTY",
                format!("constant table {index} has no entries"),
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
    // The first block is the unique CFG/SSA root.  Allowing an edge back to
    // it would require an entry phi/environment that MIR does not represent,
    // and would leave the root without a well-defined immediate dominator.
    if !predecessors[&entry].is_empty() {
        return Err(MirVerifyError::block(
            "CFG.ENTRY_HAS_NO_PREDECESSORS",
            entry,
            format!("entry block has predecessors {:?}", predecessors[&entry]),
        ));
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
            verify_instruction_constraints(func, block.id, index, inst)?;
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
    func: &MFunction,
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
        MInst::LoadConstantTableAddr { table, .. } if func.constant_table(*table).is_none() => {
            Err(MirVerifyError::instruction(
                "OPCODE.CONSTANT_TABLE_EXISTS",
                block,
                index,
                format!(
                    "{table} is outside constant table range 0..{}",
                    func.constant_tables().len()
                ),
            ))
        }
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
    let mut successors = reachable
        .iter()
        .copied()
        .map(|block| (block, Vec::new()))
        .collect::<BTreeMap<_, _>>();
    for (&block, block_predecessors) in predecessors {
        if !reachable.contains(&block) {
            continue;
        }
        for &predecessor in block_predecessors {
            if reachable.contains(&predecessor) {
                successors.get_mut(&predecessor).unwrap().push(block);
            }
        }
    }
    let mut visited = BTreeSet::new();
    let mut postorder = Vec::with_capacity(reachable.len());
    visited.insert(entry);
    let mut stack = vec![(entry, 0usize)];
    while let Some((block, next)) = stack.last_mut() {
        if *next == successors[block].len() {
            postorder.push(*block);
            stack.pop();
            continue;
        }
        let successor = successors[block][*next];
        *next += 1;
        if visited.insert(successor) {
            stack.push((successor, 0));
        }
    }
    postorder.reverse();
    let index = postorder
        .iter()
        .enumerate()
        .map(|(index, &id)| (id, index))
        .collect::<BTreeMap<_, _>>();
    let mut idom = vec![None; postorder.len()];
    idom[0] = Some(0);
    let intersect = |mut left: usize, mut right: usize, idom: &[Option<usize>]| {
        while left != right {
            while left > right {
                left = idom[left].expect("processed dominator");
            }
            while right > left {
                right = idom[right].expect("processed dominator");
            }
        }
        left
    };
    loop {
        let mut changed = false;
        for block_index in 1..postorder.len() {
            let block = postorder[block_index];
            let mut processed = predecessors[&block]
                .iter()
                .map(|predecessor| index[predecessor])
                .filter(|predecessor| idom[*predecessor].is_some());
            let Some(first) = processed.next() else {
                continue;
            };
            let next = processed.fold(first, |current, predecessor| {
                intersect(current, predecessor, &idom)
            });
            if idom[block_index] != Some(next) {
                idom[block_index] = Some(next);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut children = vec![Vec::new(); postorder.len()];
    for (block, parent) in idom.iter().enumerate().skip(1) {
        children[parent.expect("reachable block has idom")].push(block);
    }
    let mut enter = vec![0usize; postorder.len()];
    let mut exit = vec![0usize; postorder.len()];
    let mut time = 0usize;
    let mut events = vec![(0usize, false)];
    while let Some((block, leaving)) = events.pop() {
        if leaving {
            exit[block] = time;
            time += 1;
        } else {
            enter[block] = time;
            time += 1;
            events.push((block, true));
            events.extend(children[block].iter().rev().map(|child| (*child, false)));
        }
    }
    Dominators { index, enter, exit }
}

struct Dominators {
    index: BTreeMap<BlockId, usize>,
    enter: Vec<usize>,
    exit: Vec<usize>,
}

impl Dominators {
    fn dominates(&self, dominator: BlockId, block: BlockId) -> bool {
        let dominator = self.index[&dominator];
        let block = self.index[&block];
        self.enter[dominator] <= self.enter[block] && self.exit[block] <= self.exit[dominator]
    }
}

#[cfg(test)]
mod tests {
    use super::super::mir::{ConstantTableId, MBlock, PhiNode, SpillDesc, VRegAllocator};
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
    fn rejects_predecessors_of_the_entry_block() {
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
        left.push(MInst::Jump { target: BlockId(0) });
        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::Jump { target: BlockId(0) });

        let error = function(1, vec![entry, left, right])
            .verify_result()
            .unwrap_err();

        assert_eq!(error.invariant, "CFG.ENTRY_HAS_NO_PREDECESSORS");
        assert_eq!(error.block, Some(BlockId(0)));
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

    #[test]
    fn accepts_existing_constant_table_reference() {
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadConstantTableAddr {
            dst: VReg(0),
            table: ConstantTableId(0),
        });
        block.push(MInst::Return);
        let mut func = function(1, vec![block]);
        assert_eq!(
            func.intern_constant_table(vec![3, 5, 8]),
            ConstantTableId(0)
        );

        assert_eq!(func.verify_result(), Ok(()));
    }

    #[test]
    fn rejects_missing_constant_table_reference() {
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadConstantTableAddr {
            dst: VReg(0),
            table: ConstantTableId(4),
        });
        block.push(MInst::Return);

        let error = function(1, vec![block]).verify_result().unwrap_err();
        assert_eq!(error.invariant, "OPCODE.CONSTANT_TABLE_EXISTS");
        assert_eq!(error.block, Some(BlockId(0)));
        assert_eq!(error.instruction, Some(0));
    }

    #[test]
    fn rejects_empty_constant_table() {
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadConstantTableAddr {
            dst: VReg(0),
            table: ConstantTableId(0),
        });
        block.push(MInst::Return);
        let mut func = function(1, vec![block]);
        func.intern_constant_table(Vec::new());

        assert_eq!(
            func.verify_result().unwrap_err().invariant,
            "SIDETABLE.CONSTANT_TABLE_NON_EMPTY"
        );
    }
}
