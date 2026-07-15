//! Shared control-flow analysis for SIR execution units.
//!
//! The optimizer used to carry several pass-local CFG implementations.  This
//! module builds one deterministic reachable graph and derives dominators,
//! post-dominators, frontiers, control dependence, SCCs, and natural loops.
//! All graph walks are iterative so generated RTL cannot overflow the host
//! stack merely by containing a deep control-flow chain.

use std::collections::BTreeSet;
use std::fmt;

use crate::{HashMap, HashSet};

use super::{BlockId, ExecutionUnit, SIRTerminator};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SirCfgError {
    Empty,
    MissingEntry(BlockId),
    BlockIdentity { key: BlockId, embedded: BlockId },
    MissingTarget { source: BlockId, target: BlockId },
    Unreachable(Vec<BlockId>),
    InvalidGraph(&'static str),
}

impl fmt::Display for SirCfgError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("SIR CFG has no blocks"),
            Self::MissingEntry(entry) => write!(formatter, "SIR CFG entry b{} is absent", entry.0),
            Self::BlockIdentity { key, embedded } => write!(
                formatter,
                "SIR block map key b{} contains block b{}",
                key.0, embedded.0
            ),
            Self::MissingTarget { source, target } => write!(
                formatter,
                "SIR block b{} targets absent block b{}",
                source.0, target.0
            ),
            Self::Unreachable(blocks) => {
                formatter.write_str("SIR CFG contains unreachable blocks:")?;
                for block in blocks {
                    write!(formatter, " b{}", block.0)?;
                }
                Ok(())
            }
            Self::InvalidGraph(message) => write!(formatter, "invalid SIR CFG: {message}"),
        }
    }
}

impl std::error::Error for SirCfgError {}

#[derive(Debug, Clone)]
#[allow(dead_code)] // Queries are consumed incrementally as SIR passes migrate.
pub(crate) struct DominatorTree {
    pub idom: Vec<Option<usize>>,
    pub children: Vec<Vec<usize>>,
    enter: Vec<usize>,
    exit: Vec<usize>,
    depth: Vec<usize>,
}

#[allow(dead_code)]
impl DominatorTree {
    fn compute(successors: &[Vec<usize>], root: usize) -> Result<Self, SirCfgError> {
        let idom = lengauer_tarjan(successors, root)?;
        Self::from_idom(idom, root)
    }

    fn from_idom(idom: Vec<Option<usize>>, root: usize) -> Result<Self, SirCfgError> {
        if root >= idom.len() {
            return Err(SirCfgError::InvalidGraph("dominator root is out of range"));
        }
        let mut children = vec![Vec::new(); idom.len()];
        for (block, parent) in idom.iter().copied().enumerate() {
            if block == root {
                if parent.is_some() {
                    return Err(SirCfgError::InvalidGraph(
                        "dominator root has an immediate dominator",
                    ));
                }
                continue;
            }
            if let Some(parent) = parent {
                let Some(parent_children) = children.get_mut(parent) else {
                    return Err(SirCfgError::InvalidGraph(
                        "immediate dominator is out of range",
                    ));
                };
                parent_children.push(block);
            }
        }
        for block_children in &mut children {
            block_children.sort_unstable();
        }

        enum Event {
            Enter(usize, usize),
            Exit(usize),
        }
        let mut enter = vec![usize::MAX; idom.len()];
        let mut exit = vec![usize::MAX; idom.len()];
        let mut depth = vec![usize::MAX; idom.len()];
        let mut time = 0usize;
        let mut events = vec![Event::Enter(root, 0)];
        while let Some(event) = events.pop() {
            match event {
                Event::Enter(block, block_depth) => {
                    enter[block] = time;
                    depth[block] = block_depth;
                    time += 1;
                    events.push(Event::Exit(block));
                    events.extend(
                        children[block]
                            .iter()
                            .rev()
                            .copied()
                            .map(|child| Event::Enter(child, block_depth + 1)),
                    );
                }
                Event::Exit(block) => {
                    exit[block] = time;
                    time += 1;
                }
            }
        }
        Ok(Self {
            idom,
            children,
            enter,
            exit,
            depth,
        })
    }

    pub(crate) fn dominates(&self, dominator: usize, block: usize) -> bool {
        let (Some(&dominator_enter), Some(&dominator_exit), Some(&block_enter), Some(&block_exit)) = (
            self.enter.get(dominator),
            self.exit.get(dominator),
            self.enter.get(block),
            self.exit.get(block),
        ) else {
            return false;
        };
        dominator_enter != usize::MAX
            && block_enter != usize::MAX
            && dominator_enter <= block_enter
            && block_exit <= dominator_exit
    }

