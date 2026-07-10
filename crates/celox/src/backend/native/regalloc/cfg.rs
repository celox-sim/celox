//! Normalized CFG information shared by every allocation phase.

use std::collections::{BTreeSet, HashMap};

use crate::backend::native::mir::{BlockId, MBlock, MFunction, MInst};

#[derive(Debug)]
pub(super) struct NaturalLoop {
    pub header: usize,
    pub blocks: BTreeSet<usize>,
    pub parent: Option<usize>,
}

#[derive(Debug)]
pub(super) struct NormalizedCfg {
    pub block_index: HashMap<BlockId, usize>,
    pub predecessors: Vec<Vec<usize>>,
    pub successors: Vec<Vec<usize>>,
    pub idom: Vec<Option<usize>>,
    pub dominance_frontier: Vec<BTreeSet<usize>>,
    pub loops: Vec<NaturalLoop>,
    pub loop_for_header: HashMap<usize, usize>,
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
        for block in 0..blocks {
            if self.predecessors[block] != expected_predecessors[block] {
                return Err(CfgError::new(
                    "CFG.EDGE_RECIPROCITY",
                    Some(func.blocks[block].id),
                    format!(
                        "normalized predecessors {:?} differ from incoming successor edges {:?}",
                        self.predecessors[block], expected_predecessors[block]
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
        let expected_idom = immediate_dominators(&self.predecessors)?;
        if self.idom != expected_idom {
            return Err(CfgError::new(
                "CFG.IDOM_MATCHES_GRAPH",
                None,
                "immediate-dominator table does not match the normalized graph",
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
        let expected_frontier = dominance_frontiers(&self.predecessors, &self.idom)?;
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
    let (block_index, predecessors, successors) = graph(func);
    let idom = immediate_dominators(&predecessors)?;
    let dominance_frontier = dominance_frontiers(&predecessors, &idom)?;
    let loops = natural_loops(&predecessors, &successors, &idom)?;
    let loop_for_header = loops
        .iter()
        .enumerate()
        .map(|(loop_index, natural_loop)| (natural_loop.header, loop_index))
        .collect();
    Ok(NormalizedCfg {
        block_index,
        predecessors,
        successors,
        idom,
        dominance_frontier,
        loops,
        loop_for_header,
    })
}

fn split_critical_edges(func: &mut MFunction) -> Result<(), CfgError> {
    for block in &mut func.blocks {
        if let Some(MInst::Branch {
            true_bb, false_bb, ..
        }) = block.insts.last()
            && true_bb == false_bb
        {
            let target = *true_bb;
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
    match terminator {
        MInst::Branch {
            true_bb, false_bb, ..
        } => {
            if *true_bb == old {
                *true_bb = new;
            }
            if *false_bb == old {
                *false_bb = new;
            }
        }
        MInst::Jump { target } if *target == old => *target = new,
        _ => {
            return Err(CfgError::new(
                "CFG.TERMINATOR_NAMES_SPLIT_EDGE",
                Some(predecessor),
                "branch edge is not named by predecessor terminator",
            ));
        }
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

fn immediate_dominators(predecessors: &[Vec<usize>]) -> Result<Vec<Option<usize>>, CfgError> {
    if predecessors.is_empty() {
        return Err(CfgError::new(
            "CFG.NON_EMPTY",
            None,
            "cannot construct dominators for an empty graph",
        ));
    }
    if predecessors
        .iter()
        .flatten()
        .any(|predecessor| *predecessor >= predecessors.len())
    {
        return Err(CfgError::new(
            "CFG.PREDECESSOR_RANGE",
            None,
            "cannot construct dominators with an out-of-range predecessor",
        ));
    }
    let mut idom = vec![None; predecessors.len()];
    idom[0] = Some(0);
    loop {
        let mut changed = false;
        for block in 1..predecessors.len() {
            let mut processed = predecessors[block]
                .iter()
                .copied()
                .filter(|predecessor| idom[*predecessor].is_some());
            let Some(first) = processed.next() else {
                continue;
            };
            let mut next = first;
            for predecessor in processed {
                next = intersect(next, predecessor, &idom)?;
            }
            if idom[block] != Some(next) {
                idom[block] = Some(next);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    idom[0] = None;
    Ok(idom)
}

fn intersect(mut left: usize, mut right: usize, idom: &[Option<usize>]) -> Result<usize, CfgError> {
    while left != right {
        while left > right {
            let Some(parent) = idom.get(left).copied().flatten() else {
                return Err(CfgError::new(
                    "CFG.DOMINATOR_STATE",
                    None,
                    format!("processed block index {left} has no immediate dominator"),
                ));
            };
            left = parent;
        }
        while right > left {
            let Some(parent) = idom.get(right).copied().flatten() else {
                return Err(CfgError::new(
                    "CFG.DOMINATOR_STATE",
                    None,
                    format!("processed block index {right} has no immediate dominator"),
                ));
            };
            right = parent;
        }
    }
    Ok(left)
}

fn dominance_frontiers(
    predecessors: &[Vec<usize>],
    idom: &[Option<usize>],
) -> Result<Vec<BTreeSet<usize>>, CfgError> {
    if predecessors.len() != idom.len() {
        return Err(CfgError::new(
            "CFG.MODEL_SHAPE",
            None,
            "predecessor and dominator tables have different lengths",
        ));
    }
    let mut frontiers = vec![BTreeSet::new(); predecessors.len()];
    for block in 0..predecessors.len() {
        if predecessors[block].len() < 2 {
            continue;
        }
        let Some(immediate) = idom[block] else {
            return Err(CfgError::new(
                "CFG.DOMINANCE_FRONTIER_STATE",
                None,
                format!("join block index {block} has no immediate dominator"),
            ));
        };
        for &predecessor in &predecessors[block] {
            let mut runner = predecessor;
            let mut steps = 0usize;
            while runner != immediate {
                let Some(frontier) = frontiers.get_mut(runner) else {
                    return Err(CfgError::new(
                        "CFG.PREDECESSOR_RANGE",
                        None,
                        format!("join predecessor index {runner} is outside the graph"),
                    ));
                };
                frontier.insert(block);
                let Some(parent) = idom.get(runner).copied().flatten() else {
                    return Err(CfgError::new(
                        "CFG.DOMINANCE_FRONTIER_STATE",
                        None,
                        format!("join predecessor index {runner} is not dominated by the entry"),
                    ));
                };
                runner = parent;
                steps += 1;
                if steps > idom.len() {
                    return Err(CfgError::new(
                        "CFG.IDOM_TREE",
                        None,
                        "immediate-dominator links contain a cycle",
                    ));
                }
            }
        }
    }
    Ok(frontiers)
}

fn natural_loops(
    predecessors: &[Vec<usize>],
    successors: &[Vec<usize>],
    idom: &[Option<usize>],
) -> Result<Vec<NaturalLoop>, CfgError> {
    Ok(natural_loops_with_work(predecessors, successors, idom)?.0)
}

#[derive(Default)]
struct NaturalLoopWork {
    #[cfg(test)]
    dominator_nodes: usize,
    #[cfg(test)]
    dominance_queries: usize,
    #[cfg(test)]
    backedges: usize,
    #[cfg(test)]
    reverse_blocks_inserted: usize,
    #[cfg(test)]
    predecessor_edges_visited: usize,
    #[cfg(test)]
    parent_header_lookups: usize,
    #[cfg(test)]
    parent_membership_updates: usize,
}

impl NaturalLoopWork {
    fn record_dominator_node(&mut self) {
        #[cfg(test)]
        {
            self.dominator_nodes += 1;
        }
    }

    fn record_dominance_query(&mut self) {
        #[cfg(test)]
        {
            self.dominance_queries += 1;
        }
    }

    fn record_backedge(&mut self) {
        #[cfg(test)]
        {
            self.backedges += 1;
        }
    }

    fn record_reverse_block(&mut self) {
        #[cfg(test)]
        {
            self.reverse_blocks_inserted += 1;
        }
    }

    fn record_predecessor_edges(&mut self, edges: usize) {
        #[cfg(test)]
        {
            self.predecessor_edges_visited += edges;
        }
        #[cfg(not(test))]
        let _ = edges;
    }

    fn record_parent_header_lookup(&mut self) {
        #[cfg(test)]
        {
            self.parent_header_lookups += 1;
        }
    }

    fn record_parent_membership_update(&mut self) {
        #[cfg(test)]
        {
            self.parent_membership_updates += 1;
        }
    }
}

/// Dominator-tree preorder intervals.  Dominance is an O(1) containment query,
/// and the iterative event walk is safe for arbitrarily deep CFGs.
struct DominanceIntervals {
    preorder: Vec<usize>,
    subtree_end: Vec<usize>,
}

impl DominanceIntervals {
    fn build(idom: &[Option<usize>], work: &mut NaturalLoopWork) -> Self {
        let mut children = vec![Vec::<usize>::new(); idom.len()];
        for (block, parent) in idom.iter().enumerate().skip(1) {
            if let Some(parent) = parent {
                children[*parent].push(block);
            }
        }

        enum Event {
            Enter(usize),
            Exit(usize),
        }
        let mut preorder = vec![usize::MAX; idom.len()];
        let mut subtree_end = vec![usize::MAX; idom.len()];
        let mut next_preorder = 0usize;
        let mut events = (!idom.is_empty())
            .then_some(Event::Enter(0))
            .into_iter()
            .collect::<Vec<_>>();
        while let Some(event) = events.pop() {
            match event {
                Event::Enter(block) => {
                    preorder[block] = next_preorder;
                    next_preorder += 1;
                    work.record_dominator_node();
                    events.push(Event::Exit(block));
                    events.extend(children[block].iter().rev().copied().map(Event::Enter));
                }
                Event::Exit(block) => subtree_end[block] = next_preorder,
            }
        }
        Self {
            preorder,
            subtree_end,
        }
    }

    fn dominates(&self, dominator: usize, block: usize) -> bool {
        let dominator_preorder = self.preorder[dominator];
        let block_preorder = self.preorder[block];
        dominator_preorder != usize::MAX
            && block_preorder != usize::MAX
            && dominator_preorder <= block_preorder
            && block_preorder < self.subtree_end[dominator]
    }
}

/// Construct natural loops in O(B + E + output log B), then derive the loop
/// forest in O(output) rather than comparing every pair of loop block sets.
///
/// Natural loops form a laminar family after backedges with the same header are
/// merged.  Processing that family outer-to-inner means the innermost loop
/// currently recorded for a child's header is exactly its immediate parent.
/// `output` is the total number of materialized `(loop, block)` memberships,
/// which the public `NaturalLoop::blocks` representation necessarily stores.
fn natural_loops_with_work(
    predecessors: &[Vec<usize>],
    successors: &[Vec<usize>],
    idom: &[Option<usize>],
) -> Result<(Vec<NaturalLoop>, NaturalLoopWork), CfgError> {
    let mut work = NaturalLoopWork::default();
    let dominance = DominanceIntervals::build(idom, &mut work);
    let mut by_header = (0..successors.len())
        .map(|_| None::<BTreeSet<usize>>)
        .collect::<Vec<_>>();
    for (tail, tail_successors) in successors.iter().enumerate() {
        for &header in tail_successors {
            work.record_dominance_query();
            if !dominance.dominates(header, tail) {
                continue;
            }
            work.record_backedge();
            let blocks = by_header[header].get_or_insert_with(BTreeSet::new);
            if blocks.insert(header) {
                work.record_reverse_block();
            }
            let mut stack = vec![tail];
            while let Some(block) = stack.pop() {
                if blocks.insert(block) {
                    work.record_reverse_block();
                    work.record_predecessor_edges(predecessors[block].len());
                    stack.extend(predecessors[block].iter().copied());
                }
            }
        }
    }
    let mut loops = by_header
        .into_iter()
        .enumerate()
        .filter_map(|(header, blocks)| {
            blocks.map(|blocks| NaturalLoop {
                header,
                blocks,
                parent: None,
            })
        })
        .collect::<Vec<_>>();
    loops.sort_by_key(|natural_loop| (natural_loop.blocks.len(), natural_loop.header));

    let mut innermost_for_block = vec![None::<usize>; successors.len()];
    for child in (0..loops.len()).rev() {
        work.record_parent_header_lookup();
        let parent = innermost_for_block[loops[child].header];
        if parent.is_some_and(|parent| !loops[parent].blocks.is_superset(&loops[child].blocks)) {
            return Err(CfgError::new(
                "CFG.LOOP_FOREST",
                None,
                format!("natural loop {child} overlaps its candidate parent without nesting"),
            ));
        }
        loops[child].parent = parent;
        for &block in &loops[child].blocks {
            innermost_for_block[block] = Some(child);
            work.record_parent_membership_update();
        }
    }
    Ok((loops, work))
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

    #[test]
    fn deep_acyclic_cfg_uses_one_interval_query_per_edge() {
        const BLOCKS: usize = 20_000;

        let mut predecessors = vec![Vec::new(); BLOCKS];
        let mut successors = vec![Vec::new(); BLOCKS];
        let mut idom = vec![None; BLOCKS];
        for block in 1..BLOCKS {
            predecessors[block].push(block - 1);
            successors[block - 1].push(block);
            idom[block] = Some(block - 1);
        }

        let (loops, work) = natural_loops_with_work(&predecessors, &successors, &idom).unwrap();

        assert!(loops.is_empty());
        assert_eq!(work.dominator_nodes, BLOCKS);
        assert_eq!(work.dominance_queries, BLOCKS - 1);
        assert_eq!(work.backedges, 0);
        assert_eq!(work.reverse_blocks_inserted, 0);
        assert_eq!(work.predecessor_edges_visited, 0);
        assert_eq!(work.parent_header_lookups, 0);
        assert_eq!(work.parent_membership_updates, 0);
    }

    #[test]
    fn multiple_backedges_to_one_header_form_their_union() {
        let predecessors = vec![vec![], vec![0, 2, 3], vec![1], vec![1]];
        let successors = vec![vec![1], vec![2, 3], vec![1], vec![1]];
        let idom = vec![None, Some(0), Some(1), Some(1)];

        let (loops, work) = natural_loops_with_work(&predecessors, &successors, &idom).unwrap();

        assert_eq!(loops.len(), 1);
        assert_eq!(loops[0].header, 1);
        assert_eq!(loops[0].blocks, BTreeSet::from([1, 2, 3]));
        assert_eq!(loops[0].parent, None);
        assert_eq!(work.dominance_queries, 5);
        assert_eq!(work.backedges, 2);
        assert_eq!(work.reverse_blocks_inserted, 3);
        assert_eq!(work.parent_header_lookups, 1);
        assert_eq!(work.parent_membership_updates, 3);
    }

    #[test]
    fn deeply_nested_loop_forest_is_output_linear_not_all_pairs() {
        const DEPTH: usize = 512;
        const BLOCKS: usize = DEPTH + 1;

        // A dominator chain followed by one tail edge to every header creates
        // DEPTH perfectly nested natural loops.  Materializing their block sets
        // necessarily produces DEPTH * (DEPTH + 1) / 2 memberships; parent
        // discovery must add only one lookup per loop, not DEPTH^2 set tests.
        let mut predecessors = vec![Vec::new(); BLOCKS];
        let mut successors = vec![Vec::new(); BLOCKS];
        let mut idom = vec![None; BLOCKS];
        for block in 1..BLOCKS {
            predecessors[block].push(block - 1);
            successors[block - 1].push(block);
            idom[block] = Some(block - 1);
        }
        let tail = DEPTH;
        for header in 1..=DEPTH {
            successors[tail].push(header);
            predecessors[header].push(tail);
        }

        let (loops, work) = natural_loops_with_work(&predecessors, &successors, &idom).unwrap();

        let memberships = DEPTH * (DEPTH + 1) / 2;
        assert_eq!(loops.len(), DEPTH);
        assert_eq!(work.dominator_nodes, BLOCKS);
        assert_eq!(work.dominance_queries, DEPTH * 2);
        assert_eq!(work.backedges, DEPTH);
        assert_eq!(work.reverse_blocks_inserted, memberships);
        assert!(work.predecessor_edges_visited <= memberships * 2);
        assert_eq!(work.parent_header_lookups, DEPTH);
        assert_eq!(work.parent_membership_updates, memberships);
        for child in 0..DEPTH - 1 {
            assert_eq!(loops[child].parent, Some(child + 1));
            assert_eq!(loops[child].header, DEPTH - child);
        }
        assert_eq!(loops[DEPTH - 1].header, 1);
        assert_eq!(loops[DEPTH - 1].parent, None);
    }
}
