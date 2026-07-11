//! Machine constraints expressed as SSA Perm boundaries.

use std::collections::{BTreeSet, HashMap, VecDeque};

use crate::backend::native::mir::{BlockId, MBlock, MFunction, MInst, PhiNode, SpillDesc, VReg};

use super::analysis::AnalysisResult;
use super::assignment::{ALLOCATABLE_REGS, PhysReg, RegConstraint, clobbers, use_constraints};
use super::cfg::NormalizedCfg;

#[derive(Debug, Clone)]
pub(super) struct PermRow {
    pub source: VReg,
    pub destination: VReg,
    allowed_colors: u16,
}

#[derive(Debug, Clone)]
pub(super) struct PermBoundary {
    pub block: BlockId,
    pub predecessor: BlockId,
    pub rows: Vec<PermRow>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct PermModel {
    pub boundaries: Vec<PermBoundary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PermError {
    pub rule: &'static str,
    pub block: Option<BlockId>,
    pub instruction: Option<usize>,
    pub values: Vec<VReg>,
    pub message: String,
}

impl PermError {
    fn new(
        rule: &'static str,
        block: Option<BlockId>,
        instruction: Option<usize>,
        values: Vec<VReg>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            rule,
            block,
            instruction,
            values,
            message: message.into(),
        }
    }
}

/// Insert the complete post-spill register-live set as a one-input Perm before
/// every constrained instruction.  Pressure has already been proved <= K, so
/// each materialized boundary contains at most K rows.
pub(super) fn materialize_constraint_perms(
    func: &mut MFunction,
    initial_cfg: &NormalizedCfg,
) -> Result<(NormalizedCfg, PermModel), PermError> {
    if func.blocks.len() != initial_cfg.predecessors.len() {
        return Err(PermError::new(
            "PERM.INPUT_CFG_SHAPE",
            None,
            None,
            Vec::new(),
            format!(
                "function has {} blocks but input CFG has {}",
                func.blocks.len(),
                initial_cfg.predecessors.len()
            ),
        ));
    }
    let analysis = super::analysis::analyze(func);
    let live = constraint_boundary_liveness(func, &analysis);
    let mut logical_for_vreg = (0..func.vregs.count()).map(VReg).collect::<Vec<_>>();
    let boundary_blocks = split_constraint_blocks(func, &live, &mut logical_for_vreg)?;
    let cfg = super::cfg::normalize(func).map_err(|error| {
        PermError::new(error.rule, error.block, None, Vec::new(), error.message)
    })?;
    let merge_phis =
        insert_permutation_merge_phis(func, &cfg, &boundary_blocks, &mut logical_for_vreg)?;
    rename_permutation_representatives(func, &cfg, &logical_for_vreg, &merge_phis)?;
    let model = PermModel::build(func, &cfg, &boundary_blocks)?;
    model.verify(func, &cfg, super::NUM_REGS)?;
    Ok((cfg, model))
}

impl PermRow {
    pub(super) fn allows(&self, color: PhysReg) -> bool {
        self.allowed_colors & color_bit(color) != 0
    }
}

impl PermBoundary {
    /// Find a total row-to-color matching.  Source colors are preferences only;
    /// fixed operands and clobber exclusions are represented by each row's
    /// allowed-color mask.
    pub(super) fn match_colors(
        &self,
        source_color: impl Fn(VReg) -> Option<PhysReg>,
    ) -> Option<HashMap<VReg, PhysReg>> {
        let mut row_order = (0..self.rows.len()).collect::<Vec<_>>();
        row_order.sort_by_key(|&row| {
            (
                self.rows[row].allowed_colors.count_ones(),
                self.rows[row].destination,
            )
        });
        let mut owner = vec![None::<usize>; ALLOCATABLE_REGS.len()];
        let mut assigned = vec![None::<usize>; self.rows.len()];
        for row in row_order {
            let mut visited = vec![false; ALLOCATABLE_REGS.len()];
            if !augment_row(
                row,
                &self.rows,
                &source_color,
                &mut owner,
                &mut assigned,
                &mut visited,
            ) {
                return None;
            }
        }
        let mut result = HashMap::with_capacity(self.rows.len());
        for (row, facts) in self.rows.iter().enumerate() {
            let color = assigned[row]?;
            result.insert(facts.destination, ALLOCATABLE_REGS[color]);
        }
        Some(result)
    }
}

fn augment_row(
    row: usize,
    rows: &[PermRow],
    source_color: &impl Fn(VReg) -> Option<PhysReg>,
    owner: &mut [Option<usize>],
    assigned: &mut [Option<usize>],
    visited: &mut [bool],
) -> bool {
    let preferred = source_color(rows[row].source);
    let colors = preferred.into_iter().chain(
        ALLOCATABLE_REGS
            .iter()
            .copied()
            .filter(|color| Some(*color) != preferred),
    );
    for color in colors {
        let Some(color_index) = ALLOCATABLE_REGS
            .iter()
            .position(|candidate| *candidate == color)
        else {
            continue;
        };
        if visited[color_index] || !rows[row].allows(color) {
            continue;
        }
        visited[color_index] = true;
        let displaced = owner[color_index];
        if displaced
            .is_none_or(|other| augment_row(other, rows, source_color, owner, assigned, visited))
        {
            if let Some(old_color) = assigned[row] {
                owner[old_color] = None;
            }
            owner[color_index] = Some(row);
            assigned[row] = Some(color_index);
            return true;
        }
    }
    false
}

impl PermModel {
    fn build(
        func: &MFunction,
        cfg: &NormalizedCfg,
        boundary_blocks: &[BlockId],
    ) -> Result<Self, PermError> {
        let analysis = super::analysis::analyze(func);
        let mut boundaries = Vec::with_capacity(boundary_blocks.len());
        for &block_id in boundary_blocks {
            let Some(&block_index) = cfg.block_index.get(&block_id) else {
                return Err(PermError::new(
                    "PERM.BOUNDARY_BLOCK_EXISTS",
                    Some(block_id),
                    None,
                    Vec::new(),
                    "materialized boundary block is absent from normalized CFG",
                ));
            };
            let block = &func.blocks[block_index];
            if cfg.predecessors[block_index].len() != 1 {
                return Err(PermError::new(
                    "PERM.SINGLE_PREDECESSOR",
                    Some(block_id),
                    None,
                    Vec::new(),
                    format!(
                        "Perm boundary has {} predecessors",
                        cfg.predecessors[block_index].len()
                    ),
                ));
            }
            let predecessor_index = cfg.predecessors[block_index][0];
            let predecessor = func.blocks[predecessor_index].id;
            let Some(instruction) = block.insts.first() else {
                return Err(PermError::new(
                    "PERM.CONSTRAINT_INSTRUCTION_EXISTS",
                    Some(block_id),
                    None,
                    Vec::new(),
                    "Perm boundary has no constrained instruction",
                ));
            };
            let live_after =
                live_after_first_instruction(block, &analysis.exit_distances[block_index]);
            let mut fixed = HashMap::<VReg, PhysReg>::new();
            for (value, constraint) in instruction.uses().into_iter().zip(use_constraints(
                instruction,
                func.target_features.variable_shift_encoding(),
            )) {
                let RegConstraint::Fixed(required) = constraint else {
                    continue;
                };
                if let Some(previous) = fixed.insert(value, required)
                    && previous != required
                {
                    return Err(PermError::new(
                        "PERM.FIXED_USE_CONSISTENT",
                        Some(block_id),
                        Some(0),
                        vec![value],
                        format!(
                            "operand requires incompatible registers {previous:?} and {required:?}"
                        ),
                    ));
                }
            }
            let clobbered = clobbers(instruction)
                .iter()
                .copied()
                .fold(0u16, |mask, color| mask | color_bit(color));
            let mut rows = Vec::with_capacity(block.phis.len());
            for phi in &block.phis {
                if phi.sources.len() != 1 || phi.sources[0].0 != predecessor {
                    return Err(PermError::new(
                        "PERM.ROW_SOURCE_EDGE",
                        Some(block_id),
                        None,
                        vec![phi.dst],
                        "Perm row must have exactly one source from its predecessor",
                    ));
                }
                let mut allowed_colors = all_color_bits();
                if let Some(&required) = fixed.get(&phi.dst) {
                    allowed_colors &= color_bit(required);
                }
                if live_after.contains(&phi.dst) {
                    allowed_colors &= !clobbered;
                }
                rows.push(PermRow {
                    source: phi.sources[0].1,
                    destination: phi.dst,
                    allowed_colors,
                });
            }
            boundaries.push(PermBoundary {
                block: block_id,
                predecessor,
                rows,
            });
        }
        Ok(Self { boundaries })
    }

