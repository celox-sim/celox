//! Choosing what stays in registers: initial sets, future uses and eviction costs.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn limit_live_through_clobber(
    func: &MFunction,
    planning_recipes: &PlanningRecipes,
    homes: &SpillHomes,
    point_ops: &mut Vec<(ProgramPoint, PlannedOp)>,
    block: usize,
    point_instruction: usize,
    capacity: usize,
    resident: &mut LogicalSet,
    spilled: &mut LogicalSet,
    deferred_recipe_reloads: &mut BTreeMap<LogicalValue, PointUse>,
    future_uses: &mut impl FutureUses,
) -> Result<(), SpillPlanError> {
    let mut live_through = resident
        .iter()
        .copied()
        .filter(|value| !future_uses.distance(*value).is_dead())
        .collect::<BTreeSet<_>>();
    while live_through.len() > capacity {
        let Some(victim) = live_through.iter().copied().max_by(|left, right| {
            compare_eviction_candidates(
                func,
                planning_recipes,
                spilled,
                future_uses,
                (*left, future_uses.distance(*left)),
                (*right, future_uses.distance(*right)),
            )
        }) else {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.MIN_VICTIM",
                Some(func.blocks[block].id),
                Some(point_instruction),
                Vec::new(),
                "clobber pressure exceeded capacity but MIN had no live-through victim",
            ));
        };
        evict_value(
            func,
            planning_recipes,
            homes,
            point_ops,
            block,
            point_instruction,
            victim,
            spilled,
            deferred_recipe_reloads,
            future_uses,
        )?;
        live_through.remove(&victim);
        resident.remove(&victim);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn init_usual(
    func: &MFunction,
    cfg: &NormalizedCfg,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    plan: &SpillPlan,
    edge_translations: &EdgeTranslations,
    block: usize,
    registers: usize,
) -> LogicalSet {
    let processed = cfg.predecessors[block]
        .iter()
        .copied()
        .filter(|predecessor| *predecessor < block)
        .collect::<Vec<_>>();
    if processed.is_empty() {
        return LogicalSet::new();
    }
    if processed.len() == 1 && cfg.predecessors[block].len() == 1 {
        let predecessor = processed[0];
        return plan.w_exit[predecessor]
            .iter()
            .copied()
            .flat_map(|value| edge_translations.to_successors(predecessor, block, value))
            .collect();
    }
    let mut frequency = HashMap::<LogicalValue, usize>::default();
    for predecessor in &processed {
        for &value in &plan.w_exit[*predecessor] {
            for value in edge_translations.to_successors(*predecessor, block, value) {
                *frequency.entry(value).or_default() += 1;
            }
        }
    }
    let mut candidates = frequency
        .keys()
        .copied()
        .filter_map(|value| {
            let mut keep_cost = 0u128;
            let mut drop_cost = 0u128;
            for &predecessor in &processed {
                let predecessor_value = edge_translations.to_predecessor(predecessor, block, value);
                if plan.w_exit[predecessor].contains(&predecessor_value) {
                    if !plan.s_exit[predecessor].contains(&predecessor_value) {
                        drop_cost = drop_cost
                            .saturating_add(u128::from(spill_cost(func, predecessor_value)));
                    }
                } else {
                    keep_cost = keep_cost.saturating_add(u128::from(reload_cost_on_edge(
                        func,
                        planning_recipes,
                        predecessor,
                        block,
                        predecessor_value,
                    )));
                }
            }
            if next_use.anticipated_at_entry(block, VReg(value.0)) {
                drop_cost = drop_cost.saturating_add((processed.len() as u128).saturating_mul(
                    local_use_cluster_cost(func, next_use, planning_recipes, block, 0, value),
                ));
            }
            let savings = drop_cost
                .checked_sub(keep_cost)
                .filter(|saving| *saving != 0)?;
            Some((
                value,
                savings,
                logical_entry_distance(func, next_use, block, value),
            ))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| compare_join_retention_candidates(*left, *right));
    candidates
        .into_iter()
        .take(registers)
        .map(|(value, _, _)| value)
        .collect()
}

/// Prefer the values for which entry residency avoids the most coupling and
/// guaranteed-use work per instruction of live-range occupancy.  Loop exits
/// dominate straight-line distance, matching global next-use ordering.
pub(super) fn compare_join_retention_candidates(
    left: (LogicalValue, u128, NextUseDistance),
    right: (LogicalValue, u128, NextUseDistance),
) -> Ordering {
    match (left.2, right.2) {
        (NextUseDistance::Dead, NextUseDistance::Dead) => left.0.cmp(&right.0),
        (NextUseDistance::Dead, _) => Ordering::Greater,
        (_, NextUseDistance::Dead) => Ordering::Less,
        (
            NextUseDistance::Finite {
                loop_exits: left_exits,
                instructions: left_instructions,
            },
            NextUseDistance::Finite {
                loop_exits: right_exits,
                instructions: right_instructions,
            },
        ) => left_exits.cmp(&right_exits).then_with(|| {
            let left_span = left_instructions as u128 + 1;
            let right_span = right_instructions as u128 + 1;
            right
                .1
                .saturating_mul(left_span)
                .cmp(&left.1.saturating_mul(right_span))
                .then_with(|| left_instructions.cmp(&right_instructions))
                .then_with(|| left.0.cmp(&right.0))
        }),
    }
}

pub(super) fn init_loop_region(
    func: &MFunction,
    next_use: &NextUseAnalysis,
    plan: &SpillPlan,
    block: usize,
    region: usize,
    registers: usize,
) -> Result<LogicalSet, SpillPlanError> {
    let mut alive = next_use.entry[block]
        .keys()
        .copied()
        .map(|value| {
            plan.logical
                .checked_of(value, Some(func.blocks[block].id), Some(0))
        })
        .collect::<Result<LogicalSet, _>>()?;
    for phi in &func.blocks[block].phis {
        alive.insert(
            plan.logical
                .checked_of(phi.dst, Some(func.blocks[block].id), Some(0))?,
        );
    }
    let Some(facts) = next_use.loop_regions.get(region) else {
        return Err(SpillPlanError::new(
            "SPILL_PLAN.NEXT_USE_REGION",
            Some(func.blocks[block].id),
            Some(0),
            Vec::new(),
            format!("next-use analysis references absent loop region {region}"),
        ));
    };
    let mut candidates = BinaryHeap::new();
    let mut live_through_count = 0;
    for &value in &alive {
        if next_use.used_in_region(region, VReg(value.0)) {
            let distance = logical_entry_distance(func, next_use, block, value);
            retain_closest(&mut candidates, registers, (distance, value));
        } else {
            live_through_count += 1;
        }
    }
    if candidates.len() >= registers {
        return Ok(candidates
            .into_sorted_vec()
            .into_iter()
            .map(|(_, value)| value)
            .collect());
    }
    let internal_pressure = facts.max_pressure.saturating_sub(live_through_count);
    let free_loop = registers.saturating_sub(internal_pressure);
    let mut live_through = BinaryHeap::new();
    if free_loop > 0 {
        for value in alive {
            if !next_use.used_in_region(region, VReg(value.0)) {
                let distance = logical_entry_distance(func, next_use, block, value);
                retain_closest(&mut live_through, free_loop, (distance, value));
            }
        }
    }
    Ok(candidates
        .into_sorted_vec()
        .into_iter()
        .chain(live_through.into_sorted_vec().into_iter().take(free_loop))
        .take(registers)
        .map(|(_, value)| value)
        .collect())
}

/// Keep only the register-sized prefix of the distance ordering. Logical IDs
/// break ties exactly as stable sorting the original ascending set did.
pub(super) fn retain_closest(
    closest: &mut BinaryHeap<(NextUseDistance, LogicalValue)>,
    limit: usize,
    candidate: (NextUseDistance, LogicalValue),
) {
    if closest.len() < limit {
        closest.push(candidate);
    } else if let Some(mut furthest) = closest.peek_mut()
        && candidate < *furthest
    {
        *furthest = candidate;
    }
}

pub(super) trait FutureUses {
    fn distance(&self, value: LogicalValue) -> NextUseDistance;
    fn next_point(&self, value: LogicalValue) -> Option<PointUse>;
    fn exit_reload_cost(&mut self, _value: LogicalValue) -> u32 {
        0
    }
}

pub(super) struct LinearFutureUses<'a> {
    pub(super) func: &'a MFunction,
    pub(super) next_use: &'a NextUseAnalysis,
    pub(super) block: usize,
    pub(super) instruction: usize,
}

