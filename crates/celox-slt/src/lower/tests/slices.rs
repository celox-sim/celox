use super::*;

#[test]
fn static_input_slice_lowers_to_an_exact_range_load() {
    let mut arena = SLTNodeArena::new();
    let packed = arena
        .alloc(SLTNode::Input {
            variable: 10,
            signed: false,
            index: Vec::new(),
            access: BitAccess::new(100, 938),
        })
        .unwrap();
    let field = arena
        .alloc(SLTNode::Slice {
            expr: packed,
            access: BitAccess::new(33, 37),
        })
        .unwrap();

    let mut builder = SIRBuilder::new();
    SLTToSIRLowerer::new(false).lower(&mut builder, field, &arena, &mut crate::HashMap::default());
    let eu = finish_lowering(builder);
    let instructions = &eu.blocks[&eu.entry_block_id].instructions;

    assert!(matches!(
        instructions.as_slice(),
        [SIRInstruction::Load(_, 10, SIROffset::Static(133), 5)]
    ));
}

#[test]
fn pointwise_slice_lowers_only_requested_input_ranges() {
    let mut arena = SLTNodeArena::new();
    let lhs = arena
        .alloc(SLTNode::Input {
            variable: 10,
            signed: false,
            index: Vec::new(),
            access: BitAccess::new(100, 115),
        })
        .unwrap();
    let rhs = arena
        .alloc(SLTNode::Input {
            variable: 20,
            signed: false,
            index: Vec::new(),
            access: BitAccess::new(200, 215),
        })
        .unwrap();
    let bitwise = arena
        .alloc(SLTNode::Binary(lhs, BinaryOp::And, rhs))
        .unwrap();
    let field = arena
        .alloc(SLTNode::Slice {
            expr: bitwise,
            access: BitAccess::new(4, 7),
        })
        .unwrap();

    let mut builder = SIRBuilder::new();
    SLTToSIRLowerer::new(false).lower(&mut builder, field, &arena, &mut crate::HashMap::default());
    let eu = finish_lowering(builder);
    let instructions = &eu.blocks[&eu.entry_block_id].instructions;

    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        SIRInstruction::Load(_, 10, SIROffset::Static(104), 4)
    )));
    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        SIRInstruction::Load(_, 20, SIROffset::Static(204), 4)
    )));
    assert!(instructions.iter().all(|instruction| !matches!(
        instruction,
        SIRInstruction::Load(_, 10 | 20, _, width) if *width != 4
    )));
}

#[test]
fn pointwise_slice_does_not_lower_an_input_annihilated_by_zero() {
    let mut arena = SLTNodeArena::new();
    let input = arena
        .alloc(SLTNode::Input {
            variable: 10,
            signed: false,
            index: Vec::new(),
            access: BitAccess::new(100, 115),
        })
        .unwrap();
    let zero = arena
        .alloc(SLTNode::Constant(0u8.into(), 0u8.into(), 16, false))
        .unwrap();
    let bitwise = arena
        .alloc(SLTNode::Binary(input, BinaryOp::And, zero))
        .unwrap();
    let field = arena
        .alloc(SLTNode::Slice {
            expr: bitwise,
            access: BitAccess::new(4, 7),
        })
        .unwrap();

    let mut builder = SIRBuilder::new();
    SLTToSIRLowerer::new(true).lower(&mut builder, field, &arena, &mut crate::HashMap::default());
    let eu = finish_lowering(builder);
    let instructions = &eu.blocks[&eu.entry_block_id].instructions;

    assert!(matches!(
        instructions.as_slice(),
        [SIRInstruction::Imm(_, value)] if value.payload.is_zero() && value.mask.is_zero()
    ));
}

