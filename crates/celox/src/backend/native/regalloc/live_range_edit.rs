//! Strict-SSA live-range editing for the greedy allocator.
//!
//! LLVM's SplitEditor can attach several value numbers to one virtual
//! register because it runs after machine SSA.  Celox keeps allocation IR in
//! strict SSA until atomic publication, so each split boundary is a real copy
//! definition.  Pruned iterated dominance frontiers receive merge phis and a
//! dominator-tree rename gives every resulting representative one definition.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use crate::backend::native::mir::{BlockId, VReg};

use super::allocation_ir::{
    AllocationIr, AllocationIrError, InsertedSplitCopy, InsertedSyntheticPhi, SplitCopyPlacement,
};
use super::cfg::NormalizedCfg;
use super::live_interval::{
    DefinitionSite, LiveInterval, LiveIntervals, LivenessProgram, SlotIndex, UseSite,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct LiveRangeCut {
    pub block: BlockId,
    pub slot: SlotIndex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct EditedUse {
    pub site: UseSite,
    pub value: VReg,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LiveRangeEditResult {
    pub source: VReg,
    pub copies: Vec<InsertedSplitCopy>,
    pub phis: Vec<InsertedSyntheticPhi>,
    /// New owner of every semantic use from the source interval.  Transition
    /// copy/phi uses are intentionally excluded from this map.
    pub semantic_uses: Vec<EditedUse>,
    pub representatives: Vec<VReg>,
    pub changed_blocks: BTreeSet<BlockId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LiveRangeEditError {
    pub rule: &'static str,
    pub block: Option<BlockId>,
    pub value: Option<VReg>,
    pub message: String,
}

impl LiveRangeEditError {
    fn new(
        rule: &'static str,
        block: Option<BlockId>,
        value: Option<VReg>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            rule,
            block,
            value,
            message: message.into(),
        }
    }

    fn ir(error: AllocationIrError) -> Self {
        Self::new(
            error.rule,
            error.block,
            error.values.first().copied(),
            error.message,
        )
    }
}

impl fmt::Display for LiveRangeEditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.rule)?;
        if let Some(block) = self.block {
            write!(formatter, " at {block}")?;
        }
        if let Some(value) = self.value {
            write!(formatter, " value={value}")?;
        }
        write!(formatter, ": {}", self.message)
    }
}

impl std::error::Error for LiveRangeEditError {}