impl FutureUses for LinearFutureUses<'_> {
    fn distance(&self, value: LogicalValue) -> NextUseDistance {
        self.next_use
            .distance_at(self.func, self.block, self.instruction, VReg(value.0))
    }

    fn next_point(&self, value: LogicalValue) -> Option<PointUse> {
        let instruction =
            self.next_use
                .next_local_use(self.block, self.instruction, VReg(value.0))?;
        Some(PointUse {
            block: self.func.blocks[self.block].id,
            instruction,
            value: VReg(value.0),
        })
    }
}

#[derive(Default)]
pub(super) struct RemainingUses {
    pub(super) points: std::ops::Range<usize>,
    pub(super) next: usize,
    pub(super) count: usize,
}

pub(super) struct RemainingBlockUses<'a> {
    pub(super) block: BlockId,
    pub(super) preferred_rank: Vec<usize>,
    pub(super) remaining: HashMap<LogicalValue, RemainingUses>,
    /// All use positions of this block, partitioned into one range per value.
    /// One allocation replaces a separate growable vector for every register.
    pub(super) points: Vec<(usize, usize)>,
    pub(super) exit: &'a DistanceMap,
    pub(super) exit_reload_costs: &'a mut dyn ExitReloadCosts,
    pub(super) emitted: Vec<bool>,
    pub(super) emitted_count: usize,
}

