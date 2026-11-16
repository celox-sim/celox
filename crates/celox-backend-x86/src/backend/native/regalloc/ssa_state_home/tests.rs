use super::*;
use crate::native::mir::{MBlock, OpSize, SpillDesc, StateHomeId, VRegAllocator};
use crate::native::regalloc::{cfg, next_use, reconstruct, reload, spill_plan};

fn home(id: u32, offset: i32) -> PackedStateHome {
    PackedStateHome {
        id: StateHomeId(id),
        offset,
        size: OpSize::S64,
        live_on_entry: false,
    }
}

fn blank_plan(
    func: &MFunction,
    cfg: &NormalizedCfg,
    next_use: &super::super::next_use::NextUseAnalysis,
) -> SpillPlan {
    let mut plan = spill_plan::plan(func, cfg, next_use, 32).unwrap();
    plan.point_ops.clear();
    plan.edge_ops.clear();
    plan.recipe_reloads.clear();
    plan.recipe_homes.clear();
    plan.state_homes.clear();
    plan.state_reload_recipes.clear();
    plan
}

fn point(instruction: usize) -> super::super::spill_plan::ProgramPoint {
    super::super::spill_plan::ProgramPoint {
        block: BlockId(0),
        instruction,
        side: super::super::spill_plan::PointSide::Before,
    }
}

#[test]
fn branch_edge_operations_use_the_single_predecessor_successor_entry() {
    let mut vregs = VRegAllocator::new();
    let condition = vregs.alloc();
    let value = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);

    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: condition,
        value: 1,
    });
    entry.push(MInst::LoadImm {
        dst: value,
        value: 0x1234,
    });
    entry.push(MInst::Branch {
        cond: condition,
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });
    let mut left = MBlock::new(BlockId(1));
    left.push(MInst::Return);
    let mut right = MBlock::new(BlockId(2));
    right.push(MInst::Return);
    func.blocks = vec![entry, left, right];

    let cfg = cfg::normalize(&mut func).unwrap();
    let next_use = next_use::analyze(&func, &cfg).unwrap();
    let mut plan = blank_plan(&func, &cfg, &next_use);
    let entry = cfg.block_index[&BlockId(0)];
    let successor = cfg.successors[entry][0];
    assert_eq!(cfg.predecessors[successor], [entry]);

    let logical = plan.logical.of(value);
    let spill_home = plan.homes.of_vreg(value);
    plan.edge_ops.insert(
        (entry, successor),
        vec![
            PlannedEdgeOp::Spill {
                source: logical,
                destination: logical,
                destination_home: spill_home,
            },
            PlannedEdgeOp::Reload {
                source: logical,
                source_home: spill_home,
                destination: logical,
            },
        ],
    );

    assert_eq!(
        planned_spills(&func, &cfg, &plan).unwrap(),
        vec![PlannedSpillInsertion {
            block: successor,
            instruction: 0,
            value: logical,
            home: spill_home,
            edge_transfer: false,
        }]
    );
    let queries = super::super::ssa::planner_reload_queries(&func, &cfg, &plan).unwrap();
    assert_eq!(
        queries,
        BTreeSet::from([PointUse {
            block: func.blocks[successor].id,
            instruction: 0,
            value: VReg(logical.0),
        }])
    );
}

#[test]
fn selected_home_reconstructs_as_verified_state_store_and_load() {
    let mut vregs = VRegAllocator::new();
    let value = vregs.alloc();
    let marker = vregs.alloc();
    let mut func = MFunction::new(
        vregs,
        vec![
            SpillDesc::transient().with_deferred_state_home(home(0, 0)),
            SpillDesc::transient(),
        ],
    );
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm {
        dst: value,
        value: 0x1234,
    });
    block.push(MInst::LoadImm {
        dst: marker,
        value: 1,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 16,
        src: value,
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    func.blocks.push(block);

    let cfg = cfg::normalize(&mut func).unwrap();
    let next_use = next_use::analyze(&func, &cfg).unwrap();
    let mut plan = blank_plan(&func, &cfg, &next_use);
    let logical = plan.logical.of(value);
    let spill_home = plan.homes.of_vreg(value);
    plan.point_ops = vec![
        (
            point(1),
            PlannedOp::Spill {
                value: logical,
                home: spill_home,
            },
        ),
        (
            point(2),
            PlannedOp::Reload {
                value: logical,
                home: spill_home,
            },
        ),
    ];

    select(&func, &cfg, &mut plan, true).unwrap();
    assert_eq!(plan.state_homes.get(&spill_home), Some(&home(0, 0)));
    assert_eq!(plan.state_reload_recipes.len(), 1);

    let requested = super::super::ssa::planner_reload_queries(&func, &cfg, &plan).unwrap();
    let ordinary_recipes = reload::analyze_with_queries(&func, &cfg, &requested).unwrap();
    let result =
        reconstruct::reconstruct(&mut func, &cfg, &plan, &ordinary_recipes, false, true).unwrap();
    assert_eq!(result.frame_size, 0);
    assert!(func.blocks[0].insts.iter().any(|inst| {
        matches!(
            inst,
            MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
                ..
            }
        )
    }));
    assert!(func.blocks[0].insts.iter().any(|inst| {
        matches!(
            inst,
            MInst::Load {
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
                ..
            }
        )
    }));
    assert!(!func.blocks[0].insts.iter().any(|inst| {
        matches!(
            inst,
            MInst::Store {
                base: BaseReg::StackFrame,
                ..
            } | MInst::Load {
                base: BaseReg::StackFrame,
                ..
            }
        )
    }));
    super::super::materialized_state_home::verify_materialized_state_homes(
        &func,
        &cfg,
        &result.state_stores,
        &result.state_reloads,
    )
    .unwrap();
    reload::verify_expected_materialized_reloads(&func, &cfg, &result.recipe_reloads).unwrap();
}

