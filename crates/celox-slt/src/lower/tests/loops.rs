use super::*;

#[test]
fn joint_fold_groups_share_one_counted_backedge() {
    let mut arena = SLTNodeArena::new();
    let guard = constant(&mut arena, 1, 1);
    let initial = constant(&mut arena, 0, 8);
    let one = constant(&mut arena, 1, 8);
    let previous_a = input(&mut arena, 10, 8);
    let previous_b = input(&mut arena, 11, 8);
    let update_a = arena
        .alloc(SLTNode::Binary(previous_a, BinaryOp::Add, one))
        .unwrap();
    let update_b = arena
        .alloc(SLTNode::Binary(previous_b, BinaryOp::Add, one))
        .unwrap();
    let group_a = arena
        .alloc(SLTNode::ForFoldGroup {
            loop_var: 20,
            loop_width: 8,
            loop_signed: false,
            start: BigInt::from(0),
            step: BigInt::from(1),
            trip_count: 3,
            entry_guard: guard,
            states: vec![SLTForFoldGroupState {
                target: VarAtomBase::new(10, 0, 7),
                initial,
                update: update_a,
            }],
        })
        .unwrap();
    let group_b = arena
        .alloc(SLTNode::ForFoldGroup {
            loop_var: 21,
            loop_width: 8,
            loop_signed: false,
            start: BigInt::from(0),
            step: BigInt::from(1),
            trip_count: 3,
            entry_guard: guard,
            states: vec![SLTForFoldGroupState {
                target: VarAtomBase::new(11, 0, 7),
                initial,
                update: update_b,
            }],
        })
        .unwrap();

    let mut builder = SIRBuilder::new();
    let mut cache = crate::HashMap::default();
    assert!(SLTToSIRLowerer::new(false).lower_fold_groups_jointly(
        &mut builder,
        &[group_a, group_b],
        &arena,
        &mut cache,
    ));
    let result_a = cache[&group_a];
    let result_b = cache[&group_b];
    let eu = finish_lowering(builder);

    assert_eq!(branch_count(&eu), 2, "one entry branch and one backedge");
    assert_eq!(
        eu.blocks
            .values()
            .filter(|block| matches!(
                &block.terminator,
                SIRTerminator::Branch { true_block, .. } if true_block.0 == block.id
            ))
            .count(),
        1,
    );
    let values = execute_fold_group_sir(&eu);
    assert_eq!(values[&result_a].payload, BigUint::from(3u8));
    assert_eq!(values[&result_b].payload, BigUint::from(3u8));
}

#[test]
fn joint_fold_groups_keep_all_updates_simultaneous() {
    let mut arena = SLTNodeArena::new();
    let guard = constant(&mut arena, 1, 1);
    let initial_a = constant(&mut arena, 0x12, 8);
    let initial_b = constant(&mut arena, 0x34, 8);
    let initial_c = constant(&mut arena, 7, 8);
    let previous_a = input(&mut arena, 10, 8);
    let previous_b = input(&mut arena, 11, 8);
    let previous_c = input(&mut arena, 12, 8);
    let one = constant(&mut arena, 1, 8);
    let update_c = arena
        .alloc(SLTNode::Binary(previous_c, BinaryOp::Add, one))
        .unwrap();
    let swap_group = arena
        .alloc(SLTNode::ForFoldGroup {
            loop_var: 20,
            loop_width: 8,
            loop_signed: false,
            start: BigInt::from(0),
            step: BigInt::from(1),
            trip_count: 3,
            entry_guard: guard,
            states: vec![
                SLTForFoldGroupState {
                    target: VarAtomBase::new(10, 0, 7),
                    initial: initial_a,
                    update: previous_b,
                },
                SLTForFoldGroupState {
                    target: VarAtomBase::new(11, 0, 7),
                    initial: initial_b,
                    update: previous_a,
                },
            ],
        })
        .unwrap();
    let increment_group = arena
        .alloc(SLTNode::ForFoldGroup {
            loop_var: 21,
            loop_width: 8,
            loop_signed: false,
            start: BigInt::from(0),
            step: BigInt::from(1),
            trip_count: 3,
            entry_guard: guard,
            states: vec![SLTForFoldGroupState {
                target: VarAtomBase::new(12, 0, 7),
                initial: initial_c,
                update: update_c,
            }],
        })
        .unwrap();

    let mut builder = SIRBuilder::new();
    let mut cache = crate::HashMap::default();
    assert!(SLTToSIRLowerer::new(false).lower_fold_groups_jointly(
        &mut builder,
        &[swap_group, increment_group],
        &arena,
        &mut cache,
    ));
    let swap_result = cache[&swap_group];
    let increment_result = cache[&increment_group];
    let values = execute_fold_group_sir(&finish_lowering(builder));
    assert_eq!(values[&swap_result].payload, BigUint::from(0x3412u16));
    assert_eq!(values[&increment_result].payload, BigUint::from(10u8));
}

