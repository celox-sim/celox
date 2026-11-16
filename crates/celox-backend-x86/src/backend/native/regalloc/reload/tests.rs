use super::*;
use crate::native::mir::{MBlock, MemoryAliasRange, PhiNode, SpillDesc, VRegAllocator};
use crate::{InstanceId, RegionedAbsoluteAddr, STABLE_REGION};
use celox_design::StateObjectId as VarId;

fn function_with_values(count: usize) -> (MFunction, Vec<VReg>) {
    let mut vregs = VRegAllocator::new();
    let values = (0..count).map(|_| vregs.alloc()).collect::<Vec<_>>();
    (
        MFunction::new(vregs, vec![SpillDesc::transient(); count]),
        values,
    )
}

fn analyze_function(mut func: MFunction) -> (MFunction, NormalizedCfg, ReloadRecipeAnalysis) {
    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let analysis = analyze(&func, &cfg).unwrap();
    (func, cfg, analysis)
}

#[test]
fn exact_physical_load_shape_becomes_recipe() {
    let (mut func, values) = function_with_values(2);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: values[0],
        base: BaseReg::SimState,
        offset: 19,
        size: OpSize::S32,
    });
    block.push(MInst::Mov32 {
        dst: values[1],
        src: values[0],
    });
    block.push(MInst::Return);
    func.push_block(block);
    let (func, cfg, analysis) = analyze_function(func);

    assert_eq!(
        analysis.state_recipe(values[0]).map(|recipe| recipe.load),
        Some(StateLoad {
            offset: 19,
            size: OpSize::S32,
        })
    );
    assert!(analysis.state_valid_at_point(PointUse {
        block: BlockId(0),
        instruction: 1,
        value: values[0],
    }));
    assert!(matches!(
        analysis.recipe(values[1]),
        Some(ReloadRecipe::Pure { .. })
    ));
    assert_eq!(
        analysis.pure_recipe(values[1]),
        Some(PureRecipe::Copy32 { source: values[0] })
    );
    assert_eq!(
        analysis.resolved_recipe(values[1]).unwrap(),
        Some(ResolvedRecipe {
            base: ResolvedBase::State(analysis.state_recipe(values[0]).unwrap().clone()),
            steps: vec![PureStep::Copy32],
        })
    );
    assert_eq!(
        analyze_for_planning(&func, &cfg)
            .unwrap()
            .global_materialization_costs()
            .unwrap(),
        vec![Some(1), Some(2)]
    );
}

#[test]
fn comparison_immediate_is_an_exact_pure_recipe() {
    let (mut func, values) = function_with_values(2);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: values[0],
        base: BaseReg::SimState,
        offset: 24,
        size: OpSize::S64,
    });
    block.push(MInst::CmpImm {
        dst: values[1],
        lhs: values[0],
        imm: 53,
        kind: CmpKind::Eq,
    });
    block.push(MInst::Return);
    func.push_block(block);
    let (func, cfg, analysis) = analyze_function(func);

    assert_eq!(
        analysis.pure_recipe(values[1]),
        Some(PureRecipe::CmpImm64 {
            source: values[0],
            immediate: 53,
            kind: CmpKind::Eq,
        })
    );
    assert_eq!(
        analysis.resolved_recipe(values[1]).unwrap(),
        Some(ResolvedRecipe {
            base: ResolvedBase::State(analysis.state_recipe(values[0]).unwrap().clone()),
            steps: vec![PureStep::CmpImm64 {
                immediate: 53,
                kind: CmpKind::Eq,
            }],
        })
    );
    assert_eq!(
        analyze_for_planning(&func, &cfg)
            .unwrap()
            .global_materialization_costs()
            .unwrap(),
        vec![Some(1), Some(2)]
    );
}

#[test]
fn immediate_product_recovery_preserves_width_and_rejects_clobbered_inputs() {
    for word32 in [false, true] {
        let (mut func, values) = function_with_values(5);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: values[0],
            base: BaseReg::SimState,
            offset: 16,
            size: OpSize::S64,
        });
        let multiply = if word32 {
            MInst::MulImm32 {
                dst: values[1],
                src: values[0],
                imm: -7,
            }
        } else {
            MInst::MulImm {
                dst: values[1],
                src: values[0],
                imm: -7,
            }
        };
        block.push(multiply.clone());
        block.push(MInst::Mov {
            dst: values[2],
            src: values[1],
        });
        block.push(MInst::LoadImm {
            dst: values[3],
            value: 0,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 16,
            src: values[3],
            size: OpSize::S64,
        });
        block.push(MInst::Mov {
            dst: values[4],
            src: values[1],
        });
        block.push(MInst::Return);
        func.push_block(block);
        let (_, _, analysis) = analyze_function(func);
        let expected = if word32 {
            PureStep::MulImm32 { immediate: -7 }
        } else {
            PureStep::MulImm64 { immediate: -7 }
        };
        assert_eq!(
            analysis.resolved_recipe(values[1]).unwrap().unwrap().steps,
            vec![expected]
        );
        assert_eq!(
            materialize_pure_step(expected, values[1], values[0]),
            multiply
        );
        assert!(
            analysis
                .resolved_recipe_at_point(PointUse {
                    block: BlockId(0),
                    instruction: 2,
                    value: values[1],
                })
                .is_some()
        );
        assert!(
            analysis
                .resolved_recipe_at_point(PointUse {
                    block: BlockId(0),
                    instruction: 5,
                    value: values[1],
                })
                .is_none()
        );
    }
}

#[test]
fn sparse_mark_preserves_only_nonoverlapping_state_recipes() {
    fn fixture(load_offset: i32) -> (VReg, ReloadRecipeAnalysis) {
        let (mut func, values) = function_with_values(3);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: values[0],
            base: BaseReg::SimState,
            offset: load_offset,
            size: OpSize::S64,
        });
        block.push(MInst::SparseMarkActive {
            active_index: 3,
            active_bits_offset: 200,
            active_capacity: 16,
        });
        block.push(MInst::Mov {
            dst: values[1],
            src: values[0],
        });
        block.push(MInst::Return);
        func.push_block(block);
        let (_, _, analysis) = analyze_function(func);
        (values[0], analysis)
    }

    let (unrelated, unrelated_analysis) = fixture(40);
    assert!(unrelated_analysis.state_valid_at_point(PointUse {
        block: BlockId(0),
        instruction: 2,
        value: unrelated,
    }));

    let (metadata, metadata_analysis) = fixture(200);
    assert!(!metadata_analysis.state_valid_at_point(PointUse {
        block: BlockId(0),
        instruction: 2,
        value: metadata,
    }));
}

#[test]
fn exact_s64_store_establishes_a_post_store_home() {
    let (mut func, values) = function_with_values(3);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: values[0],
        base: BaseReg::StackFrame,
        offset: 0,
        size: OpSize::S64,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 40,
        src: values[0],
        size: OpSize::S64,
    });
    block.push(MInst::Mov {
        dst: values[1],
        src: values[0],
    });
    block.push(MInst::Store {
        base: BaseReg::StackFrame,
        offset: 8,
        src: values[1],
        size: OpSize::S64,
    });
    block.push(MInst::LoadImm {
        dst: values[2],
        value: 0,
    });
    block.push(MInst::Return);
    func.push_block(block);
    let (_, _, analysis) = analyze_function(func);

    assert!(
        analysis
            .resolved_recipe_at_point(PointUse {
                block: BlockId(0),
                instruction: 1,
                value: values[0],
            })
            .is_none(),
        "a store cannot use the home which it has not established yet"
    );
    let recipe = analysis
        .resolved_recipe_at_point(PointUse {
            block: BlockId(0),
            instruction: 2,
            value: values[0],
        })
        .unwrap();
    assert!(matches!(
        &recipe.base,
        ResolvedBase::State(StateRecipe {
            load: StateLoad {
                offset: 40,
                size: OpSize::S64,
            },
            ..
        })
    ));
    assert!(recipe.steps.is_empty());
}