impl<'a> RemainingBlockUses<'a> {
    pub(super) fn build(
        func: &MFunction,
        next_use: &'a NextUseAnalysis,
        logical: &LogicalValues,
        block: usize,
        exit_reload_costs: &'a mut dyn ExitReloadCosts,
        preferred_order: Option<&[usize]>,
    ) -> Result<Self, SpillPlanError> {
        let instructions = func.blocks[block].insts.len();
        let preferred_rank = if let Some(order) = preferred_order {
            if order.len() != instructions {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.SCHEDULE_ORDER",
                    Some(func.blocks[block].id),
                    None,
                    Vec::new(),
                    "explicit order does not cover every block instruction",
                ));
            }
            let mut ranks = vec![usize::MAX; instructions];
            for (rank, &source) in order.iter().enumerate() {
                let Some(slot) = ranks.get_mut(source) else {
                    return Err(SpillPlanError::new(
                        "SPILL_PLAN.SCHEDULE_ORDER",
                        Some(func.blocks[block].id),
                        Some(source),
                        Vec::new(),
                        "explicit order references an instruction outside the block",
                    ));
                };
                if std::mem::replace(slot, rank) != usize::MAX {
                    return Err(SpillPlanError::new(
                        "SPILL_PLAN.SCHEDULE_ORDER",
                        Some(func.blocks[block].id),
                        Some(source),
                        Vec::new(),
                        "explicit order references one instruction more than once",
                    ));
                }
            }
            ranks
        } else {
            (0..instructions).collect::<Vec<_>>()
        };
        let mut remaining = HashMap::<LogicalValue, RemainingUses>::default();
        let mut point_count = 0;
        for (source, inst) in func.blocks[block].insts.iter().enumerate() {
            for value in inst.uses().sorted_unique() {
                let value = logical.checked_of(value, Some(func.blocks[block].id), Some(source))?;
                remaining.entry(value).or_default().count += 1;
                point_count += 1;
            }
        }
        // Count, reserve disjoint ranges, then fill the block-local arena.
        // Range placement is independent of scheduling; each range keeps the
        // exact (preferred rank, source instruction) ordering used previously.
        let mut points = vec![(0, 0); point_count];
        let mut start = 0;
        for uses in remaining.values_mut() {
            uses.points = start..start + uses.count;
            start = uses.points.end;
        }
        for (source, inst) in func.blocks[block].insts.iter().enumerate() {
            for value in inst.uses().sorted_unique() {
                let value = logical.checked_of(value, Some(func.blocks[block].id), Some(source))?;
                let uses = remaining.get_mut(&value).expect("counted use");
                points[uses.points.start + uses.next] = (preferred_rank[source], source);
                uses.next += 1;
            }
        }
        for uses in remaining.values_mut() {
            points[uses.points.clone()].sort_unstable();
            uses.next = 0;
        }
        if let Some(value) = next_use.exit[block].first_out_of_range(logical.count) {
            logical.checked_of(value, Some(func.blocks[block].id), Some(instructions))?;
        }
        Ok(Self {
            block: func.blocks[block].id,
            preferred_rank,
            remaining,
            points,
            exit: &next_use.exit[block],
            exit_reload_costs,
            emitted: vec![false; instructions],
            emitted_count: 0,
        })
    }

    pub(super) fn emit(
        &mut self,
        source: usize,
        inst: &MInst,
        logical: &LogicalValues,
    ) -> Result<Vec<LogicalValue>, SpillPlanError> {
        if source >= self.emitted.len() || std::mem::replace(&mut self.emitted[source], true) {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.SCHEDULE_ORDER",
                Some(self.block),
                Some(source),
                Vec::new(),
                "a source instruction was committed more than once",
            ));
        }
        self.emitted_count += 1;
        let uses = inst.uses().sorted_unique();
        let mut changed = Vec::with_capacity(uses.len());
        for value in uses {
            let value = logical.checked_of(value, Some(self.block), Some(source))?;
            let uses = self.remaining.get_mut(&value).ok_or_else(|| {
                SpillPlanError::new(
                    "SPILL_PLAN.SCHEDULE_ORDER",
                    Some(self.block),
                    Some(source),
                    vec![VReg(value.0)],
                    "committed use has no remaining-use entry",
                )
            })?;
            let point = (self.preferred_rank[source], source);
            let points = &self.points[uses.points.clone()];
            if points.binary_search(&point).is_err() || uses.count == 0 {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.SCHEDULE_ORDER",
                    Some(self.block),
                    Some(source),
                    vec![VReg(value.0)],
                    "committed use was absent from its remaining-use set",
                ));
            }
            uses.count -= 1;
            while points
                .get(uses.next)
                .is_some_and(|&(_, instruction)| self.emitted[instruction])
            {
                uses.next += 1;
            }
            // candidate_score only observes use counts through last-use
            // pressure (and whether a definition is live). Keep the full
            // use/next-use state current above, but do not invalidate every
            // ready consumer on decrements such as 100 -> 99. With F users,
            // those notifications otherwise cause F^2 use-list visits.
            // Live-out values never die within this block.
            if uses.count <= 1 && !self.exit.contains_key(&VReg(value.0)) {
                changed.push(value);
            }
        }
        Ok(changed)
    }

    pub(super) fn remaining_uses(&self, value: LogicalValue) -> usize {
        self.remaining.get(&value).map_or(0, |uses| uses.count)
    }

    pub(super) fn is_live_out(&self, value: LogicalValue) -> bool {
        self.exit.contains_key(&VReg(value.0))
    }

    pub(super) fn distance(&self, value: LogicalValue) -> NextUseDistance {
        if let Some((rank, _)) = self
            .remaining
            .get(&value)
            .and_then(|uses| self.points[uses.points.clone()].get(uses.next))
            .copied()
        {
            return NextUseDistance::Finite {
                loop_exits: 0,
                instructions: rank.saturating_sub(self.emitted_count),
            };
        }
        let remaining_instructions = self.emitted.len().saturating_sub(self.emitted_count);
        match self.exit.get(&VReg(value.0)) {
            Some(NextUseDistance::Finite {
                loop_exits,
                instructions,
            }) => NextUseDistance::Finite {
                loop_exits,
                instructions: instructions.saturating_add(remaining_instructions),
            },
            _ => NextUseDistance::Dead,
        }
    }

    pub(super) fn next_point(&self, value: LogicalValue) -> Option<PointUse> {
        let uses = self.remaining.get(&value)?;
        let &(_, instruction) = self.points[uses.points.clone()].get(uses.next)?;
        Some(PointUse {
            block: self.block,
            instruction,
            value: VReg(value.0),
        })
    }

    pub(super) fn exit_reload_cost(&mut self, value: LogicalValue) -> u32 {
        self.exit_reload_costs.cost(value)
    }
}

