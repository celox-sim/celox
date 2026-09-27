use super::*;

fn guarded_lane_concat(
    arena: &mut SLTNodeArena<u32>,
    lanes: usize,
    ungated_lane: Option<usize>,
) -> (NodeId, NodeId, NodeId) {
    let valid = input(arena, 10_000, 1);
    let is_store = input(arena, 10_001, 1);
    let guard = arena
        .alloc(SLTNode::Binary(valid, BinaryOp::LogicAnd, is_store))
        .unwrap();
    let threshold = constant(arena, 0x8000_0000_0000_0000, 64);
    let mut parts = Vec::with_capacity(lanes);
    let mut first_predicate = None;
    for lane in 0..lanes {
        let source = input(arena, 11_000 + lane as u32, 64);
        let expensive = operation_chain(arena, source, BinaryOp::Add, 6, 3, 64);
        let predicate = arena
            .alloc(SLTNode::Binary(expensive, BinaryOp::GeU, threshold))
            .unwrap();
        first_predicate.get_or_insert(predicate);
        let value = if ungated_lane == Some(lane) {
            predicate
        } else {
            arena
                .alloc(SLTNode::Binary(guard, BinaryOp::LogicAnd, predicate))
                .unwrap()
        };
        parts.push((value, 1));
    }
    let root = arena.alloc(SLTNode::Concat(parts)).unwrap();
    (root, guard, first_predicate.unwrap())
}

#[test]
fn common_zero_controller_guards_rob_sized_lane_concat() {
    let mut arena = SLTNodeArena::new();
    let (root, guard, first_predicate) = guarded_lane_concat(&mut arena, 32, None);
    let lowerer = SLTToSIRLowerer::new(false);
    let mut builder = SIRBuilder::new();
    let mut cache = crate::HashMap::default();
    lowerer.reset_cost_cache(root, &arena, &cache, true);
    assert_eq!(
        lowerer
            .guarded_concat_plan(root, &arena, &cache)
            .expect("ROB-sized guarded scan must be profitable")
            .guard,
        guard,
        "the maximal compound guard must dominate its individual leaves"
    );
    let result = lowerer.lower(&mut builder, root, &arena, &mut cache);

    assert_eq!(cache.get(&root), Some(&result));
    assert!(
        cache.contains_key(&guard),
        "the compound valid/store guard must dominate the outlined region"
    );
    assert!(
        !cache.contains_key(&first_predicate),
        "true-only values must be rolled back at the merge"
    );

    let eu = finish_lowering(builder);
    assert_eq!(branch_count(&eu), 1, "one guard, not one branch per lane");
    assert_eq!(eu.blocks.len(), 4);
    let entry = &eu.blocks[&BlockId(0)];
    let SIRTerminator::Branch {
        true_block,
        false_block,
        ..
    } = &entry.terminator
    else {
        panic!("guarded concat entry must branch")
    };
    assert_eq!(
        entry
            .instructions
            .iter()
            .filter(|inst| matches!(inst, SIRInstruction::Load(..)))
            .count(),
        2,
        "only valid and is_store may be loaded before the branch"
    );
    assert_eq!(
        eu.blocks[&true_block.0]
            .instructions
            .iter()
            .filter(|inst| matches!(inst, SIRInstruction::Load(..)))
            .count(),
        32,
        "all lane inputs belong to the selected arm"
    );
    let false_instructions = &eu.blocks[&false_block.0].instructions;
    assert_eq!(false_instructions.len(), 1);
    assert!(matches!(
        &false_instructions[0],
        SIRInstruction::Imm(_, value) if value.payload.is_zero() && value.mask.is_zero()
    ));
    let merge = eu
        .blocks
        .values()
        .find(|block| block.params.contains(&result))
        .expect("guarded concat result must be a merge parameter");
    assert_eq!(eu.register_map[&merge.params[0]].width(), 32);
}

