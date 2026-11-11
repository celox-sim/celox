use super::*;
use crate::native::mir::{BlockId, MBlock, VRegAllocator};
use crate::native::regalloc::reload::PureStep;

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
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 0,
        src: output,
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    func.push_block(block);

    let mut recipe_reloads = Vec::new();
    assert_eq!(
        eliminate_dead_definitions(&mut func, &mut recipe_reloads),
        BTreeSet::from([dead_left, dead_right])
    );
    assert_eq!(func.blocks[0].phis.len(), 1);
    assert_eq!(func.blocks[0].phis[0].dst, live);
}

#[test]
fn removes_unused_original_and_planned_definitions() {
    let mut vregs = VRegAllocator::new();
    let original_dead = vregs.alloc();
    let dead = vregs.alloc();
    let live = vregs.alloc();
    let output = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: original_dead,
        base: BaseReg::SimState,
        offset: 0,
        size: OpSize::S64,
    });
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
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 8,
        src: output,
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    func.push_block(block);

    let mut recipe_reloads = vec![
        ExpectedMaterializedReload {
            reload: dead,
            expected: ResolvedRecipe {
                base: ResolvedBase::Constant(0),
                steps: Vec::new(),
            },
            planned_use: None,
        },
        ExpectedMaterializedReload {
            reload: live,
            expected: ResolvedRecipe {
                base: ResolvedBase::Constant(1),
                steps: Vec::new(),
            },
            planned_use: None,
        },
    ];
    assert_eq!(
        eliminate_dead_definitions(&mut func, &mut recipe_reloads),
        BTreeSet::from([original_dead, dead])
    );
    assert_eq!(func.blocks[0].insts.len(), 4);
    assert_eq!(func.blocks[0].insts[0].def(), Some(live));
    assert_eq!(recipe_reloads.len(), 1);
    assert_eq!(recipe_reloads[0].reload, live);
}

#[test]
fn fresh_representative_tracks_the_logical_value() {
    let mut vregs = VRegAllocator::new();
    let original = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient()]);
    let mut logical_for_vreg = vec![LogicalValue(original.0)];

    let fresh = alloc_fresh(&mut func, &mut logical_for_vreg, LogicalValue(original.0)).unwrap();

    assert_eq!(logical_for_vreg[fresh.0 as usize], LogicalValue(original.0));
}

#[test]
fn recipe_materialization_preserves_the_planned_pressure_bound() {
    let mut vregs = VRegAllocator::new();
    let residents = (0..13).map(|_| vregs.alloc()).collect::<Vec<_>>();
    let first_logical = vregs.alloc();
    let second_logical = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 15]);
    let mut logical_for_vreg = (0..15).map(LogicalValue).collect::<Vec<_>>();
    let recipe = |immediate| ResolvedRecipe {
        base: ResolvedBase::Constant(0x1234_5678),
        steps: vec![PureStep::CmpImm64 {
            immediate,
            kind: crate::native::mir::CmpKind::Eq,
        }],
    };
    let (first_result, first) = prepare_recipe(
        &mut func,
        &mut logical_for_vreg,
        LogicalValue(first_logical.0),
        recipe(1),
    )
    .unwrap();
    let (second_result, second) = prepare_recipe(
        &mut func,
        &mut logical_for_vreg,
        LogicalValue(second_logical.0),
        recipe(2),
    )
    .unwrap();

    assert_eq!(first.instructions.len(), 2);
    assert_eq!(second.instructions.len(), 2);
    let mut block = MBlock::new(BlockId(0));
    for (value, destination) in residents.iter().copied().enumerate() {
        block.push(MInst::LoadImm {
            dst: destination,
            value: value as u64,
        });
    }
    block.insts.extend(first.instructions);
    block.insts.extend(second.instructions);
    for (offset, result) in [first_result, second_result].into_iter().enumerate() {
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: offset as i32 * 8,
            src: result,
            size: OpSize::S64,
        });
    }
    for (offset, resident) in residents.into_iter().enumerate() {
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 16 + offset as i32 * 8,
            src: resident,
            size: OpSize::S64,
        });
    }
    block.push(MInst::Return);
    func.push_block(block);

    let analysis = super::super::analysis::analyze(&func);
    super::super::pressure::verify(&func, &analysis, 15).unwrap();
}

