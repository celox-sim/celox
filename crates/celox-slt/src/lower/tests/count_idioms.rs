use super::super::count_idioms::{
    match_slt_priority_count, resolve_slt_extended_bit, unwrap_slt_one_bit_procedural_truth,
};
use super::*;

#[derive(Clone, Copy, Debug)]
enum ConditionalPriorityCorruption {
    StageGate(usize),
    SeedGate,
    SeedDefault,
    StageFallback(usize),
    BitOrder(usize),
    ValueOrder(usize),
}

fn corrupted_conditional_priority(
    corruption: ConditionalPriorityCorruption,
) -> (SLTNodeArena<u32>, NodeId) {
    let width = 8;
    let result_width = UnaryOp::CountLeadingZeros.result_width(width);
    let mut arena = SLTNodeArena::new();
    let source = input(&mut arena, 0, width);
    let gate = input(&mut arena, 1, 1);
    let other_gate = input(&mut arena, 2, 1);
    let fallback = input(&mut arena, 3, result_width);
    let other_fallback = input(&mut arena, 4, result_width);
    let sentinel = constant(&mut arena, width as u64, result_width);
    let other_default = constant(&mut arena, width as u64 - 1, result_width);
    let seed_gate = if matches!(corruption, ConditionalPriorityCorruption::SeedGate) {
        other_gate
    } else {
        gate
    };
    let seed_default = if matches!(corruption, ConditionalPriorityCorruption::SeedDefault) {
        other_default
    } else {
        sentinel
    };
    let mut acc = arena
        .alloc(SLTNode::Mux {
            cond: seed_gate,
            then_expr: seed_default,
            else_expr: fallback,
        })
        .unwrap();

    for stage in 0..width {
        let source_bit = if matches!(
            corruption,
            ConditionalPriorityCorruption::BitOrder(corrupt_stage)
                if corrupt_stage == stage
        ) {
            width - 1 - ((stage + 1) % width)
        } else {
            width - 1 - stage
        };
        let bit = arena
            .alloc(SLTNode::Slice {
                expr: source,
                access: BitAccess::new(source_bit, source_bit),
            })
            .unwrap();
        let unmatched = arena
            .alloc(SLTNode::Binary(acc, BinaryOp::Eq, sentinel))
            .unwrap();
        let write = arena
            .alloc(SLTNode::Binary(bit, BinaryOp::LogicAnd, unmatched))
            .unwrap();
        let selected_value = if matches!(
            corruption,
            ConditionalPriorityCorruption::ValueOrder(corrupt_stage)
                if corrupt_stage == stage
        ) {
            (stage + 1) % width
        } else {
            stage
        };
        let value = constant(&mut arena, selected_value as u64, result_width);
        let stage_gate = if matches!(
            corruption,
            ConditionalPriorityCorruption::StageGate(corrupt_stage)
                if corrupt_stage == stage
        ) {
            other_gate
        } else {
            gate
        };
        let stage_fallback = if matches!(
            corruption,
            ConditionalPriorityCorruption::StageFallback(corrupt_stage)
                if corrupt_stage == stage
        ) {
            other_fallback
        } else {
            acc
        };
        let candidate = arena
            .alloc(SLTNode::Mux {
                cond: stage_gate,
                then_expr: value,
                else_expr: stage_fallback,
            })
            .unwrap();
        acc = arena
            .alloc(SLTNode::Mux {
                cond: write,
                then_expr: candidate,
                else_expr: acc,
            })
            .unwrap();
    }
    (arena, acc)
}

fn assert_conditional_priority_rejected(corruption: ConditionalPriorityCorruption) {
    let (arena, root) = corrupted_conditional_priority(corruption);
    assert!(
        match_slt_priority_count(root, &arena).is_none(),
        "conditionally seeded priority chain with {corruption:?} must not match"
    );
}