    pub(crate) fn lca(&self, left: usize, right: usize) -> Option<usize> {
        let (Some(&left_depth), Some(&right_depth)) = (self.depth.get(left), self.depth.get(right))
        else {
            return None;
        };
        if left_depth == usize::MAX || right_depth == usize::MAX {
            return None;
        }
        let mut left = left;
        let mut right = right;
        while self.depth[left] > self.depth[right] {
            left = self.idom[left]?;
        }
        while self.depth[right] > self.depth[left] {
            right = self.idom[right]?;
        }
        while left != right {
            left = self.idom[left]?;
            right = self.idom[right]?;
        }
        Some(left)
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // Branch/control placement migrates in a later plan step.
pub(crate) struct PostDominatorTree {
    tree: DominatorTree,
    virtual_exit: usize,
    original_blocks: usize,
}

#[allow(dead_code)]
impl PostDominatorTree {
    pub(crate) fn postdominates(&self, postdominator: usize, block: usize) -> bool {
        postdominator < self.original_blocks
            && block < self.original_blocks
            && self.tree.dominates(postdominator, block)
    }

    pub(crate) fn common_postdominator(&self, left: usize, right: usize) -> Option<usize> {
        let candidate = self.tree.lca(left, right)?;
        (candidate != self.virtual_exit && candidate < self.original_blocks).then_some(candidate)
    }

    pub(crate) fn immediate_postdominator(&self, block: usize) -> Option<usize> {
        let parent = *self.tree.idom.get(block)?.as_ref()?;
        (parent != self.virtual_exit && parent < self.original_blocks).then_some(parent)
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // Exposed now so StateSSA and placement share one SCC identity.
pub(crate) struct StronglyConnectedRegion {
    pub blocks: Vec<usize>,
    pub entries: Vec<usize>,
    pub cyclic: bool,
    pub reducible_header: Option<usize>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // Loop consumers migrate after the MemorySSA CFG migration.
pub(crate) struct NaturalLoop {
    pub header: usize,
    pub blocks: BTreeSet<usize>,
    pub parent: Option<usize>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // Some analyses are intentionally ahead of their first consumer.
pub(crate) struct SirCfg {
    /// Reachable blocks in deterministic reverse postorder. The entry is 0.
    pub block_ids: Vec<BlockId>,
    pub index: HashMap<BlockId, usize>,
    pub predecessors: Vec<Vec<usize>>,
    pub successors: Vec<Vec<usize>>,
    pub dominators: DominatorTree,
    pub dom_children: Vec<Vec<usize>>,
    pub dominance_frontier: Vec<Vec<usize>>,
    pub postdominators: PostDominatorTree,
    pub postdominance_frontier: Vec<Vec<usize>>,
    /// For each block, branches on which its execution is control-dependent.
    pub controllers: Vec<Vec<usize>>,
    /// For each branch, blocks whose execution it controls.
    pub control_dependents: Vec<Vec<usize>>,
    pub sccs: Vec<StronglyConnectedRegion>,
    pub scc_for_block: Vec<usize>,
    pub loops: Vec<NaturalLoop>,
}

#[allow(dead_code)]
impl SirCfg {
    pub(crate) fn analyze<A>(eu: &ExecutionUnit<A>) -> Result<Self, SirCfgError> {
        if eu.blocks.is_empty() {
            return Err(SirCfgError::Empty);
        }
        if !eu.blocks.contains_key(&eu.entry_block_id) {
            return Err(SirCfgError::MissingEntry(eu.entry_block_id));
        }

        let mut all_blocks = eu.blocks.keys().copied().collect::<Vec<_>>();
        all_blocks.sort_unstable();
        let mut successor_ids = HashMap::<BlockId, Vec<BlockId>>::default();
        for &block_id in &all_blocks {
            let block = &eu.blocks[&block_id];
            if block.id != block_id {
                return Err(SirCfgError::BlockIdentity {
                    key: block_id,
                    embedded: block.id,
                });
            }
            let mut outgoing = terminator_successors(&block.terminator);
            outgoing.sort_unstable();
            outgoing.dedup();
            for &target in &outgoing {
                if !eu.blocks.contains_key(&target) {
                    return Err(SirCfgError::MissingTarget {
                        source: block_id,
                        target,
                    });
                }
            }
            successor_ids.insert(block_id, outgoing);
        }

        // Iterative DFS with an explicit successor cursor records exact
        // postorder without recursion or duplicate stack entries.
        let mut visited = HashSet::default();
        let mut postorder = Vec::with_capacity(all_blocks.len());
        visited.insert(eu.entry_block_id);
        let mut stack = vec![(eu.entry_block_id, 0usize)];
        while let Some((block, next_successor)) = stack.last_mut() {
            let outgoing = &successor_ids[block];
            if *next_successor == outgoing.len() {
                postorder.push(*block);
                stack.pop();
                continue;
            }
            let successor = outgoing[*next_successor];
            *next_successor += 1;
            if visited.insert(successor) {
                stack.push((successor, 0));
            }
        }
        if visited.len() != all_blocks.len() {
            let unreachable = all_blocks
                .into_iter()
                .filter(|block| !visited.contains(block))
                .collect();
            return Err(SirCfgError::Unreachable(unreachable));
        }
        postorder.reverse();
        let block_ids = postorder;
        let index = block_ids
            .iter()
            .copied()
            .enumerate()
            .map(|(index, block)| (block, index))
            .collect::<HashMap<_, _>>();
        let mut successors = vec![Vec::new(); block_ids.len()];
        let mut predecessors = vec![Vec::new(); block_ids.len()];
        for (block, &block_id) in block_ids.iter().enumerate() {
            for successor_id in &successor_ids[&block_id] {
                let successor = index[successor_id];
                successors[block].push(successor);
                predecessors[successor].push(block);
            }
        }
        for edges in successors.iter_mut().chain(&mut predecessors) {
            edges.sort_unstable();
            edges.dedup();
        }

        let dominators = DominatorTree::compute(&successors, 0)?;
        if dominators.idom.iter().skip(1).any(Option::is_none) {
            return Err(SirCfgError::InvalidGraph(
                "reachable block has no immediate dominator",
            ));
        }
        let dom_children = dominators.children.clone();
        let dominance_frontier = dominance_frontiers(&successors, &dominators, 0);
        let (postdominators, postdominance_frontier) =
            build_postdominators(&predecessors, &successors)?;
        let controllers = postdominance_frontier.clone();
        let mut control_dependents = vec![Vec::new(); block_ids.len()];
        for (dependent, dependent_controllers) in controllers.iter().enumerate() {
            for &controller in dependent_controllers {
                control_dependents[controller].push(dependent);
            }
        }
        for dependents in &mut control_dependents {
            dependents.sort_unstable();
            dependents.dedup();
        }
        let (sccs, scc_for_block) =
            strongly_connected_regions(&predecessors, &successors, &dominators);
        let loops = natural_loops(&predecessors, &successors, &dominators)?;

        Ok(Self {
            block_ids,
            index,
            predecessors,
            successors,
            dominators,
            dom_children,
            dominance_frontier,
            postdominators,
            postdominance_frontier,
            controllers,
            control_dependents,
            sccs,
            scc_for_block,
            loops,
        })
    }

    pub(crate) fn block_index(&self, block: BlockId) -> Option<usize> {
        self.index.get(&block).copied()
    }

    pub(crate) fn dominates(&self, dominator: BlockId, block: BlockId) -> bool {
        let (Some(dominator), Some(block)) = (self.block_index(dominator), self.block_index(block))
        else {
            return false;
        };
        self.dominators.dominates(dominator, block)
    }

    pub(crate) fn postdominates(&self, postdominator: BlockId, block: BlockId) -> bool {
        let (Some(postdominator), Some(block)) =
            (self.block_index(postdominator), self.block_index(block))
        else {
            return false;
        };
        self.postdominators.postdominates(postdominator, block)
    }

    pub(crate) fn common_postdominator(&self, left: BlockId, right: BlockId) -> Option<BlockId> {
        let left = self.block_index(left)?;
        let right = self.block_index(right)?;
        self.postdominators
            .common_postdominator(left, right)
            .map(|block| self.block_ids[block])
    }
}

pub(crate) fn terminator_successors(terminator: &SIRTerminator) -> Vec<BlockId> {
    match terminator {
        SIRTerminator::Jump(target, _) => vec![*target],
        SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } => vec![true_block.0, false_block.0],
        SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
    }
}

/// Lengauer--Tarjan immediate dominators over a dense graph. DFS numbers are
/// internal; the returned table uses the caller's node indices.
fn lengauer_tarjan(
    successors: &[Vec<usize>],
    root: usize,
) -> Result<Vec<Option<usize>>, SirCfgError> {
    if successors.is_empty() || root >= successors.len() {
        return Err(SirCfgError::InvalidGraph(
            "dominator graph has no valid root",
        ));
    }
    if successors
        .iter()
        .flatten()
        .any(|successor| *successor >= successors.len())
    {
        return Err(SirCfgError::InvalidGraph(
            "dominator graph contains an out-of-range edge",
        ));
    }

    let mut dfs_number = vec![0usize; successors.len()];
    let mut vertex = vec![usize::MAX];
    let mut parent = vec![0usize; successors.len() + 1];
    dfs_number[root] = 1;
    vertex.push(root);
    let mut stack = vec![(root, 0usize)];
    while let Some((block, next_successor)) = stack.last_mut() {
        if *next_successor == successors[*block].len() {
            stack.pop();
            continue;
        }
        let successor = successors[*block][*next_successor];
        *next_successor += 1;
        if dfs_number[successor] == 0 {
            let number = vertex.len();
            dfs_number[successor] = number;
            vertex.push(successor);
            parent[number] = dfs_number[*block];
            stack.push((successor, 0));
        }
    }

    let reachable = vertex.len() - 1;
    let mut predecessors = vec![Vec::new(); reachable + 1];
    for (source, outgoing) in successors.iter().enumerate() {
        let source_number = dfs_number[source];
        if source_number == 0 {
            continue;
        }
        for &target in outgoing {
            let target_number = dfs_number[target];
            if target_number != 0 {
                predecessors[target_number].push(source_number);
            }
        }
    }

    let mut semi = (0..=reachable).collect::<Vec<_>>();
    let mut idom_number = vec![0usize; reachable + 1];
    let mut ancestor = vec![0usize; reachable + 1];
    let mut label = (0..=reachable).collect::<Vec<_>>();
    let mut bucket = vec![Vec::<usize>::new(); reachable + 1];

    fn eval(value: usize, ancestor: &mut [usize], label: &mut [usize], semi: &[usize]) -> usize {
        if ancestor[value] == 0 {
            return label[value];
        }
        let mut path = Vec::new();
        let mut current = value;
        while ancestor[current] != 0 && ancestor[ancestor[current]] != 0 {
            path.push(current);
            current = ancestor[current];
        }
        for node in path.into_iter().rev() {
            let parent = ancestor[node];
            if semi[label[parent]] < semi[label[node]] {
                label[node] = label[parent];
            }
            ancestor[node] = ancestor[parent];
        }
        label[value]
    }

    for block in (2..=reachable).rev() {
        for &predecessor in &predecessors[block] {
            let representative = eval(predecessor, &mut ancestor, &mut label, &semi);
            semi[block] = semi[block].min(semi[representative]);
        }
        bucket[semi[block]].push(block);
        let block_parent = parent[block];
        if block_parent == 0 {
            return Err(SirCfgError::InvalidGraph("non-root DFS node has no parent"));
        }
        ancestor[block] = block_parent;
        let pending = std::mem::take(&mut bucket[block_parent]);
        for candidate in pending {
            let representative = eval(candidate, &mut ancestor, &mut label, &semi);
            idom_number[candidate] = if semi[representative] < semi[candidate] {
                representative
            } else {
                block_parent
            };
        }
    }
    for block in 2..=reachable {
        if idom_number[block] != semi[block] {
            let parent = idom_number[block];
            if parent == 0 {
                return Err(SirCfgError::InvalidGraph(
                    "dominator correction references no parent",
                ));
            }
            idom_number[block] = idom_number[parent];
        }
    }

    let mut result = vec![None; successors.len()];
    for block in 2..=reachable {
        let parent = idom_number[block];
        if parent == 0 || parent >= vertex.len() {
            return Err(SirCfgError::InvalidGraph(
                "computed immediate dominator is out of range",
            ));
        }
        result[vertex[block]] = Some(vertex[parent]);
    }
    Ok(result)
}

/// Cytron dominance frontiers using a bottom-up dominator-tree walk. Work is
/// proportional to CFG edges plus the materialized frontier sets.
fn dominance_frontiers(
    successors: &[Vec<usize>],
    dominators: &DominatorTree,
    root: usize,
) -> Vec<Vec<usize>> {
    let mut frontiers = vec![BTreeSet::<usize>::new(); successors.len()];
    let mut tree_postorder = Vec::with_capacity(successors.len());
    let mut stack = vec![(root, false)];
    while let Some((block, expanded)) = stack.pop() {
        if expanded {
            tree_postorder.push(block);
            continue;
        }
        stack.push((block, true));
        stack.extend(
            dominators.children[block]
                .iter()
                .rev()
                .copied()
                .map(|child| (child, false)),
        );
    }
    for block in tree_postorder {
        for &successor in &successors[block] {
            if dominators.idom[successor] != Some(block) {
                frontiers[block].insert(successor);
            }
        }
        for &child in &dominators.children[block] {
            let child_frontier = frontiers[child].iter().copied().collect::<Vec<_>>();
            for member in child_frontier {
                if dominators.idom[member] != Some(block) {
                    frontiers[block].insert(member);
                }
            }
        }
    }
    frontiers
        .into_iter()
        .map(|frontier| frontier.into_iter().collect())
        .collect()
}

fn build_postdominators(
    predecessors: &[Vec<usize>],
    successors: &[Vec<usize>],
) -> Result<(PostDominatorTree, Vec<Vec<usize>>), SirCfgError> {
    let original_blocks = successors.len();
    let virtual_exit = original_blocks;
    let mut reverse_successors = vec![Vec::new(); original_blocks + 1];
    reverse_successors[virtual_exit] = successors
        .iter()
        .enumerate()
        .filter_map(|(block, outgoing)| outgoing.is_empty().then_some(block))
        .collect();
    for (block, incoming) in predecessors.iter().enumerate() {
        reverse_successors[block] = incoming.clone();
    }
    let tree = DominatorTree::compute(&reverse_successors, virtual_exit)?;
    let mut frontiers = dominance_frontiers(&reverse_successors, &tree, virtual_exit);
    frontiers.truncate(original_blocks);
    for frontier in &mut frontiers {
        frontier.retain(|block| *block < original_blocks);
    }
    Ok((
        PostDominatorTree {
            tree,
            virtual_exit,
            original_blocks,
        },
        frontiers,
    ))
}

fn strongly_connected_regions(
    predecessors: &[Vec<usize>],
    successors: &[Vec<usize>],
    dominators: &DominatorTree,
) -> (Vec<StronglyConnectedRegion>, Vec<usize>) {
    // Node indices are forward reverse-postorder. Traversing the transpose in
    // that order is Kosaraju's second pass.
    let mut component = vec![usize::MAX; successors.len()];
    let mut raw_components = Vec::<Vec<usize>>::new();
    for seed in 0..successors.len() {
        if component[seed] != usize::MAX {
            continue;
        }
        let component_id = raw_components.len();
        component[seed] = component_id;
        let mut members = Vec::new();
        let mut stack = vec![seed];
        while let Some(block) = stack.pop() {
            members.push(block);
            for &predecessor in predecessors[block].iter().rev() {
                if component[predecessor] == usize::MAX {
                    component[predecessor] = component_id;
                    stack.push(predecessor);
                }
            }
        }
        members.sort_unstable();
        raw_components.push(members);
    }
    let regions = raw_components
        .iter()
        .enumerate()
        .map(|(component_id, members)| {
            let mut entries = BTreeSet::new();
            for &block in members {
                if block == 0
                    || predecessors[block]
                        .iter()
                        .any(|predecessor| component[*predecessor] != component_id)
                {
                    entries.insert(block);
                }
            }
            let cyclic = members.len() > 1
                || members
                    .first()
                    .is_some_and(|block| successors[*block].contains(block));
            let reducible_header = if entries.len() == 1 {
                entries.iter().next().copied().filter(|header| {
                    members
                        .iter()
                        .all(|block| dominators.dominates(*header, *block))
                })
            } else {
                None
            };
            StronglyConnectedRegion {
                blocks: members.clone(),
                entries: entries.into_iter().collect(),
                cyclic,
                reducible_header,
            }
        })
        .collect();
    (regions, component)
}

fn natural_loops(
    predecessors: &[Vec<usize>],
    successors: &[Vec<usize>],
    dominators: &DominatorTree,
) -> Result<Vec<NaturalLoop>, SirCfgError> {
    let mut by_header = vec![None::<BTreeSet<usize>>; successors.len()];
    for (tail, outgoing) in successors.iter().enumerate() {
        for &header in outgoing {
            if !dominators.dominates(header, tail) {
                continue;
            }
            let blocks = by_header[header].get_or_insert_with(BTreeSet::new);
            blocks.insert(header);
            let mut stack = vec![tail];
            while let Some(block) = stack.pop() {
                if blocks.insert(block) {
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
        let parent = innermost_for_block[loops[child].header];
        if parent.is_some_and(|parent| !loops[parent].blocks.is_superset(&loops[child].blocks)) {
            return Err(SirCfgError::InvalidGraph(
                "natural loops overlap without nesting",
            ));
        }
        loops[child].parent = parent;
        for &block in &loops[child].blocks {
            innermost_for_block[block] = Some(child);
        }
    }
    Ok(loops)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HashMap;
    use crate::ir::{BasicBlock, RegisterType};

    fn block(id: usize, terminator: SIRTerminator) -> BasicBlock<()> {
        BasicBlock {
            id: BlockId(id),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator,
        }
    }

    fn eu(entry: usize, blocks: Vec<BasicBlock<()>>) -> ExecutionUnit<()> {
        ExecutionUnit {
            entry_block_id: BlockId(entry),
            blocks: blocks.into_iter().map(|block| (block.id, block)).collect(),
            register_map: HashMap::<_, RegisterType>::default(),
        }
    }

    #[test]
    fn analyzes_linear_cfg() {
        let cfg = SirCfg::analyze(&eu(
            0,
            vec![
                block(0, SIRTerminator::Jump(BlockId(1), Vec::new())),
                block(1, SIRTerminator::Jump(BlockId(2), Vec::new())),
                block(2, SIRTerminator::Return),
            ],
        ))
        .unwrap();

        assert_eq!(cfg.block_ids, vec![BlockId(0), BlockId(1), BlockId(2)]);
        assert_eq!(cfg.dominators.idom, vec![None, Some(0), Some(1)]);
        assert!(cfg.dominates(BlockId(0), BlockId(2)));
        assert!(cfg.postdominates(BlockId(2), BlockId(0)));
        assert_eq!(
            cfg.common_postdominator(BlockId(0), BlockId(1)),
            Some(BlockId(1))
        );
    }

    #[test]
    fn computes_diamond_frontiers_and_control_dependence() {
        let condition = crate::ir::RegisterId(0);
        let mut unit = eu(
            0,
            vec![
                block(
                    0,
                    SIRTerminator::Branch {
                        cond: condition,
                        true_block: (BlockId(1), Vec::new()),
                        false_block: (BlockId(2), Vec::new()),
                    },
                ),
                block(1, SIRTerminator::Jump(BlockId(3), Vec::new())),
                block(2, SIRTerminator::Jump(BlockId(3), Vec::new())),
                block(3, SIRTerminator::Return),
            ],
        );
        unit.register_map.insert(
            condition,
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        );
        let cfg = SirCfg::analyze(&unit).unwrap();
        let true_block = cfg.index[&BlockId(1)];
        let false_block = cfg.index[&BlockId(2)];
        let branch = cfg.index[&BlockId(0)];
        let join = cfg.index[&BlockId(3)];

        assert_eq!(cfg.dominance_frontier[true_block], vec![join]);
        assert_eq!(cfg.dominance_frontier[false_block], vec![join]);
        assert_eq!(cfg.controllers[true_block], vec![branch]);
        assert_eq!(cfg.controllers[false_block], vec![branch]);
        let mut arms = vec![true_block, false_block];
        arms.sort_unstable();
        assert_eq!(cfg.control_dependents[branch], arms);
        assert_eq!(
            cfg.common_postdominator(BlockId(1), BlockId(2)),
            Some(BlockId(3))
        );
    }

    #[test]
    fn computes_nested_diamond_control_dependence() {
        let condition = crate::ir::RegisterId(0);
        let mut unit = eu(
            0,
            vec![
                block(
                    0,
                    SIRTerminator::Branch {
                        cond: condition,
                        true_block: (BlockId(1), Vec::new()),
                        false_block: (BlockId(2), Vec::new()),
                    },
                ),
                block(
                    1,
                    SIRTerminator::Branch {
                        cond: condition,
                        true_block: (BlockId(3), Vec::new()),
                        false_block: (BlockId(4), Vec::new()),
                    },
                ),
                block(2, SIRTerminator::Jump(BlockId(5), Vec::new())),
                block(3, SIRTerminator::Jump(BlockId(5), Vec::new())),
                block(4, SIRTerminator::Jump(BlockId(5), Vec::new())),
                block(5, SIRTerminator::Return),
            ],
        );
        unit.register_map.insert(
            condition,
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        );
        let cfg = SirCfg::analyze(&unit).unwrap();
        let outer = cfg.index[&BlockId(0)];
        let inner = cfg.index[&BlockId(1)];
        let outer_false = cfg.index[&BlockId(2)];
        let inner_true = cfg.index[&BlockId(3)];
        let inner_false = cfg.index[&BlockId(4)];

        assert_eq!(cfg.controllers[inner], vec![outer]);
        assert_eq!(cfg.controllers[outer_false], vec![outer]);
        assert_eq!(cfg.controllers[inner_true], vec![inner]);
        assert_eq!(cfg.controllers[inner_false], vec![inner]);
        assert_eq!(
            cfg.common_postdominator(BlockId(1), BlockId(2)),
            Some(BlockId(5))
        );
        assert_eq!(
            cfg.common_postdominator(BlockId(3), BlockId(4)),
            Some(BlockId(5))
        );
    }

    #[test]
    fn handles_multiple_exits_with_a_virtual_postdominator_root() {
        let condition = crate::ir::RegisterId(0);
        let mut unit = eu(
            0,
            vec![
                block(
                    0,
                    SIRTerminator::Branch {
                        cond: condition,
                        true_block: (BlockId(1), Vec::new()),
                        false_block: (BlockId(2), Vec::new()),
                    },
                ),
                block(1, SIRTerminator::Return),
                block(2, SIRTerminator::Error(1)),
            ],
        );
        unit.register_map.insert(
            condition,
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        );
        let cfg = SirCfg::analyze(&unit).unwrap();

        assert_eq!(cfg.common_postdominator(BlockId(1), BlockId(2)), None);
        assert!(!cfg.postdominates(BlockId(1), BlockId(0)));
        assert_eq!(
            cfg.postdominators
                .immediate_postdominator(cfg.index[&BlockId(0)]),
            None
        );
    }

    #[test]
    fn finds_natural_loop_and_irreducible_scc() {
        let condition = crate::ir::RegisterId(0);
        let mut natural = eu(
            0,
            vec![
                block(0, SIRTerminator::Jump(BlockId(1), Vec::new())),
                block(
                    1,
                    SIRTerminator::Branch {
                        cond: condition,
                        true_block: (BlockId(2), Vec::new()),
                        false_block: (BlockId(3), Vec::new()),
                    },
                ),
                block(2, SIRTerminator::Jump(BlockId(1), Vec::new())),
                block(3, SIRTerminator::Return),
            ],
        );
        natural.register_map.insert(
            condition,
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        );
        let natural_cfg = SirCfg::analyze(&natural).unwrap();
        assert_eq!(natural_cfg.loops.len(), 1);
        assert_eq!(
            natural_cfg.block_ids[natural_cfg.loops[0].header],
            BlockId(1)
        );

        let mut irreducible = eu(
            0,
            vec![
                block(
                    0,
                    SIRTerminator::Branch {
                        cond: condition,
                        true_block: (BlockId(1), Vec::new()),
                        false_block: (BlockId(2), Vec::new()),
                    },
                ),
                block(1, SIRTerminator::Jump(BlockId(2), Vec::new())),
                block(
                    2,
                    SIRTerminator::Branch {
                        cond: condition,
                        true_block: (BlockId(1), Vec::new()),
                        false_block: (BlockId(3), Vec::new()),
                    },
                ),
                block(3, SIRTerminator::Return),
            ],
        );
        irreducible.register_map.insert(
            condition,
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        );
        let irreducible_cfg = SirCfg::analyze(&irreducible).unwrap();
        let region = irreducible_cfg
            .sccs
            .iter()
            .find(|region| region.cyclic && region.blocks.len() == 2)
            .unwrap();
        assert_eq!(region.entries.len(), 2);
        assert_eq!(region.reducible_header, None);
    }

    #[test]
    fn reports_missing_and_unreachable_blocks() {
        let missing = eu(
            0,
            vec![block(0, SIRTerminator::Jump(BlockId(9), Vec::new()))],
        );
        assert_eq!(
            SirCfg::analyze(&missing).unwrap_err(),
            SirCfgError::MissingTarget {
                source: BlockId(0),
                target: BlockId(9),
            }
        );

        let unreachable = eu(
            0,
            vec![
                block(0, SIRTerminator::Return),
                block(1, SIRTerminator::Return),
            ],
        );
        assert_eq!(
            SirCfg::analyze(&unreachable).unwrap_err(),
            SirCfgError::Unreachable(vec![BlockId(1)])
        );
    }

    #[test]
    fn deep_chain_uses_no_recursive_graph_walk() {
        const BLOCKS: usize = 20_000;
        let mut blocks = Vec::with_capacity(BLOCKS);
        for id in 0..BLOCKS - 1 {
            blocks.push(block(id, SIRTerminator::Jump(BlockId(id + 1), Vec::new())));
        }
        blocks.push(block(BLOCKS - 1, SIRTerminator::Return));

        let cfg = SirCfg::analyze(&eu(0, blocks)).unwrap();

        assert_eq!(cfg.block_ids.len(), BLOCKS);
        assert!(cfg.dominates(BlockId(0), BlockId(BLOCKS - 1)));
        assert!(cfg.postdominates(BlockId(BLOCKS - 1), BlockId(0)));
    }

    #[test]
    fn analyzes_large_wide_cfg() {
        const LEAVES: usize = 4_096;
        const TREE_BLOCKS: usize = LEAVES * 2 - 1;
        const JOIN: usize = TREE_BLOCKS;

        let condition = crate::ir::RegisterId(0);
        let mut blocks = Vec::with_capacity(TREE_BLOCKS + 1);
        for id in 0..LEAVES - 1 {
            blocks.push(block(
                id,
                SIRTerminator::Branch {
                    cond: condition,
                    true_block: (BlockId(id * 2 + 1), Vec::new()),
                    false_block: (BlockId(id * 2 + 2), Vec::new()),
                },
            ));
        }
        for id in LEAVES - 1..TREE_BLOCKS {
            blocks.push(block(id, SIRTerminator::Jump(BlockId(JOIN), Vec::new())));
        }
        blocks.push(block(JOIN, SIRTerminator::Return));
        let mut unit = eu(0, blocks);
        unit.register_map.insert(
            condition,
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        );

        let cfg = SirCfg::analyze(&unit).unwrap();
        let join = cfg.index[&BlockId(JOIN)];

        assert_eq!(cfg.block_ids.len(), TREE_BLOCKS + 1);
        assert!(cfg.dominates(BlockId(0), BlockId(JOIN)));
        assert!(cfg.postdominates(BlockId(JOIN), BlockId(0)));
        for id in LEAVES - 1..TREE_BLOCKS {
            assert_eq!(cfg.dominance_frontier[cfg.index[&BlockId(id)]], [join]);
        }
    }

    fn reference_idom(predecessors: &[Vec<usize>], root: usize) -> Vec<Option<usize>> {
        let blocks = predecessors.len();
        let universe = (0..blocks).collect::<BTreeSet<_>>();
        let mut dominators = vec![universe; blocks];
        dominators[root] = BTreeSet::from([root]);
        loop {
            let mut changed = false;
            for block in 0..blocks {
                if block == root {
                    continue;
                }
                let mut incoming = predecessors[block].iter().copied();
                let Some(first) = incoming.next() else {
                    continue;
                };
                let mut next = dominators[first].clone();
                for predecessor in incoming {
                    next.retain(|candidate| dominators[predecessor].contains(candidate));
                }
                next.insert(block);
                if dominators[block] != next {
                    dominators[block] = next;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        (0..blocks)
            .map(|block| {
                if block == root {
                    return None;
                }
                dominators[block]
                    .iter()
                    .copied()
                    .filter(|candidate| *candidate != block)
                    .max_by_key(|candidate| dominators[*candidate].len())
            })
            .collect()
    }

    fn reference_frontier(
        predecessors: &[Vec<usize>],
        dominators: &DominatorTree,
    ) -> Vec<Vec<usize>> {
        (0..predecessors.len())
            .map(|candidate| {
                (0..predecessors.len())
                    .filter(|&join| {
                        predecessors[join]
                            .iter()
                            .any(|predecessor| dominators.dominates(candidate, *predecessor))
                            && (candidate == join || !dominators.dominates(candidate, join))
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn lengauer_tarjan_and_frontiers_match_set_equations() {
        let condition = crate::ir::RegisterId(0);
        let mut random = 0x4d59_5df4_d0f3_3173u64;
        for blocks in 2..40usize {
            for _case in 0..20 {
                let mut graph_blocks = Vec::with_capacity(blocks);
                for id in 0..blocks - 1 {
                    random = random
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1_442_695_040_888_963_407);
                    let alternate = (random as usize) % blocks;
                    let terminator = if random & 3 == 0 {
                        SIRTerminator::Branch {
                            cond: condition,
                            true_block: (BlockId(id + 1), Vec::new()),
                            false_block: (BlockId(alternate), Vec::new()),
                        }
                    } else {
                        SIRTerminator::Jump(BlockId(id + 1), Vec::new())
                    };
                    graph_blocks.push(block(id, terminator));
                }
                graph_blocks.push(block(blocks - 1, SIRTerminator::Return));
                let mut unit = eu(0, graph_blocks);
                unit.register_map.insert(
                    condition,
                    RegisterType::Bit {
                        width: 1,
                        signed: false,
                    },
                );
                let cfg = SirCfg::analyze(&unit).unwrap();

                assert_eq!(
                    cfg.dominators.idom,
                    reference_idom(&cfg.predecessors, 0),
                    "dominator mismatch for {}-block generated graph",
                    blocks
                );
                assert_eq!(
                    cfg.dominance_frontier,
                    reference_frontier(&cfg.predecessors, &cfg.dominators),
                    "frontier mismatch for {}-block generated graph",
                    blocks
                );
            }
        }
    }
}