#[test]
fn planning_cost_is_exact_at_each_use_and_falls_back_after_overwrite() {
    let (mut func, values) = function_with_values(4);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: values[0],
        base: BaseReg::StackFrame,
        offset: 0,
        size: OpSize::S64,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 40,
        src: values[0],
        size: OpSize::S64,
    });
    block.push(MInst::Mov {
        dst: values[1],
        src: values[0],
    });
    block.push(MInst::LoadImm {
        dst: values[2],
        value: 9,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 40,
        src: values[2],
        size: OpSize::S64,
    });
    block.push(MInst::Mov {
        dst: values[3],
        src: values[0],
    });
    block.push(MInst::Return);
    func.push_block(block);
    let cfg = super::super::cfg::normalize(&mut func).unwrap();

    let costs = analyze_for_planning(&func, &cfg).unwrap();

    assert_eq!(costs.global_materialization_cost(values[0]), None);
    assert_eq!(
        costs.materialization_cost_at_point(PointUse {
            block: BlockId(0),
            instruction: 2,
            value: values[0],
        }),
        Some(1)
    );
    assert_eq!(
        costs.materialization_cost_at_point(PointUse {
            block: BlockId(0),
            instruction: 5,
            value: values[0],
        }),
        None,
        "an overwritten MemorySSA version must use the stack fallback"
    );
}

#[test]
fn planning_cost_preserves_exact_phi_edge_homes() {
    let (mut func, values) = function_with_values(5);
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: values[0],
        value: 1,
    });
    entry.push(MInst::Branch {
        cond: values[0],
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });
    let mut left = MBlock::new(BlockId(1));
    left.push(MInst::Load {
        dst: values[1],
        base: BaseReg::StackFrame,
        offset: 0,
        size: OpSize::S64,
    });
    left.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 40,
        src: values[1],
        size: OpSize::S64,
    });
    left.push(MInst::Jump { target: BlockId(3) });
    let mut right = MBlock::new(BlockId(2));
    right.push(MInst::Load {
        dst: values[2],
        base: BaseReg::StackFrame,
        offset: 8,
        size: OpSize::S64,
    });
    right.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 40,
        src: values[2],
        size: OpSize::S64,
    });
    right.push(MInst::Jump { target: BlockId(3) });
    let mut join = MBlock::new(BlockId(3));
    join.phis.push(PhiNode {
        dst: values[3],
        sources: vec![(BlockId(1), values[1]), (BlockId(2), values[2])],
    });
    join.push(MInst::Mov {
        dst: values[4],
        src: values[3],
    });
    join.push(MInst::Return);
    func.blocks = vec![entry, left, right, join];
    let cfg = super::super::cfg::normalize(&mut func).unwrap();

    let costs = analyze_for_planning(&func, &cfg).unwrap();

    assert_eq!(
        costs.materialization_cost_at_point(PointUse {
            block: BlockId(3),
            instruction: 0,
            value: values[3],
        }),
        Some(1)
    );
    for (predecessor, value) in [(BlockId(1), values[1]), (BlockId(2), values[2])] {
        assert_eq!(
            costs.materialization_cost_on_edge(EdgeUse {
                predecessor,
                successor: BlockId(3),
                value,
            }),
            Some(1)
        );
    }
}

#[test]
fn exact_store_of_pure_result_precedes_expression_reconstruction() {
    let (mut func, values) = function_with_values(3);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: values[0],
        base: BaseReg::StackFrame,
        offset: 0,
        size: OpSize::S64,
    });
    block.push(MInst::AddImm {
        dst: values[1],
        src: values[0],
        imm: 7,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 40,
        src: values[1],
        size: OpSize::S64,
    });
    block.push(MInst::Mov {
        dst: values[2],
        src: values[1],
    });
    block.push(MInst::Return);
    func.push_block(block);
    let (_, _, analysis) = analyze_function(func);

    let point = PointUse {
        block: BlockId(0),
        instruction: 3,
        value: values[1],
    };
    let recipe = analysis.resolved_recipe_at_point(point).unwrap();
    assert!(analysis.point_recipe_uses_store_home(point));
    assert!(matches!(
        &recipe.base,
        ResolvedBase::State(StateRecipe {
            load: StateLoad {
                offset: 40,
                size: OpSize::S64,
            },
            ..
        })
    ));
    assert!(
        recipe.steps.is_empty(),
        "the stored pure result is already the requested value"
    );
}

#[test]
fn proved_zero_extended_narrow_store_is_an_exact_home() {
    let (mut func, values) = function_with_values(3);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: values[0],
        base: BaseReg::StackFrame,
        offset: 0,
        size: OpSize::S64,
    });
    block.push(MInst::AndImm {
        dst: values[1],
        src: values[0],
        imm: 0xff,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 40,
        src: values[1],
        size: OpSize::S8,
    });
    block.push(MInst::Mov {
        dst: values[2],
        src: values[1],
    });
    block.push(MInst::Return);
    func.push_block(block);
    let (_, _, analysis) = analyze_function(func);

    let recipe = analysis
        .resolved_recipe_at_point(PointUse {
            block: BlockId(0),
            instruction: 3,
            value: values[1],
        })
        .unwrap();
    assert!(matches!(
        &recipe.base,
        ResolvedBase::State(StateRecipe {
            load: StateLoad {
                offset: 40,
                size: OpSize::S8,
            },
            ..
        })
    ));
    assert!(recipe.steps.is_empty());
}

#[test]
fn potentially_overflowing_value_is_not_a_narrow_store_home() {
    let (mut func, values) = function_with_values(4);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: values[0],
        base: BaseReg::StackFrame,
        offset: 0,
        size: OpSize::S64,
    });
    block.push(MInst::AndImm {
        dst: values[1],
        src: values[0],
        imm: 0xff,
    });
    block.push(MInst::AddImm {
        dst: values[2],
        src: values[1],
        imm: 1,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 40,
        src: values[2],
        size: OpSize::S8,
    });
    block.push(MInst::Mov {
        dst: values[3],
        src: values[2],
    });
    block.push(MInst::Return);
    func.push_block(block);
    let (_, _, analysis) = analyze_function(func);

    assert!(
        analysis
            .resolved_recipe_at_point(PointUse {
                block: BlockId(0),
                instruction: 4,
                value: values[2],
            })
            .is_none()
    );
}

fn phi_of_committed_values(right_offset: i32) -> (MFunction, VReg) {
    let (mut func, values) = function_with_values(7);
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: values[0],
        value: 1,
    });
    entry.push(MInst::Branch {
        cond: values[0],
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });

    let mut left = MBlock::new(BlockId(1));
    left.push(MInst::Load {
        dst: values[1],
        base: BaseReg::StackFrame,
        offset: 0,
        size: OpSize::S64,
    });
    left.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 40,
        src: values[1],
        size: OpSize::S64,
    });
    left.push(MInst::Mov {
        dst: values[3],
        src: values[1],
    });
    left.push(MInst::Jump { target: BlockId(3) });

    let mut right = MBlock::new(BlockId(2));
    right.push(MInst::Load {
        dst: values[2],
        base: BaseReg::StackFrame,
        offset: 8,
        size: OpSize::S64,
    });
    right.push(MInst::Store {
        base: BaseReg::SimState,
        offset: right_offset,
        src: values[2],
        size: OpSize::S64,
    });
    right.push(MInst::Mov {
        dst: values[4],
        src: values[2],
    });
    right.push(MInst::Jump { target: BlockId(3) });

    let mut join = MBlock::new(BlockId(3));
    join.phis.push(PhiNode {
        dst: values[5],
        sources: vec![(BlockId(1), values[3]), (BlockId(2), values[4])],
    });
    join.push(MInst::Mov {
        dst: values[6],
        src: values[5],
    });
    join.push(MInst::Return);
    func.blocks = vec![entry, left, right, join];
    (func, values[5])
}