#[test]
fn joint_fold_groups_reject_mismatched_domains_atomically() {
    let mut arena = SLTNodeArena::new();
    let guard = constant(&mut arena, 1, 1);
    let initial = constant(&mut arena, 0, 8);
    let update_a = input(&mut arena, 10, 8);
    let update_b = input(&mut arena, 11, 8);
    let make_group = |arena: &mut SLTNodeArena<u32>, loop_var, target, update, trip_count| {
        arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var,
                loop_width: 8,
                loop_signed: false,
                start: BigInt::from(0),
                step: BigInt::from(1),
                trip_count,
                entry_guard: guard,
                states: vec![SLTForFoldGroupState {
                    target: VarAtomBase::new(target, 0, 7),
                    initial,
                    update,
                }],
            })
            .unwrap()
    };
    let group_a = make_group(&mut arena, 20, 10, update_a, 3);
    let group_b = make_group(&mut arena, 21, 11, update_b, 4);

    let mut builder = SIRBuilder::new();
    let mut cache = crate::HashMap::default();
    assert!(!SLTToSIRLowerer::new(false).lower_fold_groups_jointly(
        &mut builder,
        &[group_a, group_b],
        &arena,
        &mut cache,
    ));
    assert!(cache.is_empty());
    assert_eq!(builder.block_count(), 1);
    let value = builder.alloc_bit(1, false);
    builder.emit(SIRInstruction::Imm(value, SIRValue::new(1u8)));
    let eu = finish_lowering(builder);
    assert_eq!(branch_count(&eu), 0);
    assert_eq!(eu.blocks[&BlockId(0)].instructions.len(), 1);
}

#[test]
fn joint_fold_groups_accept_pre_loop_target_initials() {
    let mut arena = SLTNodeArena::new();
    let guard = constant(&mut arena, 1, 1);
    let previous_a = input(&mut arena, 10, 8);
    let previous_b = input(&mut arena, 11, 8);
    let make_group = |arena: &mut SLTNodeArena<u32>, loop_var, target, previous| {
        arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var,
                loop_width: 8,
                loop_signed: false,
                start: BigInt::from(0),
                step: BigInt::from(1),
                trip_count: 2,
                entry_guard: guard,
                states: vec![SLTForFoldGroupState {
                    target: VarAtomBase::new(target, 0, 7),
                    initial: previous,
                    update: previous,
                }],
            })
            .unwrap()
    };
    let group_a = make_group(&mut arena, 20, 10, previous_a);
    let group_b = make_group(&mut arena, 21, 11, previous_b);

    let mut builder = SIRBuilder::new();
    let mut cache = crate::HashMap::default();
    assert!(SLTToSIRLowerer::new(false).lower_fold_groups_jointly(
        &mut builder,
        &[group_a, group_b],
        &arena,
        &mut cache,
    ));
    assert!(cache.contains_key(&group_a));
    assert!(cache.contains_key(&group_b));
    let eu = finish_lowering(builder);
    assert_eq!(branch_count(&eu), 2);
}

