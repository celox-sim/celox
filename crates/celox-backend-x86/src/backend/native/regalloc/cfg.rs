//! Normalized CFG information shared by every allocation phase.

use std::collections::{BTreeSet, HashMap};

use celox_analysis::cfg::ForwardControlFlowGraph;
pub(super) use celox_analysis::cfg::NaturalLoop;
use celox_analysis::ssa::SsaCfg;

use crate::backend::native::mir::{BlockId, MBlock, MFunction, MInst};

#[derive(Debug)]
pub(super) struct NormalizedCfg {
    pub block_index: HashMap<BlockId, usize>,
    pub predecessors: Vec<Vec<usize>>,
    pub successors: Vec<Vec<usize>>,
    pub idom: Vec<Option<usize>>,
    pub dominator_children: Vec<Vec<usize>>,
    pub dominance_frontier: Vec<BTreeSet<usize>>,
    pub loops: Vec<NaturalLoop>,
    pub loop_for_header: HashMap<usize, usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct EdgeInsertionPoint {
    pub block: usize,
    pub instruction: usize,
}

/// Return the concrete point that executes on exactly one normalized CFG edge.
/// A single-successor predecessor uses its terminator; a branch edge uses the
/// entry of its dedicated single-predecessor successor block.  CSSA may place
/// phi-source copies in that successor, so its edge identity must not depend on
/// the block remaining syntactically Jump-only.
pub(super) fn edge_insertion_point(
    func: &MFunction,
    cfg: &NormalizedCfg,
    predecessor: usize,
    successor: usize,
) -> Option<EdgeInsertionPoint> {
    if !cfg.successors.get(predecessor)?.contains(&successor) {
        return None;
    }
    if cfg.successors[predecessor].len() == 1 {
        return Some(EdgeInsertionPoint {
            block: predecessor,
            instruction: func.blocks.get(predecessor)?.insts.len().checked_sub(1)?,
        });
    }
    if cfg.predecessors.get(successor)?.as_slice() == [predecessor]
        && !func.blocks.get(successor)?.insts.is_empty()
    {
        return Some(EdgeInsertionPoint {
            block: successor,
            instruction: 0,
        });
    }
    None
}

impl SsaCfg for NormalizedCfg {
    type FrontierIter<'a> = std::iter::Copied<std::collections::btree_set::Iter<'a, usize>>;

    fn root(&self) -> usize {
        0
    }

    fn predecessors(&self) -> &[Vec<usize>] {
        &self.predecessors
    }

    fn successors(&self) -> &[Vec<usize>] {
        &self.successors
    }

    fn dominator_children(&self) -> &[Vec<usize>] {
        &self.dominator_children
    }

    fn dominance_frontier_len(&self) -> usize {
        self.dominance_frontier.len()
    }

