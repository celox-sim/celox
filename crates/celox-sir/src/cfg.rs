//! SIR adapter for the IR-independent control-flow analyses.

use std::fmt;

use celox_analysis::cfg::{ControlFlowGraph, ForwardControlFlowGraph};
pub use celox_analysis::cfg::{
    DominatorTree, NaturalLoop, PostDominatorTree, StronglyConnectedRegion,
};

use crate::HashMap;

use super::{BlockId, ExecutionUnit, SIRTerminator};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SirCfgError {
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

/// Dominance queries for SIR rewrites which do not inspect loops or build SSA.
/// Keeping this separate from `SirCfg` avoids building and dropping unused
/// per-block analysis tables after each local CFG rewrite.
#[derive(Debug, Clone)]
pub struct SirDominance {
    index: HashMap<BlockId, usize>,
    dominators: DominatorTree,
}

impl SirDominance {
    pub fn analyze<A>(eu: &ExecutionUnit<A>) -> Result<Self, SirCfgError> {
        let IndexedSirGraph {
            index, successors, ..
        } = index_sir_graph(eu)?;
        let dominators = DominatorTree::compute(&successors, 0).map_err(map_analysis_error)?;
        Ok(Self { index, dominators })
    }

    pub fn dominates(&self, dominator: BlockId, block: BlockId) -> bool {
        let (Some(&dominator), Some(&block)) = (self.index.get(&dominator), self.index.get(&block))
        else {
            return false;
        };
        self.dominators.dominates(dominator, block)
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // Some analyses are intentionally ahead of their first consumer.
pub struct SirCfg {
    /// Reachable blocks in deterministic reverse postorder. The entry is 0.
    pub block_ids: Vec<BlockId>,
    pub index: HashMap<BlockId, usize>,
    pub predecessors: Vec<Vec<usize>>,
    pub successors: Vec<Vec<usize>>,
    pub dominators: DominatorTree,
    pub dom_children: Vec<Vec<usize>>,
    pub dominance_frontier: Vec<Vec<usize>>,
    pub postdominators: Option<PostDominatorTree>,
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
    pub fn analyze<A>(eu: &ExecutionUnit<A>) -> Result<Self, SirCfgError> {
        let IndexedSirGraph {
            block_ids,
            index,
            successors,
        } = index_sir_graph(eu)?;
        let analysis = ControlFlowGraph::analyze(successors, 0).map_err(map_analysis_error)?;
        let dom_children = analysis.dominators.children.clone();
        Ok(Self {
            block_ids,
            index,
            predecessors: analysis.predecessors,
            successors: analysis.successors,
            dominators: analysis.dominators,
            dom_children,
            dominance_frontier: analysis.dominance_frontier,
            postdominators: Some(analysis.postdominators),
            postdominance_frontier: analysis.postdominance_frontier,
            controllers: analysis.controllers,
            control_dependents: analysis.control_dependents,
            sccs: analysis.sccs,
            scc_for_block: analysis.scc_for_block,
            loops: analysis.loops,
        })
    }

    /// Analyze only forward dominance, loops, and SCCs.
    ///
    /// This mode intentionally omits postdominance and control-dependence
    /// tables, which can be dense on a large branchy SIR function.
    pub fn analyze_forward<A>(eu: &ExecutionUnit<A>) -> Result<Self, SirCfgError> {
        let IndexedSirGraph {
            block_ids,
            index,
            successors,
        } = index_sir_graph(eu)?;
        let analysis =
            ForwardControlFlowGraph::analyze(successors, 0).map_err(map_analysis_error)?;
        let dom_children = analysis.dominators.children.clone();
        let blocks = block_ids.len();
        Ok(Self {
            block_ids,
            index,
            predecessors: analysis.predecessors,
            successors: analysis.successors,
            dominators: analysis.dominators,
            dom_children,
            dominance_frontier: analysis.dominance_frontier,
            postdominators: None,
            postdominance_frontier: vec![Vec::new(); blocks],
            controllers: vec![Vec::new(); blocks],
            control_dependents: vec![Vec::new(); blocks],
            sccs: analysis.sccs,
            scc_for_block: analysis.scc_for_block,
            loops: analysis.loops,
        })
    }

    /// Analyze only forward structural properties without constructing a
    /// dominance frontier.  Use this for repeated legality queries which need
    /// dominance but do not build SSA.
    pub fn analyze_forward_structure<A>(eu: &ExecutionUnit<A>) -> Result<Self, SirCfgError> {
        let IndexedSirGraph {
            block_ids,
            index,
            successors,
        } = index_sir_graph(eu)?;
        let analysis = ForwardControlFlowGraph::analyze_structure(successors, 0)
            .map_err(map_analysis_error)?;
        let dom_children = analysis.dominators.children.clone();
        let blocks = block_ids.len();
        Ok(Self {
            block_ids,
            index,
            predecessors: analysis.predecessors,
            successors: analysis.successors,
            dominators: analysis.dominators,
            dom_children,
            dominance_frontier: analysis.dominance_frontier,
            postdominators: None,
            postdominance_frontier: vec![Vec::new(); blocks],
            controllers: vec![Vec::new(); blocks],
            control_dependents: vec![Vec::new(); blocks],
            sccs: analysis.sccs,
            scc_for_block: analysis.scc_for_block,
            loops: analysis.loops,
        })
    }

    /// Analyze forward and reverse structural properties without materializing
    /// dominance frontiers or control-dependence tables.
    pub fn analyze_structure<A>(eu: &ExecutionUnit<A>) -> Result<Self, SirCfgError> {
        let IndexedSirGraph {
            block_ids,
            index,
            successors,
        } = index_sir_graph(eu)?;
        let analysis =
            ControlFlowGraph::analyze_structure(successors, 0).map_err(map_analysis_error)?;
        let dom_children = analysis.dominators.children.clone();
        Ok(Self {
            block_ids,
            index,
            predecessors: analysis.predecessors,
            successors: analysis.successors,
            dominators: analysis.dominators,
            dom_children,
            dominance_frontier: analysis.dominance_frontier,
            postdominators: Some(analysis.postdominators),
            postdominance_frontier: analysis.postdominance_frontier,
            controllers: analysis.controllers,
            control_dependents: analysis.control_dependents,
            sccs: analysis.sccs,
            scc_for_block: analysis.scc_for_block,
            loops: analysis.loops,
        })
    }

    pub fn block_index(&self, block: BlockId) -> Option<usize> {
        self.index.get(&block).copied()
    }

    pub fn dominates(&self, dominator: BlockId, block: BlockId) -> bool {
        let (Some(dominator), Some(block)) = (self.block_index(dominator), self.block_index(block))
        else {
            return false;
        };
        self.dominators.dominates(dominator, block)
    }

    pub fn common_dominator(&self, left: BlockId, right: BlockId) -> Option<BlockId> {
        let left = self.block_index(left)?;
        let right = self.block_index(right)?;
        self.dominators
            .lca(left, right)
            .map(|block| self.block_ids[block])
    }

    pub fn postdominates(&self, postdominator: BlockId, block: BlockId) -> bool {
        let (Some(postdominator), Some(block)) =
            (self.block_index(postdominator), self.block_index(block))
        else {
            return false;
        };
        self.postdominators
            .as_ref()
            .is_some_and(|tree| tree.postdominates(postdominator, block))
    }

    pub fn common_postdominator(&self, left: BlockId, right: BlockId) -> Option<BlockId> {
        let left = self.block_index(left)?;
        let right = self.block_index(right)?;
        self.postdominators
            .as_ref()?
            .common_postdominator(left, right)
            .map(|block| self.block_ids[block])
    }

    pub fn immediate_postdominator(&self, block: usize) -> Option<usize> {
        self.postdominators.as_ref()?.immediate_postdominator(block)
    }
}

struct IndexedSirGraph {
    block_ids: Vec<BlockId>,
    index: HashMap<BlockId, usize>,
    successors: Vec<Vec<usize>>,
}

fn index_sir_graph<A>(eu: &ExecutionUnit<A>) -> Result<IndexedSirGraph, SirCfgError> {
    if eu.blocks.is_empty() {
        return Err(SirCfgError::Empty);
    }
    if !eu.blocks.contains_key(&eu.entry_block_id) {
        return Err(SirCfgError::MissingEntry(eu.entry_block_id));
    }

    let mut source_block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    source_block_ids.sort_unstable();
    let source_index = source_block_ids
        .iter()
        .copied()
        .enumerate()
        .map(|(index, block)| (block, index))
        .collect::<HashMap<_, _>>();
    let mut source_successors = vec![Vec::new(); source_block_ids.len()];
    for (source, &block_id) in source_block_ids.iter().enumerate() {
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
        for target in outgoing {
            let Some(&target_index) = source_index.get(&target) else {
                return Err(SirCfgError::MissingTarget {
                    source: block_id,
                    target,
                });
            };
            source_successors[source].push(target_index);
        }
    }

    let source_entry = source_index[&eu.entry_block_id];
    let order = celox_analysis::cfg::reverse_postorder(&source_successors, source_entry)
        .map_err(map_analysis_error)?;
    if order.len() != source_block_ids.len() {
        let mut reached = vec![false; source_block_ids.len()];
        for &block in &order {
            reached[block] = true;
        }
        return Err(SirCfgError::Unreachable(
            source_block_ids
                .iter()
                .copied()
                .enumerate()
                .filter_map(|(index, block)| (!reached[index]).then_some(block))
                .collect(),
        ));
    }

    let block_ids = order
        .iter()
        .map(|&source| source_block_ids[source])
        .collect::<Vec<_>>();
    let index = block_ids
        .iter()
        .copied()
        .enumerate()
        .map(|(index, block)| (block, index))
        .collect::<HashMap<_, _>>();
    let mut successors = vec![Vec::new(); block_ids.len()];
    for (block, &source) in order.iter().enumerate() {
        successors[block] = source_successors[source]
            .iter()
            .map(|&target| index[&source_block_ids[target]])
            .collect();
    }

    Ok(IndexedSirGraph {
        block_ids,
        index,
        successors,
    })
}

impl celox_analysis::ssa::SsaCfg for SirCfg {
    type FrontierIter<'a> = std::iter::Copied<std::slice::Iter<'a, usize>>;

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
        &self.dom_children
    }

    fn dominance_frontier_len(&self) -> usize {
        self.dominance_frontier.len()
    }

    fn dominance_frontier(&self, block: usize) -> Self::FrontierIter<'_> {
        self.dominance_frontier[block].iter().copied()
    }
}

fn map_analysis_error(error: celox_analysis::cfg::CfgError) -> SirCfgError {
    match error {
        celox_analysis::cfg::CfgError::InvalidGraph(message) => SirCfgError::InvalidGraph(message),
        celox_analysis::cfg::CfgError::Empty => SirCfgError::Empty,
        celox_analysis::cfg::CfgError::Unreachable(_) => {
            SirCfgError::InvalidGraph("shared CFG unexpectedly found an unreachable SIR block")
        }
        celox_analysis::cfg::CfgError::InvalidRoot { .. }
        | celox_analysis::cfg::CfgError::EdgeOutOfRange { .. } => {
            SirCfgError::InvalidGraph("shared CFG rejected validated SIR block indices")
        }
    }
}

pub fn terminator_successors(terminator: &SIRTerminator) -> Vec<BlockId> {
    match terminator {
        SIRTerminator::Jump(target, _) => vec![*target],
        SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } => vec![true_block.0, false_block.0],
        SIRTerminator::Switch { cases, default, .. } => cases
            .iter()
            .map(|case| case.target)
            .chain(std::iter::once(*default))
            .collect(),
        SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests;