#[test]
fn sequential_priority_index_becomes_clz_and_subtract() {
    let mut arena = SLTNodeArena::new();
    let mut acc = constant(&mut arena, u64::MAX, 64);
    for index in 0..8 {
        let cond = input(&mut arena, index, 1);
        let value = constant(&mut arena, index as u64, 64);
        acc = arena
            .alloc(SLTNode::Mux {
                cond,
                then_expr: value,
                else_expr: acc,
            })
            .unwrap();
    }

    let mut builder = SIRBuilder::new();
    SLTToSIRLowerer::new(false).lower(&mut builder, acc, &arena, &mut crate::HashMap::default());
    let eu = finish_lowering(builder);

    assert_eq!(
        instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Mux(..))),
        0
    );
    assert_eq!(
        instruction_count(&eu, |inst| matches!(
            inst,
            SIRInstruction::Unary(_, UnaryOp::CountLeadingZeros, _)
        )),
        1
    );
    assert_eq!(
        instruction_count(&eu, |inst| matches!(
            inst,
            SIRInstruction::Binary(_, _, BinaryOp::Sub, _)
        )),
        1
    );
    assert_eq!(
        instruction_count(&eu, |inst| matches!(
            inst,
            SIRInstruction::Concat(_, args) if args.len() == 8
        )),
        1
    );
}

#[test]
fn nested_conditional_priority_writes_use_combined_predicates() {
    let mut arena = SLTNodeArena::new();
    let mut acc = constant(&mut arena, u64::MAX, 64);
    for index in 0..8 {
        let outer = input(&mut arena, index * 2, 1);
        let inner = input(&mut arena, index * 2 + 1, 1);
        let value = constant(&mut arena, index as u64, 64);
        let write = arena
            .alloc(SLTNode::Mux {
                cond: inner,
                then_expr: value,
                else_expr: acc,
            })
            .unwrap();
        acc = arena
            .alloc(SLTNode::Mux {
                cond: outer,
                then_expr: write,
                else_expr: acc,
            })
            .unwrap();
    }

    let mut builder = SIRBuilder::new();
    SLTToSIRLowerer::new(false).lower(&mut builder, acc, &arena, &mut crate::HashMap::default());
    let eu = finish_lowering(builder);

    assert_eq!(
        instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Mux(..))),
        0
    );
    assert_eq!(
        instruction_count(&eu, |inst| matches!(
            inst,
            SIRInstruction::Binary(_, _, BinaryOp::LogicAnd, _)
        )),
        8
    );
    assert_eq!(
        instruction_count(&eu, |inst| matches!(
            inst,
            SIRInstruction::Unary(_, UnaryOp::CountLeadingZeros, _)
        )),
        1
    );
}

#[test]
fn first_write_found_recurrence_uses_outer_predicates_and_ctz() {
    let mut arena = SLTNodeArena::new();
    let mut acc = constant(&mut arena, u64::MAX, 64);
    let mut found = constant(&mut arena, 0, 1);
    let one = constant(&mut arena, 1, 1);
    for index in 0..8 {
        let outer = input(&mut arena, index, 1);
        let not_found = arena
            .alloc(SLTNode::Unary(UnaryOp::LogicNot, found))
            .unwrap();
        let value = constant(&mut arena, index as u64, 64);
        let write = arena
            .alloc(SLTNode::Mux {
                cond: not_found,
                then_expr: value,
                else_expr: acc,
            })
            .unwrap();
        acc = arena
            .alloc(SLTNode::Mux {
                cond: outer,
                then_expr: write,
                else_expr: acc,
            })
            .unwrap();

        let set_found = arena
            .alloc(SLTNode::Mux {
                cond: not_found,
                then_expr: one,
                else_expr: found,
            })
            .unwrap();
        found = arena
            .alloc(SLTNode::Mux {
                cond: outer,
                then_expr: set_found,
                else_expr: found,
            })
            .unwrap();
    }

    let mut builder = SIRBuilder::new();
    SLTToSIRLowerer::new(false).lower(&mut builder, acc, &arena, &mut crate::HashMap::default());
    let eu = finish_lowering(builder);

    assert_eq!(
        instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Mux(..))),
        1,
        "only the zero-input sentinel select should remain"
    );
    assert_eq!(
        instruction_count(&eu, |inst| matches!(
            inst,
            SIRInstruction::Unary(_, UnaryOp::CountTrailingZeros, _)
        )),
        1
    );
    assert_eq!(
        instruction_count(&eu, |inst| matches!(
            inst,
            SIRInstruction::Concat(_, args) if args.len() == 8
        )),
        1
    );
    assert_eq!(
        instruction_count(&eu, |inst| matches!(
            inst,
            SIRInstruction::Unary(_, UnaryOp::LogicNot, _)
        )),
        0,
        "the found prefix recurrence must not be lowered"
    );
}