    fn dominance_frontier(&self, block: usize) -> Self::FrontierIter<'_> {
        self.dominance_frontier[block].iter().copied()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CfgError {
    pub rule: &'static str,
    pub block: Option<BlockId>,
    pub message: String,
}

impl CfgError {
    fn new(rule: &'static str, block: Option<BlockId>, message: impl Into<String>) -> Self {
        Self {
            rule,
            block,
            message: message.into(),
        }
    }
}

impl NormalizedCfg {
    pub(super) fn verify(&self, func: &MFunction) -> Result<(), CfgError> {
        let blocks = func.blocks.len();
        if blocks == 0 {
            return Err(CfgError::new(
                "CFG.NON_EMPTY",
                None,
                "normalized CFG cannot describe an empty function",
            ));
        }
        if self.block_index.len() != blocks
            || self.predecessors.len() != blocks
            || self.successors.len() != blocks
            || self.idom.len() != blocks
            || self.dominator_children.len() != blocks
            || self.dominance_frontier.len() != blocks
        {
            return Err(CfgError::new(
                "CFG.MODEL_SHAPE",
                None,
                "normalized CFG tables do not cover every MIR block",
            ));
        }
        for (index, block) in func.blocks.iter().enumerate() {
            if self.block_index.get(&block.id) != Some(&index) {
                return Err(CfgError::new(
                    "CFG.BLOCK_INDEX_BIJECTION",
                    Some(block.id),
                    format!(
                        "block-index table maps {} to {:?}, expected {index}",
                        block.id,
                        self.block_index.get(&block.id)
                    ),
                ));
            }
        }
        for (block, predecessors) in self.predecessors.iter().enumerate() {
            if let Some(&predecessor) = predecessors.iter().find(|&&index| index >= blocks) {
                return Err(CfgError::new(
                    "CFG.PREDECESSOR_RANGE",
                    Some(func.blocks[block].id),
                    format!("predecessor index {predecessor} is outside the function"),
                ));
            }
        }
        for (block, successors) in self.successors.iter().enumerate() {
            if let Some(&successor) = successors.iter().find(|&&index| index >= blocks) {
                return Err(CfgError::new(
                    "CFG.SUCCESSOR_RANGE",
                    Some(func.blocks[block].id),
                    format!("successor index {successor} is outside the function"),
                ));
            }
        }

        let mut expected_predecessors = vec![Vec::new(); blocks];
        for (block, mir_block) in func.blocks.iter().enumerate() {
            let mut expected_successors = Vec::new();
            for successor_id in mir_block.successors() {
                let Some(&successor) = self.block_index.get(&successor_id) else {
                    return Err(CfgError::new(
                        "CFG.MIR_TARGET_EXISTS",
                        Some(mir_block.id),
                        format!("terminator targets missing block {successor_id}"),
                    ));
                };
                if !expected_successors.contains(&successor) {
                    expected_successors.push(successor);
                    expected_predecessors[successor].push(block);
                }
            }
            if self.successors[block] != expected_successors {
                return Err(CfgError::new(
                    "CFG.SUCCESSORS_MATCH_MIR",
                    Some(mir_block.id),
                    format!(
                        "normalized successors {:?} differ from MIR successors {expected_successors:?}",
                        self.successors[block]
                    ),
                ));
            }
        }
        for (block, expected) in expected_predecessors.iter().enumerate() {
            if self.predecessors[block] != *expected {
                return Err(CfgError::new(
                    "CFG.EDGE_RECIPROCITY",
                    Some(func.blocks[block].id),
                    format!(
                        "normalized predecessors {:?} differ from incoming successor edges {:?}",
                        self.predecessors[block], expected
                    ),
                ));
            }
        }
        if !self.idom.first().is_some_and(Option::is_none) {
            return Err(CfgError::new(
                "CFG.ENTRY_IDOM",
                func.blocks.first().map(|block| block.id),
                "entry block must not have an immediate dominator",
            ));
        }
        if !self.predecessors.first().is_some_and(Vec::is_empty) {
            return Err(CfgError::new(
                "CFG.ENTRY_HAS_NO_PREDECESSORS",
                func.blocks.first().map(|block| block.id),
                "entry block must be the unique predecessor-free CFG root",
            ));
        }
        if let Some((block, _)) = self
            .idom
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, idom)| idom.is_none())
        {
            return Err(CfgError::new(
                "CFG.REACHABLE_IDOM",
                Some(func.blocks[block].id),
                "reachable non-entry block has no immediate dominator",
            ));
        }
        for block in 1..blocks {
            let Some(parent) = self.idom[block] else {
                return Err(CfgError::new(
                    "CFG.REACHABLE_IDOM",
                    Some(func.blocks[block].id),
                    "reachable non-entry block has no immediate dominator",
                ));
            };
            if parent >= blocks {
                return Err(CfgError::new(
                    "CFG.IDOM_RANGE",
                    Some(func.blocks[block].id),
                    format!("immediate dominator index {parent} is outside the function"),
                ));
            }
            let mut current = block;
            let mut reaches_entry = false;
            for _ in 0..blocks {
                let Some(parent) = self.idom[current] else {
                    reaches_entry = current == 0;
                    break;
                };
                current = parent;
            }
            if !reaches_entry {
                return Err(CfgError::new(
                    "CFG.IDOM_TREE",
                    Some(func.blocks[block].id),
                    "immediate-dominator links do not form a tree rooted at entry",
                ));
            }
        }
        let expected = analyze_shared_graph(self.successors.clone())?;
        if self.idom != expected.dominators.idom {
            return Err(CfgError::new(
                "CFG.IDOM_MATCHES_GRAPH",
                None,
                "immediate-dominator table does not match the normalized graph",
            ));
        }
        if self.dominator_children != expected.dominators.children {
            return Err(CfgError::new(
                "CFG.DOMINATOR_CHILDREN_MATCH_GRAPH",
                None,
                "dominator-tree children do not match the normalized graph",
            ));
        }
        for (block, frontier) in self.dominance_frontier.iter().enumerate() {
            if frontier.iter().any(|member| *member >= blocks) {
                return Err(CfgError::new(
                    "CFG.DOMINANCE_FRONTIER_RANGE",
                    Some(func.blocks[block].id),
                    "dominance frontier contains a block outside the function",
                ));
            }
        }
        let expected_frontier = expected
            .dominance_frontier
            .iter()
            .map(|frontier| frontier.iter().copied().collect::<BTreeSet<_>>())
            .collect::<Vec<_>>();
        if self.dominance_frontier != expected_frontier {
            return Err(CfgError::new(
                "CFG.DOMINANCE_FRONTIER_MATCHES_GRAPH",
                None,
                "dominance-frontier table does not match the normalized graph",
            ));
        }