pub(super) struct DynamicFutureUses<'view, 'data>(pub(super) &'view mut RemainingBlockUses<'data>);

impl FutureUses for DynamicFutureUses<'_, '_> {
    fn distance(&self, value: LogicalValue) -> NextUseDistance {
        self.0.distance(value)
    }

    fn next_point(&self, value: LogicalValue) -> Option<PointUse> {
        self.0.next_point(value)
    }

    fn exit_reload_cost(&mut self, value: LogicalValue) -> u32 {
        self.0.exit_reload_cost(value)
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn limit(
    func: &MFunction,
    planning_recipes: &PlanningRecipes,
    homes: &SpillHomes,
    point_ops: &mut Vec<(ProgramPoint, PlannedOp)>,
    block: usize,
    point_instruction: usize,
    maximum: usize,
    pinned: &[LogicalValue],
    resident: &mut LogicalSet,
    spilled: &mut LogicalSet,
    deferred_recipe_reloads: &mut BTreeMap<LogicalValue, PointUse>,
    future_uses: &mut impl FutureUses,
) -> Result<(), SpillPlanError> {
    while resident.len() > maximum {
        let Some(victim) = resident
            .iter()
            .copied()
            .filter(|value| !pinned.contains(value))
            .max_by(|left, right| {
                compare_eviction_candidates(
                    func,
                    planning_recipes,
                    spilled,
                    future_uses,
                    (*left, future_uses.distance(*left)),
                    (*right, future_uses.distance(*right)),
                )
            })
        else {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.OPERAND_PRESSURE",
                Some(func.blocks[block].id),
                Some(point_instruction),
                pinned.iter().map(|value| VReg(value.0)).collect(),
                format!(
                    "{} simultaneously pinned operands exceed the {maximum}-register capacity",
                    pinned.len()
                ),
            ));
        };
        evict_value(
            func,
            planning_recipes,
            homes,
            point_ops,
            block,
            point_instruction,
            victim,
            spilled,
            deferred_recipe_reloads,
            future_uses,
        )?;
        resident.remove(&victim);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn evict_value(
    func: &MFunction,
    planning_recipes: &PlanningRecipes,
    homes: &SpillHomes,
    point_ops: &mut Vec<(ProgramPoint, PlannedOp)>,
    block: usize,
    point_instruction: usize,
    value: LogicalValue,
    spilled: &mut LogicalSet,
    deferred_recipe_reloads: &mut BTreeMap<LogicalValue, PointUse>,
    future_uses: &mut impl FutureUses,
) -> Result<(), SpillPlanError> {
    if spilled.contains(&value) {
        return Ok(());
    }
    if let Some((point, recipe_cost)) = next_use_point_recipe(planning_recipes, value, future_uses)
    {
        let stack_cost =
            spill_cost(func, value).saturating_add(reload_cost(func, planning_recipes, value));
        if recipe_cost < stack_cost {
            if let Some(previous) = deferred_recipe_reloads.insert(value, point) {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.RECIPE_RELOAD_POINT",
                    Some(func.blocks[block].id),
                    Some(point_instruction),
                    vec![VReg(value.0)],
                    format!("logical value already had deferred recipe reload {previous:?}"),
                ));
            }
            return Ok(());
        }
    }
    spilled.insert(value);
    point_ops.push((
        ProgramPoint {
            block: func.blocks[block].id,
            instruction: point_instruction,
            side: PointSide::Before,
        },
        PlannedOp::Spill {
            value,
            home: homes.of_logical(value),
        },
    ));
    Ok(())
}