#[test]
fn leading_state_home_spill_is_a_recipe_hazard() {
    let first = LogicalValue(0);
    let second = LogicalValue(1);
    let first_home = SpillHome(0);
    let operations = [
        PlannedEdgeOp::Spill {
            source: first,
            destination: first,
            destination_home: first_home,
        },
        PlannedEdgeOp::Reload {
            source: second,
            source_home: SpillHome(2),
            destination: second,
        },
        PlannedEdgeOp::Spill {
            source: second,
            destination: second,
            destination_home: SpillHome(1),
        },
    ];
    let load = super::super::reload::StateLoad {
        offset: 16,
        size: OpSize::S64,
    };
    let base = ResolvedBase::State(super::super::reload::StateRecipe::live_on_entry_for_test(
        load,
    ));
    let recipes = [
        None,
        Some(ResolvedRecipe {
            base,
            steps: vec![PureStep::ShrImm64 { immediate: 1 }],
        }),
        None,
    ];
    let mut homes = std::collections::BTreeMap::new();
    homes.insert(
        first_home,
        crate::native::mir::PackedStateHome {
            id: crate::native::mir::StateHomeId(0),
            offset: 16,
            size: OpSize::S64,
            live_on_entry: false,
        },
    );

    assert_eq!(
        hazardous_state_home_writes(&operations, &recipes, &homes),
        BTreeSet::from([first_home])
    );
}