#[test]
fn joint_fold_groups_reject_cross_group_carried_reads() {
    let mut arena = SLTNodeArena::new();
    let guard = constant(&mut arena, 1, 1);
    let initial = constant(&mut arena, 0, 8);
    let previous_a = input(&mut arena, 10, 8);
    let group_a = arena
        .alloc(SLTNode::ForFoldGroup {
            loop_var: 20,
            loop_width: 8,
            loop_signed: false,
            start: BigInt::from(0),
            step: BigInt::from(1),
            trip_count: 2,
            entry_guard: guard,
            states: vec![SLTForFoldGroupState {
                target: VarAtomBase::new(10, 0, 7),
                initial,
                update: previous_a,
            }],
        })
        .unwrap();
    let group_b = arena
        .alloc(SLTNode::ForFoldGroup {
            loop_var: 21,
            loop_width: 8,
            loop_signed: false,
            start: BigInt::from(0),
            step: BigInt::from(1),
            trip_count: 2,
            entry_guard: guard,
            states: vec![SLTForFoldGroupState {
                target: VarAtomBase::new(11, 0, 7),
                initial,
                update: previous_a,
            }],
        })
        .unwrap();

    let mut builder = SIRBuilder::new();
    let mut cache = crate::HashMap::default();
    assert!(!SLTToSIRLowerer::new(false).lower_fold_groups_jointly(
        &mut builder,
        &[group_a, group_b],
        &arena,
        &mut cache,
    ));
    assert!(cache.is_empty());
    assert_eq!(builder.block_count(), 1);
}

#[test]
fn four_state_joint_fold_groups_preserve_unknown_guard_per_result() {
    let mut arena = SLTNodeArena::new();
    let guard = arena
        .alloc(SLTNode::Constant(
            BigUint::from(0u8),
            BigUint::from(1u8),
            1,
            false,
        ))
        .unwrap();
    let initial_a = constant(&mut arena, 0x12, 8);
    let initial_b = constant(&mut arena, 0x34, 8);
    let update_a = input(&mut arena, 10, 8);
    let update_b = input(&mut arena, 11, 8);
    let make_group = |arena: &mut SLTNodeArena<u32>, loop_var, target, initial, update| {
        arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var,
                loop_width: 8,
                loop_signed: false,
                start: BigInt::from(0),
                step: BigInt::from(1),
                trip_count: 2,
                entry_guard: guard,
                states: vec![SLTForFoldGroupState {
                    target: VarAtomBase::new(target, 0, 7),
                    initial,
                    update,
                }],
            })
            .unwrap()
    };
    let group_a = make_group(&mut arena, 20, 10, initial_a, update_a);
    let group_b = make_group(&mut arena, 21, 11, initial_b, update_b);

    let mut builder = SIRBuilder::new();
    let mut cache = crate::HashMap::default();
    assert!(SLTToSIRLowerer::new(true).lower_fold_groups_jointly(
        &mut builder,
        &[group_a, group_b],
        &arena,
        &mut cache,
    ));
    let result_a = cache[&group_a];
    let result_b = cache[&group_b];
    let eu = finish_lowering(builder);
    assert_eq!(branch_count(&eu), 2);
    assert_eq!(
        instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Mux(..))),
        2,
    );
    let values = execute_fold_group_sir(&eu);
    assert_eq!(values[&result_a].mask, BigUint::from(0xffu8));
    assert_eq!(values[&result_b].mask, BigUint::from(0xffu8));
}

