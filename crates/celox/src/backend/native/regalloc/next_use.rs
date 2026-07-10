//! Braun--Hack section 4.1: CFG-global next-use distances.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};

use crate::backend::native::mir::{MFunction, VReg};

use super::cfg::NormalizedCfg;

/// Lexicographic next-use distance.
///
/// Exiting any loop region is more expensive than every instruction-only path,
/// independent of function size.  `Dead` sorts after every finite distance so
/// MIN naturally evicts values with no next use first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NextUseDistance {
    Finite {
        loop_exits: usize,
        instructions: usize,
    },
    Dead,
}

impl NextUseDistance {
    fn local(instructions: usize) -> Self {
        Self::Finite {
            loop_exits: 0,
            instructions,
        }
    }

    fn across_edge(self, loop_exits: usize) -> Self {
        match self {
            Self::Finite {
                loop_exits: current,
                instructions,
            } => Self::Finite {
                loop_exits: current
                    .checked_add(loop_exits)
                    .expect("loop-region exit distance exceeds addressable CFG size"),
                instructions,
            },
            Self::Dead => Self::Dead,
        }
    }

    fn prepend_instructions(self, instructions: usize) -> Self {
        match self {
            Self::Finite {
                loop_exits,
                instructions: current,
            } => Self::Finite {
                loop_exits,
                instructions: current
                    .checked_add(instructions)
                    .expect("next-use distance exceeds addressable MIR size"),
            },
            Self::Dead => Self::Dead,
        }
    }

    pub(super) fn is_dead(self) -> bool {
        matches!(self, Self::Dead)
    }
}

impl Ord for NextUseDistance {
    fn cmp(&self, other: &Self) -> Ordering {
        match (*self, *other) {
            (Self::Dead, Self::Dead) => Ordering::Equal,
            (Self::Dead, _) => Ordering::Greater,
            (_, Self::Dead) => Ordering::Less,
            (
                Self::Finite {
                    loop_exits: left_exits,
                    instructions: left_instructions,
                },
                Self::Finite {
                    loop_exits: right_exits,
                    instructions: right_instructions,
                },
            ) => (left_exits, left_instructions).cmp(&(right_exits, right_instructions)),
        }
    }
}