#[test]
fn shares_identical_edge_reload_tails_without_reloading_the_resident_edge() {
    let mut vregs = VRegAllocator::new();
    let original = vregs.alloc();
    let first_condition = vregs.alloc();
    let second_condition = vregs.alloc();
    let left_reload = vregs.alloc();
    let right_reload = vregs.alloc();
    let merged = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 6]);

    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::Load {
        dst: original,
        base: BaseReg::SimState,
        offset: 80,
        size: OpSize::S64,
    });
    entry.push(MInst::LoadImm {
        dst: first_condition,
        value: 1,
    });
    entry.push(MInst::Branch {
        cond: first_condition,
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });

    let mut left = MBlock::new(BlockId(1));
    left.push(MInst::Load {
        dst: left_reload,
        base: BaseReg::StackFrame,
        offset: 0,
        size: OpSize::S64,
    });
    left.push(MInst::Jump { target: BlockId(4) });

    let mut decision = MBlock::new(BlockId(2));
    decision.push(MInst::LoadImm {
        dst: second_condition,
        value: 1,
    });
    decision.push(MInst::Branch {
        cond: second_condition,
        true_bb: BlockId(3),
        false_bb: BlockId(5),
    });

    let mut right = MBlock::new(BlockId(3));
    right.push(MInst::Load {
        dst: right_reload,
        base: BaseReg::StackFrame,
        offset: 0,
        size: OpSize::S64,
    });
    right.push(MInst::Jump { target: BlockId(4) });

    let mut join = MBlock::new(BlockId(4));
    join.phis.push(PhiNode {
        dst: merged,
        sources: vec![
            (BlockId(1), left_reload),
            (BlockId(3), right_reload),
            (BlockId(5), original),
        ],
    });
    join.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 88,
        src: merged,
        size: OpSize::S64,
    });
    join.push(MInst::Return);

    let mut resident = MBlock::new(BlockId(5));
    resident.push(MInst::Jump { target: BlockId(4) });
    func.blocks = vec![entry, left, decision, right, join, resident];
    func.verify();

    let shape = EdgeReloadShape {
        source: LogicalValue(original.0),
        source_home: SpillHome(original.0),
        destination: LogicalValue(original.0),
        materialization: ReloadMaterialization::Stack,
    };
    let bundles = vec![
        EdgeReloadBundle {
            predecessor: 1,
            successor: 4,
            shape: vec![shape.clone()],
            final_definitions: vec![left_reload],
            definitions: vec![left_reload],
            instruction_count: 1,
        },
        EdgeReloadBundle {
            predecessor: 3,
            successor: 4,
            shape: vec![shape],
            final_definitions: vec![right_reload],
            definitions: vec![right_reload],
            instruction_count: 1,
        },
    ];

    let shared =
        share_identical_edge_reload_bundles(&mut func, &bundles, &mut Vec::new(), &mut Vec::new())
            .unwrap();

    assert_eq!(shared.len(), 1);
    assert_eq!(func.blocks.len(), 7);
    assert_eq!(
        func.blocks[4].phis[0].sources,
        vec![(BlockId(6), left_reload), (BlockId(5), original)]
    );
    assert_eq!(
        func.blocks
            .iter()
            .flat_map(|block| &block.insts)
            .filter(|instruction| matches!(
                instruction,
                MInst::Load {
                    base: BaseReg::StackFrame,
                    offset: 0,
                    size: OpSize::S64,
                    ..
                }
            ))
            .count(),
        1
    );
    assert_eq!(
        func.blocks[1].insts,
        vec![MInst::Jump { target: BlockId(6) }]
    );
    assert_eq!(
        func.blocks[3].insts,
        vec![MInst::Jump { target: BlockId(6) }]
    );
    func.verify();
    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    cfg.verify(&func).unwrap();
    func.verify();
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
    let fresh = alloc_fresh(&mut func, &mut logical_for_vreg, LogicalValue(original.0)).unwrap();
    let successor = cfg.block_index[&BlockId(1)];
    func.blocks[successor].phis.push(PhiNode {
        dst: fresh,
        sources: Vec::new(),
    });
    let reconstruction_phis = [((successor, LogicalValue(original.0)), fresh)]
        .into_iter()
        .collect();
    let mut children = vec![Vec::new(); func.blocks.len()];
    children[0].push(successor);
    let recipe_homes = BTreeSet::new();
    let mut recipe_reloads = Vec::new();
    let mut pending_state_stores = Vec::new();
    let mut state_reloads = Vec::new();

    let error = rename_block(
        0,
        &mut func,
        &cfg,
        &plan,
        &children,
        &reconstruction_phis,
        &HashMap::default(),
        &logical_for_vreg,
        &mut HashMap::default(),
        &mut HashMap::default(),
        &recipe_homes,
        &mut recipe_reloads,
        &mut pending_state_stores,
        &mut state_reloads,
    )
    .unwrap_err();

    assert_eq!(error.rule, "RECONSTRUCT.PHI_REPRESENTATIVE_EXISTS");
    assert_eq!(error.block, Some(BlockId(1)));
}

fn planner_recipe_fixture(overwrite_home: bool) -> MFunction {
    let mut vregs = VRegAllocator::new();
    let stored = vregs.alloc();
    let pressure = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: stored,
        base: BaseReg::StackFrame,
        offset: 0,
        size: OpSize::S64,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 80,
        src: stored,
        size: OpSize::S64,
    });
    block.push(MInst::Load {
        dst: pressure,
        base: BaseReg::StackFrame,
        offset: 8,
        size: OpSize::S64,
    });
    if overwrite_home {
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 80,
            src: pressure,
            size: OpSize::S64,
        });
    }
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 96,
        src: stored,
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    func.push_block(block);
    func
}

