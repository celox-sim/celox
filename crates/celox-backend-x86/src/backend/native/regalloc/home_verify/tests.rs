use super::*;
use crate::native::mir::{MBlock, MInst, PhiNode, SpillDesc, VRegAllocator};

use super::super::{cfg, next_use, spill_plan};

fn blank_plan(func: &MFunction, cfg: &NormalizedCfg) -> SpillPlan {
    let next_use = next_use::analyze(func, cfg).unwrap();
    let mut plan = spill_plan::plan(func, cfg, &next_use, 32).unwrap();
    plan.point_ops.clear();
    plan.edge_ops.clear();
    plan.recipe_reloads.clear();
    plan.recipe_homes.clear();
    plan.state_homes.clear();
    plan.state_reload_recipes.clear();
    for state in plan
        .w_entry
        .iter_mut()
        .chain(&mut plan.w_exit)
        .chain(&mut plan.s_entry)
        .chain(&mut plan.s_exit)
    {
        state.clear();
    }
    plan
}

fn point(block: BlockId, instruction: usize) -> ProgramPoint {
    ProgramPoint {
        block,
        instruction,
        side: super::super::spill_plan::PointSide::Before,
    }
}

fn one_value_function(desc: SpillDesc) -> (MFunction, VReg) {
    let mut vregs = VRegAllocator::new();
    let value = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![desc]);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm {
        dst: value,
        value: 7,
    });
    block.push(MInst::Return);
    func.push_block(block);
    (func, value)
}

#[test]
fn same_point_store_precedes_reload() {
    let (mut func, value) = one_value_function(SpillDesc::transient());
    let cfg = cfg::normalize(&mut func).unwrap();
    let mut plan = blank_plan(&func, &cfg);
    let logical = plan.logical.of(value);
    let home = plan.homes.of_vreg(value);
    // Deliberately put Reload first: a plan point is parallel and lowering
    // orders stores before reloads.
    plan.point_ops.push((
        point(BlockId(0), 1),
        PlannedOp::Reload {
            value: logical,
            home,
        },
    ));
    plan.point_ops.push((
        point(BlockId(0), 1),
        PlannedOp::Spill {
            value: logical,
            home,
        },
    ));
    verify(&func, &cfg, &plan).unwrap();
}

#[test]
fn later_store_does_not_dominate_point_reload() {
    let mut vregs = VRegAllocator::new();
    let value = vregs.alloc();
    let copy = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm {
        dst: value,
        value: 7,
    });
    block.push(MInst::Mov {
        dst: copy,
        src: value,
    });
    block.push(MInst::Return);
    func.push_block(block);
    let cfg = cfg::normalize(&mut func).unwrap();
    let mut plan = blank_plan(&func, &cfg);
    let logical = plan.logical.of(value);
    let home = plan.homes.of_vreg(value);
    plan.point_ops.push((
        point(BlockId(0), 1),
        PlannedOp::Reload {
            value: logical,
            home,
        },
    ));
    plan.point_ops.push((
        point(BlockId(0), 2),
        PlannedOp::Spill {
            value: logical,
            home,
        },
    ));
    let error = verify(&func, &cfg, &plan).unwrap_err();
    assert_eq!(error.rule, RELOAD_RULE);
}

fn diamond() -> (MFunction, VReg) {
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
        value: 7,
    });
    entry.push(MInst::Branch {
        cond: condition,
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });
    let mut left = MBlock::new(BlockId(1));
    left.push(MInst::Jump { target: BlockId(3) });
    let mut right = MBlock::new(BlockId(2));
    right.push(MInst::Jump { target: BlockId(3) });
    let mut join = MBlock::new(BlockId(3));
    join.push(MInst::Return);
    func.blocks = vec![entry, left, right, join];
    (func, value)
}

#[test]
fn store_on_only_one_incoming_edge_does_not_dominate_join_reload() {
    let (mut func, value) = diamond();
    let cfg = cfg::normalize(&mut func).unwrap();
    let mut plan = blank_plan(&func, &cfg);
    let logical = plan.logical.of(value);
    let home = plan.homes.of_vreg(value);
    let left = cfg.block_index[&BlockId(1)];
    let join = cfg.block_index[&BlockId(3)];
    plan.edge_ops.insert(
        (left, join),
        vec![PlannedEdgeOp::Spill {
            source: logical,
            destination: logical,
            destination_home: home,
        }],
    );
    plan.point_ops.push((
        point(BlockId(3), 0),
        PlannedOp::Reload {
            value: logical,
            home,
        },
    ));
    let error = verify(&func, &cfg, &plan).unwrap_err();
    assert_eq!(error.rule, RELOAD_RULE);
}