#[test]
fn for_fold_group_lowers_swap_updates_as_one_counted_cfg() {
    let mut arena = SLTNodeArena::new();
    let guard = constant(&mut arena, 1, 1);
    let initial_a = constant(&mut arena, 0x12, 8);
    let initial_b = constant(&mut arena, 0x34, 8);
    let previous_a = input(&mut arena, 10, 8);
    let previous_b = input(&mut arena, 11, 8);
    let target_a = VarAtomBase::new(10, 0, 7);
    let target_b = VarAtomBase::new(11, 0, 7);
    let group = arena
        .alloc(SLTNode::ForFoldGroup {
            loop_var: 20,
            loop_width: 8,
            loop_signed: false,
            start: BigInt::from(2),
            step: BigInt::from(3),
            trip_count: 3,
            entry_guard: guard,
            states: vec![
                SLTForFoldGroupState {
                    target: target_a,
                    initial: initial_a,
                    update: previous_b,
                },
                SLTForFoldGroupState {
                    target: target_b,
                    initial: initial_b,
                    update: previous_a,
                },
            ],
        })
        .unwrap();

    let mut builder = SIRBuilder::new();
    let result = SLTToSIRLowerer::new(false).lower(
        &mut builder,
        group,
        &arena,
        &mut crate::HashMap::default(),
    );
    let eu = finish_lowering(builder);

    assert_eq!(eu.register_map[&result].width(), 16);
    assert_eq!(
        execute_fold_group_sir(&eu)[&result].payload,
        BigUint::from(0x3412u16),
        "three simultaneous swaps leave the first state in the MSBs"
    );
    assert_eq!(branch_count(&eu), 2, "entry guard plus counted backedge");
    assert_eq!(
        instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Concat(..))),
        1,
        "the final states must be packed once at the common exit"
    );
    assert_eq!(
        instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Mux(..))),
        0
    );
    assert!(
        eu.blocks
            .values()
            .all(|block| !matches!(block.terminator, SIRTerminator::Error(_)))
    );

    let body = eu
        .blocks
        .values()
        .find(|block| {
            block.params.len() == 4 && matches!(block.terminator, SIRTerminator::Branch { .. })
        })
        .expect("counted body block");
    assert_eq!(eu.register_map[&body.params[0]].width(), 2);
    let SIRTerminator::Branch { true_block, .. } = &body.terminator else {
        unreachable!()
    };
    assert_eq!(true_block.0, body.id);
    let backedge_args = &true_block.1;
    assert_eq!(backedge_args[2], body.params[3]);
    assert_eq!(backedge_args[3], body.params[2]);

    let exit = eu
        .blocks
        .values()
        .find(|block| {
            block.params.len() == 2
                && block
                    .instructions
                    .iter()
                    .any(|inst| matches!(inst, SIRInstruction::Concat(..)))
        })
        .expect("packed common exit");
    let packed_args = exit
        .instructions
        .iter()
        .find_map(|inst| match inst {
            SIRInstruction::Concat(_, args) => Some(args),
            _ => None,
        })
        .unwrap();
    assert_eq!(packed_args, &exit.params, "state zero occupies the MSBs");
}

#[test]
fn for_fold_group_executes_exactly_one_and_three_iterations() {
    for (trip_count, expected) in [(1usize, 1u8), (3, 3)] {
        let mut arena = SLTNodeArena::new();
        let guard = constant(&mut arena, 1, 1);
        let initial = constant(&mut arena, 0, 8);
        let previous = input(&mut arena, 10, 8);
        let one = constant(&mut arena, 1, 8);
        let update = arena
            .alloc(SLTNode::Binary(previous, BinaryOp::Add, one))
            .unwrap();
        let group = arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var: 20,
                loop_width: 8,
                loop_signed: false,
                start: BigInt::from(0),
                step: BigInt::from(1),
                trip_count,
                entry_guard: guard,
                states: vec![SLTForFoldGroupState {
                    target: VarAtomBase::new(10, 0, 7),
                    initial,
                    update,
                }],
            })
            .unwrap();

        let mut builder = SIRBuilder::new();
        let result = SLTToSIRLowerer::new(false).lower(
            &mut builder,
            group,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);

        assert_eq!(
            execute_fold_group_sir(&eu)[&result].payload,
            BigUint::from(expected),
            "trip_count={trip_count}"
        );
        assert!(
            eu.blocks
                .values()
                .all(|block| !matches!(block.terminator, SIRTerminator::Error(_)))
        );
    }
}