#[test]
fn hazardous_selected_home_falls_back_to_stack() {
    let mut vregs = VRegAllocator::new();
    let value = vregs.alloc();
    let marker = vregs.alloc();
    let mut func = MFunction::new(
        vregs,
        vec![
            SpillDesc::transient().with_deferred_state_home(home(0, 0)),
            SpillDesc::transient(),
        ],
    );
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm {
        dst: value,
        value: 0x1234,
    });
    block.push(MInst::LoadImm {
        dst: marker,
        value: 1,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 16,
        src: value,
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    func.blocks.push(block);

    let cfg = cfg::normalize(&mut func).unwrap();
    let next_use = next_use::analyze(&func, &cfg).unwrap();
    let mut plan = blank_plan(&func, &cfg, &next_use);
    let logical = plan.logical.of(value);
    let spill_home = plan.homes.of_vreg(value);
    plan.point_ops = vec![
        (
            point(1),
            PlannedOp::Spill {
                value: logical,
                home: spill_home,
            },
        ),
        (
            point(2),
            PlannedOp::Reload {
                value: logical,
                home: spill_home,
            },
        ),
    ];
    select(&func, &cfg, &mut plan, true).unwrap();
    assert!(plan.state_homes.contains_key(&spill_home));

    fallback_to_stack(&func, &cfg, &mut plan, &BTreeSet::from([spill_home])).unwrap();

    assert!(!plan.state_homes.contains_key(&spill_home));
    assert!(plan.state_reload_recipes.is_empty());
    let requested = super::super::ssa::planner_reload_queries(&func, &cfg, &plan).unwrap();
    let ordinary_recipes = reload::analyze_with_queries(&func, &cfg, &requested).unwrap();
    let result =
        reconstruct::reconstruct(&mut func, &cfg, &plan, &ordinary_recipes, false, true).unwrap();
    assert_eq!(result.frame_size, 8);
    assert!(func.blocks[0].insts.iter().any(|inst| {
        matches!(
            inst,
            MInst::Store {
                base: BaseReg::StackFrame,
                ..
            }
        )
    }));
    assert!(func.blocks[0].insts.iter().any(|inst| {
        matches!(
            inst,
            MInst::Load {
                base: BaseReg::StackFrame,
                ..
            }
        )
    }));
}

#[test]
fn overlapping_original_write_rejects_deferred_state_home() {
    let mut vregs = VRegAllocator::new();
    let value = vregs.alloc();
    let clobber = vregs.alloc();
    let mut func = MFunction::new(
        vregs,
        vec![
            SpillDesc::transient().with_deferred_state_home(home(0, 0)),
            SpillDesc::transient(),
        ],
    );
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm {
        dst: value,
        value: 0x1234,
    });
    block.push(MInst::LoadImm {
        dst: clobber,
        value: 0x5678,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 0,
        src: clobber,
        size: OpSize::S64,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 16,
        src: value,
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    func.blocks.push(block);

    let cfg = cfg::normalize(&mut func).unwrap();
    let next_use = next_use::analyze(&func, &cfg).unwrap();
    let mut plan = blank_plan(&func, &cfg, &next_use);
    let logical = plan.logical.of(value);
    let spill_home = plan.homes.of_vreg(value);
    plan.point_ops = vec![
        (
            point(1),
            PlannedOp::Spill {
                value: logical,
                home: spill_home,
            },
        ),
        (
            point(3),
            PlannedOp::Reload {
                value: logical,
                home: spill_home,
            },
        ),
    ];

    select(&func, &cfg, &mut plan, true).unwrap();
    assert!(plan.state_homes.is_empty());
    assert!(plan.state_reload_recipes.is_empty());
}
