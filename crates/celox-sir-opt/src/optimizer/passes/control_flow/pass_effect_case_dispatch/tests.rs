use super::*;
use crate::ir::{AbsoluteAddr, InstanceId, SIROffset, STABLE_REGION};
use celox_design::StateObjectId as VarId;

fn bit(width: usize) -> RegisterType {
    RegisterType::Bit {
        width,
        signed: false,
    }
}

fn address() -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr::from_absolute_addr(
        STABLE_REGION,
        AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: VarId::from_raw(0),
        },
    )
}

#[test]
fn path_local_exit_preserves_the_merge_and_bypasses_the_final_dispatch() {
    let selector = RegisterId(0);
    let outer = RegisterId(1);
    let lhs = RegisterId(2);
    let rhs = RegisterId(3);
    let zero = RegisterId(4);
    let one = RegisterId(5);
    let guard_zero = RegisterId(6);
    let merged = RegisterId(7);
    let origin = BasicBlock {
        id: BlockId(0),
        params: vec![selector, outer, lhs, rhs],
        instructions: vec![
            SIRInstruction::Imm(zero, SIRValue::new(0u8)),
            SIRInstruction::Imm(one, SIRValue::new(1u8)),
            SIRInstruction::Binary(guard_zero, selector, crate::ir::BinaryOp::Eq, zero),
        ],
        terminator: SIRTerminator::Branch {
            cond: outer,
            true_block: (BlockId(1), Vec::new()),
            false_block: (BlockId(2), Vec::new()),
        },
    };
    let true_arm = BasicBlock {
        id: BlockId(1),
        params: Vec::new(),
        instructions: Vec::new(),
        terminator: SIRTerminator::Jump(BlockId(3), vec![lhs]),
    };
    let false_arm = BasicBlock {
        id: BlockId(2),
        params: Vec::new(),
        instructions: Vec::new(),
        terminator: SIRTerminator::Jump(BlockId(3), vec![rhs]),
    };
    let merge = BasicBlock {
        id: BlockId(3),
        params: vec![merged],
        instructions: Vec::new(),
        terminator: SIRTerminator::Jump(BlockId(4), Vec::new()),
    };
    let sink = BasicBlock {
        id: BlockId(4),
        params: Vec::new(),
        instructions: vec![SIRInstruction::Store(
            address(),
            SIROffset::Static(0),
            2,
            lhs,
            Vec::new(),
            Vec::new(),
        )],
        terminator: SIRTerminator::Jump(BlockId(5), Vec::new()),
    };
    let continuation = BasicBlock {
        id: BlockId(5),
        params: Vec::new(),
        instructions: Vec::new(),
        terminator: SIRTerminator::Return,
    };
    let source = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [
            (BlockId(0), origin),
            (BlockId(1), true_arm),
            (BlockId(2), false_arm),
            (BlockId(3), merge),
            (BlockId(4), sink),
            (BlockId(5), continuation),
        ]
        .into_iter()
        .collect(),
        register_map: [
            (selector, bit(2)),
            (outer, bit(1)),
            (lhs, bit(2)),
            (rhs, bit(2)),
            (zero, bit(2)),
            (one, bit(2)),
            (guard_zero, bit(1)),
            (merged, bit(2)),
        ]
        .into_iter()
        .collect(),
    };
    let recipe = |selected_case, source| ExecutableCaseRecipe {
        selected_case,
        source,
        clone_order: Vec::new(),
        aliases: BTreeMap::new(),
        known_values: BTreeMap::new(),
    };
    let plan = EffectCaseRewritePlan {
        origin: BlockId(0),
        selector,
        explicit_cases: vec![BigUint::from(0u8), BigUint::from(1u8)],
        sinks: vec![
            super::super::control_region_feasibility::EffectSinkDispatchPlan {
                sink: (BlockId(4), 0),
                continuation: BlockId(5),
                cases: vec![recipe(Some(BigUint::from(1u8)), rhs), recipe(None, lhs)],
            },
        ],
        path_local_exits: vec![
            super::super::control_region_feasibility::PathLocalEffectExitPlan {
                sink: (BlockId(4), 0),
                continuation: BlockId(5),
                insertion_block: BlockId(3),
                guard: guard_zero,
                recipe: recipe(Some(BigUint::from(0u8)), merged),
            },
        ],
        estimated_saving: 100,
    };
    let mut rewritten = source.clone();

    apply_plan(&source, &mut rewritten, &plan).expect("the closed plan must apply");

    rewritten
        .verify_result()
        .expect("rewritten SIR must verify");
    assert!(matches!(
        rewritten.blocks[&BlockId(3)].terminator,
        SIRTerminator::Branch { cond, .. } if cond == guard_zero
    ));
    assert!(matches!(
        rewritten.blocks[&BlockId(4)].terminator,
        SIRTerminator::Switch { ref cases, .. } if cases.len() == 1
    ));
    assert_eq!(
        rewritten
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| matches!(instruction, SIRInstruction::Store(..)))
            .count(),
        3
    );
}
// Exercise the real planner, including its profitability threshold, rather
// than supplying an already-approved rewrite plan to apply_plan.
fn path_local_exit_unit() -> ExecutionUnit<RegionedAbsoluteAddr> {
    use crate::ir::{BinaryOp, UnaryOp};
    let r = RegisterId;
    let mut origin = vec![
        SIRInstruction::Imm(r(5), SIRValue::new(0u8)),
        SIRInstruction::Imm(r(6), SIRValue::new(1u8)),
        SIRInstruction::Binary(r(7), r(0), BinaryOp::Eq, r(5)),
        SIRInstruction::Unary(r(8), UnaryOp::ToTwoState, r(7)),
        SIRInstruction::Binary(r(9), r(0), BinaryOp::Eq, r(6)),
    ];
    let mut register_map = (0..13)
        .map(|i| (r(i), bit(if [1, 7, 8, 9].contains(&i) { 1 } else { 2 })))
        .collect::<HashMap<_, _>>();
    // Expensive alternatives make case dispatch profitable.
    for (base, op) in [(100, BinaryOp::Add), (200, BinaryOp::Sub)] {
        for i in 0..16 {
            register_map.insert(r(base + i), bit(2));
            origin.push(SIRInstruction::Binary(
                r(base + i),
                if i == 0 { r(4) } else { r(base + i - 1) },
                op,
                r(4),
            ));
        }
    }
    origin.push(SIRInstruction::Mux(r(10), r(9), r(115), r(215)));
    let block = |id, params, instructions, terminator| {
        (
            BlockId(id),
            BasicBlock {
                id: BlockId(id),
                params,
                instructions,
                terminator,
            },
        )
    };
    ExecutionUnit {
        entry_block_id: BlockId(0),
        register_map,
        blocks: [
            block(
                0,
                (0..5).map(r).collect(),
                origin,
                SIRTerminator::Branch {
                    cond: r(8),
                    true_block: (BlockId(6), vec![]),
                    false_block: (BlockId(5), vec![]),
                },
            ),
            block(
                6,
                vec![],
                vec![],
                SIRTerminator::Branch {
                    cond: r(1),
                    true_block: (BlockId(1), vec![]),
                    false_block: (BlockId(2), vec![]),
                },
            ),
            block(
                1,
                vec![],
                vec![],
                SIRTerminator::Jump(BlockId(3), vec![r(2)]),
            ),
            block(
                2,
                vec![],
                vec![],
                SIRTerminator::Jump(BlockId(3), vec![r(3)]),
            ),
            block(
                3,
                vec![r(11)],
                vec![],
                SIRTerminator::Jump(BlockId(8), vec![]),
            ),
            block(
                8,
                vec![],
                vec![],
                SIRTerminator::Jump(BlockId(4), vec![r(11)]),
            ),
            block(
                5,
                vec![],
                vec![],
                SIRTerminator::Jump(BlockId(4), vec![r(10)]),
            ),
            block(
                4,
                vec![r(12)],
                vec![SIRInstruction::Store(
                    address(),
                    SIROffset::Static(0),
                    2,
                    r(12),
                    vec![],
                    vec![],
                )],
                SIRTerminator::Jump(BlockId(7), vec![]),
            ),
            block(7, vec![], vec![], SIRTerminator::Return),
        ]
        .into_iter()
        .collect(),
    }
}