#[test]
fn conditionally_seeded_priority_count_preserves_fallback() {
    let width = 8;
    let result_width = UnaryOp::CountLeadingZeros.result_width(width);
    let mut arena = SLTNodeArena::new();
    let source = input(&mut arena, 0, width);
    let gate = input(&mut arena, 1, 1);
    let fallback = input(&mut arena, 2, result_width);
    let sentinel = constant(&mut arena, width as u64, result_width);
    let mut acc = arena
        .alloc(SLTNode::Mux {
            cond: gate,
            then_expr: sentinel,
            else_expr: fallback,
        })
        .unwrap();

    for value in 0..width {
        let bit = arena
            .alloc(SLTNode::Slice {
                expr: source,
                access: BitAccess::new(width - 1 - value, width - 1 - value),
            })
            .unwrap();
        let unmatched = arena
            .alloc(SLTNode::Binary(acc, BinaryOp::Eq, sentinel))
            .unwrap();
        let write = arena
            .alloc(SLTNode::Binary(bit, BinaryOp::LogicAnd, unmatched))
            .unwrap();
        let value = constant(&mut arena, value as u64, result_width);
        let candidate = arena
            .alloc(SLTNode::Mux {
                cond: gate,
                then_expr: value,
                else_expr: acc,
            })
            .unwrap();
        acc = arena
            .alloc(SLTNode::Mux {
                cond: write,
                then_expr: candidate,
                else_expr: acc,
            })
            .unwrap();
    }

    let mut builder = SIRBuilder::new();
    SLTToSIRLowerer::new(false).lower(&mut builder, acc, &arena, &mut crate::HashMap::default());
    let eu = finish_lowering(builder);

    assert_eq!(
        instruction_count(&eu, |inst| matches!(
            inst,
            SIRInstruction::Unary(_, UnaryOp::CountLeadingZeros, _)
        )),
        1
    );
    assert_eq!(
        instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Mux(..))),
        1,
        "only gate ? clz(source) : fallback should remain"
    );
}

#[test]
fn conditional_priority_rejects_mismatched_per_stage_gate() {
    assert_conditional_priority_rejected(ConditionalPriorityCorruption::StageGate(3));
}

#[test]
fn conditional_priority_rejects_mismatched_seed_gate_and_default() {
    assert_conditional_priority_rejected(ConditionalPriorityCorruption::SeedGate);
    assert_conditional_priority_rejected(ConditionalPriorityCorruption::SeedDefault);
}

#[test]
fn conditional_priority_rejects_mismatched_stage_fallback() {
    assert_conditional_priority_rejected(ConditionalPriorityCorruption::StageFallback(3));
}

#[test]
fn conditional_priority_rejects_reordered_bit_or_value() {
    assert_conditional_priority_rejected(ConditionalPriorityCorruption::BitOrder(3));
    assert_conditional_priority_rejected(ConditionalPriorityCorruption::ValueOrder(3));
}