#[test]
fn static_input_slice_preserves_a_cached_snapshot() {
    let mut arena = SLTNodeArena::new();
    let packed = input(&mut arena, 10, 839);
    let field = arena
        .alloc(SLTNode::Slice {
            expr: packed,
            access: BitAccess::new(133, 133),
        })
        .unwrap();

    let lowerer = SLTToSIRLowerer::new(false);
    let mut builder = SIRBuilder::new();
    let mut cache = crate::HashMap::default();
    let snapshot = lowerer.lower(&mut builder, packed, &arena, &mut cache);
    lowerer.lower(&mut builder, field, &arena, &mut cache);
    let eu = finish_lowering(builder);
    let instructions = &eu.blocks[&eu.entry_block_id].instructions;

    assert_eq!(
        instructions
            .iter()
            .filter(|instruction| matches!(instruction, SIRInstruction::Load(..)))
            .count(),
        1
    );
    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        SIRInstruction::Load(_, 10, SIROffset::Static(0), 839)
    )));
    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        SIRInstruction::Binary(_, source, BinaryOp::Shr, _) if *source == snapshot
    )));
}

#[test]
fn region_slice_caches_shared_rmw_projections_once() {
    const UPDATES: usize = 18;

    for width in [8, 65] {
        let mut arena = SLTNodeArena::new();
        let mut previous = input(&mut arena, 10, width);
        for update in 0..UPDATES {
            let condition = input(&mut arena, 100 + update as u32, 1);
            let payload = constant(&mut arena, 1 << (update % 8), width);
            let modified = arena
                .alloc(SLTNode::Binary(previous, BinaryOp::Or, payload))
                .unwrap();
            previous = arena
                .alloc(SLTNode::Mux {
                    cond: condition,
                    then_expr: modified,
                    else_expr: previous,
                })
                .unwrap();
        }
        let bit = arena
            .alloc(SLTNode::Slice {
                expr: previous,
                access: BitAccess::new(0, 0),
            })
            .unwrap();

        for four_state in [false, true] {
            let mut builder = SIRBuilder::new();
            SLTToSIRLowerer::new(four_state).lower(
                &mut builder,
                bit,
                &arena,
                &mut crate::HashMap::default(),
            );
            let eu = finish_lowering(builder);
            let instructions = eu
                .blocks
                .values()
                .map(|block| block.instructions.len())
                .sum::<usize>();

            assert!(
                instructions < 256,
                "{width}-bit shared RMW DAG expanded to {instructions} instructions in four_state={four_state}"
            );
        }
    }
}