#[test]
fn memory_phi_is_an_exact_home_for_matching_register_phi() {
    let (func, phi) = phi_of_committed_values(40);
    let (_, _, analysis) = analyze_function(func);

    let recipe = analysis
        .resolved_recipe_at_point(PointUse {
            block: BlockId(3),
            instruction: 0,
            value: phi,
        })
        .unwrap();
    assert!(matches!(
        &recipe.base,
        ResolvedBase::State(StateRecipe {
            load: StateLoad {
                offset: 40,
                size: OpSize::S64,
            },
            ..
        })
    ));
    assert!(recipe.steps.is_empty());
}

#[test]
fn logical_state_load_width_proves_a_narrow_store_phi_home() {
    let (mut func, values) = function_with_values(5);
    let address = RegionedAbsoluteAddr {
        region: STABLE_REGION,
        instance_id: InstanceId(0),
        var_id: VarId::default(),
    };
    func.spill_descs[values[1].0 as usize] = SpillDesc::sim_state(address, 0, 5, false);
    func.spill_descs[values[2].0 as usize] = SpillDesc::sim_state(address, 0, 5, false);

    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: values[0],
        value: 1,
    });
    entry.push(MInst::Branch {
        cond: values[0],
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });

    let mut left = MBlock::new(BlockId(1));
    left.push(MInst::Load {
        dst: values[1],
        base: BaseReg::SimState,
        offset: 8,
        size: OpSize::S64,
    });
    left.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 40,
        src: values[1],
        size: OpSize::S8,
    });
    left.push(MInst::Jump { target: BlockId(3) });

    let mut right = MBlock::new(BlockId(2));
    right.push(MInst::Load {
        dst: values[2],
        base: BaseReg::SimState,
        offset: 16,
        size: OpSize::S64,
    });
    right.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 40,
        src: values[2],
        size: OpSize::S8,
    });
    right.push(MInst::Jump { target: BlockId(3) });

    let mut join = MBlock::new(BlockId(3));
    join.phis.push(PhiNode {
        dst: values[3],
        sources: vec![(BlockId(1), values[1]), (BlockId(2), values[2])],
    });
    join.push(MInst::Mov {
        dst: values[4],
        src: values[3],
    });
    join.push(MInst::Return);

    // Keep the CFG valid but place the phi before its source definitions
    // in the block vector. A single linear width scan cannot prove it.
    func.blocks = vec![entry, join, left, right];
    let bits = canonical_value_bits(&func).unwrap();
    assert_eq!(bits[values[3].0 as usize], 5);

    let (_, _, analysis) = analyze_function(func);
    let recipe = analysis
        .resolved_recipe_at_point(PointUse {
            block: BlockId(3),
            instruction: 0,
            value: values[3],
        })
        .unwrap();
    assert!(matches!(
        &recipe.base,
        ResolvedBase::State(StateRecipe {
            load: StateLoad {
                offset: 40,
                size: OpSize::S8,
            },
            ..
        })
    ));
    assert!(recipe.steps.is_empty());
}

#[test]
fn partial_rmw_store_phi_reconstructs_inserted_value_from_state() {
    let (mut func, values) = function_with_values(11);
    let low_mask = (1u64 << 52) - 1;
    let high_mask = !low_mask;
    func.spill_descs[values[4].0 as usize] =
        SpillDesc::transient().with_state_insert(values[1], 0, 52);
    func.spill_descs[values[8].0 as usize] =
        SpillDesc::transient().with_state_insert(values[5], 0, 52);

    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: values[0],
        value: 1,
    });
    entry.push(MInst::Branch {
        cond: values[0],
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });

    let mut left = MBlock::new(BlockId(1));
    left.push(MInst::LoadImm {
        dst: values[1],
        value: 0x123,
    });
    left.push(MInst::Load {
        dst: values[2],
        base: BaseReg::SimState,
        offset: 40,
        size: OpSize::S64,
    });
    left.push(MInst::AndImm {
        dst: values[3],
        src: values[2],
        imm: high_mask,
    });
    left.push(MInst::Or {
        dst: values[4],
        lhs: values[3],
        rhs: values[1],
    });
    left.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 40,
        src: values[4],
        size: OpSize::S64,
    });
    left.push(MInst::Jump { target: BlockId(3) });

    let mut right = MBlock::new(BlockId(2));
    right.push(MInst::LoadImm {
        dst: values[5],
        value: 0x456,
    });
    right.push(MInst::Load {
        dst: values[6],
        base: BaseReg::SimState,
        offset: 40,
        size: OpSize::S64,
    });
    right.push(MInst::AndImm {
        dst: values[7],
        src: values[6],
        imm: high_mask,
    });
    right.push(MInst::Or {
        dst: values[8],
        lhs: values[7],
        rhs: values[5],
    });
    right.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 40,
        src: values[8],
        size: OpSize::S64,
    });
    right.push(MInst::Jump { target: BlockId(3) });

    let mut join = MBlock::new(BlockId(3));
    join.phis.push(PhiNode {
        dst: values[9],
        sources: vec![(BlockId(1), values[1]), (BlockId(2), values[5])],
    });
    join.push(MInst::Mov {
        dst: values[10],
        src: values[9],
    });
    join.push(MInst::Return);
    func.blocks = vec![entry, left, right, join];

    let (_, _, analysis) = analyze_function(func);
    let recipe = analysis
        .resolved_recipe_at_point(PointUse {
            block: BlockId(3),
            instruction: 0,
            value: values[9],
        })
        .unwrap();
    assert!(matches!(
        &recipe.base,
        ResolvedBase::State(StateRecipe {
            load: StateLoad {
                offset: 40,
                size: OpSize::S64,
            },
            ..
        })
    ));
    assert_eq!(
        recipe.steps,
        vec![
            PureStep::ShlImm64 { immediate: 12 },
            PureStep::ShrImm64 { immediate: 12 },
        ]
    );
}