        if self.loop_for_header.len() != self.loops.len() {
            return Err(CfgError::new(
                "CFG.LOOP_HEADER_INDEX",
                None,
                "loop-header index is not a bijection over natural loops",
            ));
        }
        for (loop_index, natural_loop) in self.loops.iter().enumerate() {
            if natural_loop.header >= blocks || !natural_loop.blocks.contains(&natural_loop.header)
            {
                return Err(CfgError::new(
                    "CFG.LOOP_CONTAINS_HEADER",
                    func.blocks.get(natural_loop.header).map(|block| block.id),
                    "natural loop does not contain a valid header",
                ));
            }
            if let Some(&member) = natural_loop.blocks.iter().find(|&&member| member >= blocks) {
                return Err(CfgError::new(
                    "CFG.LOOP_MEMBER_RANGE",
                    Some(func.blocks[natural_loop.header].id),
                    format!("natural loop contains out-of-range block index {member}"),
                ));
            }
            if self.loop_for_header.get(&natural_loop.header) != Some(&loop_index) {
                return Err(CfgError::new(
                    "CFG.LOOP_HEADER_INDEX",
                    Some(func.blocks[natural_loop.header].id),
                    "loop header index does not point back to its loop",
                ));
            }
            if let Some(parent) = natural_loop.parent {
                if parent >= self.loops.len() || parent <= loop_index {
                    return Err(CfgError::new(
                        "CFG.LOOP_FOREST",
                        Some(func.blocks[natural_loop.header].id),
                        format!(
                            "loop parent {parent} must be a later valid loop than child {loop_index}"
                        ),
                    ));
                }
                if !self.loops[parent].blocks.is_superset(&natural_loop.blocks) {
                    return Err(CfgError::new(
                        "CFG.LOOP_PARENT_CONTAINS_CHILD",
                        Some(func.blocks[natural_loop.header].id),
                        "loop parent does not contain every child block",
                    ));
                }
            }
        }
        if self.loops != expected.loops {
            return Err(CfgError::new(
                "CFG.LOOPS_MATCH_GRAPH",
                None,
                "natural-loop forest does not match the normalized graph",
            ));
        }
        for (block, successors) in self.successors.iter().enumerate() {
            if successors.len() < 2 {
                continue;
            }
            for &successor in successors {
                if self.predecessors[successor].as_slice() != [block]
                    || !func.blocks[successor].phis.is_empty()
                    || !matches!(
                        func.blocks[successor].insts.as_slice(),
                        [MInst::Jump { .. }]
                    )
                {
                    return Err(CfgError::new(
                        "CFG.BRANCH_EDGE_ISOLATED",
                        Some(func.blocks[successor].id),
                        "branch successor is not a dedicated one-predecessor edge block",
                    ));
                }
            }
        }
        Ok(())
    }
}

pub(super) fn normalize(func: &mut MFunction) -> Result<NormalizedCfg, CfgError> {
    if func.blocks.is_empty() {
        return Err(CfgError::new(
            "CFG.NON_EMPTY",
            None,
            "cannot normalize an empty function",
        ));
    }
    let entry = func.blocks[0].id;
    if func
        .blocks
        .iter()
        .any(|block| block.successors().contains(&entry))
    {
        return Err(CfgError::new(
            "CFG.ENTRY_HAS_NO_PREDECESSORS",
            Some(entry),
            "entry block must be the unique predecessor-free CFG root",
        ));
    }
    split_critical_edges(func)?;
    super::reorder_blocks_rpo(func)
        .map_err(|message| CfgError::new("CFG.RPO_BIJECTION", None, message))?;
    let (block_index, _, successors) = graph(func);
    let analysis = analyze_shared_graph(successors)?;
    let dominance_frontier = analysis
        .dominance_frontier
        .iter()
        .map(|frontier| frontier.iter().copied().collect::<BTreeSet<_>>())
        .collect();
    let loop_for_header = analysis
        .loops
        .iter()
        .enumerate()
        .map(|(loop_index, natural_loop)| (natural_loop.header, loop_index))
        .collect();
    Ok(NormalizedCfg {
        block_index,
        predecessors: analysis.predecessors,
        successors: analysis.successors,
        idom: analysis.dominators.idom,
        dominator_children: analysis.dominators.children,
        dominance_frontier,
        loops: analysis.loops,
        loop_for_header,
    })
}