#[test]
fn stores_on_every_incoming_edge_establish_join_home() {
    let (mut func, value) = diamond();
    let cfg = cfg::normalize(&mut func).unwrap();
    let mut plan = blank_plan(&func, &cfg);
    let logical = plan.logical.of(value);
    let home = plan.homes.of_vreg(value);
    let join = cfg.block_index[&BlockId(3)];
    for predecessor in [BlockId(1), BlockId(2)] {
        let predecessor = cfg.block_index[&predecessor];
        plan.edge_ops.insert(
            (predecessor, join),
            vec![PlannedEdgeOp::Spill {
                source: logical,
                destination: logical,
                destination_home: home,
            }],
        );
    }
    plan.point_ops.push((
        point(BlockId(3), 0),
        PlannedOp::Reload {
            value: logical,
            home,
        },
    ));
    verify(&func, &cfg, &plan).unwrap();
}

#[test]
fn edge_store_precedes_edge_reload() {
    let (mut func, value) = one_value_function(SpillDesc::transient());
    func.blocks[0].insts.pop();
    func.blocks[0].push(MInst::Jump { target: BlockId(1) });
    let mut exit = MBlock::new(BlockId(1));
    exit.push(MInst::Return);
    func.push_block(exit);
    let cfg = cfg::normalize(&mut func).unwrap();
    let mut plan = blank_plan(&func, &cfg);
    let logical = plan.logical.of(value);
    let home = plan.homes.of_vreg(value);
    let entry = cfg.block_index[&BlockId(0)];
    let exit = cfg.block_index[&BlockId(1)];
    plan.edge_ops.insert(
        (entry, exit),
        vec![
            PlannedEdgeOp::Reload {
                source: logical,
                source_home: home,
                destination: logical,
            },
            PlannedEdgeOp::Spill {
                source: logical,
                destination: logical,
                destination_home: home,
            },
        ],
    );
    verify(&func, &cfg, &plan).unwrap();
}

#[test]
fn rematerialized_reload_needs_no_store() {
    let (mut func, value) = one_value_function(SpillDesc::remat(7));
    let cfg = cfg::normalize(&mut func).unwrap();
    let mut plan = blank_plan(&func, &cfg);
    let logical = plan.logical.of(value);
    let home = plan.homes.of_vreg(value);
    plan.point_ops.push((
        point(BlockId(0), 1),
        PlannedOp::Reload {
            value: logical,
            home,
        },
    ));
    verify(&func, &cfg, &plan).unwrap();
}

#[test]
fn spilled_phi_transfers_from_a_distinct_source_home() {
    let mut vregs = VRegAllocator::new();
    let condition = vregs.alloc();
    let left_value = vregs.alloc();
    let right_value = vregs.alloc();
    let merged = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: condition,
        value: 1,
    });
    entry.push(MInst::LoadImm {
        dst: left_value,
        value: 11,
    });
    entry.push(MInst::LoadImm {
        dst: right_value,
        value: 22,
    });
    entry.push(MInst::Branch {
        cond: condition,
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });
    let mut left = MBlock::new(BlockId(1));
    left.push(MInst::Jump { target: BlockId(3) });
    let mut right = MBlock::new(BlockId(2));
    right.push(MInst::Jump { target: BlockId(3) });
    let mut join = MBlock::new(BlockId(3));
    join.phis.push(PhiNode {
        dst: merged,
        sources: vec![(BlockId(1), left_value), (BlockId(2), right_value)],
    });
    join.push(MInst::Return);
    func.blocks = vec![entry, left, right, join];

    let cfg = cfg::normalize(&mut func).unwrap();
    let mut plan = blank_plan(&func, &cfg);
    let merged_logical = plan.logical.of(merged);
    let home = plan.homes.of_vreg(merged);
    let left = cfg.block_index[&BlockId(1)];
    // This claim suppresses reconstruction's implicit store on the left
    // edge, but there is no actual store establishing it.
    plan.s_exit[left].insert(plan.logical.of(left_value));
    plan.point_ops.push((
        point(BlockId(3), 0),
        PlannedOp::SpillPhi {
            value: merged_logical,
            home,
        },
    ));
    verify(&func, &cfg, &plan).unwrap();
}