impl PartialOrd for NextUseDistance {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug)]
pub(super) struct NextUseAnalysis {
    pub entry: Vec<HashMap<VReg, NextUseDistance>>,
    pub exit: Vec<HashMap<VReg, NextUseDistance>>,
    pub block_max_pressure: Vec<usize>,
    pub loop_regions: Vec<LoopRegionFacts>,
    entry_region: Vec<Option<usize>>,
    use_positions: Vec<HashMap<VReg, Vec<usize>>>,
    #[cfg(test)]
    region_work: RegionAggregationWork,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LoopRegionKind {
    Natural,
    IrreducibleScc,
}

/// Values and maximum pressure for one loop region.  Natural loops and
/// multi-entry cyclic SCCs use the same facts at their entry blocks.
#[derive(Debug)]
pub(super) struct LoopRegionFacts {
    pub kind: LoopRegionKind,
    pub entries: Vec<usize>,
    pub used: HashSet<VReg>,
    pub max_pressure: usize,
}

pub(super) fn analyze(func: &MFunction, cfg: &NormalizedCfg) -> NextUseAnalysis {
    let transfers = block_transfers(func);
    let use_positions = func
        .blocks
        .iter()
        .map(|block| {
            let mut positions = HashMap::<VReg, Vec<usize>>::new();
            for (instruction, inst) in block.insts.iter().enumerate() {
                for value in inst.uses() {
                    positions.entry(value).or_default().push(instruction);
                }
            }
            positions
        })
        .collect::<Vec<_>>();
    let phi_uses = phi_edge_uses(func, cfg);
    let region_topology = RegionTopology::build(cfg);
    let edge_loop_exits = &region_topology.edge_exits;
    let mut entry: Vec<HashMap<VReg, NextUseDistance>> = vec![HashMap::new(); func.blocks.len()];
    let mut exit: Vec<HashMap<VReg, NextUseDistance>> = vec![HashMap::new(); func.blocks.len()];
    let mut queue = (0..func.blocks.len()).rev().collect::<VecDeque<_>>();
    let mut queued = vec![true; func.blocks.len()];
    while let Some(block) = queue.pop_front() {
        queued[block] = false;
        let mut next_exit = HashMap::<VReg, NextUseDistance>::new();
        for (edge, &successor) in cfg.successors[block].iter().enumerate() {
            let edge_exits = edge_loop_exits[block][edge];
            for (&value, &distance) in &entry[successor] {
                let distance = distance.across_edge(edge_exits);
                next_exit
                    .entry(value)
                    .and_modify(|current| *current = (*current).min(distance))
                    .or_insert(distance);
            }
            for &value in &phi_uses[block][edge] {
                let distance = NextUseDistance::Finite {
                    loop_exits: edge_exits,
                    instructions: 0,
                };
                next_exit
                    .entry(value)
                    .and_modify(|current| *current = (*current).min(distance))
                    .or_insert(distance);
            }
        }
        let transfer = &transfers[block];
        let mut next_entry = HashMap::new();
        for (&value, &distance) in &next_exit {
            if !transfer.definitions.contains(&value) {
                next_entry.insert(value, distance.prepend_instructions(transfer.length));
            }
        }
        for &(value, position) in &transfer.local_uses {
            next_entry.insert(value, NextUseDistance::local(position));
        }
        if next_entry != entry[block] || next_exit != exit[block] {
            entry[block] = next_entry;
            exit[block] = next_exit;
            for &predecessor in &cfg.predecessors[block] {
                if !queued[predecessor] {
                    queue.push_back(predecessor);
                    queued[predecessor] = true;
                }
            }
        }
    }

    let (block_summaries, mut region_work) = block_region_summaries(func, &exit);
    let block_max_pressure = block_summaries
        .iter()
        .map(|summary| summary.max_pressure)
        .collect::<Vec<_>>();
    let (region_uses, region_pressure) =
        aggregate_region_summaries(&region_topology, &block_summaries, &mut region_work);
    let loop_regions = region_topology
        .regions
        .iter()
        .zip(region_uses)
        .zip(region_pressure)
        .map(|((shape, used), max_pressure)| LoopRegionFacts {
            kind: shape.kind,
            entries: shape.entries.clone(),
            used,
            max_pressure,
        })
        .collect();
    NextUseAnalysis {
        entry,
        exit,
        block_max_pressure,
        loop_regions,
        entry_region: region_topology.entry_region,
        use_positions,
        #[cfg(test)]
        region_work,
    }
}

impl NextUseAnalysis {
    pub(super) fn verify(&self, func: &MFunction, cfg: &NormalizedCfg) {
        assert_eq!(self.entry.len(), func.blocks.len());
        assert_eq!(self.exit.len(), func.blocks.len());
        assert_eq!(self.block_max_pressure.len(), func.blocks.len());
        assert_eq!(self.entry_region.len(), func.blocks.len());
        assert_eq!(self.use_positions.len(), func.blocks.len());
        let topology = RegionTopology::build(cfg);
        assert_eq!(self.entry_region, topology.entry_region);
        assert_eq!(self.loop_regions.len(), topology.regions.len());
        let (summaries, mut work) = block_region_summaries(func, &self.exit);
        assert_eq!(
            self.block_max_pressure,
            summaries
                .iter()
                .map(|summary| summary.max_pressure)
                .collect::<Vec<_>>()
        );
        let (used, pressure) = aggregate_region_summaries(&topology, &summaries, &mut work);
        for (region, facts) in self.loop_regions.iter().enumerate() {
            assert_eq!(facts.kind, topology.regions[region].kind);
            assert_eq!(facts.entries, topology.regions[region].entries);
            assert_eq!(facts.used, used[region]);
            assert_eq!(facts.max_pressure, pressure[region]);
        }
        for block in 0..func.blocks.len() {
            if let Some(&value) = self.entry[block].keys().next() {
                assert!(!self.distance_at(func, block, 0, value).is_dead());
            }
        }
    }