#[test]
fn for_fold_group_inherits_outer_inputs_with_inner_partial_state_priority() {
    let mut arena = SLTNodeArena::new();
    let guard = input(&mut arena, 20, 1);
    let initial = constant(&mut arena, 0x12, 8);
    let outer_wide = input(&mut arena, 10, 16);
    let update = arena
        .alloc(SLTNode::Slice {
            expr: outer_wide,
            access: BitAccess::new(0, 7),
        })
        .unwrap();
    let group = arena
        .alloc(SLTNode::ForFoldGroup {
            loop_var: 30,
            loop_width: 8,
            loop_signed: false,
            start: BigInt::from(0),
            step: BigInt::from(1),
            trip_count: 2,
            entry_guard: guard,
            states: vec![SLTForFoldGroupState {
                target: VarAtomBase::new(10, 0, 7),
                initial,
                update,
            }],
        })
        .unwrap();

    let mut builder = SIRBuilder::new();
    let outer_value = builder.alloc_logic(16);
    builder.emit(SIRInstruction::Imm(outer_value, SIRValue::new(0xabcdu16)));
    let outer_guard = builder.alloc_logic(1);
    builder.emit(SIRInstruction::Imm(outer_guard, SIRValue::new(1u8)));
    let mut inputs = crate::HashMap::default();
    inputs.insert(VarAtomBase::new(10, 0, 15), outer_value);
    inputs.insert(VarAtomBase::new(20, 0, 0), outer_guard);

    let result = SLTToSIRLowerer::new(false).lower_with_inputs(
        &mut builder,
        group,
        &arena,
        &mut crate::HashMap::default(),
        inputs,
    );
    let eu = finish_lowering(builder);

    assert_eq!(
        execute_fold_group_sir(&eu)[&result].payload,
        BigUint::from(0x12u8),
        "the inner carried low byte must override the overlapping outer full value"
    );
    assert!(eu.blocks.values().all(|block| {
        block
            .instructions
            .iter()
            .all(|instruction| !matches!(instruction, SIRInstruction::Load(..)))
    }));
}

#[test]
fn for_fold_group_dynamic_array_read_stays_a_narrow_load_under_env() {
    let mut arena = SLTNodeArena::new();
    let guard = constant(&mut arena, 1, 1);
    let initial = constant(&mut arena, 0, 8);
    let loop_index = input(&mut arena, 20, 8);
    let raw_array = arena
        .alloc(SLTNode::Input {
            variable: 30,
            signed: false,
            index: vec![crate::SLTIndex {
                node: loop_index,
                stride: 8,
                kind: crate::SLTIndexKind::Packed,
            }],
            access: BitAccess::new(0, 255),
        })
        .unwrap();
    let update = arena
        .alloc(SLTNode::Slice {
            expr: raw_array,
            access: BitAccess::new(0, 7),
        })
        .unwrap();
    let group = arena
        .alloc(SLTNode::ForFoldGroup {
            loop_var: 20,
            loop_width: 8,
            loop_signed: false,
            start: BigInt::from(0),
            step: BigInt::from(1),
            trip_count: 3,
            entry_guard: guard,
            states: vec![SLTForFoldGroupState {
                target: VarAtomBase::new(10, 0, 7),
                initial,
                update,
            }],
        })
        .unwrap();

    let mut builder = SIRBuilder::new();
    SLTToSIRLowerer::new(false).lower(&mut builder, group, &arena, &mut crate::HashMap::default());
    let eu = finish_lowering(builder);
    let load_widths = eu
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            SIRInstruction::Load(_, variable, SIROffset::Dynamic(_), width) if *variable == 30 => {
                Some(*width)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(load_widths, vec![8]);
}

#[test]
fn constant_unpacked_index_lowers_to_a_direct_logical_offset() {
    let mut arena = SLTNodeArena::new();
    let index = constant(&mut arena, 3, 8);
    let element = arena
        .alloc(SLTNode::Input {
            variable: 30,
            signed: false,
            index: vec![crate::SLTIndex {
                node: index,
                stride: 14,
                kind: crate::SLTIndexKind::Unpacked { element_width: 14 },
            }],
            access: BitAccess::new(4, 7),
        })
        .unwrap();

    let mut builder = SIRBuilder::new();
    SLTToSIRLowerer::new(false).lower(
        &mut builder,
        element,
        &arena,
        &mut crate::HashMap::default(),
    );
    let eu = finish_lowering(builder);

    assert!(eu.blocks.values().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                SIRInstruction::Load(_, 30, SIROffset::Static(46), 4)
            )
        })
    }));
    assert!(eu.blocks.values().all(|block| {
        block.instructions.iter().all(|instruction| {
            !matches!(
                instruction,
                SIRInstruction::Load(_, 30, SIROffset::Dynamic(_) | SIROffset::Element { .. }, _)
            )
        })
    }));
}