fn global_state_recipe_fixture(with_pure_step: bool) -> MFunction {
    let mut vregs = VRegAllocator::new();
    let state = vregs.alloc();
    let derived = with_pure_step.then(|| vregs.alloc());
    let pressure = vregs.alloc();
    let second_pressure = with_pure_step.then(|| vregs.alloc());
    let mut func = MFunction::new(
        vregs,
        vec![SpillDesc::transient(); if with_pure_step { 4 } else { 2 }],
    );
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: state,
        base: BaseReg::SimState,
        offset: 80,
        size: OpSize::S64,
    });
    if let Some(derived) = derived {
        block.push(MInst::ShrImm {
            dst: derived,
            src: state,
            imm: 3,
        });
    }
    block.push(MInst::Load {
        dst: pressure,
        base: BaseReg::StackFrame,
        offset: 0,
        size: OpSize::S64,
    });
    if let Some(second_pressure) = second_pressure {
        block.push(MInst::Load {
            dst: second_pressure,
            base: BaseReg::StackFrame,
            offset: 8,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 96,
            src: pressure,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 104,
            src: second_pressure,
            size: OpSize::S64,
        });
    }
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 112,
        src: derived.unwrap_or(state),
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    func.push_block(block);
    func
}

fn path_specific_final_recipe_fixture() -> MFunction {
    let mut vregs = VRegAllocator::new();
    let stored = vregs.alloc();
    let condition = vregs.alloc();
    let left_first = vregs.alloc();
    let left_second = vregs.alloc();
    let overwrite = vregs.alloc();
    let right_first = vregs.alloc();
    let right_second = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 7]);

    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::Load {
        dst: stored,
        base: BaseReg::StackFrame,
        offset: 0,
        size: OpSize::S64,
    });
    entry.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 80,
        src: stored,
        size: OpSize::S64,
    });
    entry.push(MInst::LoadImm {
        dst: condition,
        value: 1,
    });
    entry.push(MInst::Branch {
        cond: condition,
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });

    let mut left = MBlock::new(BlockId(1));
    left.push(MInst::Load {
        dst: left_first,
        base: BaseReg::StackFrame,
        offset: 8,
        size: OpSize::S64,
    });
    left.push(MInst::Load {
        dst: left_second,
        base: BaseReg::StackFrame,
        offset: 16,
        size: OpSize::S64,
    });
    left.push(MInst::Store {
        base: BaseReg::StackFrame,
        offset: 24,
        src: left_first,
        size: OpSize::S64,
    });
    left.push(MInst::Store {
        base: BaseReg::StackFrame,
        offset: 32,
        src: left_second,
        size: OpSize::S64,
    });
    left.push(MInst::Store {
        base: BaseReg::StackFrame,
        offset: 40,
        src: stored,
        size: OpSize::S64,
    });
    left.push(MInst::Jump { target: BlockId(3) });

    let mut right = MBlock::new(BlockId(2));
    right.push(MInst::Load {
        dst: overwrite,
        base: BaseReg::StackFrame,
        offset: 48,
        size: OpSize::S64,
    });
    right.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 80,
        src: overwrite,
        size: OpSize::S64,
    });
    right.push(MInst::Load {
        dst: right_first,
        base: BaseReg::StackFrame,
        offset: 56,
        size: OpSize::S64,
    });
    right.push(MInst::Load {
        dst: right_second,
        base: BaseReg::StackFrame,
        offset: 64,
        size: OpSize::S64,
    });
    right.push(MInst::Store {
        base: BaseReg::StackFrame,
        offset: 72,
        src: right_first,
        size: OpSize::S64,
    });
    right.push(MInst::Store {
        base: BaseReg::StackFrame,
        offset: 80,
        src: right_second,
        size: OpSize::S64,
    });
    right.push(MInst::Store {
        base: BaseReg::StackFrame,
        offset: 88,
        src: stored,
        size: OpSize::S64,
    });
    right.push(MInst::Jump { target: BlockId(3) });

    let mut join = MBlock::new(BlockId(3));
    join.push(MInst::Return);
    func.blocks = vec![entry, left, right, join];
    func
}