/// Insert real copies at exact frontier cuts and reconstruct strict SSA.
///
/// This mutates only private allocation IR.  The caller publishes the pending
/// liveness journal through `allocation_expand::refresh`, then requeues every
/// live representative returned here.
pub(super) fn edit_live_range(
    ir: &mut AllocationIr,
    cfg: &NormalizedCfg,
    intervals: &LiveIntervals,
    source: VReg,
    cuts: &[LiveRangeCut],
) -> Result<LiveRangeEditResult, LiveRangeEditError> {
    let interval = intervals
        .intervals
        .get(source.0 as usize)
        .and_then(Option::as_ref)
        .filter(|interval| interval.value == source)
        .cloned()
        .ok_or_else(|| {
            LiveRangeEditError::new(
                "LIVE_RANGE_EDIT.SOURCE_INTERVAL",
                None,
                Some(source),
                "split source has no current exact live interval",
            )
        })?;
    verify_cfg_shape(ir, cfg, intervals, source)?;
    if cuts.is_empty() {
        return Err(LiveRangeEditError::new(
            "LIVE_RANGE_EDIT.EMPTY_CUTS",
            Some(interval.definition.block()),
            Some(source),
            "live-range edit requires at least one exact frontier cut",
        ));
    }

    let mut placements = cuts
        .iter()
        .copied()
        .map(|cut| {
            ir.plan_split_copy_before(&interval, cut.block, cut.slot)
                .map_err(LiveRangeEditError::ir)
        })
        .collect::<Result<Vec<_>, _>>()?;
    placements.sort_unstable_by_key(|placement| {
        (
            placement.block,
            placement.use_slot,
            placement.definition_slot,
        )
    });
    placements.dedup_by_key(|placement| {
        (
            placement.block,
            placement.use_slot,
            placement.definition_slot,
        )
    });

    ir.begin_instruction_transaction()
        .map_err(LiveRangeEditError::ir)?;
    let copies = insert_copies(ir, source, &placements)?;
    ir.publish_instruction_transaction()
        .map_err(LiveRangeEditError::ir)?;

    let phis_by_block = place_merge_phis(ir, cfg, intervals, &interval, &copies)?;
    let (semantic_uses, mut changed_blocks) =
        rename_representatives(ir, cfg, &interval, &copies, &phis_by_block)?;
    changed_blocks.extend(copies.iter().map(|copy| copy.definition_site.block()));
    changed_blocks.extend(phis_by_block.values().map(|phi| phi.block));
    for &block in phis_by_block.keys() {
        changed_blocks.extend(
            cfg.predecessors[block]
                .iter()
                .map(|&predecessor| ir.block_id(predecessor)),
        );
    }

    let phis = phis_by_block.values().copied().collect::<Vec<_>>();
    let mut representatives = vec![source];
    representatives.extend(copies.iter().map(|copy| copy.definition));
    representatives.extend(phis.iter().map(|phi| phi.definition));
    representatives.sort_unstable();
    representatives.dedup();
    Ok(LiveRangeEditResult {
        source,
        copies,
        phis,
        semantic_uses,
        representatives,
        changed_blocks,
    })
}

fn verify_cfg_shape(
    ir: &AllocationIr,
    cfg: &NormalizedCfg,
    intervals: &LiveIntervals,
    source: VReg,
) -> Result<(), LiveRangeEditError> {
    let blocks = ir.block_count();
    if blocks == 0
        || cfg.block_index.len() != blocks
        || cfg.predecessors.len() != blocks
        || cfg.successors.len() != blocks
        || cfg.idom.len() != blocks
        || cfg.dominance_frontier.len() != blocks
        || intervals.block_slots.len() != blocks
        || (0..blocks).any(|block| cfg.block_index.get(&ir.block_id(block)) != Some(&block))
    {
        return Err(LiveRangeEditError::new(
            "LIVE_RANGE_EDIT.CFG_SHAPE",
            None,
            Some(source),
            "allocation IR, exact liveness, and normalized CFG have different block domains",
        ));
    }
    Ok(())
}

fn insert_copies(
    ir: &mut AllocationIr,
    source: VReg,
    placements: &[SplitCopyPlacement],
) -> Result<Vec<InsertedSplitCopy>, LiveRangeEditError> {
    placements
        .iter()
        .copied()
        .map(|placement| {
            ir.insert_planned_split_copy(placement, source)
                .map_err(LiveRangeEditError::ir)
        })
        .collect()
}

