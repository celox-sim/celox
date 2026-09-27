//! Eliminate muxes at joins already controlled by branch conditions.

use super::*;

pub(super) fn eliminate_controlled_join_muxes(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    controlled_join_after: Option<usize>,
) {
    let Some(cfg) = CfgAnalysis::compute(eu) else {
        return;
    };
    let def_blocks = all_def_blocks(eu);
    let def_locations = instruction_def_locations(eu);
    let use_counts = count_uses(eu);
    let first_effect = eu
        .blocks
        .iter()
        .map(|(&block_id, block)| {
            let index = block
                .instructions
                .iter()
                .position(|instruction| {
                    memory_write(instruction).is_some() || is_memory_barrier(instruction)
                })
                .unwrap_or(block.instructions.len());
            (block_id, index)
        })
        .collect::<HashMap<_, _>>();
    let mut branches_by_root = HashMap::<RegisterId, Vec<BranchInfo>>::default();

    let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_unstable_by_key(|id| id.0);
    for block_id in block_ids.iter().copied() {
        let block = &eu.blocks[&block_id];
        let SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } = &block.terminator
        else {
            continue;
        };
        let (root, _) = resolve_boolean_alias(eu, &def_locations, *cond);
        branches_by_root.entry(root).or_default().push(BranchInfo {
            source: block_id,
            true_target: true_block.0,
            false_target: false_block.0,
        });
    }
    if branches_by_root.is_empty() {
        return;
    }

    let mut plans = Vec::new();
    for block_id in block_ids {
        if controlled_join_after.is_some_and(|watermark| block_id.0 <= watermark) {
            continue;
        }
        let block = &eu.blocks[&block_id];
        for (mux_idx, inst) in block.instructions.iter().enumerate() {
            let SIRInstruction::Mux(dst, condition, true_val, false_val) = inst else {
                continue;
            };
            let (root, _) = resolve_boolean_alias(eu, &def_locations, *condition);
            let plan = branches_by_root
                .get(&root)
                .into_iter()
                .flatten()
                .find_map(|branch| {
                    plan_controlled_join_mux(
                        eu,
                        &cfg,
                        &def_blocks,
                        &def_locations,
                        branch,
                        block_id,
                        mux_idx,
                        *condition,
                        *dst,
                        *true_val,
                        *false_val,
                        &use_counts,
                        &first_effect,
                    )
                })
                .or_else(|| {
                    plan_path_conditioned_join_mux(
                        eu,
                        &cfg,
                        &def_blocks,
                        &def_locations,
                        block_id,
                        mux_idx,
                        *dst,
                        *true_val,
                        *false_val,
                    )
                });
            let Some(plan) = plan else {
                continue;
            };
            plans.push(plan);
        }
    }

    apply_controlled_join_mux_plans(eu, plans);
}

fn apply_controlled_join_mux_plans(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plans: Vec<ControlledMuxPlan>,
) {
    let mut by_join = HashMap::<BlockId, Vec<ControlledMuxPlan>>::default();
    for plan in plans {
        by_join.entry(plan.join).or_default().push(plan);
    }
    let mut joins = by_join.keys().copied().collect::<Vec<_>>();
    joins.sort_unstable_by_key(|block| block.0);

    for join_id in joins {
        let Some(mut plans) = by_join.remove(&join_id) else {
            continue;
        };
        plans.sort_unstable_by_key(|plan| plan.mux_idx);
        let Some(original) = eu.blocks.get(&join_id).cloned() else {
            continue;
        };

        let mut removed = BTreeSet::new();
        let mut moved_by_predecessor = HashMap::<BlockId, BTreeSet<usize>>::default();
        let mut valid = true;
        for plan in &plans {
            if !matches!(
                original.instructions.get(plan.mux_idx),
                Some(SIRInstruction::Mux(dst, ..)) if *dst == plan.dst
            ) || !removed.insert(plan.mux_idx)
            {
                valid = false;
                break;
            }
            for moved in &plan.moved {
                if removed.contains(&moved.index)
                    || !moved_by_predecessor
                        .entry(moved.predecessor)
                        .or_default()
                        .insert(moved.index)
                {
                    valid = false;
                    break;
                }
                removed.insert(moved.index);
            }
            if !valid {
                break;
            }
        }
        if !valid
            || moved_by_predecessor.iter().any(|(&predecessor, _)| {
                !matches!(
                    eu.blocks.get(&predecessor).map(|block| &block.terminator),
                    Some(SIRTerminator::Jump(target, _)) if *target == join_id
                )
            })
        {
            continue;
        }

        let mut predecessors = moved_by_predecessor.keys().copied().collect::<Vec<_>>();
        predecessors.sort_unstable_by_key(|block| block.0);
        for predecessor in predecessors {
            let instructions = moved_by_predecessor[&predecessor]
                .iter()
                .map(|&index| original.instructions[index].clone())
                .collect::<Vec<_>>();
            eu.blocks
                .get_mut(&predecessor)
                .expect("preflighted controlled predecessor must remain present")
                .instructions
                .extend(instructions);
        }

        {
            let join = eu
                .blocks
                .get_mut(&join_id)
                .expect("controlled join must remain present");
            join.instructions = original
                .instructions
                .into_iter()
                .enumerate()
                .filter_map(|(index, instruction)| {
                    (!removed.contains(&index)).then_some(instruction)
                })
                .collect();
            join.params.extend(plans.iter().map(|plan| plan.dst));
        }

        // Parameter order and edge-argument order are both the ascending Mux
        // order above.  This publishes the edge-sunk definitions and the
        // select-to-phi rewrite as one valid SSA change.
        for plan in plans {
            for edge in plan.incoming {
                let value = if edge.select_true {
                    plan.true_val
                } else {
                    plan.false_val
                };
                append_controlled_edge_argument(
                    eu,
                    edge.predecessor,
                    plan.join,
                    edge.edge_truth,
                    value,
                );
            }
        }
    }
}