fn two_partial_rmw_stores(second_bit: usize) -> (MFunction, VReg, PointUse) {
    let (mut func, values) = function_with_values(12);
    func.spill_descs[values[4].0 as usize] =
        SpillDesc::transient().with_state_insert(values[1], 0, 1);
    func.spill_descs[values[10].0 as usize] =
        SpillDesc::transient().with_state_insert(values[6], second_bit, 1);

    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: values[0],
        base: BaseReg::StackFrame,
        offset: 0,
        size: OpSize::S64,
    });
    block.push(MInst::AndImm {
        dst: values[1],
        src: values[0],
        imm: 1,
    });
    block.push(MInst::Load {
        dst: values[2],
        base: BaseReg::SimState,
        offset: 40,
        size: OpSize::S8,
    });
    block.push(MInst::AndImm {
        dst: values[3],
        src: values[2],
        imm: !1,
    });
    block.push(MInst::Or {
        dst: values[4],
        lhs: values[3],
        rhs: values[1],
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 40,
        src: values[4],
        size: OpSize::S8,
    });
    block.push(MInst::Load {
        dst: values[5],
        base: BaseReg::StackFrame,
        offset: 8,
        size: OpSize::S64,
    });
    block.push(MInst::AndImm {
        dst: values[6],
        src: values[5],
        imm: 1,
    });
    block.push(MInst::ShlImm {
        dst: values[7],
        src: values[6],
        imm: second_bit as u8,
    });
    block.push(MInst::Load {
        dst: values[8],
        base: BaseReg::SimState,
        offset: 40,
        size: OpSize::S8,
    });
    block.push(MInst::AndImm {
        dst: values[9],
        src: values[8],
        imm: !(1u64 << second_bit),
    });
    block.push(MInst::Or {
        dst: values[10],
        lhs: values[9],
        rhs: values[7],
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 40,
        src: values[10],
        size: OpSize::S8,
    });
    block.push(MInst::Mov {
        dst: values[11],
        src: values[1],
    });
    block.push(MInst::Return);
    func.push_block(block);
    let point = PointUse {
        block: BlockId(0),
        instruction: 13,
        value: values[1],
    };
    (func, values[1], point)
}

#[test]
fn disjoint_partial_rmw_preserves_an_existing_bit_home() {
    let (func, value, point) = two_partial_rmw_stores(1);
    let (_, _, analysis) = analyze_function(func);

    let recipe = analysis.resolved_recipe_at_point(point).unwrap();
    assert!(analysis.point_recipe_uses_store_home(point));
    assert!(matches!(
        &recipe.base,
        ResolvedBase::State(StateRecipe {
            load: StateLoad {
                offset: 40,
                size: OpSize::S8,
            },
            observed_bits: StateBitRange {
                start: 320,
                end: 321,
            },
            ..
        })
    ));
    assert_eq!(recipe.steps, vec![PureStep::AndImm32 { immediate: 1 }]);
    assert_eq!(point.value, value);
}

#[test]
fn overlapping_partial_rmw_invalidates_an_existing_bit_home() {
    let (func, _, point) = two_partial_rmw_stores(0);
    let (_, _, analysis) = analyze_function(func);

    assert!(analysis.resolved_recipe_at_point(point).is_none());
}

#[test]
fn register_phi_with_different_state_slots_keeps_stack_fallback() {
    let (func, phi) = phi_of_committed_values(48);
    let (_, _, analysis) = analyze_function(func);

    assert!(
        analysis
            .resolved_recipe_at_point(PointUse {
                block: BlockId(3),
                instruction: 0,
                value: phi,
            })
            .is_none()
    );
}

#[test]
fn requested_terminator_point_observes_a_post_store_home() {
    let (mut func, values) = function_with_values(1);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: values[0],
        base: BaseReg::StackFrame,
        offset: 0,
        size: OpSize::S64,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 40,
        src: values[0],
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    func.push_block(block);
    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let query = PointUse {
        block: BlockId(0),
        instruction: 2,
        value: values[0],
    };
    let analysis = analyze_with_queries(&func, &cfg, &BTreeSet::from([query])).unwrap();

    let recipe = analysis.resolved_recipe_at_point(query).unwrap();
    assert!(analysis.point_recipe_uses_store_home(query));
    assert!(matches!(
        &recipe.base,
        ResolvedBase::State(StateRecipe {
            load: StateLoad {
                offset: 40,
                size: OpSize::S64,
            },
            ..
        })
    ));
    assert!(recipe.steps.is_empty());
}

#[test]
fn contiguous_memory_effects_preserve_write_boundaries_and_clobbers() {
    let (mut func, values) = function_with_values(1);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm {
        dst: values[0],
        value: 1,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 8,
        src: values[0],
        size: OpSize::S64,
    });
    block.push(MInst::SparseCommit {
        src_offset: 0,
        dst_offset: 100,
        byte_size: 17,
        dirty_words_offset: 200,
        dirty_word_count: 2,
        summary_words_offset: 300,
        summary_word_count: 1,
        four_state: true,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 104,
        src: values[0],
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    func.push_block(block);
    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let mut memory = build_reload_memory_ssa(&func, &cfg).unwrap();
    for instruction in 0..4 {
        let point = memory.versions.event(0, instruction).unwrap().1;
        for offset in 0..320 {
            let ordinal = if instruction >= 3 && (104..112).contains(&offset) {
                Some(2)
            } else if instruction >= 2
                && [(100..134), (200..216), (300..308)]
                    .iter()
                    .any(|range| range.contains(&offset))
            {
                Some(1)
            } else if instruction >= 1 && (8..16).contains(&offset) {
                Some(0)
            } else {
                None
            };
            let expected = ordinal.map_or(SnapshotAccess::LiveOnEntry, |ordinal| {
                SnapshotAccess::Write {
                    block: BlockId(0),
                    ordinal,
                }
            });
            let actual = memory
                .snapshot_at(
                    point,
                    StateLoad {
                        offset,
                        size: OpSize::S8,
                    },
                )
                .unwrap();
            assert_eq!(
                actual.root_at(i64::from(offset)),
                Some(expected),
                "i{instruction} byte {offset}"
            );
            assert_eq!(actual.segments().len(), 1);
            assert!(actual.reachable_phis().is_empty());
        }
    }
}

#[test]
fn sparse_memory_ssa_matches_full_analysis_at_requested_points() {
    let (mut func, values) = function_with_values(5);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: values[0],
        base: BaseReg::SimState,
        offset: 8,
        size: OpSize::S64,
    });
    block.push(MInst::Load {
        dst: values[1],
        base: BaseReg::SimState,
        offset: 40,
        size: OpSize::S64,
    });
    block.push(MInst::LoadImm {
        dst: values[2],
        value: 9,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 8,
        src: values[2],
        size: OpSize::S64,
    });
    block.push(MInst::Mov {
        dst: values[3],
        src: values[1],
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 44,
        src: values[2],
        size: OpSize::S8,
    });
    block.push(MInst::Mov {
        dst: values[4],
        src: values[1],
    });
    block.push(MInst::Return);
    func.push_block(block);
    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let before_overlap = PointUse {
        block: BlockId(0),
        instruction: 4,
        value: values[1],
    };
    let after_overlap = PointUse {
        block: BlockId(0),
        instruction: 6,
        value: values[1],
    };
    let requested = BTreeSet::from([before_overlap, after_overlap]);

    let full = analyze(&func, &cfg).unwrap();
    let sparse = analyze_with_queries(&func, &cfg, &requested).unwrap();

    assert_eq!(full.recipe(values[1]), sparse.recipe(values[1]));
    for point in requested {
        assert_eq!(
            full.resolved_recipe_at_point(point),
            sparse.resolved_recipe_at_point(point),
            "sparse MemorySSA changed the selected reload recipe at {point:?}"
        );
        assert_eq!(
            full.state_valid_at_point(point),
            sparse.resolved_recipe_at_point(point).is_some(),
            "sparse MemorySSA changed reload validity at {point:?}"
        );
    }
    assert!(sparse.resolved_recipe_at_point(before_overlap).is_some());
    assert!(sparse.resolved_recipe_at_point(after_overlap).is_none());
}