/// Standard pruned iterated-dominance-frontier placement.  Liveness is read
/// from the pre-edit interval: copies preserve the logical value, so exactly
/// those block entries need a merge definition after its def set is enlarged.
fn place_merge_phis(
    ir: &mut AllocationIr,
    cfg: &NormalizedCfg,
    intervals: &LiveIntervals,
    source: &LiveInterval,
    copies: &[InsertedSplitCopy],
) -> Result<BTreeMap<usize, InsertedSyntheticPhi>, LiveRangeEditError> {
    let source_block = cfg
        .block_index
        .get(&source.definition.block())
        .copied()
        .ok_or_else(|| {
            LiveRangeEditError::new(
                "LIVE_RANGE_EDIT.DEFINITION_BLOCK",
                Some(source.definition.block()),
                Some(source.value),
                "source definition is outside the normalized CFG",
            )
        })?;
    let mut definition_blocks = BTreeSet::from([source_block]);
    for copy in copies {
        let block = cfg
            .block_index
            .get(&copy.definition_site.block())
            .copied()
            .ok_or_else(|| {
                LiveRangeEditError::new(
                    "LIVE_RANGE_EDIT.COPY_BLOCK",
                    Some(copy.definition_site.block()),
                    Some(copy.definition),
                    "split copy is outside the normalized CFG",
                )
            })?;
        definition_blocks.insert(block);
    }

    let live_in = (0..ir.block_count())
        .map(|block| source.covers(ir.block_id(block), intervals.block_slots[block].entry))
        .collect::<Vec<_>>();
    let mut work = definition_blocks.iter().copied().collect::<VecDeque<_>>();
    let mut phis = BTreeMap::<usize, InsertedSyntheticPhi>::new();
    while let Some(definition_block) = work.pop_front() {
        for &frontier in &cfg.dominance_frontier[definition_block] {
            if !live_in[frontier] || phis.contains_key(&frontier) {
                continue;
            }
            let block = ir.block_id(frontier);
            let phi = ir
                .insert_synthetic_phi(block)
                .map_err(LiveRangeEditError::ir)?;
            phis.insert(frontier, phi);
            if !definition_blocks.contains(&frontier) {
                work.push_back(frontier);
            }
        }
    }
    Ok(phis)
}

#[derive(Debug, Clone, Copy)]
struct RenameUse {
    site: UseSite,
    semantic: bool,
}