#[test]
fn region_slice_keeps_wide_shared_multiply_narrow() {
    const WIDTH: usize = 4096;

    let mut arena = SLTNodeArena::new();
    let lhs = input(&mut arena, 10, WIDTH);
    let rhs = input(&mut arena, 20, WIDTH);
    let shared = arena
        .alloc(SLTNode::Binary(lhs, BinaryOp::Mul, rhs))
        .unwrap();
    let ones = constant(&mut arena, 1, WIDTH);
    let first_user = arena
        .alloc(SLTNode::Binary(shared, BinaryOp::And, ones))
        .unwrap();
    let second_user = arena
        .alloc(SLTNode::Binary(shared, BinaryOp::Xor, ones))
        .unwrap();
    let first_bit = arena
        .alloc(SLTNode::Slice {
            expr: first_user,
            access: BitAccess::new(0, 0),
        })
        .unwrap();
    let second_bit = arena
        .alloc(SLTNode::Slice {
            expr: second_user,
            access: BitAccess::new(0, 0),
        })
        .unwrap();
    let root = arena
        .alloc(SLTNode::Concat(vec![(first_bit, 1), (second_bit, 1)]))
        .unwrap();

    for four_state in [false, true] {
        let mut builder = SIRBuilder::new();
        SLTToSIRLowerer::new(four_state).lower(
            &mut builder,
            root,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);

        let multiply_widths = eu
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter_map(|instruction| match instruction {
                SIRInstruction::Binary(dst, _, BinaryOp::Mul, _) => {
                    Some(eu.register_map[dst].width())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            multiply_widths,
            [1],
            "wide multiply was materialized in four_state={four_state}"
        );
    }
}

#[test]
fn region_slice_caches_shared_rmw_with_arithmetic_payloads() {
    const UPDATES: usize = 18;
    const WIDTH: usize = 4096;

    let mut arena = SLTNodeArena::new();
    let mut previous = input(&mut arena, 10, WIDTH);
    let one = constant(&mut arena, 1, WIDTH);
    for update in 0..UPDATES {
        let condition = input(&mut arena, 100 + update as u32, 1);
        let payload_input = input(&mut arena, 200 + update as u32, WIDTH);
        let payload = arena
            .alloc(SLTNode::Binary(payload_input, BinaryOp::Add, one))
            .unwrap();
        let modified = arena
            .alloc(SLTNode::Binary(previous, BinaryOp::Or, payload))
            .unwrap();
        previous = arena
            .alloc(SLTNode::Mux {
                cond: condition,
                then_expr: modified,
                else_expr: previous,
            })
            .unwrap();
    }
    let bit = arena
        .alloc(SLTNode::Slice {
            expr: previous,
            access: BitAccess::new(0, 0),
        })
        .unwrap();

    for four_state in [false, true] {
        let mut builder = SIRBuilder::new();
        SLTToSIRLowerer::new(four_state).lower(
            &mut builder,
            bit,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);
        let instructions = eu
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .collect::<Vec<_>>();

        assert!(
            instructions.len() < 512,
            "arithmetic RMW DAG expanded to {} instructions in four_state={four_state}",
            instructions.len()
        );
        assert!(instructions.iter().all(|instruction| !matches!(
            instruction,
            SIRInstruction::Binary(dst, _, BinaryOp::Add, _)
                if eu.register_map[dst].width() != 1
        )));
    }
}

#[test]
fn region_slice_annihilates_shared_zero_before_expensive_operands() {
    const WIDTH: usize = 4096;

    let mut arena = SLTNodeArena::new();
    let dividend = input(&mut arena, 10, WIDTH);
    let divisor = input(&mut arena, 20, WIDTH);
    let division = arena
        .alloc(SLTNode::Binary(dividend, BinaryOp::DivU, divisor))
        .unwrap();
    let zero = constant(&mut arena, 0, WIDTH);
    let dead = arena
        .alloc(SLTNode::Binary(division, BinaryOp::And, zero))
        .unwrap();
    let one = constant(&mut arena, 1, WIDTH);
    let first_user = arena
        .alloc(SLTNode::Binary(dead, BinaryOp::Or, one))
        .unwrap();
    let second_user = arena
        .alloc(SLTNode::Binary(dead, BinaryOp::Xor, one))
        .unwrap();
    let first_bit = arena
        .alloc(SLTNode::Slice {
            expr: first_user,
            access: BitAccess::new(0, 0),
        })
        .unwrap();
    let second_bit = arena
        .alloc(SLTNode::Slice {
            expr: second_user,
            access: BitAccess::new(0, 0),
        })
        .unwrap();
    let root = arena
        .alloc(SLTNode::Concat(vec![(first_bit, 1), (second_bit, 1)]))
        .unwrap();

    for four_state in [false, true] {
        let mut builder = SIRBuilder::new();
        SLTToSIRLowerer::new(four_state).lower(
            &mut builder,
            root,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);

        assert!(
            eu.blocks.values().all(
                |block| block.instructions.iter().all(|instruction| !matches!(
                    instruction,
                    SIRInstruction::Binary(_, _, BinaryOp::DivU, _)
                        | SIRInstruction::Load(_, 10 | 20, _, _)
                ))
            )
        );
    }
}

#[test]
fn region_slice_skips_deep_dead_constant_mux_arm() {
    const DEPTH: usize = 20_000;
    const WIDTH: usize = 65;

    let mut arena = SLTNodeArena::new();
    let condition = constant(&mut arena, 0, 1);
    let live = input(&mut arena, 10, WIDTH);
    let dead_input = input(&mut arena, 20, WIDTH);
    let dead = operation_chain(&mut arena, dead_input, BinaryOp::Xor, DEPTH, 100, WIDTH);
    let shared = arena
        .alloc(SLTNode::Mux {
            cond: condition,
            then_expr: dead,
            else_expr: live,
        })
        .unwrap();
    let one = constant(&mut arena, 1, WIDTH);
    let first_user = arena
        .alloc(SLTNode::Binary(shared, BinaryOp::And, one))
        .unwrap();
    let second_user = arena
        .alloc(SLTNode::Binary(shared, BinaryOp::Xor, one))
        .unwrap();
    let first_bit = arena
        .alloc(SLTNode::Slice {
            expr: first_user,
            access: BitAccess::new(0, 0),
        })
        .unwrap();
    let second_bit = arena
        .alloc(SLTNode::Slice {
            expr: second_user,
            access: BitAccess::new(0, 0),
        })
        .unwrap();
    let root = arena
        .alloc(SLTNode::Concat(vec![(first_bit, 1), (second_bit, 1)]))
        .unwrap();

    let mut builder = SIRBuilder::new();
    SLTToSIRLowerer::new(false).lower(&mut builder, root, &arena, &mut crate::HashMap::default());
    let eu = finish_lowering(builder);
    let instructions = eu
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .collect::<Vec<_>>();

    assert!(instructions.len() < 32);
    assert!(
        instructions
            .iter()
            .all(|instruction| !matches!(instruction, SIRInstruction::Load(_, 20, _, _)))
    );
}

#[test]
fn static_input_slice_uses_an_exact_override_range() {
    let mut arena = SLTNodeArena::new();
    let packed = input(&mut arena, 10, 16);
    let field = arena
        .alloc(SLTNode::Slice {
            expr: packed,
            access: BitAccess::new(4, 7),
        })
        .unwrap();

    let mut builder = SIRBuilder::new();
    let materialized = builder.alloc_logic(16);
    builder.emit(SIRInstruction::Imm(materialized, SIRValue::new(0xabcdu16)));
    let mut inputs = crate::HashMap::default();
    inputs.insert(VarAtomBase::new(10, 0, 15), materialized);
    SLTToSIRLowerer::new(false).lower_with_inputs(
        &mut builder,
        field,
        &arena,
        &mut crate::HashMap::default(),
        inputs,
    );
    let eu = finish_lowering(builder);
    let instructions = &eu.blocks[&eu.entry_block_id].instructions;

    assert!(
        instructions
            .iter()
            .all(|instruction| !matches!(instruction, SIRInstruction::Load(..)))
    );
    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        SIRInstruction::Binary(_, source, BinaryOp::Shr, _) if *source == materialized
    )));
}

#[test]
fn slice_uses_slice_aware_cfg_cost_and_verifies() {
    let mut arena = SLTNodeArena::new();
    let cond = input(&mut arena, 0, 1);
    let then_input = input(&mut arena, 1, 256);
    let else_input = input(&mut arena, 2, 256);
    let then_expr = operation_chain(&mut arena, then_input, BinaryOp::And, 12, 10, 256);
    let else_expr = operation_chain(&mut arena, else_input, BinaryOp::Xor, 12, 100, 256);
    let mux = arena
        .alloc(SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        })
        .unwrap();
    let slice = arena
        .alloc(SLTNode::Slice {
            expr: mux,
            access: BitAccess::new(0, 63),
        })
        .unwrap();
    let mut builder = SIRBuilder::new();
    SLTToSIRLowerer::new(false).lower(&mut builder, slice, &arena, &mut crate::HashMap::default());
    let eu = finish_lowering(builder);

    assert_eq!(branch_count(&eu), 1);
}