#[test]
fn narrow_store_is_not_an_unproved_full_register_home() {
    let (mut func, values) = function_with_values(2);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: values[0],
        base: BaseReg::StackFrame,
        offset: 0,
        size: OpSize::S64,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 40,
        src: values[0],
        size: OpSize::S32,
    });
    block.push(MInst::Mov {
        dst: values[1],
        src: values[0],
    });
    block.push(MInst::Return);
    func.push_block(block);
    let (_, _, analysis) = analyze_function(func);

    assert!(
        analysis
            .resolved_recipe_at_point(PointUse {
                block: BlockId(0),
                instruction: 2,
                value: values[0],
            })
            .is_none()
    );
}

#[test]
fn overlapping_write_kills_a_store_backed_home() {
    let (mut func, values) = function_with_values(3);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: values[0],
        base: BaseReg::StackFrame,
        offset: 0,
        size: OpSize::S64,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 40,
        src: values[0],
        size: OpSize::S64,
    });
    block.push(MInst::LoadImm {
        dst: values[1],
        value: 7,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 43,
        src: values[1],
        size: OpSize::S8,
    });
    block.push(MInst::Mov {
        dst: values[2],
        src: values[0],
    });
    block.push(MInst::Return);
    func.push_block(block);
    let (_, _, analysis) = analyze_function(func);

    assert!(
        analysis
            .resolved_recipe_at_point(PointUse {
                block: BlockId(0),
                instruction: 4,
                value: values[0],
            })
            .is_none()
    );
}

#[test]
fn overlapping_partial_store_invalidates_recipe() {
    let (mut func, values) = function_with_values(3);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: values[0],
        base: BaseReg::SimState,
        offset: 16,
        size: OpSize::S64,
    });
    block.push(MInst::LoadImm {
        dst: values[1],
        value: 0xaa,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 19,
        src: values[1],
        size: OpSize::S8,
    });
    block.push(MInst::Mov {
        dst: values[2],
        src: values[0],
    });
    block.push(MInst::Return);
    func.push_block(block);
    let (_, _, analysis) = analyze_function(func);

    assert!(!analysis.state_valid_at_point(PointUse {
        block: BlockId(0),
        instruction: 3,
        value: values[0],
    }));
}

#[test]
fn disjoint_and_stack_stores_preserve_recipe() {
    let (mut func, values) = function_with_values(3);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: values[0],
        base: BaseReg::SimState,
        offset: 16,
        size: OpSize::S32,
    });
    block.push(MInst::LoadImm {
        dst: values[1],
        value: 7,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 24,
        src: values[1],
        size: OpSize::S64,
    });
    block.push(MInst::Store {
        base: BaseReg::StackFrame,
        offset: 0,
        src: values[1],
        size: OpSize::S64,
    });
    block.push(MInst::Mov {
        dst: values[2],
        src: values[0],
    });
    block.push(MInst::Return);
    func.push_block(block);
    let (_, _, analysis) = analyze_function(func);

    assert!(analysis.state_valid_at_point(PointUse {
        block: BlockId(0),
        instruction: 4,
        value: values[0],
    }));
}

#[test]
fn indirect_runtime_store_preserves_direct_state_recipe() {
    let (mut func, values) = function_with_values(4);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: values[0],
        base: BaseReg::SimState,
        offset: 16,
        size: OpSize::S32,
    });
    block.push(MInst::LoadImm {
        dst: values[1],
        value: 0,
    });
    block.push(MInst::LoadImm {
        dst: values[2],
        value: 7,
    });
    block.push(MInst::StorePtr {
        ptr: values[1],
        offset: 0,
        src: values[2],
        size: OpSize::S64,
    });
    block.push(MInst::Mov {
        dst: values[3],
        src: values[0],
    });
    block.push(MInst::Return);
    func.push_block(block);
    let (_, _, analysis) = analyze_function(func);

    assert!(analysis.state_valid_at_point(PointUse {
        block: BlockId(0),
        instruction: 4,
        value: values[0],
    }));
}

#[test]
fn indexed_state_store_kills_every_state_recipe() {
    let (mut func, values) = function_with_values(4);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: values[0],
        base: BaseReg::SimState,
        offset: 128,
        size: OpSize::S64,
    });
    block.push(MInst::LoadImm {
        dst: values[1],
        value: 0,
    });
    block.push(MInst::LoadImm {
        dst: values[2],
        value: 1,
    });
    block.push(MInst::StoreIndexed {
        base: BaseReg::SimState,
        offset: 0,
        index: values[1],
        src: values[2],
        size: OpSize::S8,
        alias_range: None,
    });
    block.push(MInst::Mov {
        dst: values[3],
        src: values[0],
    });
    block.push(MInst::Return);
    func.push_block(block);
    let (_, _, analysis) = analyze_function(func);

    assert!(!analysis.state_valid_at_point(PointUse {
        block: BlockId(0),
        instruction: 4,
        value: values[0],
    }));
}

#[test]
fn bounded_indexed_store_preserves_only_nonoverlapping_state_recipes() {
    fn fixture(load_offset: i32) -> (VReg, ReloadRecipeAnalysis) {
        let (mut func, values) = function_with_values(4);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: values[0],
            base: BaseReg::SimState,
            offset: load_offset,
            size: OpSize::S64,
        });
        block.push(MInst::LoadImm {
            dst: values[1],
            value: 0,
        });
        block.push(MInst::LoadImm {
            dst: values[2],
            value: 1,
        });
        block.push(MInst::StoreIndexed {
            base: BaseReg::SimState,
            offset: 16,
            index: values[1],
            src: values[2],
            size: OpSize::S8,
            alias_range: MemoryAliasRange::new(16, 64),
        });
        block.push(MInst::Mov {
            dst: values[3],
            src: values[0],
        });
        block.push(MInst::Return);
        func.push_block(block);
        let (_, _, analysis) = analyze_function(func);
        (values[0], analysis)
    }

    let (overlapping, overlapping_analysis) = fixture(32);
    assert!(!overlapping_analysis.state_valid_at_point(PointUse {
        block: BlockId(0),
        instruction: 4,
        value: overlapping,
    }));

    let (disjoint, disjoint_analysis) = fixture(128);
    assert!(disjoint_analysis.state_valid_at_point(PointUse {
        block: BlockId(0),
        instruction: 4,
        value: disjoint,
    }));
}

fn diamond(overlap_left: bool) -> (MFunction, VReg, VReg) {
    let (mut func, values) = function_with_values(4);
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: values[0],
        value: 1,
    });
    entry.push(MInst::Load {
        dst: values[1],
        base: BaseReg::SimState,
        offset: 64,
        size: OpSize::S64,
    });
    entry.push(MInst::LoadImm {
        dst: values[2],
        value: 9,
    });
    entry.push(MInst::Branch {
        cond: values[0],
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });
    let mut left = MBlock::new(BlockId(1));
    left.push(MInst::Store {
        base: BaseReg::SimState,
        offset: if overlap_left { 68 } else { 80 },
        src: values[2],
        size: OpSize::S32,
    });
    left.push(MInst::Jump { target: BlockId(3) });
    let mut right = MBlock::new(BlockId(2));
    right.push(MInst::Jump { target: BlockId(3) });
    let mut join = MBlock::new(BlockId(3));
    join.push(MInst::Mov {
        dst: values[3],
        src: values[1],
    });
    join.push(MInst::Return);
    func.blocks = vec![entry, left, right, join];
    (func, values[1], values[3])
}

#[test]
fn write_on_one_diamond_arm_invalidates_join_recipe() {
    let (func, loaded, _) = diamond(true);
    let (_, _, analysis) = analyze_function(func);
    assert!(!analysis.state_valid_at_point(PointUse {
        block: BlockId(3),
        instruction: 0,
        value: loaded,
    }));
}