#[test]
fn guarded_concat_cfg_matches_eager_two_state_truth_table() {
    const LANES: usize = 4;
    let mut arena = SLTNodeArena::new();
    let (root, _, _) = guarded_lane_concat(&mut arena, LANES, None);
    let mut builder = SIRBuilder::new();
    let result = SLTToSIRLowerer::new(false).lower(
        &mut builder,
        root,
        &arena,
        &mut crate::HashMap::default(),
    );
    let eu = finish_lowering(builder);
    assert_eq!(branch_count(&eu), 1);

    for valid in [false, true] {
        for is_store in [false, true] {
            for predicates in 0u8..(1 << LANES) {
                let mut memory = crate::HashMap::from_iter([
                    (
                        10_000,
                        TestSIRValue {
                            payload: u8::from(valid).into(),
                            mask: 0u8.into(),
                        },
                    ),
                    (
                        10_001,
                        TestSIRValue {
                            payload: u8::from(is_store).into(),
                            mask: 0u8.into(),
                        },
                    ),
                ]);
                for lane in 0..LANES {
                    let predicate = predicates & (1 << lane) != 0;
                    memory.insert(
                        11_000 + lane as u32,
                        TestSIRValue {
                            payload: if predicate {
                                BigUint::from(0x8000_0000_0000_0000u64)
                            } else {
                                BigUint::from(0u8)
                            },
                            mask: BigUint::from(0u8),
                        },
                    );
                }
                let actual = &execute_fold_group_sir_with_memory(&eu, &memory)[&result];
                let mut expected = 0u8;
                for lane in 0..LANES {
                    expected <<= 1;
                    expected |= u8::from(valid && is_store && predicates & (1 << lane) != 0);
                }
                assert_eq!(actual.payload, BigUint::from(expected));
                assert!(actual.mask.is_zero());
            }
        }
    }
}

#[test]
fn common_zero_controller_rejects_one_ungated_lane() {
    let mut arena = SLTNodeArena::new();
    let (root, _, _) = guarded_lane_concat(&mut arena, 32, Some(17));
    let mut builder = SIRBuilder::new();
    SLTToSIRLowerer::new(false).lower(&mut builder, root, &arena, &mut crate::HashMap::default());
    let eu = finish_lowering(builder);

    assert_eq!(branch_count(&eu), 0);
    assert_eq!(eu.blocks.len(), 1);
}

#[test]
fn guarded_concat_recomputes_true_only_value_after_merge() {
    let mut arena = SLTNodeArena::new();
    let (root, guard, first_predicate) = guarded_lane_concat(&mut arena, 32, None);
    let lowerer = SLTToSIRLowerer::new(false);
    let mut builder = SIRBuilder::new();
    let mut cache = crate::HashMap::default();
    lowerer.lower(&mut builder, root, &arena, &mut cache);
    assert!(cache.contains_key(&guard));
    assert!(!cache.contains_key(&first_predicate));

    let merge_block = builder.current_block();
    let predicate = lowerer.lower(&mut builder, first_predicate, &arena, &mut cache);
    assert_eq!(cache.get(&first_predicate), Some(&predicate));
    let eu = finish_lowering(builder);
    assert!(eu.verify_result().is_ok());
    assert!(eu.blocks[&merge_block].instructions.iter().any(|inst| {
        matches!(
            inst,
            SIRInstruction::Binary(dst, _, BinaryOp::GeU, _) if *dst == predicate
        )
    }));
}

#[test]
fn four_state_common_zero_controller_stays_eager() {
    let mut arena = SLTNodeArena::new();
    let (root, _, first_predicate) = guarded_lane_concat(&mut arena, 32, None);
    let mut builder = SIRBuilder::new();
    let mut cache = crate::HashMap::default();
    SLTToSIRLowerer::new(true).lower(&mut builder, root, &arena, &mut cache);
    assert!(cache.contains_key(&first_predicate));
    let eu = finish_lowering(builder);

    assert_eq!(branch_count(&eu), 0);
    assert_eq!(eu.blocks.len(), 1);
    assert_eq!(
        instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Load(..))),
        34
    );
}