fn check_dispatch_semantics(
    original: &ExecutionUnit<RegionedAbsoluteAddr>,
    rewritten: &ExecutionUnit<RegionedAbsoluteAddr>,
) {
    use crate::optimizer::passes::load_motion_tests::evaluate;
    for selector in 0..4 {
        for outer in 0..2 {
            let inputs = [selector, outer, 2, 3, 1];
            assert_eq!(
                evaluate(original, &inputs, HashMap::default()),
                evaluate(rewritten, &inputs, HashMap::default()),
                "selector={selector}, outer={outer}"
            );
        }
    }
}

#[test]
fn path_local_exit_keeps_effects_on_the_route_to_the_sink() {
    for id in [3, 8] {
        let mut unit = path_local_exit_unit();
        let mut other = address();
        other.instance_id = InstanceId(1);
        unit.blocks
            .get_mut(&BlockId(id))
            .unwrap()
            .instructions
            .push(SIRInstruction::Store(
                other,
                SIROffset::Static(0),
                2,
                RegisterId(11),
                vec![],
                vec![],
            ));
        let original = unit.clone();
        run(&mut unit);
        check_dispatch_semantics(&original, &unit);
    }
}

#[test]
fn path_local_exit_requires_every_route_to_reach_the_sink() {
    let mut unit = path_local_exit_unit();
    unit.blocks.get_mut(&BlockId(8)).unwrap().terminator = SIRTerminator::Branch {
        cond: RegisterId(1),
        true_block: (BlockId(4), vec![RegisterId(11)]),
        false_block: (BlockId(7), vec![]),
    };
    let original = unit.clone();
    run(&mut unit);
    check_dispatch_semantics(&original, &unit);
}