fn reconstruct_with_registers(
    mut func: MFunction,
    registers: usize,
) -> (MFunction, ReconstructionResult) {
    func.verify();
    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let planning_recipes = super::super::reload::analyze_for_planning(&func, &cfg).unwrap();
    let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
    next_use.verify(&func, &cfg).unwrap();
    let mut plan = super::super::spill_plan::plan_with_recipe_costs(
        &func,
        &cfg,
        &next_use,
        &planning_recipes,
        registers,
    )
    .unwrap();
    let requested_points = super::super::ssa::planner_reload_queries(&func, &cfg, &plan).unwrap();
    let recipes =
        super::super::reload::analyze_with_queries(&func, &cfg, &requested_points).unwrap();
    plan.select_recipe_homes(&func, &cfg, &recipes).unwrap();
    plan.verify(&func, &cfg, registers).unwrap();
    plan.verify_recipe_homes(&func, &cfg, &recipes).unwrap();
    super::super::home_verify::verify(&func, &cfg, &plan).unwrap();
    let mut projected = super::super::spill_plan::plan_with_recipe_costs(
        &func,
        &cfg,
        &next_use,
        &planning_recipes,
        registers,
    )
    .unwrap();
    projected.retain_reconstruction_states(&func, &cfg).unwrap();
    projected
        .select_recipe_homes(&func, &cfg, &recipes)
        .unwrap();
    assert_eq!(
        super::super::ssa_state_home::planned_spills(&func, &cfg, &plan).unwrap(),
        super::super::ssa_state_home::planned_spills(&func, &cfg, &projected).unwrap(),
    );
    assert_eq!(plan.recipe_homes, projected.recipe_homes);
    let mut projected_func = func.clone();
    let projected_result = reconstruct(
        &mut projected_func,
        &cfg,
        &projected,
        &recipes,
        false,
        false,
    )
    .unwrap();
    let result = reconstruct(&mut func, &cfg, &plan, &recipes, false, true).unwrap();
    assert_eq!(func.to_string(), projected_func.to_string());
    assert_eq!(result.frame_size, projected_result.frame_size);

    let rebuilt_cfg = (!result.shared_reload_blocks.is_empty())
        .then(|| super::super::cfg::normalize(&mut func).unwrap());
    let cfg = rebuilt_cfg.as_ref().unwrap_or(&cfg);
    super::super::reload::verify_expected_materialized_reloads_after_state_spills(
        &func,
        cfg,
        &result.recipe_reloads,
        &[],
        &result.shared_reload_blocks,
    )
    .unwrap();
    (func, result)
}

fn reconstruct_with_one_register(func: MFunction) -> (MFunction, ReconstructionResult) {
    reconstruct_with_registers(func, 1)
}

#[test]
fn planner_reload_uses_valid_state_home_without_a_stack_slot() {
    let (func, result) = reconstruct_with_one_register(planner_recipe_fixture(false));

    assert_eq!(result.frame_size, 0);
    assert_eq!(result.recipe_reloads.len(), 1);
    assert!(func.blocks[0].insts.iter().any(|inst| matches!(
        inst,
        MInst::Load {
            base: BaseReg::SimState,
            offset: 80,
            size: OpSize::S64,
            ..
        }
    )));
    assert!(func.blocks[0].insts.iter().all(|inst| !matches!(
        inst,
        MInst::Store {
            base: BaseReg::StackFrame,
            ..
        }
    )));
}