#[test]
fn additive_popcount_accepts_only_lsb_zero_extension() {
    let mut arena = SLTNodeArena::new();
    let bit = input(&mut arena, 0, 1);
    let zero3 = constant(&mut arena, 0, 3);
    let lsb_extended = arena
        .alloc(SLTNode::Concat(vec![(zero3, 3), (bit, 1)]))
        .unwrap();
    let msb_shifted = arena
        .alloc(SLTNode::Concat(vec![(bit, 1), (zero3, 3)]))
        .unwrap();
    let wide = input(&mut arena, 1, 4);
    let multi_bit_slice = arena
        .alloc(SLTNode::Slice {
            expr: wide,
            access: BitAccess::new(0, 1),
        })
        .unwrap();

    assert!(resolve_slt_extended_bit(lsb_extended, &arena).is_some());
    assert!(resolve_slt_extended_bit(msb_shifted, &arena).is_none());
    assert!(resolve_slt_extended_bit(multi_bit_slice, &arena).is_none());
}

#[test]
fn procedural_truth_unwrap_keeps_wide_reduction() {
    let mut arena = SLTNodeArena::new();
    let wide = input(&mut arena, 0, 4);
    let truth = arena.alloc(SLTNode::Unary(UnaryOp::Or, wide)).unwrap();
    let normalized = arena
        .alloc(SLTNode::Unary(UnaryOp::ToTwoState, truth))
        .unwrap();

    assert_eq!(
        unwrap_slt_one_bit_procedural_truth(normalized, &arena),
        normalized,
        "a wide reduction is a real booleanization, not an identity"
    );
}

#[test]
fn cached_popcount_accumulator_only_lowers_new_increment() {
    let width = 8;
    let result_width = 4;
    let mut arena = SLTNodeArena::new();
    let source = input(&mut arena, 0, width);
    let one = constant(&mut arena, 1, result_width);
    let mut base = constant(&mut arena, 0, result_width);
    for bit in 0..width {
        let predicate = arena
            .alloc(SLTNode::Slice {
                expr: source,
                access: BitAccess::new(bit, bit),
            })
            .unwrap();
        let incremented = arena
            .alloc(SLTNode::Binary(base, BinaryOp::Add, one))
            .unwrap();
        base = arena
            .alloc(SLTNode::Mux {
                cond: predicate,
                then_expr: incremented,
                else_expr: base,
            })
            .unwrap();
    }

    let delta = input(&mut arena, 1, 1);
    let incremented = arena
        .alloc(SLTNode::Binary(base, BinaryOp::Add, one))
        .unwrap();
    let root = arena
        .alloc(SLTNode::Mux {
            cond: delta,
            then_expr: incremented,
            else_expr: base,
        })
        .unwrap();

    let mut builder = SIRBuilder::new();
    let mut cache = crate::HashMap::default();
    let lowerer = SLTToSIRLowerer::new(false);
    let base_reg = lowerer.lower(&mut builder, base, &arena, &mut cache);
    let root_reg = lowerer.lower(&mut builder, root, &arena, &mut cache);
    let eu = finish_lowering(builder);

    assert_eq!(
        instruction_count(&eu, |instruction| matches!(
            instruction,
            SIRInstruction::Unary(_, UnaryOp::PopCount, _)
        )),
        1,
        "the already materialized population count must not be rebuilt"
    );
    assert_eq!(
        instruction_count(&eu, |instruction| matches!(
            instruction,
            SIRInstruction::Concat(_, arguments) if arguments.len() == width + 1
        )),
        0
    );
    assert!(
        eu.blocks
            .values()
            .flat_map(|block| &block.instructions)
            .any(|instruction| matches!(
                instruction,
                SIRInstruction::Binary(dst, lhs, BinaryOp::Add, _)
                    if *dst == root_reg && *lhs == base_reg
            ))
    );
}

