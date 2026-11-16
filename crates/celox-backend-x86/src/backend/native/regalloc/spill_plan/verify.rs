//! Recipe homes of a plan and its verification.

use super::*;

impl SpillPlan {
    /// Keep only states queried after scheduling. Reconstruction still needs
    /// W_entry for phi placement, but S_exit is only queried for phi sources.
    /// Full tables must be retained when spill-plan verification is enabled.
    pub(in super::super) fn retain_reconstruction_states(
        &mut self,
        func: &MFunction,
        cfg: &NormalizedCfg,
    ) -> Result<(), SpillPlanError> {
        let mut exits = vec![LogicalSet::new(); func.blocks.len()];
        for block in &func.blocks {
            for phi in &block.phis {
                for &(predecessor, source) in &phi.sources {
                    let previous = cfg.block_index.get(&predecessor).copied().ok_or_else(|| {
                        SpillPlanError::new(
                            "SPILL_PLAN.RECONSTRUCTION_STATE",
                            Some(block.id),
                            None,
                            vec![phi.dst, source],
                            "phi predecessor is outside the normalized CFG",
                        )
                    })?;
                    let state = self.s_exit.get(previous).ok_or_else(|| {
                        SpillPlanError::new(
                            "SPILL_PLAN.RECONSTRUCTION_STATE",
                            Some(predecessor),
                            None,
                            vec![source],
                            "phi predecessor has no spill exit state",
                        )
                    })?;
                    let source = self.logical.of(source);
                    if state.contains(&source) {
                        exits[previous].insert(source);
                    }
                }
            }
        }
        self.s_exit = exits;
        self.s_entry = Vec::new();
        self.w_exit = Vec::new();
        Ok(())
    }

