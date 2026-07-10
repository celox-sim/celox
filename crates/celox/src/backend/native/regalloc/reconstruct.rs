//! Materialize a SpillPlan and reconstruct strict SSA with dominance frontiers.

use std::collections::{BTreeSet, HashMap, VecDeque};

use crate::backend::native::mir::{
    BaseReg, BlockId, MFunction, MInst, OpSize, PhiNode, SpillDesc, SpillKind, VReg,
};

use super::cfg::NormalizedCfg;
use super::next_use::NextUseAnalysis;
use super::spill_plan::{LogicalValue, PlannedOp, SpillHome, SpillPlan};

pub(super) struct ReconstructionResult {
    pub frame_size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReconstructError {
    pub rule: &'static str,
    pub block: Option<BlockId>,
    pub instruction: Option<usize>,
    pub values: Vec<VReg>,
    pub message: String,
}

impl ReconstructError {
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

#[derive(Clone, Copy)]
enum MaterializedOp {
    Spill {
        value: LogicalValue,
        home: SpillHome,
    },
    Reload {
        value: LogicalValue,
        home: SpillHome,
        fresh: VReg,
    },
}

pub(super) fn reconstruct(
    func: &mut MFunction,
    cfg: &NormalizedCfg,
    plan: &SpillPlan,
    _next_use: &NextUseAnalysis,
) -> Result<ReconstructionResult, ReconstructError> {
    let stack_offsets = stack_layout(func, plan)?;
    verify_reload_homes(func, plan, &stack_offsets)?;
    let original_vregs = func.vregs.count() as usize;
    let mut logical_for_vreg = (0..original_vregs)
        .map(|index| plan.logical.of(VReg(index as u32)))
        .collect::<Vec<_>>();
    let mut insertions = HashMap::<(usize, usize), Vec<MaterializedOp>>::new();
    let mut reload_blocks = HashMap::<LogicalValue, BTreeSet<usize>>::new();
    let mut reload_definitions = BTreeSet::<VReg>::new();
    let spilled_phis = plan
        .point_ops
        .iter()
        .filter_map(|(_, operation)| match operation {
            PlannedOp::SpillPhi { value, .. } => Some(*value),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    for block in 0..func.blocks.len() {
        let removed = func.blocks[block]
            .phis
            .iter()
            .filter(|phi| spilled_phis.contains(&plan.logical.of(phi.dst)))
            .cloned()
            .collect::<Vec<_>>();
        func.blocks[block]
            .phis
            .retain(|phi| !spilled_phis.contains(&plan.logical.of(phi.dst)));
        for phi in removed {
            let home = plan.homes.of_vreg(phi.dst);
            for (predecessor, source) in phi.sources {
                let Some(&predecessor) = cfg.block_index.get(&predecessor) else {
                    return Err(ReconstructError::new(
                        "RECONSTRUCT.PHI_PREDECESSOR_EXISTS",
                        Some(func.blocks[block].id),
                        None,
                        vec![phi.dst, source],
                        "spilled phi names a predecessor outside normalized CFG",
                    ));
                };
                let source = plan.logical.of(source);
                if plan.s_exit[predecessor].contains(&source) {
                    continue;
                }
                let instruction = func.blocks[predecessor].insts.len() - 1;
                insertions
                    .entry((predecessor, instruction))
                    .or_default()
                    .push(MaterializedOp::Spill {
                        value: source,
                        home,
                    });
            }
        }
    }
    for &(point, operation) in &plan.point_ops {
        if matches!(operation, PlannedOp::SpillPhi { .. }) {
            continue;
        }
        let Some(&block) = cfg.block_index.get(&point.block) else {
            return Err(ReconstructError::new(
                "RECONSTRUCT.POINT_BLOCK_EXISTS",
                Some(point.block),
                Some(point.instruction),
                vec![VReg(planned_value(operation).0)],
                "spill-plan point names a block outside normalized CFG",
            ));
        };
        materialize_operation(
            func,
            plan,
            block,
            point.instruction,
            operation,
            &mut logical_for_vreg,
            &mut insertions,
            &mut reload_blocks,
            &mut reload_definitions,
        )?;
    }
    for (&(predecessor, _successor), operations) in &plan.edge_ops {
        let Some(predecessor_block) = func.blocks.get(predecessor) else {
            return Err(ReconstructError::new(
                "RECONSTRUCT.EDGE_PREDECESSOR_EXISTS",
                None,
                None,
                Vec::new(),
                format!("edge operation predecessor index {predecessor} is outside function"),
            ));
        };
        let Some(instruction) = predecessor_block.insts.len().checked_sub(1) else {
            return Err(ReconstructError::new(
                "RECONSTRUCT.EDGE_PREDECESSOR_TERMINATED",
                Some(predecessor_block.id),
                None,
                Vec::new(),
                "edge operation predecessor block is empty",
            ));
        };
        for &operation in operations {
            materialize_operation(
                func,
                plan,
                predecessor,
                instruction,
                operation,
                &mut logical_for_vreg,
                &mut insertions,
                &mut reload_blocks,
                &mut reload_definitions,
            )?;
        }
    }

    let affected = reload_blocks.keys().copied().collect::<BTreeSet<_>>();
    let mut definition_blocks = HashMap::<LogicalValue, BTreeSet<usize>>::new();
    let mut existing_phi_blocks = HashMap::<LogicalValue, BTreeSet<usize>>::new();
    for (block, mir_block) in func.blocks.iter().enumerate() {
        for phi in &mir_block.phis {
            let logical = reconstruct_logical(&logical_for_vreg, phi.dst, mir_block.id)?;
            if affected.contains(&logical) {
                definition_blocks.entry(logical).or_default().insert(block);
                existing_phi_blocks
                    .entry(logical)
                    .or_default()
                    .insert(block);
            }
        }
        for inst in &mir_block.insts {
            if let Some(definition) = inst.def() {
                let logical = reconstruct_logical(&logical_for_vreg, definition, mir_block.id)?;
                if affected.contains(&logical) {
                    definition_blocks.entry(logical).or_default().insert(block);
                }
            }
        }
    }
    for (logical, blocks) in reload_blocks {
        definition_blocks.entry(logical).or_default().extend(blocks);
    }

    let mut reconstruction_phis = HashMap::<(usize, LogicalValue), VReg>::new();
    for logical in affected {
        let mut has_phi = existing_phi_blocks.remove(&logical).unwrap_or_default();
        let mut queue = definition_blocks
            .get(&logical)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect::<VecDeque<_>>();
        while let Some(definition) = queue.pop_front() {
            for &frontier in &cfg.dominance_frontier[definition] {
                if !plan.w_entry[frontier].contains(&logical) {
                    continue;
                }
                if !has_phi.insert(frontier) {
                    continue;
                }
                let fresh = alloc_fresh(func, &mut logical_for_vreg, logical)?;
                reconstruction_phis.insert((frontier, logical), fresh);
                func.blocks[frontier].phis.push(PhiNode {
                    dst: fresh,
                    sources: Vec::new(),
                });
                queue.push_back(frontier);
            }
        }
    }

    let mut children = vec![Vec::new(); func.blocks.len()];
    for (block, &idom) in cfg.idom.iter().enumerate().skip(1) {
        let Some(idom) = idom else {
            return Err(ReconstructError::new(
                "RECONSTRUCT.DOMINATOR_TREE",
                Some(func.blocks[block].id),
                None,
                Vec::new(),
                "non-entry block has no immediate dominator",
            ));
        };
        children[idom].push(block);
    }
    let mut stacks = HashMap::<LogicalValue, Vec<VReg>>::new();
    rename_block(
        0,
        func,
        cfg,
        plan,
        &children,
        &reconstruction_phis,
        &stack_offsets,
        &mut logical_for_vreg,
        &mut insertions,
        &mut stacks,
    )?;
    eliminate_dead_phis(func);
    eliminate_dead_reloads(func, &reload_definitions);

    let frame_size = u32::try_from(stack_offsets.len())
        .ok()
        .and_then(|homes| homes.checked_mul(8))
        .ok_or_else(|| {
            ReconstructError::new(
                "RECONSTRUCT.FRAME_SIZE_RANGE",
                None,
                None,
                Vec::new(),
                "spill frame size exceeds u32",
            )
        })?;
    Ok(ReconstructionResult { frame_size })
}

/// Remove phi webs which no longer reach an instruction after SSA renaming.
///
/// Reconstruction deliberately rewrites uses away from pre-spill Perm rows.
/// Keeping those now-dead rows would be more than a space leak: parallel-copy
/// emission would still assign their destinations physical registers and could
/// clobber live values.  Marking from instruction operands, then following phi
/// inputs backwards, also removes dead cyclic phi webs which a use-count queue
/// cannot discover.
fn eliminate_dead_phis(func: &mut MFunction) -> usize {
    let phi_sources = func
        .blocks
        .iter()
        .flat_map(|block| {
            block.phis.iter().map(|phi| {
                (
                    phi.dst,
                    phi.sources
                        .iter()
                        .map(|(_, source)| *source)
                        .collect::<Vec<_>>(),
                )
            })
        })
        .collect::<HashMap<_, _>>();
    let mut required = BTreeSet::<VReg>::new();
    let mut work = func
        .blocks
        .iter()
        .flat_map(|block| block.insts.iter().flat_map(MInst::uses))
        .collect::<Vec<_>>();
    while let Some(value) = work.pop() {
        if !required.insert(value) {
            continue;
        }
        if let Some(sources) = phi_sources.get(&value) {
            work.extend(sources.iter().copied());
        }
    }
    let before = phi_sources.len();
    for block in &mut func.blocks {
        block.phis.retain(|phi| required.contains(&phi.dst));
    }
    before
        - func
            .blocks
            .iter()
            .map(|block| block.phis.len())
            .sum::<usize>()
}

fn eliminate_dead_reloads(func: &mut MFunction, reloads: &BTreeSet<VReg>) -> usize {
    let used = func
        .blocks
        .iter()
        .flat_map(|block| {
            block.insts.iter().flat_map(MInst::uses).chain(
                block
                    .phis
                    .iter()
                    .flat_map(|phi| phi.sources.iter().map(|(_, source)| *source)),
            )
        })
        .collect::<BTreeSet<_>>();
    let mut removed = 0;
    for block in &mut func.blocks {
        block.insts.retain(|instruction| {
            let dead_reload = instruction.def().is_some_and(|definition| {
                reloads.contains(&definition) && !used.contains(&definition)
            });
            removed += usize::from(dead_reload);
            !dead_reload
        });
    }
    removed
}

fn stack_layout(
    func: &MFunction,
    plan: &SpillPlan,
) -> Result<HashMap<SpillHome, i32>, ReconstructError> {
    let homes = plan
        .point_ops
        .iter()
        .map(|(_, operation)| *operation)
        .chain(plan.edge_ops.values().flatten().copied())
        .filter_map(|operation| match operation {
            PlannedOp::Spill { home, .. } => {
                (!is_rematerializable(func, plan, home)).then_some(home)
            }
            PlannedOp::SpillPhi { home, .. } => Some(home),
            PlannedOp::Reload { .. } => None,
        })
        .collect::<BTreeSet<_>>();
    let mut result = HashMap::with_capacity(homes.len());
    for (index, home) in homes.into_iter().enumerate() {
        let Some(offset) = index
            .checked_mul(8)
            .and_then(|value| i32::try_from(value).ok())
        else {
            return Err(ReconstructError::new(
                "RECONSTRUCT.STACK_OFFSET_RANGE",
                None,
                None,
                Vec::new(),
                "spill frame exceeds signed 32-bit addressing range",
            ));
        };
        result.insert(home, offset);
    }
    Ok(result)
}

fn verify_reload_homes(
    func: &MFunction,
    plan: &SpillPlan,
    stack_offsets: &HashMap<SpillHome, i32>,
) -> Result<(), ReconstructError> {
    for &(point, operation) in &plan.point_ops {
        if let PlannedOp::Reload { value, home } = operation
            && rematerialized_logical_value(func, value).is_none()
        {
            if !stack_offsets.contains_key(&home) {
                return Err(ReconstructError::new(
                    "RECONSTRUCT.RELOAD_HOME_EXISTS",
                    Some(point.block),
                    Some(point.instruction),
                    vec![VReg(value.0)],
                    format!(
                        "reload has no spill home {home:?}; {}",
                        describe_missing_home(func, plan, value, home)
                    ),
                ));
            }
        }
    }
    for (&edge, operations) in &plan.edge_ops {
        for &operation in operations {
            if let PlannedOp::Reload { value, home } = operation
                && rematerialized_logical_value(func, value).is_none()
            {
                if !stack_offsets.contains_key(&home) {
                    let block = func.blocks.get(edge.0).map(|block| block.id);
                    return Err(ReconstructError::new(
                        "RECONSTRUCT.RELOAD_HOME_EXISTS",
                        block,
                        None,
                        vec![VReg(value.0)],
                        format!(
                            "edge reload has no spill home {home:?}; {}",
                            describe_missing_home(func, plan, value, home)
                        ),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn describe_missing_home(
    func: &MFunction,
    plan: &SpillPlan,
    logical: LogicalValue,
    home: SpillHome,
) -> String {
    let definitions = func
        .blocks
        .iter()
        .flat_map(|block| {
            block
                .phis
                .iter()
                .filter(move |phi| phi.dst.0 == logical.0)
                .map(move |_| format!("{}:phi", block.id))
                .chain(
                    block
                        .insts
                        .iter()
                        .enumerate()
                        .filter(move |(_, inst)| inst.def().is_some_and(|dst| dst.0 == logical.0))
                        .map(move |(instruction, _)| format!("{}:i{instruction}", block.id)),
                )
        })
        .collect::<Vec<_>>();
    let states = func
        .blocks
        .iter()
        .enumerate()
        .filter(|(block, _)| {
            plan.w_entry[*block].contains(&logical)
                || plan.s_entry[*block].contains(&logical)
                || plan.w_exit[*block].contains(&logical)
                || plan.s_exit[*block].contains(&logical)
        })
        .take(24)
        .map(|(block, mir_block)| {
            format!(
                "{}:[W{} S{} -> W{} S{}]",
                mir_block.id,
                u8::from(plan.w_entry[block].contains(&logical)),
                u8::from(plan.s_entry[block].contains(&logical)),
                u8::from(plan.w_exit[block].contains(&logical)),
                u8::from(plan.s_exit[block].contains(&logical))
            )
        })
        .collect::<Vec<_>>();
    let operations = plan
        .point_ops
        .iter()
        .filter(|(_, operation)| match operation {
            PlannedOp::Spill { home: op_home, .. }
            | PlannedOp::Reload { home: op_home, .. }
            | PlannedOp::SpillPhi { home: op_home, .. } => *op_home == home,
        })
        .take(24)
        .map(|(point, operation)| format!("{point:?}:{operation:?}"))
        .collect::<Vec<_>>();
    format!("defs={definitions:?} states={states:?} ops={operations:?}")
}

#[allow(clippy::too_many_arguments)]
fn materialize_operation(
    func: &mut MFunction,
    plan: &SpillPlan,
    block: usize,
    instruction: usize,
    operation: PlannedOp,
    logical_for_vreg: &mut Vec<LogicalValue>,
    insertions: &mut HashMap<(usize, usize), Vec<MaterializedOp>>,
    reload_blocks: &mut HashMap<LogicalValue, BTreeSet<usize>>,
    reload_definitions: &mut BTreeSet<VReg>,
) -> Result<(), ReconstructError> {
    let operation = match operation {
        PlannedOp::Spill { value, home } | PlannedOp::SpillPhi { value, home } => {
            MaterializedOp::Spill { value, home }
        }
        PlannedOp::Reload { value, home } => {
            let fresh = alloc_fresh(func, logical_for_vreg, value)?;
            reload_blocks.entry(value).or_default().insert(block);
            reload_definitions.insert(fresh);
            MaterializedOp::Reload { value, home, fresh }
        }
    };
    let _ = plan;
    insertions
        .entry((block, instruction))
        .or_default()
        .push(operation);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn rename_block(
    root: usize,
    func: &mut MFunction,
    cfg: &NormalizedCfg,
    plan: &SpillPlan,
    children: &[Vec<usize>],
    reconstruction_phis: &HashMap<(usize, LogicalValue), VReg>,
    stack_offsets: &HashMap<SpillHome, i32>,
    logical_for_vreg: &mut Vec<LogicalValue>,
    insertions: &mut HashMap<(usize, usize), Vec<MaterializedOp>>,
    stacks: &mut HashMap<LogicalValue, Vec<VReg>>,
) -> Result<(), ReconstructError> {
    enum Event {
        Enter(usize),
        Exit(Vec<LogicalValue>),
    }
    let mut work = vec![Event::Enter(root)];
    while let Some(event) = work.pop() {
        match event {
            Event::Exit(pushed) => {
                for logical in pushed.into_iter().rev() {
                    let Some(stack) = stacks.get_mut(&logical) else {
                        return Err(ReconstructError::new(
                            "RECONSTRUCT.RENAME_STACK_BALANCED",
                            None,
                            None,
                            vec![VReg(logical.0)],
                            "representative stack disappeared before dominator exit",
                        ));
                    };
                    if stack.pop().is_none() {
                        return Err(ReconstructError::new(
                            "RECONSTRUCT.RENAME_STACK_BALANCED",
                            None,
                            None,
                            vec![VReg(logical.0)],
                            "representative stack underflow at dominator exit",
                        ));
                    }
                }
            }
            Event::Enter(block) => {
                let mut pushed = Vec::<LogicalValue>::new();
                let block_id = func.blocks[block].id;
                for phi in &func.blocks[block].phis {
                    let logical = reconstruct_logical(logical_for_vreg, phi.dst, block_id)?;
                    stacks.entry(logical).or_default().push(phi.dst);
                    pushed.push(logical);
                }
                let original = std::mem::take(&mut func.blocks[block].insts);
                let mut rewritten = Vec::with_capacity(original.len());
                for (instruction, mut inst) in original.into_iter().enumerate() {
                    emit_insertions(
                        block,
                        instruction,
                        func,
                        plan,
                        stack_offsets,
                        logical_for_vreg,
                        insertions,
                        stacks,
                        &mut pushed,
                        &mut rewritten,
                    )?;
                    let uses = inst.uses().into_iter().collect::<BTreeSet<_>>();
                    for original_use in uses {
                        let logical =
                            reconstruct_logical(logical_for_vreg, original_use, block_id)?;
                        if let Some(&representative) =
                            stacks.get(&logical).and_then(|stack| stack.last())
                        {
                            inst.rewrite_use(original_use, representative);
                        }
                    }
                    if let Some(definition) = inst.def() {
                        let logical = reconstruct_logical(logical_for_vreg, definition, block_id)?;
                        stacks.entry(logical).or_default().push(definition);
                        pushed.push(logical);
                    }
                    rewritten.push(inst);
                }
                func.blocks[block].insts = rewritten;

                let predecessor_id = func.blocks[block].id;
                for &successor in &cfg.successors[block] {
                    let successor_id = func.blocks[successor].id;
                    for phi in &mut func.blocks[successor].phis {
                        let destination_logical =
                            reconstruct_logical(logical_for_vreg, phi.dst, successor_id)?;
                        if reconstruction_phis.contains_key(&(successor, destination_logical)) {
                            let Some(&representative) = stacks
                                .get(&destination_logical)
                                .and_then(|stack| stack.last())
                            else {
                                return Err(ReconstructError::new(
                                    "RECONSTRUCT.PHI_REPRESENTATIVE_EXISTS",
                                    Some(successor_id),
                                    None,
                                    vec![phi.dst, VReg(destination_logical.0)],
                                    format!(
                                        "reconstruction phi has no representative from {predecessor_id}"
                                    ),
                                ));
                            };
                            phi.sources.push((predecessor_id, representative));
                        } else if let Some(source) = phi
                            .sources
                            .iter_mut()
                            .find(|(source_predecessor, _)| *source_predecessor == predecessor_id)
                        {
                            let source_logical =
                                reconstruct_logical(logical_for_vreg, source.1, successor_id)?;
                            if let Some(&representative) =
                                stacks.get(&source_logical).and_then(|stack| stack.last())
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

#[allow(clippy::too_many_arguments)]
fn emit_insertions(
    block: usize,
    instruction: usize,
    func: &MFunction,
    plan: &SpillPlan,
    stack_offsets: &HashMap<SpillHome, i32>,
    logical_for_vreg: &[LogicalValue],
    insertions: &mut HashMap<(usize, usize), Vec<MaterializedOp>>,
    stacks: &mut HashMap<LogicalValue, Vec<VReg>>,
    pushed: &mut Vec<LogicalValue>,
    output: &mut Vec<MInst>,
) -> Result<(), ReconstructError> {
    let mut operations = insertions.remove(&(block, instruction)).unwrap_or_default();
    // A SpillPlan program point is parallel.  When materialized serially,
    // evictions must free their registers before operand reloads consume them.
    operations.sort_by_key(|operation| match operation {
        MaterializedOp::Spill { .. } => 0,
        MaterializedOp::Reload { .. } => 1,
    });
    for operation in operations {
        match operation {
            MaterializedOp::Spill {
                value: logical,
                home,
            } => {
                if is_rematerializable(func, plan, home) {
                    continue;
                }
                let Some(source) = stacks
                    .get(&logical)
                    .and_then(|representatives| representatives.last())
                    .copied()
                else {
                    return Err(ReconstructError::new(
                        "RECONSTRUCT.SPILL_REPRESENTATIVE_EXISTS",
                        func.blocks.get(block).map(|block| block.id),
                        Some(instruction),
                        vec![VReg(logical.0)],
                        "spill is not dominated by a logical definition",
                    ));
                };
                let Some(&offset) = stack_offsets.get(&home) else {
                    return Err(ReconstructError::new(
                        "RECONSTRUCT.SPILL_HOME_EXISTS",
                        func.blocks.get(block).map(|block| block.id),
                        Some(instruction),
                        vec![VReg(logical.0)],
                        format!("spill home {home:?} has no frame offset"),
                    ));
                };
                output.push(MInst::Store {
                    base: BaseReg::StackFrame,
                    offset,
                    src: source,
                    size: OpSize::S64,
                });
            }
            MaterializedOp::Reload {
                value: logical,
                home,
                fresh,
            } => {
                let reload = if let Some(value) = rematerialized_logical_value(func, logical) {
                    MInst::LoadImm { dst: fresh, value }
                } else {
                    let Some(&offset) = stack_offsets.get(&home) else {
                        return Err(ReconstructError::new(
                            "RECONSTRUCT.RELOAD_HOME_EXISTS",
                            func.blocks.get(block).map(|block| block.id),
                            Some(instruction),
                            vec![VReg(logical.0)],
                            format!("reload home {home:?} has no frame offset"),
                        ));
                    };
                    MInst::Load {
                        dst: fresh,
                        base: BaseReg::StackFrame,
                        offset,
                        size: OpSize::S64,
                    }
                };
                output.push(reload);
                stacks.entry(logical).or_default().push(fresh);
                pushed.push(logical);
            }
        }
    }
    let _ = logical_for_vreg;
    Ok(())
}

fn reconstruct_logical(
    logical_for_vreg: &[LogicalValue],
    value: VReg,
    block: BlockId,
) -> Result<LogicalValue, ReconstructError> {
    logical_for_vreg
        .get(value.0 as usize)
        .copied()
        .ok_or_else(|| {
            ReconstructError::new(
                "RECONSTRUCT.LOGICAL_SIDETABLE_COVERS_VREG",
                Some(block),
                None,
                vec![value],
                "logical-value side table does not cover VReg",
            )
        })
}

fn alloc_fresh(
    func: &mut MFunction,
    logical_for_vreg: &mut Vec<LogicalValue>,
    logical: LogicalValue,
) -> Result<VReg, ReconstructError> {
    let width = func.value_widths.get(logical.0 as usize).copied().flatten();
    let fresh = func.vregs.try_alloc().map_err(|error| {
        ReconstructError::new(
            "RECONSTRUCT.VREG_EXHAUSTED",
            None,
            None,
            vec![VReg(logical.0)],
            error.to_string(),
        )
    })?;
    if fresh.0 as usize != func.spill_descs.len()
        || (!func.value_widths.is_empty() && fresh.0 as usize != func.value_widths.len())
        || fresh.0 as usize != logical_for_vreg.len()
    {
        return Err(ReconstructError::new(
            "RECONSTRUCT.SIDETABLE_APPEND_POSITION",
            None,
            None,
            vec![fresh],
            "fresh VReg does not append consistently to reconstruction side tables",
        ));
    }
    func.spill_descs.push(SpillDesc::transient());
    if !func.value_widths.is_empty() {
        func.value_widths.push(width);
    }
    logical_for_vreg.push(logical);
    Ok(fresh)
}

fn planned_value(operation: PlannedOp) -> LogicalValue {
    match operation {
        PlannedOp::Spill { value, .. }
        | PlannedOp::Reload { value, .. }
        | PlannedOp::SpillPhi { value, .. } => value,
    }
}

fn is_rematerializable(func: &MFunction, plan: &SpillPlan, home: SpillHome) -> bool {
    rematerialized_home_value(func, plan, home).is_some()
}

fn rematerialized_home_value(func: &MFunction, plan: &SpillPlan, home: SpillHome) -> Option<u64> {
    let mut value = None;
    for member in plan.homes.members(home) {
        let SpillKind::Remat {
            value: member_value,
        } = func.spill_desc(member)?.kind
        else {
            return None;
        };
        if value.is_some_and(|value| value != member_value) {
            return None;
        }
        value = Some(member_value);
    }
    value
}

fn rematerialized_logical_value(func: &MFunction, logical: LogicalValue) -> Option<u64> {
    let SpillKind::Remat { value } = func.spill_desc(VReg(logical.0))?.kind else {
        return None;
    };
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::mir::{BlockId, MBlock, VRegAllocator};

    #[test]
    fn reconstruction_reports_vreg_exhaustion() {
        let mut vregs = VRegAllocator::new();
        vregs.set_next_for_test(u32::MAX);
        let mut func = MFunction::new(vregs, Vec::new());
        let mut logical_for_vreg = Vec::new();

        let error = alloc_fresh(&mut func, &mut logical_for_vreg, LogicalValue(0)).unwrap_err();

        assert_eq!(error.rule, "RECONSTRUCT.VREG_EXHAUSTED");
        assert_eq!(func.vregs.count(), u32::MAX);
    }

    #[test]
    fn removes_dead_cyclic_phi_webs() {
        let mut vregs = VRegAllocator::new();
        let source = vregs.alloc();
        let live = vregs.alloc();
        let dead_left = vregs.alloc();
        let dead_right = vregs.alloc();
        let output = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 5]);
        let mut block = MBlock::new(BlockId(0));
        block.phis.push(PhiNode {
            dst: live,
            sources: vec![(BlockId(0), source)],
        });
        block.phis.push(PhiNode {
            dst: dead_left,
            sources: vec![(BlockId(0), dead_right)],
        });
        block.phis.push(PhiNode {
            dst: dead_right,
            sources: vec![(BlockId(0), dead_left)],
        });
        block.push(MInst::Mov {
            dst: output,
            src: live,
        });
        block.push(MInst::Return);
        func.push_block(block);

        assert_eq!(eliminate_dead_phis(&mut func), 2);
        assert_eq!(func.blocks[0].phis.len(), 1);
        assert_eq!(func.blocks[0].phis[0].dst, live);
    }

    #[test]
    fn removes_only_unused_planned_reload_definitions() {
        let mut vregs = VRegAllocator::new();
        let dead = vregs.alloc();
        let live = vregs.alloc();
        let output = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: dead,
            base: BaseReg::StackFrame,
            offset: 0,
            size: OpSize::S64,
        });
        block.push(MInst::Load {
            dst: live,
            base: BaseReg::StackFrame,
            offset: 8,
            size: OpSize::S64,
        });
        block.push(MInst::Mov {
            dst: output,
            src: live,
        });
        block.push(MInst::Return);
        func.push_block(block);

        assert_eq!(
            eliminate_dead_reloads(&mut func, &BTreeSet::from([dead, live])),
            1
        );
        assert_eq!(func.blocks[0].insts.len(), 3);
        assert_eq!(func.blocks[0].insts[0].def(), Some(live));
    }

    #[test]
    fn fresh_representative_inherits_the_logical_value_width() {
        let mut vregs = VRegAllocator::new();
        let original = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient()]);
        func.value_widths = vec![Some(17)];
        let mut logical_for_vreg = vec![LogicalValue(original.0)];

        let fresh =
            alloc_fresh(&mut func, &mut logical_for_vreg, LogicalValue(original.0)).unwrap();

        assert_eq!(func.value_widths[fresh.0 as usize], Some(17));
        assert_eq!(logical_for_vreg[fresh.0 as usize], LogicalValue(original.0));
    }

    #[test]
    fn missing_phi_representative_is_a_structured_error() {
        let mut vregs = VRegAllocator::new();
        let original = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient()]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::Jump { target: BlockId(1) });
        let mut successor = MBlock::new(BlockId(1));
        successor.push(MInst::LoadImm {
            dst: original,
            value: 1,
        });
        successor.push(MInst::Return);
        func.blocks = vec![entry, successor];
        func.verify();
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
        let plan = super::super::spill_plan::plan(&func, &cfg, &next_use, 32).unwrap();
        let mut logical_for_vreg = vec![LogicalValue(original.0)];
        let fresh =
            alloc_fresh(&mut func, &mut logical_for_vreg, LogicalValue(original.0)).unwrap();
        let successor = cfg.block_index[&BlockId(1)];
        func.blocks[successor].phis.push(PhiNode {
            dst: fresh,
            sources: Vec::new(),
        });
        let reconstruction_phis = HashMap::from([((successor, LogicalValue(original.0)), fresh)]);
        let mut children = vec![Vec::new(); func.blocks.len()];
        children[0].push(successor);

        let error = rename_block(
            0,
            &mut func,
            &cfg,
            &plan,
            &children,
            &reconstruction_phis,
            &HashMap::new(),
            &mut logical_for_vreg,
            &mut HashMap::new(),
            &mut HashMap::new(),
        )
        .unwrap_err();

        assert_eq!(error.rule, "RECONSTRUCT.PHI_REPRESENTATIVE_EXISTS");
        assert_eq!(error.block, Some(BlockId(1)));
    }
}