#[test]
fn for_fold_group_captures_invariant_work_on_the_true_entry_edge() {
    let mut arena = SLTNodeArena::new();
    let guard = input(&mut arena, 40, 1);
    let initial = constant(&mut arena, 0, 8);
    let previous = input(&mut arena, 10, 8);
    let external = input(&mut arena, 30, 8);
    let two = constant(&mut arena, 2, 8);
    let invariant = arena
        .alloc(SLTNode::Binary(external, BinaryOp::Add, two))
        .unwrap();
    let update = arena
        .alloc(SLTNode::Binary(previous, BinaryOp::Add, invariant))
        .unwrap();
    let group = arena
        .alloc(SLTNode::ForFoldGroup {
            loop_var: 20,
            loop_width: 8,
            loop_signed: false,
            start: BigInt::from(0),
            step: BigInt::from(1),
            trip_count: 3,
            entry_guard: guard,
            states: vec![SLTForFoldGroupState {
                target: VarAtomBase::new(10, 0, 7),
                initial,
                update,
            }],
        })
        .unwrap();

    let mut builder = SIRBuilder::new();
    SLTToSIRLowerer::new(false).lower(&mut builder, group, &arena, &mut crate::HashMap::default());
    let eu = finish_lowering(builder);
    let SIRTerminator::Branch { true_block, .. } = &eu.blocks[&BlockId(0)].terminator else {
        panic!("entry must branch around the recovered loop")
    };
    assert!(
        true_block.1.is_empty(),
        "the true edge must enter the capture block"
    );
    let enter = &eu.blocks[&true_block.0];
    let SIRTerminator::Jump(body, _) = &enter.terminator else {
        panic!("capture block must jump to the counted body")
    };
    let body = *body;
    assert!(enter.instructions.iter().any(|instruction| matches!(
        instruction,
        SIRInstruction::Load(_, variable, SIROffset::Static(0), 8) if *variable == 30
    )));
    assert!(
        eu.blocks[&body]
            .instructions
            .iter()
            .all(|instruction| !matches!(
                instruction,
                SIRInstruction::Load(_, variable, _, _) if *variable == 30
            ))
    );
    assert!(matches!(
        &eu.blocks[&body].terminator,
        SIRTerminator::Branch { true_block: (target, _), .. } if *target == body
    ));
}

#[test]
fn for_fold_group_does_not_capture_invariant_division() {
    let mut arena = SLTNodeArena::new();
    let guard = input(&mut arena, 40, 1);
    let initial = constant(&mut arena, 0, 8);
    let previous = input(&mut arena, 10, 8);
    let numerator = input(&mut arena, 30, 8);
    let denominator = input(&mut arena, 31, 8);
    let quotient = arena
        .alloc(SLTNode::Binary(numerator, BinaryOp::DivU, denominator))
        .unwrap();
    let update = arena
        .alloc(SLTNode::Binary(previous, BinaryOp::Add, quotient))
        .unwrap();
    let group = arena
        .alloc(SLTNode::ForFoldGroup {
            loop_var: 20,
            loop_width: 8,
            loop_signed: false,
            start: BigInt::from(0),
            step: BigInt::from(1),
            trip_count: 3,
            entry_guard: guard,
            states: vec![SLTForFoldGroupState {
                target: VarAtomBase::new(10, 0, 7),
                initial,
                update,
            }],
        })
        .unwrap();

    let mut builder = SIRBuilder::new();
    SLTToSIRLowerer::new(false).lower(&mut builder, group, &arena, &mut crate::HashMap::default());
    let eu = finish_lowering(builder);
    assert_eq!(
        instruction_count(&eu, |instruction| matches!(
            instruction,
            SIRInstruction::Binary(_, _, BinaryOp::DivU, _)
        )),
        1,
    );
    let division_block = eu
        .blocks
        .values()
        .find(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(instruction, SIRInstruction::Binary(_, _, BinaryOp::DivU, _))
            })
        })
        .unwrap();
    assert!(matches!(
        &division_block.terminator,
        SIRTerminator::Branch { true_block: (target, _), .. }
            if *target == division_block.id
    ));
}