/// Return the exact MemorySSA recipe at the next local use.
///
/// One eviction-to-reload interval is an independent residency cluster.  The
/// value does not need a persistent stack home merely because it has another
/// use after that reload: once reloaded it is resident again, and a later
/// eviction makes a fresh home decision against the MemorySSA version at that
/// later use.
pub(super) fn next_use_point_recipe(
    planning_recipes: &PlanningRecipes,
    value: LogicalValue,
    future_uses: &mut impl FutureUses,
) -> Option<(PointUse, u16)> {
    let point = future_uses.next_point(value)?;
    planning_recipes
        .point_specific_materialization_cost(point)
        .map(|cost| (point, cost))
}

/// Compare two possible split points by the cost density of keeping the value
/// resident until its next use.  Braun--Hack MIN is the equal-cost special
/// case: with equal spill/reload costs, the farther next use is still evicted.
/// For target values with different rematerialization and memory costs, the
/// numerator is the machine-instruction cost avoided by retaining the value,
/// and the denominator is the register occupancy until that use.
pub(super) fn compare_eviction_candidates(
    func: &MFunction,
    planning_recipes: &PlanningRecipes,
    spilled: &LogicalSet,
    future_uses: &mut impl FutureUses,
    left: (LogicalValue, NextUseDistance),
    right: (LogicalValue, NextUseDistance),
) -> Ordering {
    match (left.1, right.1) {
        (NextUseDistance::Dead, NextUseDistance::Dead) => left.0.cmp(&right.0),
        (NextUseDistance::Dead, _) => Ordering::Greater,
        (_, NextUseDistance::Dead) => Ordering::Less,
        (
            NextUseDistance::Finite {
                loop_exits: left_exits,
                instructions: left_instructions,
            },
            NextUseDistance::Finite {
                loop_exits: right_exits,
                instructions: right_instructions,
            },
        ) => left_exits.cmp(&right_exits).then_with(|| {
            let left_cost =
                eviction_cost(func, planning_recipes, spilled, left.0, future_uses) as u128;
            let right_cost =
                eviction_cost(func, planning_recipes, spilled, right.0, future_uses) as u128;
            let left_span = left_instructions as u128 + 1;
            let right_span = right_instructions as u128 + 1;
            // Lower avoided-cost density is the better eviction candidate.
            // Cross multiplication keeps the decision deterministic and free
            // of floating-point rounding.
            (right_cost * left_span)
                .cmp(&(left_cost * right_span))
                .then_with(|| left_instructions.cmp(&right_instructions))
                .then_with(|| left.0.cmp(&right.0))
        }),
    }
}

