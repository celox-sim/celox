use super::*;

#[test]
fn deep_common_nodes_preserve_cached_and_scoped_values() {
    let mut arena = SLTNodeArena::new();
    let cond = input(&mut arena, 0, 1);
    let source = input(&mut arena, 1, 8);
    let zero = constant(&mut arena, 0, 8);
    let one = constant(&mut arena, 1, 8);
    let mut root = source;
    for _ in 0..1024 {
        let updated = arena
            .alloc(SLTNode::Binary(root, BinaryOp::Add, one))
            .unwrap();
        root = arena
            .alloc(SLTNode::Mux {
                cond,
                then_expr: updated,
                else_expr: zero,
            })
            .unwrap();
    }
    let projected = arena
        .alloc(SLTNode::Slice {
            expr: root,
            access: BitAccess::new(0, 3),
        })
        .unwrap();
    for (root, expected) in [(root, 0x5au8), (projected, 0xau8)] {
        for scoped in [false, true] {
            let lowerer = SLTToSIRLowerer::new(true);
            let mut builder = SIRBuilder::new();
            let mut cache = crate::HashMap::default();
            let result = if scoped {
                let mut inputs = crate::HashMap::default();
                for (variable, width, value) in [(0, 1, 1u8), (1, 8, 0x5a)] {
                    let reg = builder.alloc_bit(width, false);
                    builder.emit(SIRInstruction::Imm(
                        reg,
                        SIRValue::new_four_state(value, 0u8),
                    ));
                    inputs.insert(
                        VarAtomBase {
                            id: variable,
                            access: BitAccess::new(0, width - 1),
                        },
                        reg,
                    );
                }
                let reg = lowerer.lower_with_inputs(&mut builder, root, &arena, &mut cache, inputs);
                assert!(
                    cache.is_empty(),
                    "fold-local values cannot populate the global cache"
                );
                reg
            } else {
                lowerer.lower(&mut builder, root, &arena, &mut cache)
            };
            let eu = finish_lowering(builder);
            let memory = [(0, 1u8), (1, 0x5a)]
                .into_iter()
                .map(|(var, value)| {
                    (
                        var,
                        TestSIRValue {
                            payload: value.into(),
                            mask: 0u8.into(),
                        },
                    )
                })
                .collect();
            let values = execute_fold_group_sir_with_memory(&eu, &memory);
            assert_eq!(
                values[&result],
                TestSIRValue {
                    payload: expected.into(),
                    mask: 0u8.into(),
                }
            );
        }
    }
}

#[test]
fn deep_signedness_query_uses_heap_descent() {
    let mut arena = SLTNodeArena::<u32>::new();
    let mut root = arena
        .alloc(SLTNode::Input {
            variable: 0,
            signed: true,
            index: vec![],
            access: BitAccess::new(0, 7),
        })
        .unwrap();
    let one = arena
        .alloc(SLTNode::Constant(1u8.into(), 0u8.into(), 8, true))
        .unwrap();
    for _ in 0..16_384 {
        root = arena
            .alloc(SLTNode::Binary(root, BinaryOp::Add, one))
            .unwrap();
    }
    assert!(SLTToSIRLowerer::new(false).get_bound_signed(root, &arena));
}

#[test]
fn floating_worklist_preserves_register_types_and_full_width_before_projection() {
    use celox_design::{FloatBinaryOp, FloatPrecision, FloatUnaryOp};
    let mut arena = SLTNodeArena::new();
    let left = input(&mut arena, 0, 32);
    let right = input(&mut arena, 1, 32);
    let sum = arena
        .alloc(SLTNode::Binary(
            left,
            BinaryOp::Float {
                op: FloatBinaryOp::Add,
                precision: FloatPrecision::F32,
            },
            right,
        ))
        .unwrap();
    let signed = arena
        .alloc(SLTNode::Unary(
            UnaryOp::Float {
                op: FloatUnaryOp::ToSigned {
                    width: 8,
                    round: true,
                },
                precision: FloatPrecision::F32,
            },
            sum,
        ))
        .unwrap();
    let unsigned = arena
        .alloc(SLTNode::Unary(
            UnaryOp::Float {
                op: FloatUnaryOp::ToUnsigned {
                    width: 8,
                    round: false,
                },
                precision: FloatPrecision::F32,
            },
            sum,
        ))
        .unwrap();
    let comparison = arena
        .alloc(SLTNode::Binary(
            left,
            BinaryOp::Float {
                op: FloatBinaryOp::Lt,
                precision: FloatPrecision::F32,
            },
            right,
        ))
        .unwrap();
    let projection = arena
        .alloc(SLTNode::Slice {
            expr: sum,
            access: BitAccess::new(0, 15),
        })
        .unwrap();
    let literal_left = constant(&mut arena, u64::from(1.5f32.to_bits()), 32);
    let literal_right = constant(&mut arena, u64::from(2.0f32.to_bits()), 32);
    let literal_sum = arena
        .alloc(SLTNode::Binary(
            literal_left,
            BinaryOp::Float {
                op: FloatBinaryOp::Add,
                precision: FloatPrecision::F32,
            },
            literal_right,
        ))
        .unwrap();
    for four_state in [false, true] {
        for scoped in [false, true] {
            for (root, width, is_signed) in [
                (sum, 32, false),
                (signed, 8, true),
                (unsigned, 8, false),
                (comparison, 1, false),
            ] {
                let lowerer = SLTToSIRLowerer::new(four_state);
                let mut builder = SIRBuilder::new();
                let mut cache = crate::HashMap::default();
                let result = if scoped {
                    let mut inputs = crate::HashMap::default();
                    for (id, payload) in [(0, 1.5f32.to_bits()), (1, 2.0f32.to_bits())] {
                        let register = builder.alloc_bit(32, false);
                        builder.emit(SIRInstruction::Imm(register, SIRValue::new(payload)));
                        inputs.insert(VarAtomBase::new(id, 0, 31), register);
                    }
                    let result =
                        lowerer.lower_with_inputs(&mut builder, root, &arena, &mut cache, inputs);
                    assert!(cache.is_empty());
                    result
                } else {
                    lowerer.lower(&mut builder, root, &arena, &mut cache)
                };
                assert_eq!(
                    builder.register(&result),
                    &RegisterType::Bit {
                        width,
                        signed: is_signed
                    }
                );
                assert_eq!(lowerer.get_bound_signed(root, &arena), is_signed);
                finish_lowering(builder);
            }
        }
        let lowerer = SLTToSIRLowerer::new(four_state);
        let mut builder = SIRBuilder::new();
        let mut cache = crate::HashMap::default();
        let result = lowerer.lower(&mut builder, projection, &arena, &mut cache);
        assert_eq!(builder.register(&result).width(), 16);
        let unit = finish_lowering(builder);
        let sum_register = unit
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .find_map(|instruction| {
                if let SIRInstruction::Binary(result, _, BinaryOp::Float { .. }, _) = instruction {
                    Some(result)
                } else {
                    None
                }
            })
            .expect("projection must compute the full float before selecting payload bits");
        assert_eq!(
            unit.register_map[sum_register],
            RegisterType::Bit {
                width: 32,
                signed: false
            }
        );
        assert!(
            lowerer
                .projected_mask_constant(literal_sum, BitAccess::new(0, 15), &arena)
                .is_none()
        );
    }
}