#[test]
fn planner_uses_recipe_on_final_path_and_stack_on_overwritten_path() {
    let mut func = path_specific_final_recipe_fixture();
    func.verify();
    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let planning_recipes = super::super::reload::analyze_for_planning(&func, &cfg).unwrap();
    let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
    next_use.verify(&func, &cfg).unwrap();
    let mut plan = super::super::spill_plan::plan_with_recipe_costs(
        &func,
        &cfg,
        &next_use,
        &planning_recipes,
        2,
    )
    .unwrap();
    let stored = LogicalValue(0);
    assert!(plan.recipe_reloads.contains(&(BlockId(1), 4, stored)));
    assert!(plan.point_ops.iter().all(|(point, operation)| {
        !(point.block == BlockId(1)
            && matches!(operation, PlannedOp::Spill { value, .. } if *value == stored))
    }));
    assert!(plan.point_ops.iter().all(|(point, operation)| {
        !(point.block == BlockId(2)
            && matches!(operation, PlannedOp::Spill { value, .. } if *value == stored))
    }));
    let right = cfg.block_index[&BlockId(2)];
    assert_eq!(cfg.predecessors[right].len(), 1);
    let right_edge = cfg.predecessors[right][0];
    assert_eq!(cfg.successors[right_edge], [right]);
    assert!(
        plan.edge_ops
            .get(&(right_edge, right))
            .is_some_and(|operations| operations.iter().any(|operation| {
                matches!(operation, PlannedEdgeOp::Spill { source, .. } if *source == stored)
            }))
    );

    let requested = super::super::ssa::planner_reload_queries(&func, &cfg, &plan).unwrap();
    let recipes = super::super::reload::analyze_with_queries(&func, &cfg, &requested).unwrap();
    plan.select_recipe_homes(&func, &cfg, &recipes).unwrap();
    plan.verify(&func, &cfg, 2).unwrap();
    plan.verify_recipe_homes(&func, &cfg, &recipes).unwrap();
    super::super::home_verify::verify(&func, &cfg, &plan).unwrap();
    let result = reconstruct(&mut func, &cfg, &plan, &recipes, false, true).unwrap();
    super::super::reload::verify_expected_materialized_reloads(&func, &cfg, &result.recipe_reloads)
        .unwrap();

    assert_eq!(result.frame_size, 8);
    assert_eq!(result.recipe_reloads.len(), 1);
}

#[test]
fn planner_uses_memory_phi_home_without_spilling_register_phi() {
    let mut vregs = VRegAllocator::new();
    let condition = vregs.alloc();
    let left_value = vregs.alloc();
    let right_value = vregs.alloc();
    let merged = vregs.alloc();
    let pressure = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 5]);

    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: condition,
        value: 1,
    });
    entry.push(MInst::Branch {
        cond: condition,
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });

    let mut left = MBlock::new(BlockId(1));
    left.push(MInst::Load {
        dst: left_value,
        base: BaseReg::StackFrame,
        offset: 0,
        size: OpSize::S64,
    });
    left.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 40,
        src: left_value,
        size: OpSize::S64,
    });
    left.push(MInst::Jump { target: BlockId(3) });

    let mut right = MBlock::new(BlockId(2));
    right.push(MInst::Load {
        dst: right_value,
        base: BaseReg::StackFrame,
        offset: 8,
        size: OpSize::S64,
    });
    right.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 40,
        src: right_value,
        size: OpSize::S64,
    });
    right.push(MInst::Jump { target: BlockId(3) });

    let mut join = MBlock::new(BlockId(3));
    join.phis.push(PhiNode {
        dst: merged,
        sources: vec![(BlockId(1), left_value), (BlockId(2), right_value)],
    });
    join.push(MInst::Load {
        dst: pressure,
        base: BaseReg::StackFrame,
        offset: 16,
        size: OpSize::S64,
    });
    join.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 80,
        src: pressure,
        size: OpSize::S64,
    });
    join.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 88,
        src: merged,
        size: OpSize::S64,
    });
    join.push(MInst::Return);
    func.blocks = vec![entry, left, right, join];

    let (func, result) = reconstruct_with_one_register(func);

    assert_eq!(result.frame_size, 0);
    assert!(!result.recipe_reloads.is_empty());
    assert!(
        func.blocks
            .iter()
            .flat_map(|block| &block.insts)
            .all(|inst| {
                !matches!(
                    inst,
                    MInst::Store {
                        base: BaseReg::StackFrame,
                        ..
                    }
                )
            })
    );
    assert!(
        func.blocks
            .iter()
            .flat_map(|block| &block.insts)
            .any(|inst| {
                matches!(
                    inst,
                    MInst::Load {
                        base: BaseReg::SimState,
                        offset: 40,
                        size: OpSize::S64,
                        ..
                    }
                )
            })
    );
}