#[test]
fn disjoint_write_on_one_diamond_arm_preserves_join_recipe() {
    let (func, loaded, _) = diamond(false);
    let (_, _, analysis) = analyze_function(func);
    assert!(analysis.state_valid_at_point(PointUse {
        block: BlockId(3),
        instruction: 0,
        value: loaded,
    }));
}

#[test]
fn loop_write_invalidates_header_use_on_later_iterations() {
    let (mut func, values) = function_with_values(5);
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::Load {
        dst: values[0],
        base: BaseReg::SimState,
        offset: 0,
        size: OpSize::S64,
    });
    entry.push(MInst::LoadImm {
        dst: values[1],
        value: 1,
    });
    entry.push(MInst::LoadImm {
        dst: values[2],
        value: 2,
    });
    entry.push(MInst::Jump { target: BlockId(1) });
    let mut header = MBlock::new(BlockId(1));
    header.push(MInst::Mov {
        dst: values[3],
        src: values[0],
    });
    header.push(MInst::Branch {
        cond: values[1],
        true_bb: BlockId(2),
        false_bb: BlockId(3),
    });
    let mut body = MBlock::new(BlockId(2));
    body.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 0,
        src: values[2],
        size: OpSize::S64,
    });
    body.push(MInst::Jump { target: BlockId(1) });
    let mut exit = MBlock::new(BlockId(3));
    exit.push(MInst::Mov {
        dst: values[4],
        src: values[3],
    });
    exit.push(MInst::Return);
    func.blocks = vec![entry, header, body, exit];
    let (_, _, analysis) = analyze_function(func);

    assert!(!analysis.state_valid_at_point(PointUse {
        block: BlockId(1),
        instruction: 0,
        value: values[0],
    }));
}

#[test]
fn phi_edge_use_observes_predecessor_memory_version() {
    let (mut func, values) = function_with_values(4);
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::Load {
        dst: values[0],
        base: BaseReg::SimState,
        offset: 32,
        size: OpSize::S32,
    });
    entry.push(MInst::LoadImm {
        dst: values[1],
        value: 1,
    });
    entry.push(MInst::Branch {
        cond: values[1],
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });
    let mut left = MBlock::new(BlockId(1));
    left.push(MInst::Jump { target: BlockId(3) });
    let mut right = MBlock::new(BlockId(2));
    right.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 32,
        src: values[1],
        size: OpSize::S8,
    });
    right.push(MInst::Jump { target: BlockId(3) });
    let mut join = MBlock::new(BlockId(3));
    join.phis.push(PhiNode {
        dst: values[2],
        sources: vec![(BlockId(1), values[0]), (BlockId(2), values[0])],
    });
    join.push(MInst::Mov {
        dst: values[3],
        src: values[2],
    });
    join.push(MInst::Return);
    func.blocks = vec![entry, left, right, join];
    let (_, _, analysis) = analyze_function(func);

    assert!(analysis.state_valid_on_edge(EdgeUse {
        predecessor: BlockId(1),
        successor: BlockId(3),
        value: values[0],
    }));
    assert!(!analysis.state_valid_on_edge(EdgeUse {
        predecessor: BlockId(2),
        successor: BlockId(3),
        value: values[0],
    }));
}

#[test]
fn memcopy_destination_invalidates_only_overlapping_recipe() {
    let (mut func, values) = function_with_values(3);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: values[0],
        base: BaseReg::SimState,
        offset: 100,
        size: OpSize::S16,
    });
    block.push(MInst::MemCopy {
        src_offset: 0,
        dst_offset: 101,
        byte_len: 4,
    });
    block.push(MInst::Mov {
        dst: values[1],
        src: values[0],
    });
    block.push(MInst::LoadImm {
        dst: values[2],
        value: 0,
    });
    block.push(MInst::Return);
    func.push_block(block);
    let (_, _, analysis) = analyze_function(func);
    assert!(!analysis.state_valid_at_point(PointUse {
        block: BlockId(0),
        instruction: 2,
        value: values[0],
    }));
}

#[test]
fn independent_verifier_accepts_same_version_reload() {
    let (mut func, values) = function_with_values(3);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: values[0],
        base: BaseReg::SimState,
        offset: 40,
        size: OpSize::S64,
    });
    block.push(MInst::Load {
        dst: values[1],
        base: BaseReg::SimState,
        offset: 40,
        size: OpSize::S64,
    });
    block.push(MInst::Mov {
        dst: values[2],
        src: values[1],
    });
    block.push(MInst::Return);
    func.push_block(block);
    let (func, cfg, analysis) = analyze_function(func);
    let expected = analysis.resolved_recipe(values[0]).unwrap().unwrap();

    verify_expected_materialized_reloads(
        &func,
        &cfg,
        &[ExpectedMaterializedReload {
            reload: values[1],
            expected,
            planned_use: None,
        }],
    )
    .unwrap();
}