#[test]
fn cached_additive_popcount_accumulator_only_lowers_new_bit() {
    let width = 8;
    let result_width = 4;
    let mut arena = SLTNodeArena::new();
    let source = input(&mut arena, 0, width);
    let zero = constant(&mut arena, 0, result_width);
    let padding = constant(&mut arena, 0, result_width - 1);
    let mut base = zero;
    for bit in 0..width {
        let predicate = arena
            .alloc(SLTNode::Slice {
                expr: source,
                access: BitAccess::new(bit, bit),
            })
            .unwrap();
        let extended = arena
            .alloc(SLTNode::Concat(vec![
                (padding, result_width - 1),
                (predicate, 1),
            ]))
            .unwrap();
        base = arena
            .alloc(SLTNode::Binary(base, BinaryOp::Add, extended))
            .unwrap();
    }

    let delta = input(&mut arena, 1, 1);
    let extended_delta = arena
        .alloc(SLTNode::Concat(vec![
            (padding, result_width - 1),
            (delta, 1),
        ]))
        .unwrap();
    let root = arena
        .alloc(SLTNode::Binary(base, BinaryOp::Add, extended_delta))
        .unwrap();

    let mut builder = SIRBuilder::new();
    let mut cache = crate::HashMap::default();
    let lowerer = SLTToSIRLowerer::new(false);
    let base_reg = lowerer.lower(&mut builder, base, &arena, &mut cache);
    let root_reg = lowerer.lower(&mut builder, root, &arena, &mut cache);
    let eu = finish_lowering(builder);

    assert_eq!(
        instruction_count(&eu, |instruction| matches!(
            instruction,
            SIRInstruction::Unary(_, UnaryOp::PopCount, _)
        )),
        1
    );
    assert!(
        eu.blocks
            .values()
            .flat_map(|block| &block.instructions)
            .any(|instruction| matches!(
                instruction,
                SIRInstruction::Binary(dst, lhs, BinaryOp::Add, _)
                    if *dst == root_reg && *lhs == base_reg
            ))
    );
}

#[test]
fn cached_popcount_delta_preserves_wrapping_semantics() {
    let width = 7;
    let result_width = 3;
    let mut arena = SLTNodeArena::new();
    let source = input(&mut arena, 0, width);
    let one = constant(&mut arena, 1, result_width);
    let mut base = constant(&mut arena, 0, result_width);
    for bit in 0..width {
        let predicate = arena
            .alloc(SLTNode::Slice {
                expr: source,
                access: BitAccess::new(bit, bit),
            })
            .unwrap();
        let incremented = arena
            .alloc(SLTNode::Binary(base, BinaryOp::Add, one))
            .unwrap();
        base = arena
            .alloc(SLTNode::Mux {
                cond: predicate,
                then_expr: incremented,
                else_expr: base,
            })
            .unwrap();
    }
    let delta = input(&mut arena, 1, 1);
    let incremented = arena
        .alloc(SLTNode::Binary(base, BinaryOp::Add, one))
        .unwrap();
    let root = arena
        .alloc(SLTNode::Mux {
            cond: delta,
            then_expr: incremented,
            else_expr: base,
        })
        .unwrap();

    let mut builder = SIRBuilder::new();
    let mut cache = crate::HashMap::default();
    let lowerer = SLTToSIRLowerer::new(false);
    lowerer.lower(&mut builder, base, &arena, &mut cache);
    let result = lowerer.lower(&mut builder, root, &arena, &mut cache);
    let eu = finish_lowering(builder);

    assert_eq!(
        instruction_count(&eu, |instruction| matches!(
            instruction,
            SIRInstruction::Unary(_, UnaryOp::PopCount, _)
        )),
        1
    );

    for source_value in 0u64..(1 << width) {
        for delta_value in 0u64..=1 {
            let mut memory = crate::HashMap::default();
            memory.insert(
                0u32,
                TestSIRValue {
                    payload: source_value.into(),
                    mask: 0u8.into(),
                },
            );
            memory.insert(
                1u32,
                TestSIRValue {
                    payload: delta_value.into(),
                    mask: 0u8.into(),
                },
            );
            let actual = &execute_fold_group_sir_with_memory(&eu, &memory)[&result].payload;
            let expected =
                (source_value.count_ones() as u64 + delta_value) & ((1 << result_width) - 1);
            assert_eq!(actual, &BigUint::from(expected));
        }
    }
}