#[test]
fn planner_reload_falls_back_to_stack_after_state_home_is_overwritten() {
    let (func, result) = reconstruct_with_one_register(planner_recipe_fixture(true));

    assert_eq!(result.frame_size, 8);
    assert!(result.recipe_reloads.is_empty());
    assert!(func.blocks[0].insts.iter().any(|inst| matches!(
        inst,
        MInst::Store {
            base: BaseReg::StackFrame,
            size: OpSize::S64,
            ..
        }
    )));
}

#[test]
fn planner_materializes_a_global_state_recipe_only_at_the_reload() {
    let (func, result) = reconstruct_with_one_register(global_state_recipe_fixture(false));

    assert_eq!(result.frame_size, 0);
    assert_eq!(result.recipe_reloads.len(), 1);
    assert_eq!(
        result.recipe_reloads[0].expected.steps,
        Vec::<PureStep>::new()
    );
    assert_eq!(
        func.blocks[0]
            .insts
            .iter()
            .filter(|inst| matches!(
                inst,
                MInst::Load {
                    base: BaseReg::SimState,
                    offset: 80,
                    size: OpSize::S64,
                    ..
                }
            ))
            .count(),
        1
    );
}

#[test]
fn planner_materializes_a_pure_recipe_with_exact_machine_width() {
    let (func, result) = reconstruct_with_registers(global_state_recipe_fixture(true), 2);

    assert_eq!(result.frame_size, 0);
    assert_eq!(result.recipe_reloads.len(), 1);
    assert_eq!(
        result.recipe_reloads[0].expected.steps,
        vec![PureStep::ShrImm64 { immediate: 3 }]
    );
    assert_eq!(
        func.blocks[0]
            .insts
            .iter()
            .filter(|inst| matches!(inst, MInst::ShrImm { imm: 3, .. }))
            .count(),
        1
    );
}

#[test]
fn planner_edge_reload_uses_the_predecessor_state_home() {
    let mut vregs = VRegAllocator::new();
    let stored = vregs.alloc();
    let condition = vregs.alloc();
    let pressure_left = vregs.alloc();
    let pressure_right = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::Load {
        dst: stored,
        base: BaseReg::StackFrame,
        offset: 0,
        size: OpSize::S64,
    });
    entry.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 80,
        src: stored,
        size: OpSize::S64,
    });
    entry.push(MInst::LoadImm {
        dst: condition,
        value: 1,
    });
    entry.push(MInst::Branch {
        cond: condition,
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });
    let mut left = MBlock::new(BlockId(1));
    left.push(MInst::Load {
        dst: pressure_left,
        base: BaseReg::StackFrame,
        offset: 8,
        size: OpSize::S64,
    });
    left.push(MInst::Load {
        dst: pressure_right,
        base: BaseReg::StackFrame,
        offset: 16,
        size: OpSize::S64,
    });
    left.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 96,
        src: pressure_left,
        size: OpSize::S64,
    });
    left.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 104,
        src: pressure_right,
        size: OpSize::S64,
    });
    left.push(MInst::Jump { target: BlockId(3) });
    let mut right = MBlock::new(BlockId(2));
    right.push(MInst::Jump { target: BlockId(3) });
    let mut join = MBlock::new(BlockId(3));
    join.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 112,
        src: stored,
        size: OpSize::S64,
    });
    join.push(MInst::Return);
    func.blocks = vec![entry, left, right, join];
    func.verify();

    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
    let mut plan = super::super::spill_plan::plan(&func, &cfg, &next_use, 2).unwrap();
    plan.verify(&func, &cfg, 2).unwrap();
    assert!(
        plan.edge_ops.values().flatten().any(|operation| matches!(
            operation,
            PlannedEdgeOp::Reload { source, .. } if *source == LogicalValue(stored.0)
        )),
        "{plan:#?}"
    );
    let requested = super::super::ssa::planner_reload_queries(&func, &cfg, &plan).unwrap();
    assert!(!requested.is_empty());
    let recipes = super::super::reload::analyze_with_queries(&func, &cfg, &requested).unwrap();
    plan.select_recipe_homes(&func, &cfg, &recipes).unwrap();
    plan.verify(&func, &cfg, 2).unwrap();
    plan.verify_recipe_homes(&func, &cfg, &recipes).unwrap();
    super::super::home_verify::verify(&func, &cfg, &plan).unwrap();
    let result = reconstruct(&mut func, &cfg, &plan, &recipes, false, true).unwrap();
    super::super::reload::verify_expected_materialized_reloads(&func, &cfg, &result.recipe_reloads)
        .unwrap();

    assert_eq!(result.frame_size, 0);
    assert!(!result.recipe_reloads.is_empty());
    assert!(
        func.blocks
            .iter()
            .flat_map(|block| &block.insts)
            .all(|inst| {
                !matches!(
                    inst,
                    MInst::Store {
                        base: BaseReg::StackFrame,
                        ..
                    }
                )
            })
    );
}