fn analyze_shared_graph(successors: Vec<Vec<usize>>) -> Result<ForwardControlFlowGraph, CfgError> {
    ForwardControlFlowGraph::analyze(successors, 0).map_err(|error| {
        CfgError::new(
            "CFG.SHARED_ANALYSIS",
            None,
            format!("IR-independent CFG analysis failed: {error}"),
        )
    })
}

fn split_critical_edges(func: &mut MFunction) -> Result<(), CfgError> {
    for block in &mut func.blocks {
        if let Some((true_bb, false_bb)) = block.insts.last().and_then(MInst::branch_targets)
            && true_bb == false_bb
        {
            let target = true_bb;
            if let Some(terminator) = block.insts.last_mut() {
                *terminator = MInst::Jump { target };
            }
        }
    }
    let (block_index, predecessors, _) = graph(func);
    let mut edges = Vec::<(BlockId, BlockId)>::new();
    for predecessor in &func.blocks {
        let mut successors = predecessor.successors();
        successors.sort();
        successors.dedup();
        if successors.len() < 2 {
            continue;
        }
        for successor in successors {
            // Every branch edge gets a dedicated insertion block.  Critical
            // edge splitting alone is insufficient for edge-local spill and
            // parallel-copy operations when the successor has one predecessor.
            let successor_index = block_index[&successor];
            let successor_block = &func.blocks[successor_index];
            let already_edge_block = predecessors[successor_index].len() == 1
                && successor_block.phis.is_empty()
                && matches!(successor_block.insts.as_slice(), [MInst::Jump { .. }]);
            if already_edge_block {
                continue;
            }
            edges.push((predecessor.id, successor));
        }
    }
    if edges.is_empty() {
        return Ok(());
    }

    let Some(mut next_id) = func
        .blocks
        .iter()
        .map(|block| block.id.0)
        .max()
        .unwrap_or(0)
        .checked_add(1)
    else {
        return Err(CfgError::new(
            "CFG.BLOCK_ID_RANGE",
            None,
            "BlockId overflow while splitting branch edges",
        ));
    };
    for (predecessor, successor) in edges {
        let edge = BlockId(next_id);
        let Some(next) = next_id.checked_add(1) else {
            return Err(CfgError::new(
                "CFG.BLOCK_ID_RANGE",
                Some(predecessor),
                "BlockId overflow while splitting branch edges",
            ));
        };
        next_id = next;
        // New blocks are appended, so every original block keeps the index
        // recorded by the graph built above.  Looking both endpoints up in that
        // index avoids an O(blocks) scan for every split branch edge.
        let predecessor_index = block_index[&predecessor];
        let Some(terminator) = func.blocks[predecessor_index].insts.last_mut() else {
            return Err(CfgError::new(
                "CFG.EDGE_PREDECESSOR_TERMINATED",
                Some(predecessor),
                "branch-edge predecessor has no terminator",
            ));
        };
        rewrite_target(terminator, successor, edge, predecessor)?;
        let successor_index = block_index[&successor];
        for phi in &mut func.blocks[successor_index].phis {
            let Some(source) = phi
                .sources
                .iter_mut()
                .find(|(source_predecessor, _)| *source_predecessor == predecessor)
            else {
                return Err(CfgError::new(
                    "CFG.PHI_COVERS_SPLIT_EDGE",
                    Some(successor),
                    "phi has no source for branch edge being split",
                ));
            };
            source.0 = edge;
        }
        let mut edge_block = MBlock::new(edge);
        edge_block.push(MInst::Jump { target: successor });
        func.blocks.push(edge_block);
    }
    Ok(())
}