#[test]
fn false_for_fold_group_entry_guard_skips_all_updates() {
    let mut arena = SLTNodeArena::new();
    let guard = constant(&mut arena, 0, 1);
    let initial = constant(&mut arena, 0x5a, 8);
    let previous = input(&mut arena, 10, 8);
    let one = constant(&mut arena, 1, 8);
    let update = arena
        .alloc(SLTNode::Binary(previous, BinaryOp::Add, one))
        .unwrap();
    let group = arena
        .alloc(SLTNode::ForFoldGroup {
            loop_var: 20,
            loop_width: 8,
            loop_signed: false,
            start: BigInt::from(0),
            step: BigInt::from(1),
            trip_count: 3,
            entry_guard: guard,
            states: vec![SLTForFoldGroupState {
                target: VarAtomBase::new(10, 0, 7),
                initial,
                update,
            }],
        })
        .unwrap();

    let mut builder = SIRBuilder::new();
    let result = SLTToSIRLowerer::new(false).lower(
        &mut builder,
        group,
        &arena,
        &mut crate::HashMap::default(),
    );
    let eu = finish_lowering(builder);

    assert_eq!(branch_count(&eu), 2);
    assert_eq!(
        execute_fold_group_sir(&eu)[&result].payload,
        BigUint::from(0x5au8)
    );
    let SIRTerminator::Branch { false_block, .. } = &eu.blocks[&BlockId(0)].terminator else {
        panic!("entry must branch around the loop")
    };
    assert_eq!(false_block.1.len(), 1);
    let skipped_to = &eu.blocks[&false_block.0];
    assert_eq!(skipped_to.params.len(), 1);
    assert!(
        skipped_to
            .instructions
            .iter()
            .any(|inst| matches!(inst, SIRInstruction::Concat(..)))
    );
}

#[test]
fn four_state_for_fold_group_branches_then_applies_one_packed_mux() {
    let mut arena = SLTNodeArena::new();
    let guard = arena
        .alloc(SLTNode::Constant(
            BigUint::from(0u8),
            BigUint::from(1u8),
            1,
            false,
        ))
        .unwrap();
    let initial_a = constant(&mut arena, 1, 8);
    let initial_b = constant(&mut arena, 2, 8);
    let previous_a = input(&mut arena, 10, 8);
    let previous_b = input(&mut arena, 11, 8);
    let group = arena
        .alloc(SLTNode::ForFoldGroup {
            loop_var: 20,
            loop_width: 8,
            loop_signed: false,
            start: BigInt::from(0),
            step: BigInt::from(1),
            trip_count: 2,
            entry_guard: guard,
            states: vec![
                SLTForFoldGroupState {
                    target: VarAtomBase::new(10, 0, 7),
                    initial: initial_a,
                    update: previous_b,
                },
                SLTForFoldGroupState {
                    target: VarAtomBase::new(11, 0, 7),
                    initial: initial_b,
                    update: previous_a,
                },
            ],
        })
        .unwrap();

    let mut builder = SIRBuilder::new();
    let result = SLTToSIRLowerer::new(true).lower(
        &mut builder,
        group,
        &arena,
        &mut crate::HashMap::default(),
    );
    let eu = finish_lowering(builder);

    assert_eq!(
        execute_fold_group_sir(&eu)[&result].mask,
        BigUint::from(0xffffu16),
        "an unknown entry guard must make the packed result all-X"
    );

    assert_eq!(
        branch_count(&eu),
        2,
        "the guard value plane controls loop entry"
    );
    assert_eq!(
        instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Concat(..))),
        2,
        "initial and branch-selected candidates are each packed once"
    );
    assert_eq!(
        instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Mux(..))),
        1,
        "one packed Mux must restore X/Z guard semantics"
    );

    let entry_guard = match &eu.blocks[&BlockId(0)].terminator {
        SIRTerminator::Branch { cond, .. } => *cond,
        other => panic!("expected entry guard branch, got {other:?}"),
    };
    let mux_guard = eu
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .find_map(|inst| match inst {
            SIRInstruction::Mux(_, cond, _, _) => Some(*cond),
            _ => None,
        })
        .unwrap();
    assert!(eu.blocks[&BlockId(0)].instructions.iter().any(|inst| {
        matches!(
            inst,
            SIRInstruction::Unary(dst, UnaryOp::ToTwoState, src)
                if *dst == entry_guard && *src == mux_guard
        )
    }));
}

