//! Planning the transition of a block: what is in registers at its entry and at each point.

use super::*;

/// Scheduling only prices values considered for eviction. Large live-through
/// sets must not cause every block to price every successor's entire live set.
pub(super) trait ExitReloadCosts {
    fn cost(&mut self, value: LogicalValue) -> u32;
}

#[cfg(test)]
impl ExitReloadCosts for HashMap<LogicalValue, u32> {
    fn cost(&mut self, value: LogicalValue) -> u32 {
        self.get(&value).copied().unwrap_or(0)
    }
}

pub(super) struct LazyExitReloadCosts<'a> {
    pub(super) func: &'a MFunction,
    pub(super) cfg: &'a NormalizedCfg,
    pub(super) next_use: &'a NextUseAnalysis,
    pub(super) recipes: &'a PlanningRecipes,
    pub(super) translations: &'a EdgeTranslations,
    pub(super) block: usize,
    // Released after this block, with one entry per queried eviction candidate.
    pub(super) cache: HashMap<LogicalValue, u32>,
}

impl ExitReloadCosts for LazyExitReloadCosts<'_> {
    fn cost(&mut self, value: LogicalValue) -> u32 {
        if let Some(&cost) = self.cache.get(&value) {
            return cost;
        }
        let mut cost = 0u32;
        for &successor in &self.cfg.successors[self.block] {
            let translation = self.translations.by_edge.get(&(self.block, successor));
            // A phi source is demanded once even if it feeds several phis or
            // is also live through. Phi destinations belong to the successor;
            // they must not be charged under their untranslated identity.
            let phi_source = translation.is_some_and(|edge| edge.to_successor.contains_key(&value));
            let untranslated =
                translation.is_none_or(|edge| !edge.to_predecessor.contains_key(&value));
            if phi_source
                || (untranslated && self.next_use.entry[successor].contains_key(&VReg(value.0)))
            {
                cost = cost.saturating_add(u32::from(reload_cost_on_edge(
                    self.func,
                    self.recipes,
                    self.block,
                    successor,
                    value,
                )));
            }
        }
        self.cache.insert(value, cost);
        cost
    }
}

// Independent eager reference: enumerate and translate all successor demands.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) fn exit_reload_costs(
    func: &MFunction,
    cfg: &NormalizedCfg,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    logical: &LogicalValues,
    edge_translations: &EdgeTranslations,
    block: usize,
) -> Result<HashMap<LogicalValue, u32>, SpillPlanError> {
    let mut costs = HashMap::<LogicalValue, u32>::default();
    for &successor in &cfg.successors[block] {
        let mut demanded = LogicalSet::new();
        for &value in next_use.entry[successor].keys() {
            let destination =
                logical.checked_of(value, Some(func.blocks[successor].id), Some(0))?;
            demanded.insert(edge_translations.to_predecessor(block, successor, destination));
        }
        for phi in &func.blocks[successor].phis {
            if let Some((_, source)) = phi
                .sources
                .iter()
                .find(|(predecessor, _)| *predecessor == func.blocks[block].id)
            {
                demanded.insert(logical.checked_of(
                    *source,
                    Some(func.blocks[block].id),
                    Some(func.blocks[block].insts.len()),
                )?);
            }
        }
        for value in demanded {
            let reload = u32::from(reload_cost_on_edge(
                func,
                planning_recipes,
                block,
                successor,
                value,
            ));
            costs
                .entry(value)
                .and_modify(|cost| *cost = cost.saturating_add(reload))
                .or_insert(reload);
        }
    }
    Ok(costs)
}

pub(super) fn spilled_at_entry(
    cfg: &NormalizedCfg,
    plan: &SpillPlan,
    edge_translations: &EdgeTranslations,
    block: usize,
    live_entry: &LogicalSet,
    resident: &LogicalSet,
) -> LogicalSet {
    let mut spilled = live_entry
        .difference(resident)
        .copied()
        .collect::<LogicalSet>();
    if !cfg.predecessors[block].is_empty() {
        spilled.extend(resident.iter().copied().filter(|value| {
            cfg.predecessors[block].iter().all(|predecessor| {
                let predecessor_value =
                    edge_translations.to_predecessor(*predecessor, block, *value);
                plan.s_exit[*predecessor].contains(&predecessor_value)
            })
        }));
    }
    spilled
}