fn plan_controlled_join_mux(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &CfgAnalysis,
    def_blocks: &HashMap<RegisterId, BlockId>,
    def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    branch: &BranchInfo,
    join: BlockId,
    mux_idx: usize,
    condition: RegisterId,
    dst: RegisterId,
    true_val: RegisterId,
    false_val: RegisterId,
    use_counts: &HashMap<RegisterId, usize>,
    first_effect: &HashMap<BlockId, usize>,
) -> Option<ControlledMuxPlan> {
    if branch.source == join
        || !cfg.graph.dominates(branch.source, join)
        || !cfg.graph.postdominates(join, branch.true_target)
        || !cfg.graph.postdominates(join, branch.false_target)
    {
        return None;
    }

    let block = eu.blocks.get(&join)?;
    if block.params.contains(&dst)
        || !matches!(
            block.instructions.get(mux_idx),
            Some(SIRInstruction::Mux(..))
        )
    {
        return None;
    }

    let incoming_edges = cfg.incoming_edges(join)?.to_vec();
    if incoming_edges.is_empty() {
        return None;
    }

    let mut incoming = Vec::with_capacity(incoming_edges.len());
    let mut seen_predecessors = HashSet::default();
    let mut moved = HashMap::<usize, BlockId>::default();
    for (predecessor, edge_truth) in incoming_edges {
        // A block with two edges to the same join has no unambiguous edge
        // classification for this transform.  Leave it to the general
        // branchifier instead of guessing.
        if !seen_predecessors.insert(predecessor) || predecessor == join {
            return None;
        }

        // A predicate can be branched on repeatedly.  Dominance alone cannot
        // identify which occurrence controls this edge: a CFG-only walk sees
        // infeasible paths that flip the same SSA boolean later.  Derive the
        // Mux's truth value from the actual incoming edge facts instead.
        let facts =
            cfg.path_facts
                .facts_on_edge(eu, def_locations, predecessor, join, edge_truth)?;
        let selected = known_condition_truth(eu, def_locations, &facts, condition)?;
        let selected_value = if selected { true_val } else { false_val };
        for definition in controlled_value_availability(
            eu,
            cfg,
            def_blocks,
            def_locations,
            use_counts,
            first_effect,
            join,
            mux_idx,
            predecessor,
            selected_value,
        )? {
            if moved
                .insert(definition.index, definition.predecessor)
                .is_some_and(|owner| owner != definition.predecessor)
            {
                // One SSA definition cannot be moved to two predecessor
                // blocks without cloning and renaming its complete DAG.
                return None;
            }
        }

        incoming.push(ControlledIncomingEdge {
            predecessor,
            select_true: selected,
            edge_truth,
        });
    }

    Some(ControlledMuxPlan {
        join,
        mux_idx,
        dst,
        true_val,
        false_val,
        incoming,
        moved: moved
            .into_iter()
            .map(|(index, predecessor)| ControlledMovedInstruction { predecessor, index })
            .collect(),
    })
}