    pub(super) fn distance_at(
        &self,
        func: &MFunction,
        block: usize,
        instruction: usize,
        value: VReg,
    ) -> NextUseDistance {
        if let Some(positions) = self.use_positions[block].get(&value) {
            let next = positions.partition_point(|position| *position < instruction);
            if let Some(&position) = positions.get(next) {
                return NextUseDistance::local(position - instruction);
            }
        }
        let remaining = func.blocks[block].insts.len() - instruction;
        self.exit[block]
            .get(&value)
            .map(|distance| distance.prepend_instructions(remaining))
            .unwrap_or(NextUseDistance::Dead)
    }

    pub(super) fn region_at_entry(&self, block: usize) -> Option<usize> {
        self.entry_region[block]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RegionShape {
    kind: LoopRegionKind,
    parent: Option<usize>,
    entries: Vec<usize>,
}

/// One forest containing both natural loops and the additional cyclic SCC
/// regions required for irreducible control flow.  Region indices are in
/// child-before-parent order, which permits bottom-up aggregation without a
/// second graph traversal.
struct RegionTopology {
    regions: Vec<RegionShape>,
    entry_region: Vec<Option<usize>>,
    direct_region: Vec<Option<usize>>,
    edge_exits: Vec<Vec<usize>>,
}

impl RegionTopology {
    fn build(cfg: &NormalizedCfg) -> Self {
        let nesting = LoopNesting::new(cfg);
        let scc = SccInfo::build(cfg);
        let natural_regions = cfg
            .loops
            .iter()
            .map(|natural_loop| natural_loop.blocks.iter().copied().collect::<Vec<_>>())
            .collect::<HashSet<_>>();

        let mut regions = cfg
            .loops
            .iter()
            .map(|natural_loop| RegionShape {
                kind: LoopRegionKind::Natural,
                parent: natural_loop.parent,
                entries: vec![natural_loop.header],
            })
            .collect::<Vec<_>>();
        let mut component_region = vec![None; scc.components.len()];
        for (component, members) in scc.components.iter().enumerate() {
            if !scc.cyclic[component] || natural_regions.contains(members) {
                continue;
            }
            let mut entries = members
                .iter()
                .copied()
                .filter(|&block| {
                    cfg.predecessors[block]
                        .iter()
                        .any(|&predecessor| scc.component_of[predecessor] != component)
                })
                .collect::<Vec<_>>();
            // A reachable cyclic SCC normally has an outside entry.  The CFG
            // entry itself is the one exception.
            if entries.is_empty() && scc.component_of.first() == Some(&component) {
                entries.push(0);
            }
            entries.sort_unstable();
            entries.dedup();
            let region = regions.len();
            component_region[component] = Some(region);
            regions.push(RegionShape {
                kind: LoopRegionKind::IrreducibleScc,
                parent: None,
                entries,
            });
        }

        // The maximal SCC contains every natural loop nested inside an
        // irreducible region.  Attach only the outermost natural loops; their
        // existing parent links already connect all descendants.
        for (natural, natural_loop) in cfg.loops.iter().enumerate() {
            if regions[natural].parent.is_some() {
                continue;
            }
            if let Some(&component) = scc.component_of.get(natural_loop.header)
                && let Some(parent) = component_region[component]
            {
                regions[natural].parent = Some(parent);
            }
        }
        debug_assert!(
            regions
                .iter()
                .enumerate()
                .all(|(child, region)| region.parent.is_none_or(|parent| parent > child))
        );

        // A block contributes its local summary only to its innermost natural
        // loop, or directly to its irreducible SCC when it is not in a natural
        // child.  Parent summaries are formed by the bottom-up pass below.
        let direct_region = (0..cfg.successors.len())
            .map(|block| {
                let natural = nesting.block_loop[block];
                (natural != nesting.root)
                    .then_some(natural)
                    .or_else(|| component_region[scc.component_of[block]])
            })
            .collect::<Vec<_>>();

        let mut entry_region = vec![None; cfg.successors.len()];
        for (region, shape) in regions.iter().enumerate() {
            if shape.kind == LoopRegionKind::Natural {
                for &entry in &shape.entries {
                    entry_region[entry] = Some(region);
                }
            }
        }
        // A multi-entry SCC boundary takes precedence when one of its entries
        // also happens to be a natural-loop header.  All entries of that SCC
        // must make the same region-level W-entry decision.
        for (region, shape) in regions.iter().enumerate() {
            if shape.kind == LoopRegionKind::IrreducibleScc {
                for &entry in &shape.entries {
                    entry_region[entry] = Some(region);
                }
            }
        }

        // Number of loop regions left by each CFG edge.  Natural loops use an
        // LCA query; an extra cyclic SCC contributes one more component when
        // the edge leaves its maximal component.
        let edge_exits = cfg
            .successors
            .iter()
            .enumerate()
            .map(|(block, successors)| {
                successors
                    .iter()
                    .map(|&successor| {
                        let natural_exits = nesting.exits(block, successor);
                        let component = scc.component_of[block];
                        natural_exits
                            .checked_add(usize::from(
                                component_region[component].is_some()
                                    && component != scc.component_of[successor],
                            ))
                            .expect("loop-region exit count exceeds addressable CFG size")
                    })
                    .collect()
            })
            .collect();

        Self {
            regions,
            entry_region,
            direct_region,
            edge_exits,
        }
    }
}

#[derive(Debug)]
struct BlockRegionSummary {
    used: HashSet<VReg>,
    max_pressure: usize,
}

#[derive(Debug, Default)]
struct RegionAggregationWork {
    block_scans: usize,
    instruction_visits: usize,
    propagated_values: usize,
}

/// Scan every block once to obtain both inputs of loop-region aggregation.
fn block_region_summaries(
    func: &MFunction,
    exit: &[HashMap<VReg, NextUseDistance>],
) -> (Vec<BlockRegionSummary>, RegionAggregationWork) {
    let mut work = RegionAggregationWork::default();
    let summaries = func
        .blocks
        .iter()
        .enumerate()
        .map(|(block, mir_block)| {
            work.block_scans += 1;
            work.instruction_visits += mir_block.insts.len();
            let mut used = HashSet::new();
            for phi in &mir_block.phis {
                used.insert(phi.dst);
                used.extend(phi.sources.iter().map(|(_, source)| *source));
            }
            let mut live = exit[block].keys().copied().collect::<HashSet<_>>();
            let mut maximum = live.len();
            for inst in mir_block.insts.iter().rev() {
                let instruction_uses = inst.uses();
                used.extend(instruction_uses.iter().copied());
                if let Some(definition) = inst.def() {
                    live.remove(&definition);
                }
                live.extend(instruction_uses);
                maximum = maximum.max(live.len());
            }
            BlockRegionSummary {
                used,
                max_pressure: maximum,
            }
        })
        .collect();
    (summaries, work)
}

/// Aggregate block summaries through the region forest.  Instructions are not
/// revisited; each propagated set member corresponds to one member of the
/// materialized parent-region output.
fn aggregate_region_summaries(
    topology: &RegionTopology,
    blocks: &[BlockRegionSummary],
    work: &mut RegionAggregationWork,
) -> (Vec<HashSet<VReg>>, Vec<usize>) {
    assert_eq!(topology.direct_region.len(), blocks.len());
    let mut used = vec![HashSet::new(); topology.regions.len()];
    let mut pressure = vec![0usize; topology.regions.len()];
    for (block, summary) in blocks.iter().enumerate() {
        let Some(region) = topology.direct_region[block] else {
            continue;
        };
        used[region].extend(summary.used.iter().copied());
        pressure[region] = pressure[region].max(summary.max_pressure);
    }
    for child in 0..topology.regions.len() {
        let Some(parent) = topology.regions[child].parent else {
            continue;
        };
        assert!(parent > child, "loop regions must be child-before-parent");
        pressure[parent] = pressure[parent].max(pressure[child]);
        let (children, parents) = used.split_at_mut(parent);
        let child_values = &children[child];
        let parent_values = &mut parents[0];
        work.propagated_values = work
            .propagated_values
            .checked_add(child_values.len())
            .expect("loop-region summary work exceeds addressable size");
        parent_values.extend(child_values.iter().copied());
    }
    (used, pressure)
}

struct LoopNesting {
    root: usize,
    block_loop: Vec<usize>,
    depth: Vec<usize>,
    ancestors: Vec<Vec<usize>>,
}

impl LoopNesting {
    fn new(cfg: &NormalizedCfg) -> Self {
        let root = cfg.loops.len();
        let nodes = root + 1;
        let mut parent = cfg
            .loops
            .iter()
            .map(|natural_loop| natural_loop.parent.unwrap_or(root))
            .chain(std::iter::once(root))
            .collect::<Vec<_>>();
        parent[root] = root;

        let mut depth = vec![usize::MAX; nodes];
        depth[root] = 0;
        for start in 0..root {
            if depth[start] != usize::MAX {
                continue;
            }
            let mut path = Vec::new();
            let mut node = start;
            while depth[node] == usize::MAX {
                path.push(node);
                node = parent[node];
            }
            let mut current_depth = depth[node];
            for node in path.into_iter().rev() {
                current_depth = current_depth
                    .checked_add(1)
                    .expect("loop nesting exceeds addressable CFG size");
                depth[node] = current_depth;
            }
        }

        let levels = (usize::BITS - nodes.leading_zeros()) as usize;
        let mut ancestors = Vec::with_capacity(levels);
        ancestors.push(parent);
        for level in 1..levels {
            let previous = &ancestors[level - 1];
            ancestors.push(previous.iter().map(|&node| previous[node]).collect());
        }

        // cfg.loops is ordered from inner/smaller regions to outer regions.
        let mut block_loop = vec![root; cfg.successors.len()];
        for (loop_index, natural_loop) in cfg.loops.iter().enumerate() {
            for &block in &natural_loop.blocks {
                if block_loop[block] == root {
                    block_loop[block] = loop_index;
                }
            }
        }
        Self {
            root,
            block_loop,
            depth,
            ancestors,
        }
    }

    fn exits(&self, block: usize, successor: usize) -> usize {
        let source_loop = self.block_loop[block];
        let common = self.lca(source_loop, self.block_loop[successor]);
        self.depth[source_loop] - self.depth[common]
    }

    fn lca(&self, mut left: usize, mut right: usize) -> usize {
        if left == self.root || right == self.root {
            return self.root;
        }
        if self.depth[left] < self.depth[right] {
            std::mem::swap(&mut left, &mut right);
        }
        let difference = self.depth[left] - self.depth[right];
        for level in 0..self.ancestors.len() {
            if difference & (1usize << level) != 0 {
                left = self.ancestors[level][left];
            }
        }
        if left == right {
            return left;
        }
        for level in (0..self.ancestors.len()).rev() {
            if self.ancestors[level][left] != self.ancestors[level][right] {
                left = self.ancestors[level][left];
                right = self.ancestors[level][right];
            }
        }
        self.ancestors[0][left]
    }
}

struct SccInfo {
    component_of: Vec<usize>,
    components: Vec<Vec<usize>>,
    cyclic: Vec<bool>,
}

impl SccInfo {
    /// Iterative Kosaraju decomposition; large branchified functions must not
    /// consume the native call stack while discovering irreducible regions.
    fn build(cfg: &NormalizedCfg) -> Self {
        let blocks = cfg.successors.len();
        let mut visited = vec![false; blocks];
        let mut finish = Vec::with_capacity(blocks);
        for start in 0..blocks {
            if visited[start] {
                continue;
            }
            visited[start] = true;
            let mut stack = vec![(start, 0usize)];
            while let Some((block, next_successor)) = stack.pop() {
                if next_successor == cfg.successors[block].len() {
                    finish.push(block);
                    continue;
                }
                stack.push((block, next_successor + 1));
                let successor = cfg.successors[block][next_successor];
                if !visited[successor] {
                    visited[successor] = true;
                    stack.push((successor, 0));
                }
            }
        }

        let mut component_of = vec![usize::MAX; blocks];
        let mut components = Vec::<Vec<usize>>::new();
        for &start in finish.iter().rev() {
            if component_of[start] != usize::MAX {
                continue;
            }
            let component = components.len();
            let mut members = Vec::new();
            let mut stack = vec![start];
            component_of[start] = component;
            while let Some(block) = stack.pop() {
                members.push(block);
                for &predecessor in &cfg.predecessors[block] {
                    if component_of[predecessor] == usize::MAX {
                        component_of[predecessor] = component;
                        stack.push(predecessor);
                    }
                }
            }
            members.sort_unstable();
            components.push(members);
        }
        let cyclic = components
            .iter()
            .map(|members| {
                members.len() > 1
                    || cfg.successors[members[0]]
                        .iter()
                        .any(|successor| *successor == members[0])
            })
            .collect();
        Self {
            component_of,
            components,
            cyclic,
        }
    }
}

struct BlockTransfer {
    length: usize,
    definitions: HashSet<VReg>,
    local_uses: Vec<(VReg, usize)>,
}

fn block_transfers(func: &MFunction) -> Vec<BlockTransfer> {
    func.blocks
        .iter()
        .map(|block| {
            let mut definitions = block.phis.iter().map(|phi| phi.dst).collect::<HashSet<_>>();
            let mut local_uses = HashMap::<VReg, usize>::new();
            for (position, inst) in block.insts.iter().enumerate() {
                for used in inst.uses() {
                    if !definitions.contains(&used) {
                        local_uses.entry(used).or_insert(position);
                    }
                }
                if let Some(definition) = inst.def() {
                    definitions.insert(definition);
                }
            }
            let mut local_uses = local_uses.into_iter().collect::<Vec<_>>();
            local_uses.sort_by_key(|(value, _)| *value);
            BlockTransfer {
                length: block.insts.len(),
                definitions,
                local_uses,
            }
        })
        .collect()
}

fn phi_edge_uses(func: &MFunction, cfg: &NormalizedCfg) -> Vec<Vec<Vec<VReg>>> {
    cfg.successors
        .iter()
        .enumerate()
        .map(|(predecessor, successors)| {
            successors
                .iter()
                .map(|successor| {
                    let predecessor_id = func.blocks[predecessor].id;
                    func.blocks[*successor]
                        .phis
                        .iter()
                        .filter_map(|phi| {
                            phi.sources.iter().find_map(|(source_predecessor, source)| {
                                (*source_predecessor == predecessor_id).then_some(*source)
                            })
                        })
                        .collect()
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::backend::native::mir::{BlockId, MBlock, MInst, SpillDesc, VRegAllocator};

    #[test]
    fn irreducible_cyclic_scc_exit_is_a_loop_component() {
        // Blocks 1 and 2 form a cyclic SCC with two entries from block 0, so
        // neither is a natural-loop header.  Exiting the SCC must nevertheless
        // increment the lexicographic loop component.
        let cfg = NormalizedCfg {
            block_index: HashMap::new(),
            predecessors: vec![vec![], vec![0, 2], vec![0, 1], vec![1, 2]],
            successors: vec![vec![1, 2], vec![2, 3], vec![1, 3], vec![]],
            idom: vec![None, Some(0), Some(0), Some(0)],
            dominance_frontier: vec![BTreeSet::new(); 4],
            loops: Vec::new(),
            loop_for_header: HashMap::new(),
        };
        let topology = RegionTopology::build(&cfg);
        assert_eq!(topology.edge_exits[1], vec![0, 1]);
        assert_eq!(topology.edge_exits[2], vec![0, 1]);
        assert_eq!(topology.regions.len(), 1);
        assert_eq!(topology.regions[0].kind, LoopRegionKind::IrreducibleScc);
        assert_eq!(topology.regions[0].entries, vec![1, 2]);
        assert_eq!(topology.entry_region[1], Some(0));
        assert_eq!(topology.entry_region[2], Some(0));
    }

    #[test]
    fn nested_region_facts_scan_blocks_once_and_aggregate_bottom_up() {
        let mut vregs = VRegAllocator::new();
        let outer_value = vregs.alloc();
        let inner_value = vregs.alloc();
        let outer_condition = vregs.alloc();
        let inner_condition = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);

        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: outer_value,
            value: 10,
        });
        entry.push(MInst::LoadImm {
            dst: inner_value,
            value: 20,
        });
        entry.push(MInst::LoadImm {
            dst: outer_condition,
            value: 1,
        });
        entry.push(MInst::LoadImm {
            dst: inner_condition,
            value: 1,
        });
        entry.push(MInst::Jump { target: BlockId(1) });

        let mut outer_header = MBlock::new(BlockId(1));
        outer_header.push(MInst::Store {
            base: crate::backend::native::mir::BaseReg::SimState,
            offset: 0,
            src: outer_value,
            size: crate::backend::native::mir::OpSize::S64,
        });
        outer_header.push(MInst::Branch {
            cond: outer_condition,
            true_bb: BlockId(2),
            false_bb: BlockId(5),
        });

        let mut inner_header = MBlock::new(BlockId(2));
        inner_header.push(MInst::Branch {
            cond: inner_condition,
            true_bb: BlockId(3),
            false_bb: BlockId(4),
        });
        let mut inner_body = MBlock::new(BlockId(3));
        inner_body.push(MInst::Store {
            base: crate::backend::native::mir::BaseReg::SimState,
            offset: 8,
            src: inner_value,
            size: crate::backend::native::mir::OpSize::S64,
        });
        inner_body.push(MInst::Jump { target: BlockId(2) });
        let mut outer_latch = MBlock::new(BlockId(4));
        outer_latch.push(MInst::Jump { target: BlockId(1) });
        let mut exit = MBlock::new(BlockId(5));
        exit.push(MInst::Return);
        func.blocks = vec![
            entry,
            outer_header,
            inner_header,
            inner_body,
            outer_latch,
            exit,
        ];

        let cfg = super::super::cfg::normalize(&mut func);
        let analysis = analyze(&func, &cfg);
        analysis.verify(&func, &cfg);
        let outer_header = cfg.block_index[&BlockId(1)];
        let inner_header = cfg.block_index[&BlockId(2)];
        let outer_region = analysis.region_at_entry(outer_header).unwrap();
        let inner_region = analysis.region_at_entry(inner_header).unwrap();
        assert_ne!(outer_region, inner_region);
        let outer = &analysis.loop_regions[outer_region];
        let inner = &analysis.loop_regions[inner_region];
        assert_eq!(outer.kind, LoopRegionKind::Natural);
        assert_eq!(inner.kind, LoopRegionKind::Natural);
        assert!(inner.used.contains(&inner_value));
        assert!(!inner.used.contains(&outer_value));
        assert!(outer.used.contains(&inner_value));
        assert!(outer.used.contains(&outer_value));
        for (header, facts) in [(outer_header, outer), (inner_header, inner)] {
            let natural_loop = cfg
                .loops
                .iter()
                .find(|natural_loop| natural_loop.header == header)
                .unwrap();
            assert_eq!(
                facts.max_pressure,
                natural_loop
                    .blocks
                    .iter()
                    .map(|&block| analysis.block_max_pressure[block])
                    .max()
                    .unwrap()
            );
        }
        assert_eq!(analysis.region_work.block_scans, func.blocks.len());
        assert_eq!(
            analysis.region_work.instruction_visits,
            func.blocks
                .iter()
                .map(|block| block.insts.len())
                .sum::<usize>()
        );
        assert_eq!(analysis.region_work.propagated_values, inner.used.len());
    }

    #[test]
    fn loop_exit_use_is_farther_than_loop_use() {
        let mut vregs = VRegAllocator::new();
        let value = vregs.alloc();
        let condition = vregs.alloc();
        let inside = vregs.alloc();
        let outside = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: value,
            value: 1,
        });
        entry.push(MInst::LoadImm {
            dst: condition,
            value: 1,
        });
        entry.push(MInst::Jump { target: BlockId(1) });
        let mut header = MBlock::new(BlockId(1));
        header.push(MInst::Add {
            dst: inside,
            lhs: condition,
            rhs: condition,
        });
        header.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut exit = MBlock::new(BlockId(2));
        exit.push(MInst::Add {
            dst: outside,
            lhs: value,
            rhs: value,
        });
        exit.push(MInst::Return);
        func.blocks = vec![entry, header, exit];
        let cfg = super::super::cfg::normalize(&mut func);
        let analysis = analyze(&func, &cfg);
        let header = cfg.block_index[&BlockId(1)];
        assert!(matches!(
            analysis.exit[header][&value],
            NextUseDistance::Finite {
                loop_exits: 1..,
                ..
            }
        ));
    }

    #[test]
    fn next_iteration_use_beats_exit_use_after_more_than_100k_instructions() {
        const LOOP_BODY: usize = 100_001;

        let mut vregs = VRegAllocator::new();
        let inside_value = vregs.alloc();
        let exit_value = vregs.alloc();
        let condition = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);

        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: inside_value,
            value: 11,
        });
        entry.push(MInst::LoadImm {
            dst: exit_value,
            value: 22,
        });
        entry.push(MInst::LoadImm {
            dst: condition,
            value: 1,
        });
        entry.push(MInst::Jump { target: BlockId(1) });