    /// Finalize whole-home rematerialization after the W/S plan has exposed
    /// every concrete point and edge reload.  Reconstruction must materialize
    /// this decision; it may no longer infer a different home kind on its own.
    pub(in super::super) fn select_recipe_homes(
        &mut self,
        func: &MFunction,
        cfg: &NormalizedCfg,
        analysis: &ReloadRecipeAnalysis,
    ) -> Result<(), SpillPlanError> {
        let base_costs = super::super::cost::MachineSpillCosts::from_descriptors(func);
        let mut candidates = BTreeSet::<SpillHome>::new();
        let mut rejected = BTreeSet::<SpillHome>::new();
        let mut baseline_costs = BTreeMap::<SpillHome, u128>::new();
        let mut recipe_costs = BTreeMap::<SpillHome, u128>::new();
        for &(point, operation) in &self.point_ops {
            match operation {
                PlannedOp::Reload { value, home } => {
                    candidates.insert(home);
                    let query = PointUse {
                        block: point.block,
                        instruction: point.instruction,
                        value: VReg(value.0),
                    };
                    if let Some(recipe) = analysis.resolved_recipe_at_point(query) {
                        let cost = u128::try_from(recipe.steps.len().saturating_add(1))
                            .unwrap_or(u128::MAX);
                        let baseline = baseline_costs.entry(home).or_default();
                        *baseline = baseline.saturating_add(
                            if self.recipe_reloads.contains(&(
                                point.block,
                                point.instruction,
                                value,
                            )) {
                                cost
                            } else {
                                u128::from(base_costs.persistent_reload(VReg(value.0)))
                            },
                        );
                        let total = recipe_costs.entry(home).or_default();
                        *total = total.saturating_add(cost);
                    } else {
                        let baseline = baseline_costs.entry(home).or_default();
                        *baseline = baseline.saturating_add(u128::from(
                            base_costs.persistent_reload(VReg(value.0)),
                        ));
                        rejected.insert(home);
                    }
                }
                PlannedOp::Spill { value, home } => {
                    let total = baseline_costs.entry(home).or_default();
                    *total = total.saturating_add(u128::from(spill_cost(func, value)));
                }
                PlannedOp::SpillPhi { value, home } => {
                    let Some(&block) = cfg.block_index.get(&point.block) else {
                        return Err(SpillPlanError::new(
                            "SPILL_PLAN.RECIPE_HOME_PHI",
                            Some(point.block),
                            Some(point.instruction),
                            vec![VReg(value.0)],
                            "recipe-home SpillPhi block is outside the normalized CFG",
                        ));
                    };
                    let Some(phi) = func.blocks[block]
                        .phis
                        .iter()
                        .find(|phi| phi.dst.0 == value.0)
                    else {
                        return Err(SpillPlanError::new(
                            "SPILL_PLAN.RECIPE_HOME_PHI",
                            Some(point.block),
                            Some(point.instruction),
                            vec![VReg(value.0)],
                            "recipe-home SpillPhi has no matching MIR phi",
                        ));
                    };
                    for &(predecessor, source) in &phi.sources {
                        let Some(&predecessor) = cfg.block_index.get(&predecessor) else {
                            return Err(SpillPlanError::new(
                                "SPILL_PLAN.RECIPE_HOME_PHI",
                                Some(point.block),
                                Some(point.instruction),
                                vec![VReg(value.0), source],
                                "recipe-home SpillPhi source is outside the normalized CFG",
                            ));
                        };
                        let source = LogicalValue(source.0);
                        if !self.s_exit[predecessor].contains(&source) {
                            let total = baseline_costs.entry(home).or_default();
                            *total = total.saturating_add(u128::from(spill_cost(func, source)));
                        }
                    }
                }
            }
        }
        for (&(predecessor, successor), operations) in &self.edge_ops {
            let Some(predecessor_block) = func.blocks.get(predecessor) else {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.RECIPE_HOME_EDGE",
                    None,
                    None,
                    Vec::new(),
                    format!("recipe-home predecessor index {predecessor} is outside function"),
                ));
            };
            if func.blocks.get(successor).is_none() {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.RECIPE_HOME_EDGE",
                    Some(predecessor_block.id),
                    None,
                    Vec::new(),
                    format!("recipe-home successor index {successor} is outside function"),
                ));
            }
            let insertion =
                super::super::cfg::edge_insertion_point(func, cfg, predecessor, successor)
                    .ok_or_else(|| {
                        SpillPlanError::new(
                            "SPILL_PLAN.RECIPE_HOME_EDGE",
                            Some(predecessor_block.id),
                            None,
                            Vec::new(),
                            "recipe-home edge has no single-edge materialization point",
                        )
                    })?;
            let insertion_block = &func.blocks[insertion.block];
            if insertion.instruction >= insertion_block.insts.len() {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.RECIPE_HOME_EDGE",
                    Some(insertion_block.id),
                    Some(insertion.instruction),
                    Vec::new(),
                    "recipe-home edge insertion point is outside its MIR block",
                ));
            }
            for &operation in operations {
                match operation {
                    PlannedEdgeOp::Reload {
                        source,
                        source_home,
                        ..
                    } => {
                        candidates.insert(source_home);
                        let total = baseline_costs.entry(source_home).or_default();
                        *total = total.saturating_add(u128::from(
                            base_costs.persistent_reload(VReg(source.0)),
                        ));
                        let query = PointUse {
                            block: insertion_block.id,
                            instruction: insertion.instruction,
                            value: VReg(source.0),
                        };
                        if let Some(recipe) = analysis.resolved_recipe_at_point(query) {
                            let cost = u128::try_from(recipe.steps.len().saturating_add(1))
                                .unwrap_or(u128::MAX);
                            let total = recipe_costs.entry(source_home).or_default();
                            *total = total.saturating_add(cost);
                        } else {
                            rejected.insert(source_home);
                        }
                    }
                    PlannedEdgeOp::Spill {
                        source,
                        destination_home,
                        ..
                    } => {
                        let total = baseline_costs.entry(destination_home).or_default();
                        *total = total.saturating_add(u128::from(spill_cost(func, source)));
                    }
                }
            }
        }
        candidates.retain(|home| {
            !self.state_homes.contains_key(home)
                && !rejected.contains(home)
                && recipe_costs.get(home).copied().unwrap_or(u128::MAX)
                    < baseline_costs.get(home).copied().unwrap_or_default()
        });
        self.recipe_homes = candidates;
        Ok(())
    }

    /// Independently prove that every reload assigned to a recipe-only home
    /// has an exact recipe at its final insertion point.  This verifier does
    /// not trust the candidate/rejection sets used by selection.
    pub(in super::super) fn verify_recipe_homes(
        &self,
        func: &MFunction,
        cfg: &NormalizedCfg,
        analysis: &ReloadRecipeAnalysis,
    ) -> Result<(), SpillPlanError> {
        for &home in &self.recipe_homes {
            let mut reloads = 0usize;
            for &(point, operation) in &self.point_ops {
                let PlannedOp::Reload {
                    value,
                    home: reload_home,
                } = operation
                else {
                    continue;
                };
                if reload_home != home {
                    continue;
                }
                reloads += 1;
                let query = PointUse {
                    block: point.block,
                    instruction: point.instruction,
                    value: VReg(value.0),
                };
                if analysis.resolved_recipe_at_point(query).is_none() {
                    return Err(SpillPlanError::new(
                        "SPILL_PLAN.RECIPE_HOME_POINT",
                        Some(point.block),
                        Some(point.instruction),
                        vec![VReg(value.0)],
                        format!("recipe home {home:?} has a point reload without an exact recipe"),
                    ));
                }
            }
            for (&(predecessor, successor), operations) in &self.edge_ops {
                let Some(predecessor_block) = func.blocks.get(predecessor) else {
                    return Err(SpillPlanError::new(
                        "SPILL_PLAN.RECIPE_HOME_EDGE",
                        None,
                        None,
                        Vec::new(),
                        format!("recipe home {home:?} references absent predecessor {predecessor}"),
                    ));
                };
                let Some(successor_block) = func.blocks.get(successor) else {
                    return Err(SpillPlanError::new(
                        "SPILL_PLAN.RECIPE_HOME_EDGE",
                        Some(predecessor_block.id),
                        None,
                        Vec::new(),
                        format!("recipe home {home:?} references absent successor {successor}"),
                    ));
                };
                let insertion =
                    super::super::cfg::edge_insertion_point(func, cfg, predecessor, successor)
                        .ok_or_else(|| {
                            SpillPlanError::new(
                                "SPILL_PLAN.RECIPE_HOME_EDGE",
                                Some(predecessor_block.id),
                                None,
                                Vec::new(),
                                "recipe-home edge has no single-edge materialization point",
                            )
                        })?;
                let insertion_block = &func.blocks[insertion.block];
                for &operation in operations {
                    let PlannedEdgeOp::Reload {
                        source,
                        source_home: reload_home,
                        ..
                    } = operation
                    else {
                        continue;
                    };
                    if reload_home != home {
                        continue;
                    }
                    reloads += 1;
                    let query = PointUse {
                        block: insertion_block.id,
                        instruction: insertion.instruction,
                        value: VReg(source.0),
                    };
                    if analysis.resolved_recipe_at_point(query).is_none() {
                        return Err(SpillPlanError::new(
                            "SPILL_PLAN.RECIPE_HOME_EDGE",
                            Some(predecessor_block.id),
                            None,
                            vec![VReg(source.0)],
                            format!(
                                "recipe home {home:?} has no exact recipe on edge {} -> {}",
                                predecessor_block.id, successor_block.id
                            ),
                        ));
                    }
                }
            }
            if reloads == 0 {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.RECIPE_HOME_RELOAD",
                    None,
                    None,
                    Vec::new(),
                    format!("recipe home {home:?} has no selected reload"),
                ));
            }
        }
        Ok(())
    }

    pub(in super::super) fn verify(
        &self,
        func: &MFunction,
        cfg: &NormalizedCfg,
        registers: usize,
    ) -> Result<(), SpillPlanError> {
        let block_count = func.blocks.len();
        if self.w_entry.len() != block_count
            || self.w_exit.len() != block_count
            || self.s_entry.len() != block_count
            || self.s_exit.len() != block_count
        {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.STATE_SHAPE",
                None,
                None,
                Vec::new(),
                format!(
                    "spill-plan state tables must all contain {block_count} rows (W_entry={}, W_exit={}, S_entry={}, S_exit={})",
                    self.w_entry.len(),
                    self.w_exit.len(),
                    self.s_entry.len(),
                    self.s_exit.len()
                ),
            ));
        }
        if self.logical.count != func.vregs.count() || self.homes.count != self.logical.count {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.STATE_SHAPE",
                None,
                None,
                Vec::new(),
                format!(
                    "spill-plan value tables cover {} logical values and {} homes, but the function has {} virtual registers",
                    self.logical.count,
                    self.homes.count,
                    func.vregs.count()
                ),
            ));
        }

        for (block, state) in self.w_entry.iter().enumerate() {
            if state.len() > registers {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.PRESSURE",
                    Some(func.blocks[block].id),
                    Some(0),
                    state.iter().map(|value| VReg(value.0)).collect(),
                    format!(
                        "W_entry contains {} residents but only {registers} registers are available",
                        state.len()
                    ),
                ));
            }
        }
        for (block, state) in self.w_exit.iter().enumerate() {
            if state.len() > registers {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.PRESSURE",
                    Some(func.blocks[block].id),
                    Some(func.blocks[block].insts.len()),
                    state.iter().map(|value| VReg(value.0)).collect(),
                    format!(
                        "W_exit contains {} residents but only {registers} registers are available",
                        state.len()
                    ),
                ));
            }
        }

        for (block, states) in (0..block_count).map(|block| {
            (
                block,
                [
                    &self.w_entry[block],
                    &self.w_exit[block],
                    &self.s_entry[block],
                    &self.s_exit[block],
                ],
            )
        }) {
            for state in states {
                if let Some(value) = state.iter().find(|value| value.0 >= self.logical.count) {
                    return Err(SpillPlanError::new(
                        "SPILL_PLAN.VALUE_RANGE",
                        Some(func.blocks[block].id),
                        None,
                        vec![VReg(value.0)],
                        format!(
                            "spill-plan state references logical value {} but the plan contains {} values",
                            value.0, self.logical.count
                        ),
                    ));
                }
            }
        }

        for (&(predecessor, successor), operations) in &self.edge_ops {
            let Some(predecessor_block) = func.blocks.get(predecessor) else {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.EDGE_EXISTS",
                    None,
                    None,
                    Vec::new(),
                    format!("edge operation predecessor index {predecessor} is out of range"),
                ));
            };
            let Some(successor_block) = func.blocks.get(successor) else {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.EDGE_EXISTS",
                    Some(predecessor_block.id),
                    None,
                    Vec::new(),
                    format!("edge operation successor index {successor} is out of range"),
                ));
            };
            if !cfg.successors[predecessor].contains(&successor) {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.EDGE_EXISTS",
                    Some(predecessor_block.id),
                    None,
                    Vec::new(),
                    format!(
                        "planned edge operation targets {}, which is not a CFG successor",
                        successor_block.id
                    ),
                ));
            }
            if super::super::cfg::edge_insertion_point(func, cfg, predecessor, successor).is_none()
            {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.EDGE_ISOLATED",
                    Some(predecessor_block.id),
                    None,
                    Vec::new(),
                    "edge operation has no single-edge materialization point",
                ));
            }
            if operations.is_empty() {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.EDGE_EXISTS",
                    Some(predecessor_block.id),
                    None,
                    Vec::new(),
                    format!(
                        "edge-operation list for {} -> {} is empty",
                        predecessor_block.id, successor_block.id
                    ),
                ));
            }
            for (index, &operation) in operations.iter().enumerate() {
                self.verify_edge_operation(
                    operation,
                    Some(predecessor_block.id),
                    edge_reload_uses_transferred_home(operations, index),
                )?;
            }
        }
        for &(point, operation) in &self.point_ops {
            let Some(&block) = cfg.block_index.get(&point.block) else {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.POINT_RANGE",
                    Some(point.block),
                    Some(point.instruction),
                    Vec::new(),
                    "planned operation references a block absent from the normalized CFG",
                ));
            };
            if point.instruction > func.blocks[block].insts.len() {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.POINT_RANGE",
                    Some(point.block),
                    Some(point.instruction),
                    Vec::new(),
                    format!(
                        "planned operation is outside the block's {} instructions",
                        func.blocks[block].insts.len()
                    ),
                ));
            }
            self.verify_operation(operation, Some(point.block), Some(point.instruction))?;
        }
        for &(block, instruction, value) in &self.recipe_reloads {
            let matching_reload = self.point_ops.iter().any(|(point, operation)| {
                point.block == block
                    && point.instruction == instruction
                    && matches!(operation, PlannedOp::Reload { value: reload, .. } if *reload == value)
            });
            if !matching_reload {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.RECIPE_RELOAD_POINT",
                    Some(block),
                    Some(instruction),
                    vec![VReg(value.0)],
                    "recipe-reload annotation has no matching point reload",
                ));
            }
        }
        for &home in &self.recipe_homes {
            if home.0 >= self.logical.count {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.RECIPE_HOME_RANGE",
                    None,
                    None,
                    Vec::new(),
                    format!(
                        "recipe home {} is outside the plan's {} logical values",
                        home.0, self.logical.count
                    ),
                ));
            }
            let has_reload = self
                .point_ops
                .iter()
                .any(|(_, operation)| {
                    matches!(operation, PlannedOp::Reload { home: reload_home, .. } if *reload_home == home)
                })
                || self.edge_ops.values().flatten().any(|operation| {
                    matches!(operation, PlannedEdgeOp::Reload { source_home, .. } if *source_home == home)
                });
            if !has_reload {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.RECIPE_HOME_RELOAD",
                    None,
                    None,
                    Vec::new(),
                    format!("recipe home {home:?} has no selected reload"),
                ));
            }
        }
        Ok(())
    }

    pub(super) fn verify_operation(
        &self,
        operation: PlannedOp,
        block: Option<BlockId>,
        instruction: Option<usize>,
    ) -> Result<(), SpillPlanError> {
        let (value, home) = match operation {
            PlannedOp::Spill { value, home }
            | PlannedOp::Reload { value, home }
            | PlannedOp::SpillPhi { value, home } => (value, home),
        };
        if value.0 >= self.logical.count {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.VALUE_RANGE",
                block,
                instruction,
                vec![VReg(value.0)],
                format!(
                    "planned operation references logical value {} but the plan contains {} values",
                    value.0, self.logical.count
                ),
            ));
        }
        let expected = self.homes.of_logical(value);
        if home != expected {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.HOME",
                block,
                instruction,
                vec![VReg(value.0)],
                format!(
                    "planned operation uses spill home {} but logical value {} belongs to home {}",
                    home.0, value.0, expected.0
                ),
            ));
        }
        Ok(())
    }

    pub(super) fn verify_edge_operation(
        &self,
        operation: PlannedEdgeOp,
        block: Option<BlockId>,
        transferred_home: bool,
    ) -> Result<(), SpillPlanError> {
        let (source, destination, home, expected_home) = match operation {
            PlannedEdgeOp::Reload {
                source,
                source_home,
                destination,
            } => (
                source,
                destination,
                source_home,
                self.homes.of_logical(source),
            ),
            PlannedEdgeOp::Spill {
                source,
                destination,
                destination_home,
            } => (
                source,
                destination,
                destination_home,
                self.homes.of_logical(destination),
            ),
        };
        for value in [source, destination] {
            if value.0 >= self.logical.count {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.VALUE_RANGE",
                    block,
                    None,
                    vec![VReg(value.0)],
                    format!(
                        "planned edge operation references logical value {} but the plan contains {} values",
                        value.0, self.logical.count
                    ),
                ));
            }
        }
        if home != expected_home && !transferred_home {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.HOME_CLASS",
                block,
                None,
                vec![VReg(source.0), VReg(destination.0)],
                format!("planned edge operation names home {home:?}, expected {expected_home:?}"),
            ));
        }
        Ok(())
    }
}