#[test]
fn four_state_guarded_concat_retains_logical_and_mask_semantics() {
    const LANES: usize = 4;
    let mut arena = SLTNodeArena::new();
    let (root, _, _) = guarded_lane_concat(&mut arena, LANES, None);
    let mut builder = SIRBuilder::new();
    let result = SLTToSIRLowerer::new(true).lower(
        &mut builder,
        root,
        &arena,
        &mut crate::HashMap::default(),
    );
    let eu = finish_lowering(builder);
    assert_eq!(branch_count(&eu), 0);

    // Veryl encodes X=(payload 0, mask 1), Z=(payload 1, mask 1).
    for (guard_payload, guard_mask) in [(0u8, 0u8), (1, 0), (0, 1), (1, 1)] {
        for predicates in 0u8..(1 << LANES) {
            let mut memory = crate::HashMap::from_iter([
                (
                    10_000,
                    TestSIRValue {
                        payload: guard_payload.into(),
                        mask: guard_mask.into(),
                    },
                ),
                (
                    10_001,
                    TestSIRValue {
                        payload: 1u8.into(),
                        mask: 0u8.into(),
                    },
                ),
            ]);
            for lane in 0..LANES {
                let predicate = predicates & (1 << lane) != 0;
                memory.insert(
                    11_000 + lane as u32,
                    TestSIRValue {
                        payload: if predicate {
                            BigUint::from(0x8000_0000_0000_0000u64)
                        } else {
                            BigUint::from(0u8)
                        },
                        mask: BigUint::from(0u8),
                    },
                );
            }

            let actual = &execute_fold_group_sir_with_memory(&eu, &memory)[&result];
            let mut expected_payload = 0u8;
            let mut expected_mask = 0u8;
            for lane in 0..LANES {
                expected_payload <<= 1;
                expected_mask <<= 1;
                let predicate = predicates & (1 << lane) != 0;
                if guard_mask == 0 {
                    expected_payload |= u8::from(guard_payload != 0 && predicate);
                } else if predicate {
                    expected_mask |= 1;
                }
            }
            assert_eq!(actual.payload, BigUint::from(expected_payload));
            assert_eq!(actual.mask, BigUint::from(expected_mask));
        }
    }
}

#[test]
fn cheap_common_zero_controller_stays_eager() {
    let mut arena = SLTNodeArena::new();
    let valid = input(&mut arena, 0, 1);
    let is_store = input(&mut arena, 1, 1);
    let guard = arena
        .alloc(SLTNode::Binary(valid, BinaryOp::LogicAnd, is_store))
        .unwrap();
    let payload = input(&mut arena, 2, 1);
    let lane = arena
        .alloc(SLTNode::Binary(guard, BinaryOp::LogicAnd, payload))
        .unwrap();
    let root = arena.alloc(SLTNode::Concat(vec![(lane, 1)])).unwrap();
    let mut builder = SIRBuilder::new();
    SLTToSIRLowerer::new(false).lower(&mut builder, root, &arena, &mut crate::HashMap::default());
    let eu = finish_lowering(builder);

    assert_eq!(branch_count(&eu), 0);
}

#[test]
fn zero_controller_analysis_uses_iterative_postorder_on_deep_dag() {
    let mut arena = SLTNodeArena::new();
    let valid = input(&mut arena, 0, 1);
    let is_store = input(&mut arena, 1, 1);
    let guard = arena
        .alloc(SLTNode::Binary(valid, BinaryOp::LogicAnd, is_store))
        .unwrap();
    let mut parts = Vec::new();
    for lane in 0..2 {
        let mut payload = input(&mut arena, 100 + lane, 1);
        for _ in 0..20_000 {
            payload = arena
                .alloc(SLTNode::Unary(UnaryOp::Ident, payload))
                .unwrap();
        }
        let gated = arena
            .alloc(SLTNode::Binary(guard, BinaryOp::LogicAnd, payload))
            .unwrap();
        parts.push((gated, 1));
    }
    let root = arena.alloc(SLTNode::Concat(parts)).unwrap();
    let lowerer = SLTToSIRLowerer::new(false);
    let cache = crate::HashMap::default();
    lowerer.reset_cost_cache(root, &arena, &cache, true);

    let plan = lowerer
        .guarded_concat_plan(root, &arena, &cache)
        .expect("deep guarded concat must be analyzed without recursion");
    assert_eq!(plan.guard, guard);
}