#[test]
fn signed_for_fold_group_uses_a_direct_conditional_backedge() {
    let mut arena = SLTNodeArena::new();
    let guard = constant(&mut arena, 1, 1);
    let initial = constant(&mut arena, 0, 8);
    let previous = input(&mut arena, 10, 8);
    let loop_value = arena
        .alloc(SLTNode::Input {
            variable: 20,
            signed: true,
            index: vec![],
            access: BitAccess::new(0, 7),
        })
        .unwrap();
    let update = arena
        .alloc(SLTNode::Binary(previous, BinaryOp::Add, loop_value))
        .unwrap();
    let group = arena
        .alloc(SLTNode::ForFoldGroup {
            loop_var: 20,
            loop_width: 8,
            loop_signed: true,
            start: BigInt::from(-1),
            step: BigInt::from(-2),
            trip_count: 3,
            entry_guard: guard,
            states: vec![SLTForFoldGroupState {
                target: VarAtomBase::new(10, 0, 7),
                initial,
                update,
            }],
        })
        .unwrap();

    let mut builder = SIRBuilder::new();
    let result = SLTToSIRLowerer::new(false).lower(
        &mut builder,
        group,
        &arena,
        &mut crate::HashMap::default(),
    );
    let eu = finish_lowering(builder);

    assert_eq!(
        execute_fold_group_sir(&eu)[&result].payload,
        BigUint::from(0xf7u16),
        "three iterations must observe -1, -3, and -5 exactly"
    );

    let body = eu
        .blocks
        .values()
        .find(|block| {
            block.params.len() == 3 && matches!(block.terminator, SIRTerminator::Branch { .. })
        })
        .expect("counted body block");
    assert!(body.instructions.iter().any(|inst| matches!(
        inst,
        SIRInstruction::Binary(_, lhs, BinaryOp::Add, rhs)
            if *lhs == body.params[2] && *rhs == body.params[1]
    )));

    let step_reg = body
        .instructions
        .iter()
        .find_map(|inst| match inst {
            SIRInstruction::Binary(_, lhs, BinaryOp::Add, rhs) if *lhs == body.params[1] => {
                Some(*rhs)
            }
            _ => None,
        })
        .expect("loop-value step");
    let step_payload = eu
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .find_map(|inst| match inst {
            SIRInstruction::Imm(dst, value) if *dst == step_reg => Some(&value.payload),
            _ => None,
        })
        .unwrap();
    assert_eq!(step_payload, &BigUint::from(0xfeu16));
    let SIRTerminator::Branch { true_block, .. } = &body.terminator else {
        unreachable!()
    };
    assert_eq!(true_block.0, body.id);
    assert!(eu.blocks.values().all(|block| {
        !matches!(&block.terminator, SIRTerminator::Jump(target, _) if *target == body.id)
    }));
    assert!(
        eu.blocks
            .values()
            .all(|block| !matches!(block.terminator, SIRTerminator::Error(_)))
    );
}