fn loop_function() -> (MFunction, VReg) {
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
        value: 9,
    });
    entry.push(MInst::Jump { target: BlockId(1) });
    let mut header = MBlock::new(BlockId(1));
    header.push(MInst::Branch {
        cond: condition,
        true_bb: BlockId(2),
        false_bb: BlockId(3),
    });
    let mut body = MBlock::new(BlockId(2));
    body.push(MInst::Jump { target: BlockId(1) });
    let mut exit = MBlock::new(BlockId(3));
    exit.push(MInst::Return);
    func.blocks = vec![entry, header, body, exit];
    (func, value)
}

#[test]
fn backedge_store_does_not_hide_unstored_loop_entry_path() {
    let (mut func, value) = loop_function();
    let cfg = cfg::normalize(&mut func).unwrap();
    let mut plan = blank_plan(&func, &cfg);
    let logical = plan.logical.of(value);
    let home = plan.homes.of_vreg(value);
    let body = cfg.block_index[&BlockId(2)];
    let header = cfg.block_index[&BlockId(1)];
    plan.edge_ops.insert(
        (body, header),
        vec![PlannedEdgeOp::Spill {
            source: logical,
            destination: logical,
            destination_home: home,
        }],
    );
    plan.point_ops.push((
        point(BlockId(1), 0),
        PlannedOp::Reload {
            value: logical,
            home,
        },
    ));

    let error = verify(&func, &cfg, &plan).unwrap_err();

    assert_eq!(error.rule, RELOAD_RULE);
}

#[test]
fn preheader_store_establishes_home_through_loop_fixed_point() {
    let (mut func, value) = loop_function();
    let cfg = cfg::normalize(&mut func).unwrap();
    let mut plan = blank_plan(&func, &cfg);
    let logical = plan.logical.of(value);
    let home = plan.homes.of_vreg(value);
    plan.point_ops.push((
        point(BlockId(0), 2),
        PlannedOp::Spill {
            value: logical,
            home,
        },
    ));
    plan.point_ops.push((
        point(BlockId(1), 0),
        PlannedOp::Reload {
            value: logical,
            home,
        },
    ));

    verify(&func, &cfg, &plan).unwrap();
}

#[test]
fn sparse_proof_does_not_materialize_block_by_home_state() {
    const BLOCKS: usize = 1024;
    const HOMES: usize = 256;

    let mut vregs = VRegAllocator::new();
    let values = (0..HOMES).map(|_| vregs.alloc()).collect::<Vec<_>>();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); HOMES]);
    let mut entry = MBlock::new(BlockId(0));
    for (index, &value) in values.iter().enumerate() {
        entry.push(MInst::LoadImm {
            dst: value,
            value: index as u64,
        });
    }
    entry.push(MInst::Jump { target: BlockId(1) });
    func.push_block(entry);
    for block in 1..BLOCKS - 1 {
        let mut body = MBlock::new(BlockId(block as u32));
        body.push(MInst::Jump {
            target: BlockId(block as u32 + 1),
        });
        func.push_block(body);
    }
    let mut exit = MBlock::new(BlockId((BLOCKS - 1) as u32));
    exit.push(MInst::Return);
    func.push_block(exit);
    let cfg = cfg::normalize(&mut func).unwrap();
    let mut plan = blank_plan(&func, &cfg);
    for &value in &values {
        let logical = plan.logical.of(value);
        let home = plan.homes.of_vreg(value);
        plan.point_ops.push((
            point(BlockId(0), HOMES),
            PlannedOp::Spill {
                value: logical,
                home,
            },
        ));
        plan.point_ops.push((
            point(BlockId((BLOCKS - 1) as u32), 0),
            PlannedOp::Reload {
                value: logical,
                home,
            },
        ));
    }

    let work = verify_with_work(&func, &cfg, &plan).unwrap();

    assert_eq!(work.homes, HOMES);
    assert_eq!(work.dominator_blocks, BLOCKS);
    assert_eq!(work.phi_nodes, 0);
    assert_eq!(work.frontier_members_visited, 0);
    assert_eq!(work.fact_updates, HOMES);
    assert_eq!(work.query_checks, HOMES);
    let sparse_operations = work.dominator_blocks
        + work.frontier_members_visited
        + work.phi_nodes
        + work.phi_inputs
        + work.fact_updates
        + work.query_checks;
    assert!(sparse_operations <= BLOCKS + 3 * HOMES);
    assert!(sparse_operations < BLOCKS * HOMES / 16);
}