#[test]
fn planner_shares_matching_high_pressure_arm_reloads() {
    let mut vregs = VRegAllocator::new();
    let stored = vregs.alloc();
    let first_condition = vregs.alloc();
    let second_condition = vregs.alloc();
    let left_first = vregs.alloc();
    let left_second = vregs.alloc();
    let right_first = vregs.alloc();
    let right_second = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 7]);

    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::Load {
        dst: stored,
        base: BaseReg::SimState,
        offset: 80,
        size: OpSize::S64,
    });
    entry.push(MInst::LoadImm {
        dst: first_condition,
        value: 1,
    });
    entry.push(MInst::Branch {
        cond: first_condition,
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });

    let mut left = MBlock::new(BlockId(1));
    left.push(MInst::Load {
        dst: left_first,
        base: BaseReg::StackFrame,
        offset: 0,
        size: OpSize::S64,
    });
    left.push(MInst::Load {
        dst: left_second,
        base: BaseReg::StackFrame,
        offset: 8,
        size: OpSize::S64,
    });
    left.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 96,
        src: left_first,
        size: OpSize::S64,
    });
    left.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 104,
        src: left_second,
        size: OpSize::S64,
    });
    left.push(MInst::Jump { target: BlockId(4) });

    let mut decision = MBlock::new(BlockId(2));
    decision.push(MInst::LoadImm {
        dst: second_condition,
        value: 1,
    });
    decision.push(MInst::Branch {
        cond: second_condition,
        true_bb: BlockId(3),
        false_bb: BlockId(5),
    });

    let mut right = MBlock::new(BlockId(3));
    right.push(MInst::Load {
        dst: right_first,
        base: BaseReg::StackFrame,
        offset: 16,
        size: OpSize::S64,
    });
    right.push(MInst::Load {
        dst: right_second,
        base: BaseReg::StackFrame,
        offset: 24,
        size: OpSize::S64,
    });
    right.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 112,
        src: right_first,
        size: OpSize::S64,
    });
    right.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 120,
        src: right_second,
        size: OpSize::S64,
    });
    right.push(MInst::Jump { target: BlockId(4) });

    let mut join = MBlock::new(BlockId(4));
    join.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 128,
        src: stored,
        size: OpSize::S64,
    });
    join.push(MInst::Return);

    let mut resident = MBlock::new(BlockId(5));
    resident.push(MInst::Jump { target: BlockId(4) });
    func.blocks = vec![entry, left, decision, right, join, resident];

    let (func, result) = reconstruct_with_registers(func, 2);

    assert_eq!(result.shared_reload_blocks.len(), 1);
    assert_eq!(result.frame_size, 0);
    assert_eq!(
        func.blocks
            .iter()
            .flat_map(|block| &block.insts)
            .filter(|instruction| matches!(
                instruction,
                MInst::Load {
                    base: BaseReg::SimState,
                    offset: 80,
                    size: OpSize::S64,
                    ..
                }
            ))
            .count(),
        2,
        "one original resident-path load and one shared arm reload must remain"
    );
    assert!(func.blocks.iter().flat_map(|block| &block.phis).any(|phi| {
        phi.sources.len() == 2 && phi.sources.iter().any(|(_, source)| *source == stored)
    }));
}