pub(super) fn eviction_cost(
    func: &MFunction,
    planning_recipes: &PlanningRecipes,
    spilled: &LogicalSet,
    value: LogicalValue,
    future_uses: &mut impl FutureUses,
) -> u32 {
    let has_persistent_home = spilled.contains(&value);
    let local_reload =
        reload_cost_at_next_local_use(func, planning_recipes, value, future_uses).map(u32::from);
    let reload_cost = local_reload.unwrap_or_else(|| {
        let exit_cost = future_uses.exit_reload_cost(value);
        if exit_cost == 0 {
            u32::from(reload_cost(func, planning_recipes, value))
        } else {
            exit_cost
        }
    });
    let persistent_cost = reload_cost.saturating_add(if has_persistent_home {
        0
    } else {
        u32::from(spill_cost(func, value))
    });
    if has_persistent_home {
        return persistent_cost;
    }
    next_use_point_recipe(planning_recipes, value, future_uses)
        .map_or(persistent_cost, |(_, recipe_cost)| {
            persistent_cost.min(u32::from(recipe_cost))
        })
}

pub(super) fn reload_cost_at_next_local_use(
    func: &MFunction,
    planning_recipes: &PlanningRecipes,
    value: LogicalValue,
    future_uses: &mut impl FutureUses,
) -> Option<u16> {
    let costs = super::super::cost::MachineSpillCosts::with_recipes(func, planning_recipes);
    future_uses
        .next_point(value)
        .map(|point| costs.reload_at_point(point))
}

/// Cost the complete guaranteed straight-line use cluster for a value which
/// is absent at block entry.  Point-specific MemorySSA recipes may differ
/// between uses, so every concrete point is queried independently.  If the
/// first guaranteed use lies beyond this block, retain the existing
/// cross-block fallback rather than assigning a zero cost.
pub(super) fn local_use_cluster_cost(
    func: &MFunction,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    block: usize,
    instruction: usize,
    value: LogicalValue,
) -> u128 {
    let value = VReg(value.0);
    let uses = next_use.local_uses_from(block, instruction, value);
    if uses.is_empty() {
        return u128::from(reload_cost(func, planning_recipes, LogicalValue(value.0)));
    }
    let costs = super::super::cost::MachineSpillCosts::with_recipes(func, planning_recipes);
    uses.iter().fold(0u128, |cost, &instruction| {
        let reload = costs.reload_at_point(PointUse {
            block: func.blocks[block].id,
            instruction,
            value,
        });
        cost.saturating_add(u128::from(reload))
    })
}

pub(super) fn reload_cost_on_edge(
    func: &MFunction,
    planning_recipes: &PlanningRecipes,
    predecessor: usize,
    successor: usize,
    value: LogicalValue,
) -> u16 {
    let value = VReg(value.0);
    super::super::cost::MachineSpillCosts::with_recipes(func, planning_recipes).reload_on_edge(
        EdgeUse {
            predecessor: func.blocks[predecessor].id,
            successor: func.blocks[successor].id,
            value,
        },
    )
}

pub(super) fn reload_cost(
    func: &MFunction,
    planning_recipes: &PlanningRecipes,
    value: LogicalValue,
) -> u16 {
    super::super::cost::MachineSpillCosts::with_recipes(func, planning_recipes)
        .persistent_reload(VReg(value.0))
}

pub(super) fn spill_cost(func: &MFunction, value: LogicalValue) -> u16 {
    super::super::cost::MachineSpillCosts::from_descriptors(func).spill(VReg(value.0))
}

pub(super) fn logical_entry_distance(
    func: &MFunction,
    next_use: &NextUseAnalysis,
    block: usize,
    value: LogicalValue,
) -> NextUseDistance {
    next_use.distance_at(func, block, 0, VReg(value.0))
}