fn rename_representatives(
    ir: &mut AllocationIr,
    cfg: &NormalizedCfg,
    source: &LiveInterval,
    copies: &[InsertedSplitCopy],
    phis: &BTreeMap<usize, InsertedSyntheticPhi>,
) -> Result<(Vec<EditedUse>, BTreeSet<BlockId>), LiveRangeEditError> {
    let blocks = ir.block_count();
    let mut uses = vec![Vec::<RenameUse>::new(); blocks];
    for &site in &source.uses {
        let block = cfg.block_index.get(&site.block()).copied().ok_or_else(|| {
            LiveRangeEditError::new(
                "LIVE_RANGE_EDIT.USE_BLOCK",
                Some(site.block()),
                Some(source.value),
                "source use is outside the normalized CFG",
            )
        })?;
        uses[block].push(RenameUse {
            site,
            semantic: true,
        });
    }
    for copy in copies {
        let block = cfg
            .block_index
            .get(&copy.source_use.block())
            .copied()
            .ok_or_else(|| {
                LiveRangeEditError::new(
                    "LIVE_RANGE_EDIT.COPY_BLOCK",
                    Some(copy.source_use.block()),
                    Some(copy.definition),
                    "split-copy source is outside the normalized CFG",
                )
            })?;
        uses[block].push(RenameUse {
            site: copy.source_use,
            semantic: false,
        });
    }
    for block_uses in &mut uses {
        block_uses.sort_unstable_by_key(|use_| use_.site);
        if block_uses
            .windows(2)
            .any(|pair| pair[0].site == pair[1].site)
        {
            return Err(LiveRangeEditError::new(
                "LIVE_RANGE_EDIT.DUPLICATE_USE",
                block_uses.first().map(|use_| use_.site.block()),
                Some(source.value),
                "one physical use was enrolled more than once in SSA renaming",
            ));
        }
    }

    let mut definitions = vec![Vec::<(DefinitionSite, VReg)>::new(); blocks];
    let source_definition_block = cfg.block_index[&source.definition.block()];
    definitions[source_definition_block].push((source.definition, source.value));
    for copy in copies {
        let block = cfg.block_index[&copy.definition_site.block()];
        definitions[block].push((copy.definition_site, copy.definition));
    }
    for block_definitions in &mut definitions {
        block_definitions.sort_unstable_by_key(|(definition, _)| definition.slot());
        if block_definitions
            .windows(2)
            .any(|pair| pair[0].0.slot() == pair[1].0.slot())
        {
            return Err(LiveRangeEditError::new(
                "LIVE_RANGE_EDIT.DUPLICATE_DEFINITION",
                block_definitions
                    .first()
                    .map(|(definition, _)| definition.block()),
                Some(source.value),
                "two logical definitions occupy one allocation coordinate",
            ));
        }
    }
    if matches!(source.definition, DefinitionSite::Phi { .. })
        && phis.contains_key(&source_definition_block)
    {
        return Err(LiveRangeEditError::new(
            "LIVE_RANGE_EDIT.PHI_DEFINITION_COLLISION",
            Some(source.definition.block()),
            Some(source.value),
            "pruned SSA attempted to place a merge before a non-live-in source phi",
        ));
    }

    let mut children = vec![Vec::new(); blocks];
    for block in 1..blocks {
        let parent = cfg.idom[block].ok_or_else(|| {
            LiveRangeEditError::new(
                "LIVE_RANGE_EDIT.DOMINATOR_TREE",
                Some(ir.block_id(block)),
                Some(source.value),
                "reachable block has no immediate dominator",
            )
        })?;
        if parent >= blocks {
            return Err(LiveRangeEditError::new(
                "LIVE_RANGE_EDIT.DOMINATOR_TREE",
                Some(ir.block_id(block)),
                Some(source.value),
                "immediate dominator is outside the CFG",
            ));
        }
        children[parent].push(block);
    }

    let mut semantic_uses = Vec::with_capacity(source.uses.len());
    let mut changed_blocks = BTreeSet::new();
    let mut phi_sources = BTreeMap::<usize, Vec<(BlockId, VReg)>>::new();
    let mut visited = vec![false; blocks];
    let mut work = vec![(0usize, None::<VReg>)];
    while let Some((block, inherited)) = work.pop() {
        if std::mem::replace(&mut visited[block], true) {
            return Err(LiveRangeEditError::new(
                "LIVE_RANGE_EDIT.DOMINATOR_TREE",
                Some(ir.block_id(block)),
                Some(source.value),
                "dominator tree visits one block more than once",
            ));
        }
        let mut current = phis.get(&block).map(|phi| phi.definition).or(inherited);
        let mut use_index = 0usize;
        let mut definition_index = 0usize;
        while use_index < uses[block].len() || definition_index < definitions[block].len() {
            let next_use = uses[block].get(use_index).copied();
            let next_definition = definitions[block].get(definition_index).copied();
            let take_use = match (next_use, next_definition) {
                (Some(use_), Some((definition, _))) => {
                    if use_.site.slot() == definition.slot() {
                        return Err(LiveRangeEditError::new(
                            "LIVE_RANGE_EDIT.EVENT_ORDER",
                            Some(ir.block_id(block)),
                            Some(source.value),
                            "logical use and definition occupy one allocation coordinate",
                        ));
                    }
                    use_.site.slot() < definition.slot()
                }
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };
            if take_use {
                let use_ = next_use.expect("take_use requires a use event");
                let replacement = current.ok_or_else(|| {
                    LiveRangeEditError::new(
                        "LIVE_RANGE_EDIT.UNDEFINED_USE",
                        Some(use_.site.block()),
                        Some(source.value),
                        "dominator rename reached a logical use without a reaching definition",
                    )
                })?;
                if replacement != source.value {
                    ir.rewrite_use(use_.site, source.value, replacement)
                        .map_err(LiveRangeEditError::ir)?;
                    changed_blocks.insert(use_.site.block());
                }
                if use_.semantic {
                    semantic_uses.push(EditedUse {
                        site: use_.site,
                        value: replacement,
                    });
                }
                use_index += 1;
            } else {
                let (_, value) = next_definition.expect("definition event must exist");
                current = Some(value);
                definition_index += 1;
            }
        }

        for &successor in &cfg.successors[block] {
            if phis.contains_key(&successor) {
                let value = current.ok_or_else(|| {
                    LiveRangeEditError::new(
                        "LIVE_RANGE_EDIT.UNDEFINED_PHI_SOURCE",
                        Some(ir.block_id(block)),
                        Some(source.value),
                        "merge block is live-in but one predecessor has no reaching definition",
                    )
                })?;
                phi_sources
                    .entry(successor)
                    .or_default()
                    .push((ir.block_id(block), value));
            }
        }
        for &child in children[block].iter().rev() {
            work.push((child, current));
        }
    }
    if visited.iter().any(|visited| !visited) {
        return Err(LiveRangeEditError::new(
            "LIVE_RANGE_EDIT.DOMINATOR_TREE",
            None,
            Some(source.value),
            "dominator tree does not cover every normalized block",
        ));
    }

    for (&block, phi) in phis {
        let sources = phi_sources.remove(&block).unwrap_or_default();
        ir.set_synthetic_phi_sources(phi.block, phi.phi, sources)
            .map_err(LiveRangeEditError::ir)?;
    }
    if !phi_sources.is_empty() {
        return Err(LiveRangeEditError::new(
            "LIVE_RANGE_EDIT.PHI_SOURCE_SET",
            None,
            Some(source.value),
            "rename produced sources for a merge phi that was not placed",
        ));
    }
    semantic_uses.sort_unstable_by_key(|use_| use_.site);
    if semantic_uses.len() != source.uses.len() {
        return Err(LiveRangeEditError::new(
            "LIVE_RANGE_EDIT.SEMANTIC_USE_COVERAGE",
            None,
            Some(source.value),
            "dominator rename did not map every source semantic use exactly once",
        ));
    }
    Ok((semantic_uses, changed_blocks))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::mir::{MBlock, MFunction, MInst, SpillDesc, VRegAllocator};
    use crate::backend::native::regalloc::allocation_ir::AllocationIr;
    use crate::backend::native::regalloc::live_interval::IncrementalLiveness;

    fn function(value_count: u32, blocks: Vec<MBlock>) -> MFunction {
        let mut values = VRegAllocator::new();
        for _ in 0..value_count {
            values.alloc();
        }
        let mut function =
            MFunction::new(values, vec![SpillDesc::transient(); value_count as usize]);
        function.blocks = blocks;
        function
    }

    #[test]
    fn diamond_frontiers_create_copies_one_merge_and_exact_child_intervals() {
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: VReg(0),
            value: 7,
        });
        entry.push(MInst::LoadImm {
            dst: VReg(1),
            value: 1,
        });
        entry.push(MInst::Branch {
            cond: VReg(1),
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::Jump { target: BlockId(3) });
        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::Jump { target: BlockId(3) });
        let mut merge = MBlock::new(BlockId(3));
        merge.push(MInst::Mov {
            dst: VReg(2),
            src: VReg(0),
        });
        merge.push(MInst::Return);
        let mut function = function(3, vec![entry, left, right, merge]);
        let cfg = super::super::cfg::normalize(&mut function).unwrap();
        let graph = super::super::home_graph::build(&function, &cfg).unwrap();
        let mut ir = AllocationIr::from_mir(&function).unwrap();
        let mut intervals = ir.analyze(&cfg).unwrap();
        let mut incremental = IncrementalLiveness::build(&ir, &cfg, &intervals).unwrap();
        let left = cfg.block_index[&BlockId(1)];
        let right = cfg.block_index[&BlockId(2)];
        let cuts = [
            LiveRangeCut {
                block: BlockId(1),
                slot: intervals.block_slots[left].instruction_use(0).unwrap(),
            },
            LiveRangeCut {
                block: BlockId(2),
                slot: intervals.block_slots[right].instruction_use(0).unwrap(),
            },
        ];

        let edit = edit_live_range(&mut ir, &cfg, &intervals, VReg(0), &cuts).unwrap();
        assert_eq!(edit.copies.len(), 2);
        assert_eq!(edit.phis.len(), 1);
        assert_eq!(edit.phis[0].block, BlockId(3));
        assert_eq!(edit.semantic_uses.len(), 1);
        assert_eq!(edit.semantic_uses[0].value, edit.phis[0].definition);

        let delta = ir.take_liveness_delta();
        incremental
            .update_fact_delta(&ir, &cfg, &mut intervals, delta)
            .unwrap();
        assert_eq!(intervals, ir.analyze(&cfg).unwrap());
        for value in &edit.representatives {
            assert!(intervals.intervals[value.0 as usize].is_some());
        }
        assert!(
            !intervals.intervals[VReg(0).0 as usize]
                .as_ref()
                .unwrap()
                .segments
                .iter()
                .any(|segment| segment.block == BlockId(3))
        );

        let lowered = ir.materialize(&function, &graph, &[]).unwrap();
        lowered.verify_result().unwrap();
    }

    #[test]
    fn loop_backedge_copy_places_header_phi_and_renames_next_iteration() {
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: VReg(0),
            value: 7,
        });
        entry.push(MInst::LoadImm {
            dst: VReg(1),
            value: 1,
        });
        entry.push(MInst::Jump { target: BlockId(1) });
        let mut header = MBlock::new(BlockId(1));
        header.push(MInst::Mov {
            dst: VReg(2),
            src: VReg(0),
        });
        header.push(MInst::Branch {
            cond: VReg(1),
            true_bb: BlockId(2),
            false_bb: BlockId(3),
        });
        let mut body = MBlock::new(BlockId(2));
        body.push(MInst::Jump { target: BlockId(1) });
        let mut exit = MBlock::new(BlockId(3));
        exit.push(MInst::Mov {
            dst: VReg(3),
            src: VReg(0),
        });
        exit.push(MInst::Return);
        let mut function = function(4, vec![entry, header, body, exit]);
        let cfg = super::super::cfg::normalize(&mut function).unwrap();
        let graph = super::super::home_graph::build(&function, &cfg).unwrap();
        let mut ir = AllocationIr::from_mir(&function).unwrap();
        let mut intervals = ir.analyze(&cfg).unwrap();
        let mut incremental = IncrementalLiveness::build(&ir, &cfg, &intervals).unwrap();
        let body = cfg.block_index[&BlockId(2)];
        let cut = LiveRangeCut {
            block: BlockId(2),
            slot: intervals.block_slots[body].instruction_use(0).unwrap(),
        };

        let edit = edit_live_range(&mut ir, &cfg, &intervals, VReg(0), &[cut]).unwrap();
        assert_eq!(edit.copies.len(), 1);
        assert_eq!(edit.phis.len(), 1);
        assert_eq!(edit.phis[0].block, BlockId(1));
        let header_phi = edit.phis[0].definition;
        assert!(
            edit.semantic_uses
                .iter()
                .all(|use_| use_.value == header_phi)
        );

        let delta = ir.take_liveness_delta();
        incremental
            .update_fact_delta(&ir, &cfg, &mut intervals, delta)
            .unwrap();
        assert_eq!(intervals, ir.analyze(&cfg).unwrap());
        let phi_interval = intervals.intervals[header_phi.0 as usize].as_ref().unwrap();
        assert!(
            phi_interval
                .uses
                .iter()
                .any(|use_| use_.block() == BlockId(2))
        );

        let lowered = ir.materialize(&function, &graph, &[]).unwrap();
        lowered.verify_result().unwrap();
        let header = &lowered.blocks[cfg.block_index[&BlockId(1)]];
        let phi = header
            .phis
            .iter()
            .find(|phi| phi.dst == header_phi)
            .unwrap();
        assert_eq!(
            phi.sources
                .iter()
                .map(|(block, _)| *block)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([BlockId(0), BlockId(2)])
        );
    }
}