    pub(super) fn verify(
        &self,
        func: &MFunction,
        cfg: &NormalizedCfg,
        registers: usize,
    ) -> Result<(), PermError> {
        let analysis = super::analysis::analyze(func);
        let mut seen = BTreeSet::new();
        for boundary in &self.boundaries {
            if !seen.insert(boundary.block) {
                return Err(PermError::new(
                    "PERM.UNIQUE_BOUNDARY",
                    Some(boundary.block),
                    None,
                    Vec::new(),
                    "duplicate Perm boundary",
                ));
            }
            let Some(&block_index) = cfg.block_index.get(&boundary.block) else {
                return Err(PermError::new(
                    "PERM.BOUNDARY_BLOCK_EXISTS",
                    Some(boundary.block),
                    None,
                    Vec::new(),
                    "Perm model names a missing block",
                ));
            };
            let block = &func.blocks[block_index];
            if cfg.predecessors[block_index].len() != 1
                || func.blocks[cfg.predecessors[block_index][0]].id != boundary.predecessor
            {
                return Err(PermError::new(
                    "PERM.PREDECESSOR_MATCHES",
                    Some(boundary.block),
                    None,
                    Vec::new(),
                    "Perm model predecessor does not match the CFG edge",
                ));
            }
            if boundary.rows.len() > registers {
                return Err(PermError::new(
                    "PERM.PRESSURE_BOUND",
                    Some(boundary.block),
                    None,
                    boundary.rows.iter().map(|row| row.destination).collect(),
                    format!(
                        "Perm has {} rows but only {registers} registers",
                        boundary.rows.len()
                    ),
                ));
            }
            if boundary.rows.len() != block.phis.len() {
                return Err(PermError::new(
                    "PERM.ROWS_MATCH_PHIS",
                    Some(boundary.block),
                    None,
                    Vec::new(),
                    format!(
                        "model has {} rows but block has {} phis",
                        boundary.rows.len(),
                        block.phis.len()
                    ),
                ));
            }
            let Some(instruction) = block.insts.first() else {
                return Err(PermError::new(
                    "PERM.CONSTRAINT_INSTRUCTION_EXISTS",
                    Some(boundary.block),
                    None,
                    Vec::new(),
                    "Perm block is empty",
                ));
            };
            if clobbers(instruction).is_empty()
                && !use_constraints(instruction, func.target_features.variable_shift_encoding())
                    .into_iter()
                    .any(|constraint| matches!(constraint, RegConstraint::Fixed(_)))
            {
                return Err(PermError::new(
                    "PERM.IMMEDIATELY_PRECEDES_CONSTRAINT",
                    Some(boundary.block),
                    Some(0),
                    Vec::new(),
                    "Perm boundary does not precede a constrained instruction",
                ));
            }
            let live_after =
                live_after_first_instruction(block, &analysis.exit_distances[block_index]);
            let mut live_before = live_after;
            if let Some(definition) = instruction.def() {
                live_before.remove(&definition);
            }
            live_before.extend(instruction.uses());
            let destinations = boundary
                .rows
                .iter()
                .map(|row| row.destination)
                .collect::<BTreeSet<_>>();
            if destinations != live_before {
                return Err(PermError::new(
                    "PERM.COMPLETE_LIVE_DESTINATIONS",
                    Some(boundary.block),
                    Some(0),
                    destinations
                        .symmetric_difference(&live_before)
                        .copied()
                        .collect(),
                    "Perm destinations do not equal the complete live set",
                ));
            }
            let Some(&predecessor_index) = cfg.block_index.get(&boundary.predecessor) else {
                return Err(PermError::new(
                    "PERM.PREDECESSOR_EXISTS",
                    Some(boundary.block),
                    None,
                    Vec::new(),
                    "Perm predecessor is absent from CFG",
                ));
            };
            let sources = boundary
                .rows
                .iter()
                .map(|row| row.source)
                .collect::<BTreeSet<_>>();
            let edge_live = analysis.exit_distances[predecessor_index]
                .keys()
                .copied()
                .collect::<BTreeSet<_>>();
            if sources != edge_live {
                return Err(PermError::new(
                    "PERM.COMPLETE_LIVE_SOURCES",
                    Some(boundary.block),
                    None,
                    sources.symmetric_difference(&edge_live).copied().collect(),
                    "Perm sources do not cover the complete input edge",
                ));
            }
            if let Some(row) = boundary.rows.iter().find(|row| row.allowed_colors == 0) {
                return Err(PermError::new(
                    "PERM.NONEMPTY_ALLOWED_COLORS",
                    Some(boundary.block),
                    Some(0),
                    vec![row.destination],
                    "Perm row has no allowed physical color",
                ));
            }
            if boundary.match_colors(|_| None).is_none() {
                return Err(PermError::new(
                    "PERM.PERFECT_MATCHING",
                    Some(boundary.block),
                    Some(0),
                    boundary.rows.iter().map(|row| row.destination).collect(),
                    "constraint point has no local physical-color matching",
                ));
            }
        }
        Ok(())
    }
}

fn live_after_first_instruction(
    block: &MBlock,
    exit: &crate::HashMap<VReg, u32>,
) -> BTreeSet<VReg> {
    let mut live = exit.keys().copied().collect::<BTreeSet<_>>();
    for instruction in block.insts.iter().skip(1).rev() {
        if let Some(definition) = instruction.def() {
            live.remove(&definition);
        }
        live.extend(instruction.uses());
    }
    live
}

fn color_bit(color: PhysReg) -> u16 {
    1u16 << color as u8
}

fn all_color_bits() -> u16 {
    ALLOCATABLE_REGS
        .iter()
        .copied()
        .fold(0, |mask, color| mask | color_bit(color))
}

fn alloc_copy(
    vregs: &mut crate::backend::native::mir::VRegAllocator,
    spill_descs: &mut Vec<SpillDesc>,
    value_widths: &mut Vec<Option<u8>>,
    source: VReg,
) -> Result<VReg, PermError> {
    let desc = spill_descs
        .get(source.0 as usize)
        .map(SpillDesc::copy_for_snapshot)
        .unwrap_or_else(SpillDesc::transient);
    let width = value_widths.get(source.0 as usize).copied().flatten();
    let fresh = vregs.try_alloc().map_err(|error| {
        PermError::new(
            "PERM.VREG_EXHAUSTED",
            None,
            None,
            vec![source],
            error.to_string(),
        )
    })?;
    if fresh.0 as usize != spill_descs.len()
        || (!value_widths.is_empty() && fresh.0 as usize != value_widths.len())
    {
        return Err(PermError::new(
            "PERM.SIDETABLE_APPEND_POSITION",
            None,
            None,
            vec![fresh, source],
            "fresh Perm VReg does not append consistently to MIR side tables",
        ));
    }
    spill_descs.push(desc);
    if !value_widths.is_empty() {
        value_widths.push(width);
    }
    Ok(fresh)
}

fn constraint_boundary_liveness(
    func: &MFunction,
    analysis: &AnalysisResult,
) -> Vec<HashMap<usize, BTreeSet<VReg>>> {
    func.blocks
        .iter()
        .enumerate()
        .map(|(block_index, block)| {
            let boundaries =
                constraint_boundaries(block, func.target_features.variable_shift_encoding())
                    .into_iter()
                    .collect::<BTreeSet<_>>();
            let mut points = HashMap::with_capacity(boundaries.len());
            let mut live = analysis.exit_distances[block_index]
                .keys()
                .copied()
                .collect::<BTreeSet<_>>();
            for (instruction, inst) in block.insts.iter().enumerate().rev() {
                if let Some(definition) = inst.def() {
                    live.remove(&definition);
                }
                live.extend(inst.uses());
                if boundaries.contains(&instruction) {
                    points.insert(instruction, live.clone());
                }
            }
            points
        })
        .collect()
}

fn constraint_boundaries(
    block: &MBlock,
    shift_encoding: crate::backend::native::features::VariableShiftEncoding,
) -> Vec<usize> {
    let mut result = BTreeSet::new();
    for (instruction, inst) in block.insts.iter().enumerate() {
        if !clobbers(inst).is_empty() {
            result.insert(instruction);
        }
        if use_constraints(inst, shift_encoding)
            .into_iter()
            .any(|constraint| matches!(constraint, RegConstraint::Fixed(_)))
        {
            result.insert(instruction);
        }
    }
    result.into_iter().collect()
}

fn split_constraint_blocks(
    func: &mut MFunction,
    live: &[HashMap<usize, BTreeSet<VReg>>],
    logical_for_vreg: &mut Vec<VReg>,
) -> Result<Vec<BlockId>, PermError> {
    let original = std::mem::take(&mut func.blocks);
    let Some(mut next_block) = original
        .iter()
        .map(|block| block.id.0)
        .max()
        .unwrap_or(0)
        .checked_add(1)
    else {
        return Err(PermError::new(
            "PERM.BLOCK_ID_RANGE",
            None,
            None,
            Vec::new(),
            "BlockId overflow while inserting constraint boundaries",
        ));
    };
    let mut rewritten = Vec::<(MBlock, bool)>::new();
    let mut final_block = HashMap::<BlockId, BlockId>::new();
    let mut boundary_blocks = Vec::new();

    for (block_index, block) in original.into_iter().enumerate() {
        let boundaries =
            constraint_boundaries(&block, func.target_features.variable_shift_encoding());
        if boundaries.is_empty() {
            final_block.insert(block.id, block.id);
            rewritten.push((block, true));
            continue;
        }
        let original_id = block.id;
        let original_phis = block.phis;
        let instructions = block.insts;
        let mut starts = Vec::with_capacity(boundaries.len() + 2);
        starts.push(0);
        starts.extend(boundaries.iter().copied());
        starts.push(instructions.len());
        // Keep a duplicated leading zero: a constraint at instruction zero
        // needs an empty predecessor segment so the Perm block has one input.
        let mut previous_id = original_id;
        for segment in 0..starts.len() - 1 {
            let start = starts[segment];
            let end = starts[segment + 1];
            let id = if segment == 0 {
                original_id
            } else {
                let id = BlockId(next_block);
                let Some(next) = next_block.checked_add(1) else {
                    return Err(PermError::new(
                        "PERM.BLOCK_ID_RANGE",
                        Some(original_id),
                        None,
                        Vec::new(),
                        "BlockId overflow while inserting constraint boundaries",
                    ));
                };
                next_block = next;
                id
            };
            let mut next = MBlock::new(id);
            if segment == 0 {
                next.phis = original_phis.clone();
            } else {
                boundary_blocks.push(id);
                let Some(sources) = live
                    .get(block_index)
                    .and_then(|boundaries| boundaries.get(&start))
                else {
                    return Err(PermError::new(
                        "PERM.BOUNDARY_LIVENESS_EXISTS",
                        Some(original_id),
                        Some(start),
                        Vec::new(),
                        "constraint boundary has no liveness fact",
                    ));
                };
                for &source in sources {
                    let destination = alloc_copy(
                        &mut func.vregs,
                        &mut func.spill_descs,
                        &mut func.value_widths,
                        source,
                    )?;
                    let Some(&logical) = logical_for_vreg.get(source.0 as usize) else {
                        return Err(PermError::new(
                            "PERM.LOGICAL_SIDETABLE_COVERS_VREG",
                            Some(original_id),
                            Some(start),
                            vec![source],
                            "constraint source has no logical-value mapping",
                        ));
                    };
                    logical_for_vreg.push(logical);
                    next.phis.push(PhiNode {
                        dst: destination,
                        sources: vec![(previous_id, source)],
                    });
                }
            }
            next.insts.extend_from_slice(&instructions[start..end]);
            if segment + 1 < starts.len() - 1 {
                let target = BlockId(next_block);
                next.push(MInst::Jump { target });
            }
            previous_id = id;
            rewritten.push((next, segment == 0));
        }
        final_block.insert(original_id, previous_id);
    }

    // Only original phi nodes refer to original predecessor blocks.  Perm
    // phis already name the immediately preceding segment.
    for (block, is_original_entry) in &mut rewritten {
        if *is_original_entry {
            for phi in &mut block.phis {
                for (predecessor, _) in &mut phi.sources {
                    if let Some(&replacement) = final_block.get(predecessor) {
                        *predecessor = replacement;
                    }
                }
            }
        }
    }
    func.blocks = rewritten.into_iter().map(|(block, _)| block).collect();
    Ok(boundary_blocks)
}

/// Reconstruct SSA for the logical values split by late Perm definitions.
///
/// A Perm on only one arm of a branch does not dominate uses after the join.
/// Treating its destination as a plain rename would therefore either leave the
/// destination dead or rewrite a non-dominated use.  Insert pruned-IDF merge
/// phis first, exactly as for any other SSA live-range split, and let the
/// dominator-tree rename below connect their incoming representatives.
fn insert_permutation_merge_phis(
    func: &mut MFunction,
    cfg: &NormalizedCfg,
    boundary_blocks: &[BlockId],
    logical_for_vreg: &mut Vec<VReg>,
) -> Result<BTreeSet<VReg>, PermError> {
    let affected = boundary_blocks
        .iter()
        .flat_map(|block| {
            func.blocks[cfg.block_index[block]]
                .phis
                .iter()
                .map(|phi| logical_for_vreg[phi.dst.0 as usize])
        })
        .collect::<BTreeSet<_>>();
    if affected.is_empty() {
        return Ok(BTreeSet::new());
    }

    // Compute pruned liveness before adding empty-source reconstruction phis.
    // Any live SSA representative of a logical value makes that logical value
    // live at the block entry.
    let analysis = super::analysis::analyze(func);
    let live_in = analysis
        .entry_distances
        .iter()
        .map(|values| {
            values
                .keys()
                .map(|value| logical_for_vreg[value.0 as usize])
                .filter(|logical| affected.contains(logical))
                .collect::<BTreeSet<_>>()
        })
        .collect::<Vec<_>>();

    let mut definition_blocks = HashMap::<VReg, BTreeSet<usize>>::new();
    let mut existing_phi_blocks = HashMap::<VReg, BTreeSet<usize>>::new();
    for (block, mir_block) in func.blocks.iter().enumerate() {
        for phi in &mir_block.phis {
            let logical = logical_for_vreg[phi.dst.0 as usize];
            if affected.contains(&logical) {
                definition_blocks.entry(logical).or_default().insert(block);
                existing_phi_blocks
                    .entry(logical)
                    .or_default()
                    .insert(block);
            }
        }
        for instruction in &mir_block.insts {
            if let Some(definition) = instruction.def() {
                let logical = logical_for_vreg[definition.0 as usize];
                if affected.contains(&logical) {
                    definition_blocks.entry(logical).or_default().insert(block);
                }
            }
        }
    }

    let mut merge_phis = BTreeSet::new();
    for logical in affected {
        let mut has_phi = existing_phi_blocks.remove(&logical).unwrap_or_default();
        let mut work = definition_blocks
            .get(&logical)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect::<VecDeque<_>>();
        while let Some(definition) = work.pop_front() {
            for &frontier in &cfg.dominance_frontier[definition] {
                if !live_in[frontier].contains(&logical) || !has_phi.insert(frontier) {
                    continue;
                }
                let fresh = alloc_copy(
                    &mut func.vregs,
                    &mut func.spill_descs,
                    &mut func.value_widths,
                    logical,
                )?;
                logical_for_vreg.push(logical);
                func.blocks[frontier].phis.push(PhiNode {
                    dst: fresh,
                    sources: Vec::new(),
                });
                merge_phis.insert(fresh);
                work.push_back(frontier);
            }
        }
    }
    Ok(merge_phis)
}

fn rename_permutation_representatives(
    func: &mut MFunction,
    cfg: &NormalizedCfg,
    logical_for_vreg: &[VReg],
    merge_phis: &BTreeSet<VReg>,
) -> Result<(), PermError> {
    let mut children = vec![Vec::new(); func.blocks.len()];
    for (block, idom) in cfg.idom.iter().enumerate() {
        if let Some(idom) = idom {
            children[*idom].push(block);
        }
    }
    enum Event {
        Enter(usize),
        Exit(Vec<VReg>),
    }
    let mut stacks = HashMap::<VReg, Vec<VReg>>::new();
    let mut work = vec![Event::Enter(0)];
    while let Some(event) = work.pop() {
        match event {
            Event::Exit(pushed) => {
                for logical in pushed.into_iter().rev() {
                    let Some(stack) = stacks.get_mut(&logical) else {
                        return Err(PermError::new(
                            "PERM.RENAME_STACK_BALANCED",
                            None,
                            None,
                            vec![logical],
                            "rename stack disappeared before dominator exit",
                        ));
                    };
                    if stack.pop().is_none() {
                        return Err(PermError::new(
                            "PERM.RENAME_STACK_BALANCED",
                            None,
                            None,
                            vec![logical],
                            "rename stack underflow at dominator exit",
                        ));
                    }
                }
            }
            Event::Enter(block) => {
                let mut pushed = Vec::new();
                let block_id = func.blocks[block].id;
                for phi in &func.blocks[block].phis {
                    let logical = perm_logical(logical_for_vreg, phi.dst, block_id)?;
                    stacks.entry(logical).or_default().push(phi.dst);
                    pushed.push(logical);
                }
                for inst in &mut func.blocks[block].insts {
                    for used in inst.uses() {
                        let logical = perm_logical(logical_for_vreg, used, block_id)?;
                        if let Some(&representative) =
                            stacks.get(&logical).and_then(|stack| stack.last())
                        {
                            inst.rewrite_use(used, representative);
                        }
                    }
                    if let Some(definition) = inst.def() {
                        let logical = perm_logical(logical_for_vreg, definition, block_id)?;
                        stacks.entry(logical).or_default().push(definition);
                        pushed.push(logical);
                    }
                }
                let predecessor = func.blocks[block].id;
                for &successor in &cfg.successors[block] {
                    let successor_id = func.blocks[successor].id;
                    for phi in &mut func.blocks[successor].phis {
                        if merge_phis.contains(&phi.dst) {
                            let logical = perm_logical(logical_for_vreg, phi.dst, successor_id)?;
                            let Some(representative) =
                                stacks.get(&logical).and_then(|stack| stack.last()).copied()
                            else {
                                return Err(PermError::new(
                                    "PERM.MERGE_REPRESENTATIVE_EXISTS",
                                    Some(successor_id),
                                    None,
                                    vec![phi.dst, logical],
                                    format!(
                                        "late Perm merge has no representative from {predecessor}"
                                    ),
                                ));
                            };
                            phi.sources.push((predecessor, representative));
                        } else if let Some(source) = phi
                            .sources
                            .iter_mut()
                            .find(|(source_predecessor, _)| *source_predecessor == predecessor)
                        {
                            let logical = perm_logical(logical_for_vreg, source.1, successor_id)?;
                            if let Some(&representative) =
                                stacks.get(&logical).and_then(|stack| stack.last())
                            {
                                source.1 = representative;
                            }
                        }
                    }
                }
                work.push(Event::Exit(pushed));
                work.extend(children[block].iter().rev().copied().map(Event::Enter));
            }
        }
    }
    Ok(())
}

fn perm_logical(logical_for_vreg: &[VReg], value: VReg, block: BlockId) -> Result<VReg, PermError> {
    logical_for_vreg
        .get(value.0 as usize)
        .copied()
        .ok_or_else(|| {
            PermError::new(
                "PERM.LOGICAL_SIDETABLE_COVERS_VREG",
                Some(block),
                None,
                vec![value],
                "Perm logical-value side table does not cover VReg",
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::features::X86Features;
    use crate::backend::native::mir::{SpillDesc, VRegAllocator};

    fn select_legacy_shifts(func: &mut MFunction) {
        func.target_features = X86Features::for_test(false);
    }

    #[test]
    fn perm_copy_reports_vreg_exhaustion() {
        let mut vregs = VRegAllocator::new();
        vregs.set_next_for_test(u32::MAX);
        let mut spill_descs = Vec::new();
        let mut value_widths = Vec::new();

        let error =
            alloc_copy(&mut vregs, &mut spill_descs, &mut value_widths, VReg(0)).unwrap_err();

        assert_eq!(error.rule, "PERM.VREG_EXHAUSTED");
        assert_eq!(vregs.count(), u32::MAX);
    }

    #[test]
    fn fixed_shift_starts_a_single_predecessor_perm_component() {
        let mut vregs = VRegAllocator::new();
        let lhs = vregs.alloc();
        let amount = vregs.alloc();
        let result = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
        select_legacy_shifts(&mut func);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm { dst: lhs, value: 8 });
        block.push(MInst::LoadImm {
            dst: amount,
            value: 1,
        });
        block.push(MInst::Shl {
            dst: result,
            lhs,
            rhs: amount,
        });
        block.push(MInst::Return);
        func.push_block(block);

        let initial = super::super::cfg::normalize(&mut func).unwrap();
        let (cfg, model) = materialize_constraint_perms(&mut func, &initial).unwrap();
        func.verify();
        let constrained = func
            .blocks
            .iter()
            .position(|block| matches!(block.insts.first(), Some(MInst::Shl { .. })))
            .expect("shift component exists");
        assert_eq!(cfg.predecessors[constrained].len(), 1);
        assert_eq!(func.blocks[constrained].phis.len(), 2);
        assert_eq!(model.boundaries.len(), 1);
        let matching = model.boundaries[0].match_colors(|_| None).unwrap();
        let fixed = match func.blocks[constrained].insts[0] {
            MInst::Shl { rhs, .. } => rhs,
            _ => unreachable!(),
        };
        assert_eq!(matching[&fixed], PhysReg::RCX);
    }

    #[test]
    fn bmi2_shift_does_not_materialize_a_perm_boundary() {
        let mut vregs = VRegAllocator::new();
        let lhs = vregs.alloc();
        let amount = vregs.alloc();
        let result = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
        func.target_features = X86Features::for_test(true);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm { dst: lhs, value: 8 });
        block.push(MInst::LoadImm {
            dst: amount,
            value: 1,
        });
        block.push(MInst::Shl {
            dst: result,
            lhs,
            rhs: amount,
        });
        block.push(MInst::Return);
        func.push_block(block);
        let original_vregs = func.vregs.count();

        let initial = super::super::cfg::normalize(&mut func).unwrap();
        let (_cfg, model) = materialize_constraint_perms(&mut func, &initial).unwrap();

        assert!(model.boundaries.is_empty());
        assert_eq!(func.blocks.len(), 1);
        assert_eq!(func.vregs.count(), original_vregs);
        assert!(func.blocks[0].phis.is_empty());
    }

    #[test]
    fn incomplete_perm_model_is_a_structured_error() {
        let mut vregs = VRegAllocator::new();
        let lhs = vregs.alloc();
        let amount = vregs.alloc();
        let result = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
        select_legacy_shifts(&mut func);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm { dst: lhs, value: 8 });
        block.push(MInst::LoadImm {
            dst: amount,
            value: 1,
        });
        block.push(MInst::Shl {
            dst: result,
            lhs,
            rhs: amount,
        });
        block.push(MInst::Return);
        func.push_block(block);
        let initial = super::super::cfg::normalize(&mut func).unwrap();
        let (cfg, mut model) = materialize_constraint_perms(&mut func, &initial).unwrap();
        model.boundaries[0].rows.pop();

        let error = model
            .verify(&func, &cfg, super::super::NUM_REGS)
            .unwrap_err();

        assert_eq!(error.rule, "PERM.ROWS_MATCH_PHIS");
        assert!(error.block.is_some());
    }

    #[test]
    fn leading_clobber_is_moved_out_of_a_join_block() {
        let mut vregs = VRegAllocator::new();
        let lhs = vregs.alloc();
        let rhs = vregs.alloc();
        let result = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm { dst: lhs, value: 8 });
        entry.push(MInst::LoadImm { dst: rhs, value: 2 });
        entry.push(MInst::Branch {
            cond: rhs,
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::Jump { target: BlockId(3) });
        let mut other = MBlock::new(BlockId(2));
        other.push(MInst::Jump { target: BlockId(3) });
        let mut join = MBlock::new(BlockId(3));
        join.push(MInst::UDiv {
            dst: result,
            lhs,
            rhs,
        });
        join.push(MInst::Return);
        func.blocks = vec![entry, left, other, join];

        let initial = super::super::cfg::normalize(&mut func).unwrap();
        let (cfg, model) = materialize_constraint_perms(&mut func, &initial).unwrap();
        let constrained = func
            .blocks
            .iter()
            .position(|block| matches!(block.insts.first(), Some(MInst::UDiv { .. })))
            .expect("division component exists");
        assert_eq!(cfg.predecessors[constrained].len(), 1);
        assert_eq!(model.boundaries.len(), 1);
    }

    #[test]
    fn one_arm_perm_inserts_a_pruned_idf_merge_phi() {
        let mut vregs = VRegAllocator::new();
        let condition = vregs.alloc();
        let value = vregs.alloc();
        let amount = vregs.alloc();
        let shifted = vregs.alloc();
        let observed = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 5]);
        select_legacy_shifts(&mut func);

        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: condition,
            value: 1,
        });
        entry.push(MInst::LoadImm {
            dst: value,
            value: 8,
        });
        entry.push(MInst::LoadImm {
            dst: amount,
            value: 1,
        });
        entry.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut constrained_arm = MBlock::new(BlockId(1));
        constrained_arm.push(MInst::Shl {
            dst: shifted,
            lhs: value,
            rhs: amount,
        });
        constrained_arm.push(MInst::Jump { target: BlockId(3) });
        let mut other_arm = MBlock::new(BlockId(2));
        other_arm.push(MInst::Jump { target: BlockId(3) });
        let mut join = MBlock::new(BlockId(3));
        join.push(MInst::Mov {
            dst: observed,
            src: value,
        });
        join.push(MInst::Return);
        func.blocks = vec![entry, constrained_arm, other_arm, join];

        let initial = super::super::cfg::normalize(&mut func).unwrap();
        let (_cfg, model) = materialize_constraint_perms(&mut func, &initial).unwrap();
        func.verify();

        let constrained = func
            .blocks
            .iter()
            .find(|block| matches!(block.insts.first(), Some(MInst::Shl { .. })))
            .expect("shift component exists");
        let perm_value = match constrained.insts[0] {
            MInst::Shl { lhs, .. } => lhs,
            _ => unreachable!(),
        };
        assert_ne!(perm_value, value);

        let join = func
            .blocks
            .iter()
            .find(|block| block.id == BlockId(3))
            .expect("join exists");
        let merge = join
            .phis
            .iter()
            .find(|phi| {
                let sources = phi
                    .sources
                    .iter()
                    .map(|(_, source)| *source)
                    .collect::<BTreeSet<_>>();
                sources == BTreeSet::from([value, perm_value])
            })
            .expect("the split value is merged at its iterated dominance frontier");
        assert_eq!(merge.sources.len(), 2);
        assert!(matches!(
            join.insts.first(),
            Some(MInst::Mov { src, .. }) if *src == merge.dst
        ));
        assert_eq!(model.boundaries.len(), 1);
    }

    #[test]
    fn loop_perm_inserts_header_phi_and_renames_the_backedge() {
        let mut vregs = VRegAllocator::new();
        let condition = vregs.alloc();
        let value = vregs.alloc();
        let amount = vregs.alloc();
        let shifted = vregs.alloc();
        let observed = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 5]);
        select_legacy_shifts(&mut func);

        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: condition,
            value: 1,
        });
        entry.push(MInst::LoadImm {
            dst: value,
            value: 8,
        });
        entry.push(MInst::LoadImm {
            dst: amount,
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
        body.push(MInst::Shl {
            dst: shifted,
            lhs: value,
            rhs: amount,
        });
        body.push(MInst::Jump { target: BlockId(1) });
        let mut exit = MBlock::new(BlockId(3));
        exit.push(MInst::Mov {
            dst: observed,
            src: value,
        });
        exit.push(MInst::Return);
        func.blocks = vec![entry, header, body, exit];

        let initial = super::super::cfg::normalize(&mut func).unwrap();
        let (_cfg, model) = materialize_constraint_perms(&mut func, &initial).unwrap();
        func.verify();

        let constrained = func
            .blocks
            .iter()
            .find(|block| matches!(block.insts.first(), Some(MInst::Shl { .. })))
            .expect("shift component exists");
        let perm_value = match constrained.insts[0] {
            MInst::Shl { lhs, .. } => lhs,
            _ => unreachable!(),
        };
        assert_ne!(perm_value, value);

        let header = func
            .blocks
            .iter()
            .find(|block| block.id == BlockId(1))
            .expect("loop header exists");
        let merge = header
            .phis
            .iter()
            .find(|phi| {
                phi.sources
                    .iter()
                    .map(|(_, source)| *source)
                    .collect::<BTreeSet<_>>()
                    == BTreeSet::from([value, perm_value])
            })
            .expect("the loop-carried Perm representative is merged at the header");
        assert_eq!(merge.sources.len(), 2);

        let exit = func
            .blocks
            .iter()
            .find(|block| block.id == BlockId(3))
            .expect("loop exit exists");
        assert!(matches!(
            exit.insts.first(),
            Some(MInst::Mov { src, .. }) if *src == merge.dst
        ));
        assert_eq!(model.boundaries.len(), 1);
    }
}