#[test]
fn independent_verifier_keeps_original_write_identity_across_state_spills() {
    let (mut func, values) = function_with_values(3);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm {
        dst: values[0],
        value: 9,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 40,
        src: values[0],
        size: OpSize::S64,
    });
    block.push(MInst::Load {
        dst: values[1],
        base: BaseReg::SimState,
        offset: 40,
        size: OpSize::S64,
    });
    block.push(MInst::Load {
        dst: values[2],
        base: BaseReg::SimState,
        offset: 40,
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    func.push_block(block);
    let (func, _, analysis) = analyze_function(func);
    let expected = analysis.resolved_recipe(values[1]).unwrap().unwrap();

    let mut final_func = func.clone();
    let inserted = (0..40)
        .map(|index| MInst::Store {
            base: BaseReg::SimState,
            offset: 1024 + index * 8,
            src: values[0],
            size: OpSize::S64,
        })
        .collect::<Vec<_>>();
    final_func.blocks[0].insts.splice(1..1, inserted);
    let final_cfg = super::super::cfg::normalize(&mut final_func).unwrap();
    let inserted_writes = (0..40)
        .map(|ordinal| (BlockId(0), ordinal))
        .collect::<Vec<_>>();

    verify_expected_materialized_reloads_after_state_spills(
        &final_func,
        &final_cfg,
        &[ExpectedMaterializedReload {
            reload: values[2],
            expected,
            planned_use: None,
        }],
        &inserted_writes,
        &[],
    )
    .unwrap();
}

#[test]
fn independent_verifier_rejects_reaching_state_spill() {
    let (mut func, values) = function_with_values(3);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm {
        dst: values[0],
        value: 9,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 40,
        src: values[0],
        size: OpSize::S64,
    });
    block.push(MInst::Load {
        dst: values[1],
        base: BaseReg::SimState,
        offset: 40,
        size: OpSize::S64,
    });
    block.push(MInst::Load {
        dst: values[2],
        base: BaseReg::SimState,
        offset: 40,
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    func.push_block(block);
    let (func, _, analysis) = analyze_function(func);
    let expected = analysis.resolved_recipe(values[1]).unwrap().unwrap();

    let mut final_func = func.clone();
    final_func.blocks[0].insts.insert(
        2,
        MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: values[0],
            size: OpSize::S64,
        },
    );
    let final_cfg = super::super::cfg::normalize(&mut final_func).unwrap();

    let error = verify_expected_materialized_reloads_after_state_spills(
        &final_func,
        &final_cfg,
        &[ExpectedMaterializedReload {
            reload: values[2],
            expected,
            planned_use: None,
        }],
        &[(BlockId(0), 1)],
        &[],
    )
    .unwrap_err();
    assert_eq!(error.rule, "RELOAD_RECIPE.STATE_SNAPSHOT_CURRENT");
}

#[test]
fn shared_snapshot_proofs_preserve_graph_ranges_cycles_and_write_renumbering() {
    let snapshot = |ordinals: [usize; 2]| {
        let phi = SnapshotAccess::Phi(BlockId(10));
        MemorySnapshot::new(
            vec![SnapshotSegment {
                start: 0,
                end: 8,
                access: phi,
            }],
            ordinals
                .into_iter()
                .enumerate()
                .map(|(i, ordinal)| SnapshotPhi {
                    block: BlockId(10),
                    start: i as i64 * 4,
                    end: (i as i64 + 1) * 4,
                    inputs: vec![
                        (
                            BlockId(i as u32 + 1),
                            SnapshotAccess::Write {
                                block: BlockId(i as u32 + 1),
                                ordinal,
                            },
                        ),
                        (BlockId(10), phi),
                    ]
                    .into_boxed_slice(),
                })
                .collect(),
        )
    };
    let range = |snapshot: &MemorySnapshot, start, end| MemorySnapshot {
        root: vec![SnapshotSegment {
            start,
            end,
            access: SnapshotAccess::Phi(BlockId(10)),
        }]
        .into(),
        phis: Rc::clone(&snapshot.phis),
    };
    let expected = snapshot([0, 0]);
    let unchanged = snapshot([0, 0]);
    let changed = snapshot([0, 1]);
    let inserted = BTreeMap::new();
    let factorings = BTreeMap::new();
    let mut proof = SnapshotComparison::new(&inserted, &factorings);
    assert!(proof.first_difference(&expected, &unchanged).is_none());
    assert!(proof.first_difference(&expected, &unchanged).is_none());
    // Identical phi IDs in another immutable graph do not inherit proofs.
    assert!(proof.first_difference(&expected, &changed).is_some());
    assert!(
        proof
            .first_difference(&range(&expected, 0, 4), &range(&changed, 0, 4))
            .is_none()
    );
    // A proof for half a load does not validate its other bytes. Failed
    // cyclic walks also cannot publish provisional equality assumptions.
    for _ in 0..2 {
        assert!(proof.first_difference(&expected, &changed).is_some());
        assert!(
            proof
                .first_difference(&range(&expected, 4, 8), &range(&changed, 4, 8))
                .is_some()
        );
    }
    let renumbered = snapshot([1, 1]);
    let inserted = BTreeMap::from([(BlockId(1), vec![0]), (BlockId(2), vec![0])]);
    let mut renumbered_proof = SnapshotComparison::new(&inserted, &factorings);
    assert!(
        renumbered_proof
            .first_difference(&expected, &renumbered)
            .is_none()
    );
    assert!(proof.first_difference(&expected, &renumbered).is_some());
}

#[test]
fn snapshot_comparison_accepts_only_proven_write_free_phi_factoring() {
    let write = |block| SnapshotAccess::Write {
        block: BlockId(block),
        ordinal: 0,
    };
    let whole = |access| {
        vec![SnapshotSegment {
            start: 0,
            end: 8,
            access,
        }]
    };
    let phi = |block, inputs: Vec<(u32, SnapshotAccess)>| SnapshotPhi {
        block: BlockId(block),
        start: 0,
        end: 8,
        inputs: inputs
            .into_iter()
            .map(|(predecessor, access)| (BlockId(predecessor), access))
            .collect(),
    };
    let expected = MemorySnapshot::new(
        whole(SnapshotAccess::Phi(BlockId(10))),
        vec![phi(10, vec![(1, write(1)), (2, write(2)), (3, write(3))])],
    );
    let actual = MemorySnapshot::new(
        whole(SnapshotAccess::Phi(BlockId(10))),
        vec![
            phi(
                10,
                vec![(3, write(3)), (20, SnapshotAccess::Phi(BlockId(20)))],
            ),
            phi(20, vec![(1, write(1)), (2, write(2))]),
        ],
    );
    let inserted_writes = BTreeMap::new();
    assert!(!stable_memory_snapshot_matches(
        &expected,
        &actual,
        &inserted_writes,
        &BTreeMap::new(),
    ));

    let factoring = MemoryPhiFactoring {
        block: BlockId(20),
        successor: BlockId(10),
        predecessors: vec![BlockId(1), BlockId(2)].into_boxed_slice(),
    };
    let factorings = BTreeMap::from([(factoring.block, &factoring)]);
    assert!(stable_memory_snapshot_matches(
        &expected,
        &actual,
        &inserted_writes,
        &factorings,
    ));

    let folded_expected = MemorySnapshot::new(
        whole(SnapshotAccess::Phi(BlockId(10))),
        vec![phi(10, vec![(1, write(1)), (2, write(1)), (3, write(3))])],
    );
    let folded_actual = MemorySnapshot::new(
        whole(SnapshotAccess::Phi(BlockId(10))),
        vec![phi(10, vec![(3, write(3)), (20, write(1))])],
    );
    assert!(stable_memory_snapshot_matches(
        &folded_expected,
        &folded_actual,
        &inserted_writes,
        &factorings,
    ));

    let carried_phi = SnapshotAccess::Phi(BlockId(30));
    let carried_phi_equation = phi(30, vec![(4, SnapshotAccess::LiveOnEntry), (5, write(5))]);
    let carried_expected = MemorySnapshot::new(
        whole(SnapshotAccess::Phi(BlockId(10))),
        vec![
            phi(10, vec![(1, carried_phi), (2, carried_phi), (3, write(3))]),
            carried_phi_equation.clone(),
        ],
    );
    let carried_actual = MemorySnapshot::new(
        whole(SnapshotAccess::Phi(BlockId(10))),
        vec![
            phi(10, vec![(3, write(3)), (20, carried_phi)]),
            carried_phi_equation,
        ],
    );
    assert!(stable_memory_snapshot_matches(
        &carried_expected,
        &carried_actual,
        &inserted_writes,
        &factorings,
    ));

    let changed = MemorySnapshot::new(
        whole(SnapshotAccess::Phi(BlockId(10))),
        vec![
            phi(
                10,
                vec![(3, write(3)), (20, SnapshotAccess::Phi(BlockId(20)))],
            ),
            phi(20, vec![(1, write(1)), (2, write(4))]),
        ],
    );
    assert!(!stable_memory_snapshot_matches(
        &expected,
        &changed,
        &inserted_writes,
        &factorings,
    ));

    // A factored block whose phi equation is split into pieces inlines
    // piece by piece; a changed input in one piece is still rejected.
    let split_factored = |second: SnapshotAccess| {
        MemorySnapshot::new(
            whole(SnapshotAccess::Phi(BlockId(10))),
            vec![
                phi(
                    10,
                    vec![(3, write(3)), (20, SnapshotAccess::Phi(BlockId(20)))],
                ),
                SnapshotPhi {
                    block: BlockId(20),
                    start: 0,
                    end: 4,
                    inputs: vec![(BlockId(1), write(1)), (BlockId(2), write(2))].into_boxed_slice(),
                },
                SnapshotPhi {
                    block: BlockId(20),
                    start: 4,
                    end: 8,
                    inputs: vec![(BlockId(1), write(1)), (BlockId(2), second)].into_boxed_slice(),
                },
            ],
        )
    };
    assert!(stable_memory_snapshot_matches(
        &expected,
        &split_factored(write(2)),
        &inserted_writes,
        &factorings,
    ));
    assert_eq!(
        first_snapshot_difference(
            &expected,
            &split_factored(write(4)),
            &inserted_writes,
            &factorings
        ),
        Some((write(2), write(4)))
    );

    // Per-byte certificates: a difference confined to one byte of the
    // range is still a difference.
    let split_actual = MemorySnapshot::new(
        vec![
            SnapshotSegment {
                start: 0,
                end: 4,
                access: SnapshotAccess::Phi(BlockId(10)),
            },
            SnapshotSegment {
                start: 4,
                end: 8,
                access: write(3),
            },
        ],
        vec![SnapshotPhi {
            block: BlockId(10),
            start: 0,
            end: 4,
            inputs: vec![
                (BlockId(1), write(1)),
                (BlockId(2), write(2)),
                (BlockId(3), write(3)),
            ]
            .into_boxed_slice(),
        }],
    );
    assert_eq!(
        first_snapshot_difference(&expected, &split_actual, &inserted_writes, &factorings),
        Some((SnapshotAccess::Phi(BlockId(10)), write(3)))
    );
}

#[test]
fn snapshot_comparison_tracks_a_self_phi_collapsed_by_proven_factoring() {
    let old_phi = SnapshotAccess::Phi(BlockId(10));
    let new_phi = SnapshotAccess::Phi(BlockId(20));
    let write = |block| SnapshotAccess::Write {
        block: BlockId(block),
        ordinal: 0,
    };
    let snapshot = |block, inputs: Vec<(BlockId, SnapshotAccess)>| {
        MemorySnapshot::new(
            vec![SnapshotSegment {
                start: 0,
                end: 8,
                access: SnapshotAccess::Phi(block),
            }],
            vec![SnapshotPhi {
                block,
                start: 0,
                end: 8,
                inputs: inputs.into_boxed_slice(),
            }],
        )
    };
    let expected = snapshot(
        BlockId(10),
        vec![
            (BlockId(1), write(1)),
            (BlockId(2), write(2)),
            (BlockId(3), old_phi),
        ],
    );
    let actual = snapshot(
        BlockId(20),
        vec![(BlockId(1), write(1)), (BlockId(2), write(2))],
    );
    let factoring = MemoryPhiFactoring {
        block: BlockId(20),
        successor: BlockId(10),
        predecessors: vec![BlockId(1), BlockId(2)].into_boxed_slice(),
    };
    let factorings = BTreeMap::from([(factoring.block, &factoring)]);
    assert!(stable_memory_snapshot_matches(
        &expected,
        &actual,
        &BTreeMap::new(),
        &factorings
    ));
    assert!(!stable_memory_snapshot_matches(
        &expected,
        &actual,
        &BTreeMap::new(),
        &BTreeMap::new()
    ));
    // A changed input or a write on the supposedly carried edge must
    // still fail; the factoring certificate alone is not sufficient.
    let changed = snapshot(
        BlockId(20),
        vec![(BlockId(1), write(1)), (BlockId(2), write(4))],
    );
    assert_eq!(
        first_snapshot_difference(&expected, &changed, &BTreeMap::new(), &factorings),
        Some((write(2), write(4)))
    );
    let written = snapshot(
        BlockId(10),
        vec![
            (BlockId(1), write(1)),
            (BlockId(2), write(2)),
            (BlockId(3), write(3)),
        ],
    );
    assert_eq!(
        first_snapshot_difference(&written, &actual, &BTreeMap::new(), &factorings),
        Some((old_phi, new_phi))
    );
}

#[test]
fn independent_verifier_rejects_changed_input_of_same_memory_phi() {
    let (mut func, values) = function_with_values(5);
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: values[0],
        value: 1,
    });
    entry.push(MInst::LoadImm {
        dst: values[1],
        value: 11,
    });
    entry.push(MInst::LoadImm {
        dst: values[2],
        value: 22,
    });
    entry.push(MInst::Branch {
        cond: values[0],
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });

    let mut left = MBlock::new(BlockId(1));
    left.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 40,
        src: values[1],
        size: OpSize::S64,
    });
    left.push(MInst::Jump { target: BlockId(3) });

    let mut right = MBlock::new(BlockId(2));
    right.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 40,
        src: values[2],
        size: OpSize::S64,
    });
    right.push(MInst::Jump { target: BlockId(3) });

    let mut join = MBlock::new(BlockId(3));
    join.push(MInst::Load {
        dst: values[3],
        base: BaseReg::SimState,
        offset: 40,
        size: OpSize::S64,
    });
    join.push(MInst::Mov {
        dst: values[4],
        src: values[3],
    });
    join.push(MInst::Return);
    func.blocks = vec![entry, left, right, join];

    let (func, cfg, analysis) = analyze_function(func);
    let expected = analysis.resolved_recipe(values[3]).unwrap().unwrap();
    let ResolvedBase::State(StateRecipe { snapshot, .. }) = &expected.base else {
        panic!("a join load has a state recipe")
    };
    assert!(
        snapshot
            .segments()
            .iter()
            .all(|segment| segment.access == SnapshotAccess::Phi(BlockId(3)))
    );

    let mut final_func = func.clone();
    let left = final_func
        .blocks
        .iter()
        .position(|block| block.id == BlockId(1))
        .unwrap();
    final_func.blocks[left].insts.insert(
        1,
        MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: values[1],
            size: OpSize::S64,
        },
    );
    let error = verify_expected_materialized_reloads_after_state_spills(
        &final_func,
        &cfg,
        &[ExpectedMaterializedReload {
            reload: values[3],
            expected,
            planned_use: None,
        }],
        &[(BlockId(1), 1)],
        &[],
    )
    .unwrap_err();

    assert_eq!(error.rule, "RELOAD_RECIPE.STATE_SNAPSHOT_CURRENT");
}