#[test]
fn cached_accumulator_is_not_reused_for_non_unit_update() {
    let width = 8;
    let result_width = 4;
    let mut arena = SLTNodeArena::new();
    let source = input(&mut arena, 0, width);
    let one = constant(&mut arena, 1, result_width);
    let two = constant(&mut arena, 2, result_width);
    let mut base = constant(&mut arena, 0, result_width);
    for bit in 0..width {
        let predicate = arena
            .alloc(SLTNode::Slice {
                expr: source,
                access: BitAccess::new(bit, bit),
            })
            .unwrap();
        let incremented = arena
            .alloc(SLTNode::Binary(base, BinaryOp::Add, one))
            .unwrap();
        base = arena
            .alloc(SLTNode::Mux {
                cond: predicate,
                then_expr: incremented,
                else_expr: base,
            })
            .unwrap();
    }
    let delta = input(&mut arena, 1, 1);
    let incremented = arena
        .alloc(SLTNode::Binary(base, BinaryOp::Add, two))
        .unwrap();
    let root = arena
        .alloc(SLTNode::Mux {
            cond: delta,
            then_expr: incremented,
            else_expr: base,
        })
        .unwrap();

    let mut builder = SIRBuilder::new();
    let mut cache = crate::HashMap::default();
    let lowerer = SLTToSIRLowerer::new(false);
    lowerer.lower(&mut builder, base, &arena, &mut cache);
    lowerer.lower(&mut builder, root, &arena, &mut cache);
    let eu = finish_lowering(builder);

    assert_eq!(
        instruction_count(&eu, |instruction| matches!(
            instruction,
            SIRInstruction::Mux(..)
        )),
        1,
        "only an exact conditional +1 update may become a count delta"
    );
}

#[test]
fn active_bit_predicate_family_becomes_one_wide_expression() {
    let width = 8;
    let mut arena = SLTNodeArena::new();
    let bound = input(&mut arena, 0, 4);
    let vm = input(&mut arena, 1, 1);
    let zero = constant(&mut arena, 0, 4);
    let one = constant(&mut arena, 1, 4);
    let mut acc = zero;

    for index in 0..width {
        let index_value = constant(&mut arena, index as u64, 4);
        let in_range = arena
            .alloc(SLTNode::Binary(index_value, BinaryOp::LtU, bound))
            .unwrap();
        let mask = input_bit(&mut arena, 2, index);
        let enabled = arena
            .alloc(SLTNode::Binary(vm, BinaryOp::LogicOr, mask))
            .unwrap();
        let eligible = arena
            .alloc(SLTNode::Binary(in_range, BinaryOp::LogicAnd, enabled))
            .unwrap();
        let source = input_bit(&mut arena, 3, index);
        let active = arena
            .alloc(SLTNode::Binary(eligible, BinaryOp::LogicAnd, source))
            .unwrap();
        let incremented = arena
            .alloc(SLTNode::Binary(acc, BinaryOp::Add, one))
            .unwrap();
        acc = arena
            .alloc(SLTNode::Mux {
                cond: active,
                then_expr: incremented,
                else_expr: acc,
            })
            .unwrap();
    }

    let mut builder = SIRBuilder::new();
    SLTToSIRLowerer::new(false).lower(&mut builder, acc, &arena, &mut crate::HashMap::default());
    let eu = finish_lowering(builder);

    assert_eq!(
        instruction_count(&eu, |inst| matches!(
            inst,
            SIRInstruction::Unary(_, UnaryOp::PopCount, _)
        )),
        1
    );
    assert_eq!(
        instruction_count(&eu, |inst| matches!(
            inst,
            SIRInstruction::Binary(_, _, BinaryOp::LtU, _)
        )),
        0,
        "the ordered comparison ladder must become a low-ones mask"
    );
    assert_eq!(
        instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Mux(..))),
        1,
        "the saturated low-ones mask needs one word-level select"
    );
    assert_eq!(
        instruction_count(&eu, |inst| matches!(
            inst,
            SIRInstruction::Concat(_, args) if args.len() == width
        )),
        0,
        "the scalar active predicates must not be reassembled one bit at a time"
    );
}