fn rewrite_target(
    terminator: &mut MInst,
    old: BlockId,
    new: BlockId,
    predecessor: BlockId,
) -> Result<(), CfgError> {
    let mut rewritten = false;
    terminator.rewrite_successors(|target| {
        if target == old {
            rewritten = true;
            new
        } else {
            target
        }
    });
    if !rewritten {
        return Err(CfgError::new(
            "CFG.TERMINATOR_NAMES_SPLIT_EDGE",
            Some(predecessor),
            "branch edge is not named by predecessor terminator",
        ));
    }
    Ok(())
}

fn graph(func: &MFunction) -> (HashMap<BlockId, usize>, Vec<Vec<usize>>, Vec<Vec<usize>>) {
    let block_index = func
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.id, index))
        .collect::<HashMap<_, _>>();
    let mut predecessors = vec![Vec::new(); func.blocks.len()];
    let mut successors = vec![Vec::new(); func.blocks.len()];
    for (index, block) in func.blocks.iter().enumerate() {
        for successor in block.successors() {
            let successor = block_index[&successor];
            if !successors[index].contains(&successor) {
                successors[index].push(successor);
                predecessors[successor].push(index);
            }
        }
    }
    (block_index, predecessors, successors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::mir::{PhiNode, SpillDesc, VRegAllocator};

    fn two_block_cfg() -> (MFunction, NormalizedCfg) {
        let mut func = MFunction::new(VRegAllocator::new(), Vec::new());
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::Jump { target: BlockId(1) });
        let mut exit = MBlock::new(BlockId(1));
        exit.push(MInst::Return);
        func.blocks = vec![entry, exit];
        func.verify_result().unwrap();
        let cfg = normalize(&mut func).unwrap();
        (func, cfg)
    }

    fn natural_loop_cfg() -> (MFunction, NormalizedCfg) {
        let mut vregs = VRegAllocator::new();
        let condition = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient()]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: condition,
            value: 1,
        });
        entry.push(MInst::Jump { target: BlockId(1) });
        let mut header = MBlock::new(BlockId(1));
        header.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(2),
            false_bb: BlockId(3),
        });
        let mut body = MBlock::new(BlockId(2));
        body.push(MInst::Jump { target: BlockId(1) });
        let mut exit = MBlock::new(BlockId(3));
        exit.push(MInst::Return);
        func.blocks = vec![entry, header, body, exit];
        func.verify_result().unwrap();
        let cfg = normalize(&mut func).unwrap();
        assert!(!cfg.loops.is_empty());
        (func, cfg)
    }

    #[test]
    fn malformed_block_index_is_a_structured_error() {
        let (func, mut cfg) = two_block_cfg();
        cfg.block_index.insert(func.blocks[0].id, 1);

        let error = cfg.verify(&func).unwrap_err();

        assert_eq!(error.rule, "CFG.BLOCK_INDEX_BIJECTION");
        assert_eq!(error.block, Some(func.blocks[0].id));
    }

    #[test]
    fn out_of_range_predecessor_is_a_structured_error() {
        let (func, mut cfg) = two_block_cfg();
        cfg.predecessors[1].push(func.blocks.len());

        let error = cfg.verify(&func).unwrap_err();

        assert_eq!(error.rule, "CFG.PREDECESSOR_RANGE");
        assert_eq!(error.block, Some(func.blocks[1].id));
    }

    #[test]
    fn nonreciprocal_edge_is_a_structured_error() {
        let (func, mut cfg) = two_block_cfg();
        cfg.predecessors[1].clear();

        let error = cfg.verify(&func).unwrap_err();

        assert_eq!(error.rule, "CFG.EDGE_RECIPROCITY");
        assert_eq!(error.block, Some(func.blocks[1].id));
    }

    #[test]
    fn out_of_range_idom_is_a_structured_error() {
        let (func, mut cfg) = two_block_cfg();
        cfg.idom[1] = Some(func.blocks.len());

        let error = cfg.verify(&func).unwrap_err();

        assert_eq!(error.rule, "CFG.IDOM_RANGE");
        assert_eq!(error.block, Some(func.blocks[1].id));
    }

    #[test]
    fn incorrect_dominance_frontier_is_a_structured_error() {
        let (func, mut cfg) = two_block_cfg();
        cfg.dominance_frontier[0].insert(1);

        let error = cfg.verify(&func).unwrap_err();

        assert_eq!(error.rule, "CFG.DOMINANCE_FRONTIER_MATCHES_GRAPH");
    }

    #[test]
    fn out_of_range_loop_member_is_a_structured_error() {
        let (func, mut cfg) = natural_loop_cfg();
        cfg.loops[0].blocks.insert(func.blocks.len());

        let error = cfg.verify(&func).unwrap_err();

        assert_eq!(error.rule, "CFG.LOOP_MEMBER_RANGE");
    }

    #[test]
    fn cyclic_loop_parent_is_a_structured_error() {
        let (func, mut cfg) = natural_loop_cfg();
        cfg.loops[0].parent = Some(0);

        let error = cfg.verify(&func).unwrap_err();

        assert_eq!(error.rule, "CFG.LOOP_FOREST");
    }

    #[test]
    fn non_bijective_rpo_input_is_a_structured_error() {
        let mut func = MFunction::new(VRegAllocator::new(), Vec::new());
        let mut first = MBlock::new(BlockId(0));
        first.push(MInst::Return);
        let mut duplicate = MBlock::new(BlockId(0));
        duplicate.push(MInst::Return);
        func.blocks = vec![first, duplicate];

        let error = normalize(&mut func).unwrap_err();

        assert_eq!(error.rule, "CFG.RPO_BIJECTION");
    }

    #[test]
    fn branch_edge_block_id_overflow_is_a_structured_error() {
        let mut vregs = VRegAllocator::new();
        let condition = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient()]);
        let mut entry = MBlock::new(BlockId(u32::MAX));
        entry.push(MInst::LoadImm {
            dst: condition,
            value: 1,
        });
        entry.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::Return);
        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::Return);
        func.blocks = vec![entry, left, right];
        func.verify_result().unwrap();

        let error = normalize(&mut func).unwrap_err();

        assert_eq!(error.rule, "CFG.BLOCK_ID_RANGE");
        assert_eq!(error.block, None);
    }

    #[test]
    fn entry_predecessors_are_a_structured_error_before_normalization() {
        let mut vregs = VRegAllocator::new();
        let condition = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient()]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: condition,
            value: 1,
        });
        entry.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::Jump { target: BlockId(0) });
        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::Jump { target: BlockId(0) });
        func.blocks = vec![entry, left, right];

        let original = func
            .blocks
            .iter()
            .map(|block| (block.id, block.successors()))
            .collect::<Vec<_>>();
        let error = normalize(&mut func).unwrap_err();

        assert_eq!(error.rule, "CFG.ENTRY_HAS_NO_PREDECESSORS");
        assert_eq!(error.block, Some(BlockId(0)));
        assert_eq!(
            func.blocks
                .iter()
                .map(|block| (block.id, block.successors()))
                .collect::<Vec<_>>(),
            original,
            "rejection must precede CFG mutation"
        );
    }

    #[test]
    fn splits_critical_edge_and_rewrites_phi_predecessor() {
        let mut vregs = VRegAllocator::new();
        let condition = vregs.alloc();
        let left = vregs.alloc();
        let right = vregs.alloc();
        let merged = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);

        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: condition,
            value: 1,
        });
        entry.push(MInst::LoadImm {
            dst: left,
            value: 2,
        });
        entry.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut other = MBlock::new(BlockId(1));
        other.push(MInst::LoadImm {
            dst: right,
            value: 3,
        });
        other.push(MInst::Jump { target: BlockId(3) });
        let mut critical_pred = MBlock::new(BlockId(2));
        critical_pred.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(3),
            false_bb: BlockId(4),
        });
        let mut join = MBlock::new(BlockId(3));
        join.phis.push(PhiNode {
            dst: merged,
            sources: vec![(BlockId(1), right), (BlockId(2), left)],
        });
        join.push(MInst::Return);
        let mut exit = MBlock::new(BlockId(4));
        exit.push(MInst::Return);
        func.blocks = vec![entry, other, critical_pred, join, exit];

        let cfg = normalize(&mut func).unwrap();
        assert_eq!(func.blocks.len(), 9);
        let entry = cfg.block_index[&BlockId(0)];
        for &successor in &cfg.successors[entry] {
            assert_eq!(
                edge_insertion_point(&func, &cfg, entry, successor),
                Some(EdgeInsertionPoint {
                    block: successor,
                    instruction: 0,
                })
            );
        }
        let join = &func.blocks[cfg.block_index[&BlockId(3)]];
        let split_predecessor = join.phis[0]
            .sources
            .iter()
            .find(|(_, value)| *value == left)
            .unwrap()
            .0;
        assert_ne!(split_predecessor, BlockId(2));
        assert_eq!(cfg.predecessors[cfg.block_index[&BlockId(3)]].len(), 2);
    }
}