#[allow(clippy::too_many_arguments)]
fn controlled_value_availability(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &CfgAnalysis,
    def_blocks: &HashMap<RegisterId, BlockId>,
    def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    use_counts: &HashMap<RegisterId, usize>,
    first_effect: &HashMap<BlockId, usize>,
    join: BlockId,
    mux_idx: usize,
    predecessor: BlockId,
    value: RegisterId,
) -> Option<Vec<ControlledMovedInstruction>> {
    let &definition_block = def_blocks.get(&value)?;
    if cfg.graph.dominates(definition_block, predecessor) {
        return Some(Vec::new());
    }
    if definition_block != join
        || !matches!(
            eu.blocks.get(&predecessor).map(|block| &block.terminator),
            Some(SIRTerminator::Jump(target, _)) if *target == join
        )
    {
        return None;
    }

    let block = eu.blocks.get(&join)?;
    let mut definitions = HashSet::default();
    collect_controlled_edge_defs(
        block,
        join,
        def_locations,
        use_counts,
        mux_idx,
        value,
        &mut definitions,
    );
    let &(_, root_index) = def_locations.get(&value)?;
    if !definitions.contains(&root_index) {
        return None;
    }

    let moved_values = definitions
        .iter()
        .filter_map(|&index| def_reg(&block.instructions[index]))
        .collect::<HashSet<_>>();
    for &index in &definitions {
        let instruction = &block.instructions[index];
        // A state read can move from the join entry to the predecessor edge
        // only if it crosses no write or runtime-observation point.  The first
        // effect index is one scalar per block, avoiding a dense per-load
        // prefix table on very large EUs.
        if matches!(instruction, SIRInstruction::Load(..))
            && index >= first_effect.get(&join).copied().unwrap_or(0)
        {
            return None;
        }
        for operand in inst_uses(instruction) {
            if moved_values.contains(&operand) {
                continue;
            }
            let &operand_block = def_blocks.get(&operand)?;
            if !cfg.graph.dominates(operand_block, predecessor) {
                return None;
            }
        }
    }

    let mut definitions = definitions.into_iter().collect::<Vec<_>>();
    definitions.sort_unstable();
    Some(
        definitions
            .into_iter()
            .map(|index| ControlledMovedInstruction { predecessor, index })
            .collect(),
    )
}

fn collect_controlled_edge_defs(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    block_id: BlockId,
    def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    use_counts: &HashMap<RegisterId, usize>,
    user_index: usize,
    value: RegisterId,
    definitions: &mut HashSet<usize>,
) {
    if use_counts.get(&value).copied().unwrap_or(0) != 1 {
        return;
    }
    let Some(&(definition_block, index)) = def_locations.get(&value) else {
        return;
    };
    if definition_block != block_id || index >= user_index || definitions.contains(&index) {
        return;
    }
    let instruction = &block.instructions[index];
    if !matches!(
        instruction,
        SIRInstruction::Imm(..)
            | SIRInstruction::Binary(..)
            | SIRInstruction::Unary(..)
            | SIRInstruction::Load(..)
            | SIRInstruction::Concat(..)
            | SIRInstruction::Slice(..)
    ) {
        return;
    }

    definitions.insert(index);
    for operand in inst_uses(instruction) {
        collect_controlled_edge_defs(
            block,
            block_id,
            def_locations,
            use_counts,
            index,
            operand,
            definitions,
        );
    }
}

fn plan_path_conditioned_join_mux(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &CfgAnalysis,
    def_blocks: &HashMap<RegisterId, BlockId>,
    def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    join: BlockId,
    mux_idx: usize,
    dst: RegisterId,
    true_val: RegisterId,
    false_val: RegisterId,
) -> Option<ControlledMuxPlan> {
    let block = eu.blocks.get(&join)?;
    if block.params.contains(&dst)
        || !matches!(
            block.instructions.get(mux_idx),
            Some(SIRInstruction::Mux(..))
        )
    {
        return None;
    }
    let incoming_edges = cfg.incoming_edges(join)?.to_vec();
    if incoming_edges.is_empty() {
        return None;
    }
    let Some(SIRInstruction::Mux(_, condition, ..)) = block.instructions.get(mux_idx) else {
        return None;
    };
    let (_, condition_inverted) = resolve_boolean_alias(eu, def_locations, *condition);
    let mut incoming = Vec::with_capacity(incoming_edges.len());
    let mut seen_predecessors = HashSet::default();
    for (predecessor, edge_truth) in incoming_edges {
        if !seen_predecessors.insert(predecessor) || predecessor == join {
            return None;
        }
        let facts =
            cfg.path_facts
                .facts_on_edge(eu, def_locations, predecessor, join, edge_truth)?;
        let condition_truth = known_condition_truth(eu, def_locations, &facts, *condition)?;
        let select_true = condition_truth ^ condition_inverted;
        let selected_value = if select_true { true_val } else { false_val };
        let def_block = def_blocks.get(&selected_value)?;
        if !cfg.graph.dominates(*def_block, predecessor) {
            return None;
        }
        incoming.push(ControlledIncomingEdge {
            predecessor,
            select_true,
            edge_truth,
        });
    }
    Some(ControlledMuxPlan {
        join,
        mux_idx,
        dst,
        true_val,
        false_val,
        incoming,
        moved: Vec::new(),
    })
}

fn append_controlled_edge_argument(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    predecessor: BlockId,
    target: BlockId,
    edge_truth: Option<bool>,
    value: RegisterId,
) {
    let Some(block) = eu.blocks.get_mut(&predecessor) else {
        return;
    };
    match &mut block.terminator {
        SIRTerminator::Jump(destination, args) if *destination == target => args.push(value),
        SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } => match edge_truth {
            Some(true) if true_block.0 == target => true_block.1.push(value),
            Some(false) if false_block.0 == target => false_block.1.push(value),
            _ => {}
        },
        _ => {}
    }
}