#[test]
fn path_local_exit_rejects_non_store_effects_on_the_route() {
    let effects = vec![
        SIRInstruction::Commit(address(), address(), SIROffset::Static(0), 2, vec![]),
        SIRInstruction::RuntimeEvent {
            site_id: 0,
            args: vec![RegisterId(11)],
        },
        SIRInstruction::ExternCall {
            dst: None,
            func: 0,
            args: vec![],
        },
        SIRInstruction::Call {
            function: crate::ir::FunctionId(0),
            frame: RegisterId(4),
            args: vec![],
            outputs: vec![],
            dst: None,
            effects: std::sync::Arc::new(crate::ir::CallEffects::default()),
        },
    ];
    for id in [3, 8] {
        for effect in &effects {
            let mut unit = path_local_exit_unit();
            unit.blocks
                .get_mut(&BlockId(id))
                .unwrap()
                .instructions
                .push(effect.clone());
            // Check the planner directly: rejecting invalid rewritten SIR
            // would hide an unsound plan for an otherwise valid host call.
            assert!(
                plan_best_effect_case_dispatch(&unit).is_none(),
                "block={id}, {effect:?}"
            );
        }
    }
}

#[test]
fn path_local_exit_dispatches_an_effect_free_route_to_the_sink() {
    let mut unit = path_local_exit_unit();
    unit.verify_result().unwrap();
    let original = unit.clone();
    assert_eq!(
        run(&mut unit)
            .expect("safe dispatch must still apply")
            .path_local_exits,
        1
    );
    check_dispatch_semantics(&original, &unit);
}

#[test]
fn path_local_exit_rejects_cycles_and_error_routes() {
    for terminator in [
        SIRTerminator::Error(1),
        SIRTerminator::Jump(BlockId(8), vec![]),
    ] {
        let mut unit = path_local_exit_unit();
        unit.blocks.get_mut(&BlockId(8)).unwrap().terminator = SIRTerminator::Branch {
            cond: RegisterId(1),
            true_block: (BlockId(4), vec![RegisterId(11)]),
            false_block: (BlockId(9), vec![]),
        };
        unit.blocks.insert(
            BlockId(9),
            BasicBlock {
                id: BlockId(9),
                params: vec![],
                instructions: vec![],
                terminator,
            },
        );
        assert!(run(&mut unit).is_none());
    }
}
