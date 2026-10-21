use super::*;

#[test]
fn computed_mask_projection_skips_only_known_zero_bits() {
    for four_state in [false, true] {
        for selected_unknown in [false, true] {
            let mut arena = SLTNodeArena::new();
            let source = input(&mut arena, 0, 512);
            let zero = constant(&mut arena, 0, 512);
            let dead = arena
                .alloc(SLTNode::Binary(source, BinaryOp::DivU, zero))
                .unwrap();
            // The actual partial-write mask shape is ~(1 << offset), rather
            // than a literal constant. Keep an unknown outside the selected
            // bit to check that only its projected mask matters.
            let base = arena
                .alloc(SLTNode::Constant(
                    1u8.into(),
                    if selected_unknown {
                        1u8.into()
                    } else {
                        2u8.into()
                    },
                    512,
                    false,
                ))
                .unwrap();
            let offset = constant(&mut arena, 300, 64);
            let shifted = arena
                .alloc(SLTNode::Binary(base, BinaryOp::Shl, offset))
                .unwrap();
            let mask = arena
                .alloc(SLTNode::Unary(UnaryOp::BitNot, shifted))
                .unwrap();
            let cleared = arena
                .alloc(SLTNode::Binary(dead, BinaryOp::And, mask))
                .unwrap();
            let bit = arena
                .alloc(SLTNode::Slice {
                    expr: cleared,
                    access: BitAccess::new(300, 300),
                })
                .unwrap();
            let mut builder = SIRBuilder::new();
            let result = SLTToSIRLowerer::new(four_state).lower(
                &mut builder,
                bit,
                &arena,
                &mut crate::HashMap::default(),
            );
            let eu = finish_lowering(builder);
            assert_eq!(
                instruction_count(&eu, |inst| matches!(
                    inst,
                    SIRInstruction::Binary(_, _, BinaryOp::DivU, _)
                )),
                usize::from(selected_unknown)
            );
            if !selected_unknown {
                let values = execute_fold_group_sir(&eu);
                assert_eq!(
                    values[&result],
                    TestSIRValue {
                        payload: BigUint::zero(),
                        mask: BigUint::zero(),
                    }
                );
            }
        }
    }
}

#[test]
fn constant_mask_analysis_uses_heap_and_bounds_shift_before_allocation() {
    let mut arena = SLTNodeArena::<u32>::new();
    let mut root = constant(&mut arena, 1, 8);
    for _ in 0..16_384 {
        root = arena.alloc(SLTNode::Unary(UnaryOp::Ident, root)).unwrap();
    }
    root = arena.alloc(SLTNode::Unary(UnaryOp::BitNot, root)).unwrap();
    let lowerer = SLTToSIRLowerer::new(true);
    assert_eq!(
        lowerer.projected_mask_constant(root, BitAccess::new(0, 7), &arena),
        Some((254u16.into(), 0u8.into()))
    );
    let huge = arena
        .alloc(SLTNode::Constant(
            BigUint::from(1u8) << 128usize,
            BigUint::zero(),
            129,
            false,
        ))
        .unwrap();
    let shifted = arena
        .alloc(SLTNode::Binary(root, BinaryOp::Shl, huge))
        .unwrap();
    assert_eq!(
        lowerer.projected_mask_constant(shifted, BitAccess::new(0, 7), &arena),
        Some((0u8.into(), 0u8.into()))
    );
}

#[test]
fn computed_constant_projection_emits_one_immediate() {
    let mut arena = SLTNodeArena::new();
    let base = constant(&mut arena, 1, 512);
    // Actual array-mask geometry multiplies a signed32-bit positive index
    // by an unsigned64-bit stride before shifting the full-width mask.
    let index = arena
        .alloc(SLTNode::Constant(300u16.into(), 0u8.into(), 32, true))
        .unwrap();
    let stride = constant(&mut arena, 1, 64);
    let offset = arena
        .alloc(SLTNode::Binary(index, BinaryOp::Mul, stride))
        .unwrap();
    let shifted = arena
        .alloc(SLTNode::Binary(base, BinaryOp::Shl, offset))
        .unwrap();
    let mask = arena
        .alloc(SLTNode::Unary(UnaryOp::BitNot, shifted))
        .unwrap();
    let field = arena
        .alloc(SLTNode::Slice {
            expr: mask,
            access: BitAccess::new(299, 301),
        })
        .unwrap();
    for four_state in [false, true] {
        let mut builder = SIRBuilder::new();
        let result = SLTToSIRLowerer::new(four_state).lower(
            &mut builder,
            field,
            &arena,
            &mut crate::HashMap::default(),
        );
        let eu = finish_lowering(builder);
        assert_eq!(eu.blocks[&eu.entry_block_id].instructions.len(), 1);
        let values = execute_fold_group_sir(&eu);
        assert_eq!(
            values[&result],
            TestSIRValue {
                payload: 5u8.into(),
                mask: 0u8.into()
            }
        );
    }
}

#[test]
fn mixed_width_mask_arithmetic_requires_an_unambiguous_extension() {
    for index in [254u32, 0x8000_0000] {
        let mut arena = SLTNodeArena::<u32>::new();
        let narrow = arena
            .alloc(SLTNode::Constant(index.into(), 0u8.into(), 32, true))
            .unwrap();
        let stride = constant(&mut arena, 1, 64);
        let offset = arena
            .alloc(SLTNode::Binary(narrow, BinaryOp::Mul, stride))
            .unwrap();
        let actual = SLTToSIRLowerer::new(false).projected_mask_constant(
            offset,
            BitAccess::new(0, 63),
            &arena,
        );
        let expected: Option<(BigUint, BigUint)> =
            (index == 254).then(|| (index.into(), 0u8.into()));
        assert_eq!(actual, expected);
    }
}

#[test]
fn constant_projection_preserves_signed_conversion_and_mixed_width_lowering() {
    let mut arena = SLTNodeArena::<u32>::new();
    let signed = arena
        .alloc(SLTNode::Constant(15u8.into(), 0u8.into(), 4, true))
        .unwrap();
    let converted = arena
        .alloc(SLTNode::Unary(UnaryOp::ToTwoState, signed))
        .unwrap();
    let low = arena
        .alloc(SLTNode::Slice {
            expr: converted,
            access: BitAccess::new(0, 3),
        })
        .unwrap();
    let one = constant(&mut arena, 1, 8);
    let sum = arena
        .alloc(SLTNode::Binary(low, BinaryOp::Add, one))
        .unwrap();
    let result = arena
        .alloc(SLTNode::Slice {
            expr: sum,
            access: BitAccess::new(0, 7),
        })
        .unwrap();
    let lowerer = SLTToSIRLowerer::new(false);
    assert_eq!(
        lowerer.projected_mask_constant(converted, BitAccess::new(0, 3), &arena),
        None
    );
    assert_eq!(
        lowerer.projected_mask_constant(sum, BitAccess::new(0, 7), &arena),
        None
    );
    let mut builder = SIRBuilder::new();
    lowerer.lower(&mut builder, result, &arena, &mut crate::HashMap::default());
    let eu = finish_lowering(builder);
    assert!(
        eu.blocks
            .values()
            .flat_map(|block| &block.instructions)
            .any(|inst| {
                matches!(inst, SIRInstruction::Unary(dst, UnaryOp::ToTwoState, _)
            if matches!(eu.register_map[dst], RegisterType::Bit { width: 4, signed: true }))
            })
    );
    assert_eq!(
        instruction_count(&eu, |inst| matches!(
            inst,
            SIRInstruction::Binary(_, _, BinaryOp::Add, _)
        )),
        1
    );
}