/// Remove optimistic entry residents that do not survive to their first local
/// use.  Keeping such a value in W moves its inevitable store from the incoming
/// edge into the block and occupies a register before providing any use.  S is
/// therefore no more expensive on the executed path and can reuse an already
/// valid predecessor home.
///
/// Replanning is bounded by the initial W size, which is at most the target's
/// fixed register count.  Each iteration removes at least one value, so this
/// remains linear in MIR size for a fixed ISA and needs no CFG-sized copy.
pub(super) fn entry_residents_evicted_before_first_use(
    func: &MFunction,
    next_use: &NextUseAnalysis,
    block: usize,
    resident: &LogicalSet,
    transition: &BlockTransition,
    order: Option<&[usize]>,
) -> LogicalSet {
    let first_use = |value: LogicalValue| {
        order.map_or_else(
            || next_use.next_local_use(block, 0, VReg(value.0)),
            |order| {
                order.iter().position(|&source| {
                    func.blocks[block].insts[source]
                        .uses()
                        .contains(&VReg(value.0))
                })
            },
        )
    };
    transition
        .point_ops
        .iter()
        .filter_map(|(point, operation)| match *operation {
            PlannedOp::Spill { value, .. }
                if resident.contains(&value)
                    && first_use(value).is_none_or(|first_use| point.instruction < first_use) =>
            {
                Some(value)
            }
            PlannedOp::Reload { value, .. }
                if resident.contains(&value)
                    && !transition.recipe_reloads.contains(&(
                        func.blocks[block].id,
                        point.instruction,
                        value,
                    ))
                    && first_use(value) == Some(point.instruction) =>
            {
                Some(value)
            }
            _ => None,
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn plan_block_transition(
    func: &MFunction,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    logical: &LogicalValues,
    homes: &SpillHomes,
    block: usize,
    registers: usize,
    w_entry: &LogicalSet,
    spilled: LogicalSet,
) -> Result<BlockTransition, SpillPlanError> {
    let mut planner = BlockTransitionPlanner::new(
        func,
        next_use,
        planning_recipes,
        logical,
        homes,
        block,
        registers,
        w_entry,
        spilled,
    )?;
    for (instruction, inst) in func.blocks[block].insts.iter().enumerate() {
        planner.step(
            TransitionPoint {
                output: instruction,
                source: instruction,
            },
            inst,
        )?;
    }
    planner.finish()
}

#[derive(Debug, Clone, Copy)]
pub(super) struct TransitionPoint {
    /// Instruction position in the final block order.
    pub(super) output: usize,
    /// Stable position in the order consumed by next-use and planning-recipe
    /// analysis.
    pub(super) source: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct AllocationCandidateScore {
    pub(super) blocked_by_deferred_reload: bool,
    pub(super) pressure_delta: isize,
    pub(super) continuation_tie: std::cmp::Reverse<bool>,
    pub(super) resident_operand_tie: std::cmp::Reverse<usize>,
    pub(super) operand_tie: std::cmp::Reverse<usize>,
    pub(super) materialization_cost: u32,
    pub(super) preferred_rank: usize,
    pub(super) source: usize,
}

pub(super) struct AllocationReadyQueue {
    pub(super) ordered: BTreeSet<(AllocationCandidateScore, usize)>,
    pub(super) by_source: BTreeSet<(usize, usize)>,
    pub(super) scores: Vec<Option<AllocationCandidateScore>>,
}

impl AllocationReadyQueue {
    pub(super) fn new(instructions: usize) -> Self {
        Self {
            ordered: BTreeSet::new(),
            by_source: BTreeSet::new(),
            scores: vec![None; instructions],
        }
    }

    pub(super) fn insert(&mut self, instruction: usize, score: AllocationCandidateScore) {
        debug_assert!(self.scores[instruction].is_none());
        self.ordered.insert((score, instruction));
        self.by_source.insert((score.source, instruction));
        self.scores[instruction] = Some(score);
    }

    pub(super) fn refresh(&mut self, instruction: usize, score: AllocationCandidateScore) {
        let Some(previous) = self.scores[instruction].replace(score) else {
            return;
        };
        if previous != score {
            self.ordered.remove(&(previous, instruction));
            self.ordered.insert((score, instruction));
        }
    }

    pub(super) fn pop(
        &mut self,
        current_resident: usize,
        register_capacity: usize,
        demanded: Option<usize>,
    ) -> Option<(AllocationCandidateScore, usize)> {
        let &(source, source_instruction) = self.by_source.first()?;
        let source_score = self.scores[source_instruction]?;
        debug_assert_eq!(source, source_score.source);
        let source_projected = if source_score.pressure_delta < 0 {
            current_resident.saturating_sub(source_score.pressure_delta.unsigned_abs())
        } else {
            current_resident.saturating_add(source_score.pressure_delta as usize)
        };
        let &(best_score, best_instruction) = self.ordered.first()?;
        let demanded = demanded.and_then(|instruction| {
            let score = self.scores[instruction]?;
            let projected = if score.pressure_delta < 0 {
                current_resident.saturating_sub(score.pressure_delta.unsigned_abs())
            } else {
                current_resident.saturating_add(score.pressure_delta as usize)
            };
            (!score.blocked_by_deferred_reload && projected <= register_capacity)
                .then_some((score, instruction))
        });
        let best_projected = if best_score.pressure_delta < 0 {
            current_resident.saturating_sub(best_score.pressure_delta.unsigned_abs())
        } else {
            current_resident.saturating_add(best_score.pressure_delta as usize)
        };
        let best_is_legal =
            !best_score.blocked_by_deferred_reload && best_projected <= register_capacity;
        let (score, instruction) = if let Some(demanded) = demanded {
            demanded
        } else if best_is_legal
            && (best_score.continuation_tie.0 || current_resident >= register_capacity)
        {
            (best_score, best_instruction)
        } else if !source_score.blocked_by_deferred_reload && source_projected <= register_capacity
        {
            (source_score, source_instruction)
        } else {
            (best_score, best_instruction)
        };
        self.ordered.remove(&(score, instruction));
        self.by_source.remove(&(score.source, instruction));
        self.scores[instruction] = None;
        Some((score, instruction))
    }

    pub(super) fn contains(&self, instruction: usize) -> bool {
        self.scores.get(instruction).is_some_and(Option::is_some)
    }
}

pub(super) struct DependencyDemandFrame {
    pub(super) instruction: usize,
    pub(super) next_dependency: usize,
}

pub(super) struct DependencyDemand {
    pub(super) target: usize,
    pub(super) stack: Vec<DependencyDemandFrame>,
}

impl DependencyDemand {
    pub(super) fn new(target: usize) -> Self {
        Self {
            target,
            stack: vec![DependencyDemandFrame {
                instruction: target,
                next_dependency: 0,
            }],
        }
    }

    /// Follow one unfinished sink packet backwards until its next ready
    /// prerequisite. Each dependency edge in this demand is scanned once.
    pub(super) fn next_ready(
        &mut self,
        region: &super::super::schedule::ForwardReadyRegion,
    ) -> Option<usize> {
        loop {
            let frame_index = self.stack.len().checked_sub(1)?;
            let instruction = self.stack[frame_index].instruction;
            if region.is_emitted(instruction) {
                self.stack.pop();
                continue;
            }
            let dependency = {
                let dependencies = region.dependencies(instruction);
                let frame = &mut self.stack[frame_index];
                let mut dependency = None;
                while frame.next_dependency != dependencies.len() {
                    let candidate = dependencies[frame.next_dependency];
                    frame.next_dependency += 1;
                    if !region.is_emitted(candidate) {
                        dependency = Some(candidate);
                        break;
                    }
                }
                dependency
            };
            if let Some(dependency) = dependency {
                self.stack.push(DependencyDemandFrame {
                    instruction: dependency,
                    next_dependency: 0,
                });
                continue;
            }
            return region.is_ready(instruction).then_some(instruction);
        }
    }
}

pub(super) struct ScheduledStepDelta {
    pub(super) resident: Vec<LogicalValue>,
    pub(super) deferred: Vec<LogicalValue>,
    pub(super) remaining_uses: Vec<LogicalValue>,
    pub(super) continuation: Vec<LogicalValue>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn plan_scheduled_block_transition(
    func: &MFunction,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    logical: &LogicalValues,
    homes: &SpillHomes,
    block: usize,
    registers: usize,
    w_entry: &LogicalSet,
    spilled: LogicalSet,
    constraints: &[super::super::constraints::InstructionConstraints],
    exit_reload_costs: &mut dyn ExitReloadCosts,
) -> Result<(BlockTransition, Vec<usize>), SpillPlanError> {
    let instructions = &func.blocks[block].insts;
    if instructions.len() != constraints.len() {
        return Err(SpillPlanError::new(
            "SPILL_PLAN.SCHEDULE_ORDER",
            Some(func.blocks[block].id),
            None,
            Vec::new(),
            "allocation constraints do not cover every block instruction",
        ));
    }
    let original_spilled = spilled.clone();
    let mut remaining =
        RemainingBlockUses::build(func, next_use, logical, block, exit_reload_costs, None)?;
    let mut planner = BlockTransitionPlanner::new(
        func,
        next_use,
        planning_recipes,
        logical,
        homes,
        block,
        registers,
        w_entry,
        spilled,
    )?;
    let mut order = Vec::with_capacity(instructions.len());
    let mut cursor = 0usize;
    while cursor != instructions.len() {
        if !super::super::schedule::is_allocation_schedulable_at(instructions, constraints, cursor)
        {
            let output = order.len();
            planner.step_scheduled(
                TransitionPoint {
                    output,
                    source: cursor,
                },
                &instructions[cursor],
                &mut remaining,
            )?;
            order.push(cursor);
            cursor += 1;
            continue;
        }

        let start = cursor;
        while cursor != instructions.len()
            && super::super::schedule::is_allocation_schedulable_at(
                instructions,
                constraints,
                cursor,
            )
        {
            cursor += 1;
        }
        let end = cursor;
        let region = &instructions[start..end];
        let mut ready =
            super::super::schedule::ForwardReadyRegion::build(region).ok_or_else(|| {
                SpillPlanError::new(
                    "SPILL_PLAN.SCHEDULE_DEPENDENCY",
                    Some(func.blocks[block].id),
                    Some(start),
                    Vec::new(),
                    "movable region does not have a forward dependency order",
                )
            })?;
        let mut queue = AllocationReadyQueue::new(region.len());
        let sinks = ready.sinks().to_vec();
        let mut next_sink = 0usize;
        let mut demand = sinks.first().copied().map(DependencyDemand::new);
        for &local in ready.ready() {
            let source = start + local;
            queue.insert(
                local,
                planner.candidate_score(source, &instructions[source], &remaining)?,
            );
        }
        while !ready.is_complete() {
            let demanded = demand.as_mut().and_then(|demand| demand.next_ready(&ready));
            let Some((score, local)) =
                queue.pop(planner.resident.len(), planner.registers, demanded)
            else {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.SCHEDULE_DEPENDENCY",
                    Some(func.blocks[block].id),
                    Some(start),
                    Vec::new(),
                    "movable region has no dependency-ready allocation candidate",
                ));
            };
            let source = start + local;
            if score.blocked_by_deferred_reload {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.RECIPE_RELOAD_ORDER",
                    Some(func.blocks[block].id),
                    Some(source),
                    instructions[source].uses().to_vec(),
                    "every dependency-ready instruction precedes an allocator-selected recipe reload point",
                ));
            }
            let output = order.len();
            let delta = planner.step_scheduled(
                TransitionPoint { output, source },
                &instructions[source],
                &mut remaining,
            )?;
            let newly_ready = ready.emit(local).ok_or_else(|| {
                SpillPlanError::new(
                    "SPILL_PLAN.SCHEDULE_DEPENDENCY",
                    Some(func.blocks[block].id),
                    Some(source),
                    Vec::new(),
                    "selected instruction was not dependency-ready",
                )
            })?;
            order.push(source);
            if demand.as_ref().is_some_and(|demand| demand.target == local) {
                next_sink += 1;
                while sinks
                    .get(next_sink)
                    .is_some_and(|&sink| ready.is_emitted(sink))
                {
                    next_sink += 1;
                }
                demand = sinks.get(next_sink).copied().map(DependencyDemand::new);
            }

            let mut refresh = BTreeSet::<usize>::new();
            for local in newly_ready {
                refresh.insert(local);
            }
            for value in delta
                .resident
                .into_iter()
                .chain(delta.deferred)
                .chain(delta.remaining_uses)
                .chain(delta.continuation)
            {
                for &candidate in ready.use_candidates(VReg(value.0)) {
                    if ready.is_ready(candidate) {
                        refresh.insert(candidate);
                    }
                }
            }
            for local in refresh {
                let source = start + local;
                let candidate =
                    planner.candidate_score(source, &instructions[source], &remaining)?;
                if queue.contains(local) {
                    queue.refresh(local, candidate);
                } else {
                    queue.insert(local, candidate);
                }
            }
        }
    }
    let transition = planner.finish()?;
    if order.iter().copied().eq(0..instructions.len()) {
        // The original order has exactly the original pressure. In the many
        // short blocks that do not move an instruction, avoid materializing
        // the potentially large live-out set and measuring that same order
        // twice merely to establish equality.
        return Ok((transition, order));
    }
    let identity = (0..instructions.len()).collect::<Vec<_>>();
    let exit = &next_use.exit[block];
    let pressure = |order: &[usize]| {
        super::super::schedule::pressure_cost_with_membership(
            instructions,
            order,
            exit.len(),
            |value| exit.contains_key(value),
            registers,
        )
    };
    if pressure(&order) <= pressure(&identity) {
        return Ok((transition, order));
    }

    // The allocator's ready walk is intentionally local, so it can avoid one
    // immediate eviction by creating a much wider live wavefront later in the
    // block. Give a bottom-up pressure scheduler one chance to recover such a
    // block before preserving source order. The pressure scheduler itself
    // falls back to identity whenever its complete order is worse.
    let fallback = super::super::schedule::pressure_preferred_block_order(
        instructions,
        constraints,
        exit.keys().copied(),
        registers,
    )
    .unwrap_or(identity);
    let transition = plan_explicit_block_order(
        func,
        next_use,
        planning_recipes,
        logical,
        homes,
        block,
        registers,
        w_entry,
        original_spilled,
        exit_reload_costs,
        &fallback,
    )?;
    Ok((transition, fallback))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn plan_explicit_block_order(
    func: &MFunction,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    logical: &LogicalValues,
    homes: &SpillHomes,
    block: usize,
    registers: usize,
    w_entry: &LogicalSet,
    spilled: LogicalSet,
    exit_reload_costs: &mut dyn ExitReloadCosts,
    order: &[usize],
) -> Result<BlockTransition, SpillPlanError> {
    let instructions = &func.blocks[block].insts;
    let mut remaining = RemainingBlockUses::build(
        func,
        next_use,
        logical,
        block,
        exit_reload_costs,
        Some(order),
    )?;
    let mut planner = BlockTransitionPlanner::new(
        func,
        next_use,
        planning_recipes,
        logical,
        homes,
        block,
        registers,
        w_entry,
        spilled,
    )?;
    for (output, &source) in order.iter().enumerate() {
        let inst = instructions.get(source).ok_or_else(|| {
            SpillPlanError::new(
                "SPILL_PLAN.SCHEDULE_ORDER",
                Some(func.blocks[block].id),
                Some(source),
                Vec::new(),
                "explicit schedule references an instruction outside the block",
            )
        })?;
        planner.step_scheduled(TransitionPoint { output, source }, inst, &mut remaining)?;
    }
    planner.finish()
}

pub(super) struct BlockTransitionPlanner<'a> {
    pub(super) func: &'a MFunction,
    pub(super) next_use: &'a NextUseAnalysis,
    pub(super) planning_recipes: &'a PlanningRecipes,
    pub(super) logical: &'a LogicalValues,
    pub(super) homes: &'a SpillHomes,
    pub(super) block: usize,
    pub(super) registers: usize,
    pub(super) transition: BlockTransition,
    pub(super) resident: LogicalSet,
    pub(super) spilled: LogicalSet,
    pub(super) deferred_recipe_reloads: BTreeMap<LogicalValue, PointUse>,
    pub(super) last_definition: Option<LogicalValue>,
}

impl<'a> BlockTransitionPlanner<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        func: &'a MFunction,
        next_use: &'a NextUseAnalysis,
        planning_recipes: &'a PlanningRecipes,
        logical: &'a LogicalValues,
        homes: &'a SpillHomes,
        block: usize,
        registers: usize,
        w_entry: &LogicalSet,
        mut spilled: LogicalSet,
    ) -> Result<Self, SpillPlanError> {
        let mut transition = BlockTransition {
            point_ops: Vec::new(),
            recipe_reloads: BTreeSet::new(),
            w_exit: LogicalSet::new(),
            s_exit: LogicalSet::new(),
        };
        let resident = w_entry.clone();
        for phi in &func.blocks[block].phis {
            let value = logical.checked_of(phi.dst, Some(func.blocks[block].id), Some(0))?;
            if !resident.contains(&value) {
                transition.point_ops.push((
                    ProgramPoint {
                        block: func.blocks[block].id,
                        instruction: 0,
                        side: PointSide::Before,
                    },
                    PlannedOp::SpillPhi {
                        value,
                        home: homes.of_logical(value),
                    },
                ));
                spilled.insert(value);
            }
        }
        Ok(Self {
            func,
            next_use,
            planning_recipes,
            logical,
            homes,
            block,
            registers,
            transition,
            resident,
            spilled,
            deferred_recipe_reloads: BTreeMap::new(),
            last_definition: None,
        })
    }

    pub(super) fn step(
        &mut self,
        point: TransitionPoint,
        inst: &MInst,
    ) -> Result<(), SpillPlanError> {
        let mut before = LinearFutureUses {
            func: self.func,
            next_use: self.next_use,
            block: self.block,
            instruction: point.source,
        };
        let uses = self.begin_step(point, inst, &mut before)?;
        let mut after = LinearFutureUses {
            func: self.func,
            next_use: self.next_use,
            block: self.block,
            instruction: point.source + 1,
        };
        self.finish_step(point, inst, &uses, &mut after)
    }

    pub(super) fn step_scheduled(
        &mut self,
        point: TransitionPoint,
        inst: &MInst,
        remaining: &mut RemainingBlockUses<'_>,
    ) -> Result<ScheduledStepDelta, SpillPlanError> {
        let resident_before = self.resident.iter().copied().collect::<Vec<_>>();
        let deferred_before = self
            .deferred_recipe_reloads
            .iter()
            .map(|(&value, &point)| (value, point))
            .collect::<Vec<_>>();
        let last_definition_before = self.last_definition;
        let uses = {
            let mut before = DynamicFutureUses(remaining);
            self.begin_step(point, inst, &mut before)?
        };
        let remaining_uses = remaining.emit(point.source, inst, self.logical)?;
        {
            let mut after = DynamicFutureUses(remaining);
            self.finish_step(point, inst, &uses, &mut after)?;
        }
        let mut resident = resident_before
            .iter()
            .copied()
            .filter(|value| !self.resident.contains(value))
            .chain(
                self.resident
                    .iter()
                    .copied()
                    .filter(|value| resident_before.binary_search(value).is_err()),
            )
            .collect::<Vec<_>>();
        resident.sort_unstable();

        let mut deferred = Vec::new();
        let (mut before_index, mut after) =
            (0usize, self.deferred_recipe_reloads.iter().peekable());
        while before_index < deferred_before.len() || after.peek().is_some() {
            match (deferred_before.get(before_index), after.peek().copied()) {
                (Some(&(before_value, before_point)), Some((&after_value, &after_point))) => {
                    match before_value.cmp(&after_value) {
                        Ordering::Less => {
                            deferred.push(before_value);
                            before_index += 1;
                        }
                        Ordering::Equal => {
                            if before_point != after_point {
                                deferred.push(before_value);
                            }
                            before_index += 1;
                            after.next();
                        }
                        Ordering::Greater => {
                            deferred.push(after_value);
                            after.next();
                        }
                    }
                }
                (Some(&(before_value, _)), None) => {
                    deferred.push(before_value);
                    before_index += 1;
                }
                (None, Some((&after_value, _))) => {
                    deferred.push(after_value);
                    after.next();
                }
                (None, None) => break,
            }
        }

        let mut continuation = [last_definition_before, self.last_definition]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        continuation.sort_unstable();
        continuation.dedup();
        Ok(ScheduledStepDelta {
            resident,
            deferred,
            remaining_uses,
            continuation,
        })
    }

    pub(super) fn candidate_score(
        &self,
        source: usize,
        inst: &MInst,
        remaining: &RemainingBlockUses<'_>,
    ) -> Result<AllocationCandidateScore, SpillPlanError> {
        let block_id = self.func.blocks[self.block].id;
        let mut uses = inst
            .uses()
            .iter()
            .copied()
            .map(|value| self.logical.checked_of(value, Some(block_id), Some(source)))
            .collect::<Result<Vec<_>, _>>()?;
        uses.sort_unstable();
        uses.dedup();
        let mut blocked_by_deferred_reload = false;
        let mut missing = 0usize;
        let mut resident_operands = 0usize;
        let mut materialization_cost = 0u32;
        let costs =
            super::super::cost::MachineSpillCosts::with_recipes(self.func, self.planning_recipes);
        for &value in &uses {
            if self.resident.contains(&value) {
                resident_operands += 1;
                continue;
            }
            missing += 1;
            let point = PointUse {
                block: block_id,
                instruction: source,
                value: VReg(value.0),
            };
            if let Some(&expected) = self.deferred_recipe_reloads.get(&value) {
                if expected != point {
                    blocked_by_deferred_reload = true;
                    continue;
                }
                materialization_cost = materialization_cost.saturating_add(u32::from(
                    self.planning_recipes
                        .point_specific_materialization_cost(point)
                        .unwrap_or_else(|| costs.persistent_reload(VReg(value.0))),
                ));
            } else {
                materialization_cost = materialization_cost
                    .saturating_add(u32::from(costs.persistent_reload(VReg(value.0))));
            }
        }
        let dying = uses
            .iter()
            .filter(|&&value| remaining.remaining_uses(value) == 1 && !remaining.is_live_out(value))
            .count();
        let definition_live = inst
            .def()
            .map(|value| self.logical.checked_of(value, Some(block_id), Some(source)))
            .transpose()?
            .is_some_and(|value| {
                remaining.remaining_uses(value) != 0 || remaining.is_live_out(value)
            });
        let pressure_delta = isize::try_from(missing + usize::from(definition_live))
            .unwrap_or(isize::MAX)
            .saturating_sub(isize::try_from(dying).unwrap_or(isize::MAX));
        Ok(AllocationCandidateScore {
            blocked_by_deferred_reload,
            pressure_delta,
            continuation_tie: std::cmp::Reverse(
                self.last_definition
                    .is_some_and(|value| uses.contains(&value)),
            ),
            // Reusing an operand already in W closes the residency cluster
            // which made that value worth keeping.  Rank this before a new
            // root with no operands; otherwise a stream of cheap Loads fills
            // W, spills those very results, and only then visits their uses.
            resident_operand_tie: std::cmp::Reverse(resident_operands),
            // Once W is full, an operand-bearing instruction closes existing
            // work (resident, spilled, or rematerializable).  A zero-input
            // root only creates another value which must displace such work.
            operand_tie: std::cmp::Reverse(uses.len()),
            materialization_cost,
            // Preserve the incoming ISel order as the deterministic final
            // tie-breaker after dependency locality and the current W/S
            // transition have made no distinction.
            preferred_rank: source,
            source,
        })
    }

    pub(super) fn begin_step(
        &mut self,
        point: TransitionPoint,
        inst: &MInst,
        future_uses: &mut impl FutureUses,
    ) -> Result<Vec<LogicalValue>, SpillPlanError> {
        let block_id = self.func.blocks[self.block].id;
        let mut uses = inst
            .uses()
            .into_iter()
            .map(|value| {
                self.logical
                    .checked_of(value, Some(block_id), Some(point.output))
            })
            .collect::<Result<Vec<_>, _>>()?;
        uses.sort_unstable();
        uses.dedup();
        for &value in &uses {
            if self.resident.insert(value) {
                if let Some(expected) = self.deferred_recipe_reloads.remove(&value) {
                    let actual = PointUse {
                        block: block_id,
                        instruction: point.source,
                        value: VReg(value.0),
                    };
                    if expected != actual {
                        return Err(SpillPlanError::new(
                            "SPILL_PLAN.RECIPE_RELOAD_POINT",
                            Some(block_id),
                            Some(point.output),
                            vec![VReg(value.0)],
                            format!(
                                "deferred recipe reload expected {expected:?} but reached {actual:?}"
                            ),
                        ));
                    }
                    self.transition
                        .recipe_reloads
                        .insert((block_id, point.output, value));
                }
                self.transition.point_ops.push((
                    ProgramPoint {
                        block: block_id,
                        instruction: point.output,
                        side: PointSide::Before,
                    },
                    PlannedOp::Reload {
                        value,
                        home: self.homes.of_logical(value),
                    },
                ));
            }
        }
        limit(
            self.func,
            self.planning_recipes,
            self.homes,
            &mut self.transition.point_ops,
            self.block,
            point.output,
            self.registers,
            &uses,
            &mut self.resident,
            &mut self.spilled,
            &mut self.deferred_recipe_reloads,
            future_uses,
        )?;
        Ok(uses)
    }

    pub(super) fn finish_step(
        &mut self,
        point: TransitionPoint,
        inst: &MInst,
        uses: &[LogicalValue],
        future_uses: &mut impl FutureUses,
    ) -> Result<(), SpillPlanError> {
        let block_id = self.func.blocks[self.block].id;
        let clobbered = super::super::assignment::allocatable_clobber_count(inst, self.func);
        if clobbered > self.registers {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.CLOBBER_CAPACITY",
                Some(block_id),
                Some(point.output),
                inst.uses().to_vec(),
                format!(
                    "instruction clobbers {clobbered} registers but the allocator has only {}",
                    self.registers
                ),
            ));
        }
        if clobbered != 0 {
            limit_live_through_clobber(
                self.func,
                self.planning_recipes,
                self.homes,
                &mut self.transition.point_ops,
                self.block,
                point.output,
                self.registers.saturating_sub(clobbered),
                &mut self.resident,
                &mut self.spilled,
                &mut self.deferred_recipe_reloads,
                future_uses,
            )?;
        }
        if let Some(definition) = inst.def() {
            let definition =
                self.logical
                    .checked_of(definition, Some(block_id), Some(point.output))?;
            if !self.resident.contains(&definition) && self.resident.len() == self.registers {
                let Some(maximum) = self.registers.checked_sub(1) else {
                    return Err(SpillPlanError::new(
                        "SPILL_PLAN.OPERAND_PRESSURE",
                        Some(block_id),
                        Some(point.output),
                        vec![VReg(definition.0)],
                        "an instruction result requires a register but no registers are available",
                    ));
                };
                limit(
                    self.func,
                    self.planning_recipes,
                    self.homes,
                    &mut self.transition.point_ops,
                    self.block,
                    point.output,
                    maximum,
                    uses,
                    &mut self.resident,
                    &mut self.spilled,
                    &mut self.deferred_recipe_reloads,
                    future_uses,
                )?;
            }
            self.resident.insert(definition);
            self.last_definition = Some(definition);
        } else {
            self.last_definition = None;
        }
        self.resident
            .retain(|value| !future_uses.distance(*value).is_dead());
        Ok(())
    }

    pub(super) fn finish(mut self) -> Result<BlockTransition, SpillPlanError> {
        if !self.deferred_recipe_reloads.is_empty() {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.RECIPE_RELOAD_POINT",
                Some(self.func.blocks[self.block].id),
                None,
                self.deferred_recipe_reloads
                    .keys()
                    .map(|value| VReg(value.0))
                    .collect(),
                "deferred recipe reload did not reach its final local use",
            ));
        }
        self.transition.w_exit = self.resident;
        self.transition.s_exit = self.spilled;
        Ok(self.transition)
    }
}