#[test]
fn independent_verifier_rejects_stale_reload() {
    let (mut func, values) = function_with_values(3);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: values[0],
        base: BaseReg::SimState,
        offset: 40,
        size: OpSize::S64,
    });
    block.push(MInst::LoadImm {
        dst: values[1],
        value: 9,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 44,
        src: values[1],
        size: OpSize::S8,
    });
    block.push(MInst::Load {
        dst: values[2],
        base: BaseReg::SimState,
        offset: 40,
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    func.push_block(block);
    let (func, cfg, analysis) = analyze_function(func);
    let expected = analysis.resolved_recipe(values[0]).unwrap().unwrap();

    let error = verify_expected_materialized_reloads(
        &func,
        &cfg,
        &[ExpectedMaterializedReload {
            reload: values[2],
            expected,
            planned_use: None,
        }],
    )
    .unwrap_err();
    assert_eq!(error.rule, "RELOAD_RECIPE.STATE_SNAPSHOT_CURRENT");
}

#[test]
fn independent_verifier_rejects_changed_machine_width() {
    let (mut func, values) = function_with_values(2);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: values[0],
        base: BaseReg::SimState,
        offset: 40,
        size: OpSize::S64,
    });
    block.push(MInst::Load {
        dst: values[1],
        base: BaseReg::SimState,
        offset: 40,
        size: OpSize::S32,
    });
    block.push(MInst::Return);
    func.push_block(block);
    let (func, cfg, analysis) = analyze_function(func);
    let expected = analysis.resolved_recipe(values[0]).unwrap().unwrap();

    let error = verify_expected_materialized_reloads(
        &func,
        &cfg,
        &[ExpectedMaterializedReload {
            reload: values[1],
            expected,
            planned_use: None,
        }],
    )
    .unwrap_err();
    assert_eq!(error.rule, "RELOAD_RECIPE.PHYSICAL_SHAPE_MATCHES");
}