#[test]
fn low_ones_saturates_when_only_a_wide_bound_high_limb_is_set() {
    let mut arena = SLTNodeArena::new();
    let bound = arena
        .alloc(SLTNode::Constant(
            BigUint::from(1u8) << 64,
            BigUint::from(0u8),
            128,
            false,
        ))
        .unwrap();
    let mut builder = SIRBuilder::new();
    let result = SLTToSIRLowerer::new(false).lower_slt_vector_expr(
        &mut builder,
        SLTVectorExpr::LowOnes { bound },
        8,
        &arena,
        &mut crate::HashMap::default(),
        true,
    );
    let eu = finish_lowering(builder);

    assert_eq!(
        execute_fold_group_sir(&eu)[&result].payload,
        BigUint::from(0xffu8)
    );
    assert_eq!(
        instruction_count(&eu, |instruction| matches!(
            instruction,
            SIRInstruction::Binary(_, _, BinaryOp::GeU, _)
        )),
        1
    );
}

#[test]
fn compacted_unpacked_input_preserves_packed_elements_provenance() {
    let variable = 42u32;
    let mut arena = SLTNodeArena::new();
    let representative = arena
        .alloc(SLTNode::Input {
            variable,
            signed: false,
            index: Vec::new(),
            access: BitAccess::new(0, 0),
        })
        .unwrap();
    let element_widths = crate::HashMap::from_iter([(variable, 1)]);
    let lowerer = SLTToSIRLowerer::new(false).with_unpacked_input_types(&arena, &element_widths);
    let mut builder = SIRBuilder::new();
    lowerer.lower_slt_vector_expr(
        &mut builder,
        SLTVectorExpr::Origin(SLTBitOrigin::Input {
            node: representative,
            variable,
            signed: false,
            index: Vec::new(),
        }),
        32,
        &arena,
        &mut crate::HashMap::default(),
        true,
    );
    let eu = finish_lowering(builder);

    assert!(matches!(
        eu.blocks[&eu.entry_block_id].instructions.as_slice(),
        [SIRInstruction::Load(
            _,
            42,
            SIROffset::PackedElements {
                bit_offset: 0,
                element_width: 1,
            },
            32,
        )]
    ));
}

#[test]
fn masked_found_recurrence_becomes_wide_or_reduction() {
    let width = 8;
    let mut arena = SLTNodeArena::new();
    let bound = input(&mut arena, 0, 4);
    let vm = input(&mut arena, 1, 1);
    let mut found = constant(&mut arena, 0, 1);

    for index in 0..width {
        let index_value = constant(&mut arena, index as u64, 4);
        let in_range = arena
            .alloc(SLTNode::Binary(index_value, BinaryOp::LtU, bound))
            .unwrap();
        let mask = input_bit(&mut arena, 2, index);
        let enabled = arena
            .alloc(SLTNode::Binary(vm, BinaryOp::LogicOr, mask))
            .unwrap();
        let eligible = arena
            .alloc(SLTNode::Binary(in_range, BinaryOp::LogicAnd, enabled))
            .unwrap();
        let source = input_bit(&mut arena, 3, index);
        let set = arena
            .alloc(SLTNode::Binary(found, BinaryOp::LogicOr, source))
            .unwrap();
        found = arena
            .alloc(SLTNode::Mux {
                cond: eligible,
                then_expr: set,
                else_expr: found,
            })
            .unwrap();
    }

    let mut builder = SIRBuilder::new();
    SLTToSIRLowerer::new(false).lower(&mut builder, found, &arena, &mut crate::HashMap::default());
    let eu = finish_lowering(builder);

    assert_eq!(
        instruction_count(&eu, |inst| matches!(
            inst,
            SIRInstruction::Unary(_, UnaryOp::Or, _)
        )),
        1
    );
    assert_eq!(
        instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Mux(..))),
        1,
        "the saturated low-ones mask needs one word-level select"
    );
    assert_eq!(
        instruction_count(&eu, |inst| matches!(
            inst,
            SIRInstruction::Binary(_, _, BinaryOp::LtU, _)
        )),
        0
    );
}