        let mut header = MBlock::new(BlockId(1));
        header.insts.extend((0..LOOP_BODY).map(|_| MInst::MemCopy {
            src_offset: 0,
            dst_offset: 8,
            byte_len: 8,
        }));
        header.push(MInst::Store {
            base: crate::backend::native::mir::BaseReg::SimState,
            offset: 0,
            src: inside_value,
            size: crate::backend::native::mir::OpSize::S64,
        });
        header.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });

        let mut exit = MBlock::new(BlockId(2));
        exit.push(MInst::Store {
            base: crate::backend::native::mir::BaseReg::SimState,
            offset: 8,
            src: exit_value,
            size: crate::backend::native::mir::OpSize::S64,
        });
        exit.push(MInst::Return);
        func.blocks = vec![entry, header, exit];

        let cfg = super::super::cfg::normalize(&mut func);
        let analysis = analyze(&func, &cfg);
        let header = cfg.block_index[&BlockId(1)];
        // Query immediately after this iteration's inside-loop use.  Its next
        // occurrence is in the next iteration, beyond the 100,001-instruction
        // body; the other value is used immediately after leaving the loop.
        let point_after_inside_use = LOOP_BODY + 1;
        let inside = analysis.distance_at(&func, header, point_after_inside_use, inside_value);
        let outside = analysis.distance_at(&func, header, point_after_inside_use, exit_value);

        assert!(
            matches!(
                inside,
                NextUseDistance::Finite {
                    loop_exits: 0,
                    instructions
                } if instructions > 100_000
            ),
            "inside-loop distance was {inside:?}"
        );
        assert!(
            matches!(
                outside,
                NextUseDistance::Finite {
                    loop_exits: 1..,
                    ..
                }
            ),
            "loop-exit distance was {outside:?}"
        );
        assert!(
            inside < outside,
            "lexicographic distance must prefer the next loop iteration: inside={inside:?}, outside={outside:?}"
        );
    }
}
