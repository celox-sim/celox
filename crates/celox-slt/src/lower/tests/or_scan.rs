use super::super::or_scan::slt_scan_lane_bits;
use super::*;

const SCAN_VECTOR_STATE: u32 = 100;
const SCAN_FOUND_STATE: u32 = 101;
const SCAN_SOURCE: u32 = 102;
const SCAN_MASK: u32 = 103;
const SCAN_BOUND: u32 = 104;
const SCAN_UNMASKED: u32 = 105;
const SCAN_MODE: u32 = 106;
const SCAN_GUARD: u32 = 107;
const SCAN_LOOP: u32 = 108;

#[derive(Clone, Copy)]
enum ScanMutation {
    None,
    OverflowFalseGuard,
    DifferentActive,
    NonIdentityOffset,
    NonIdentityInputStride,
    NarrowLoopMask,
    WrongBeforeValue,
}

fn scan_dynamic_bit(
    arena: &mut SLTNodeArena<u32>,
    variable: u32,
    loop_value: NodeId,
    width: usize,
    stride: usize,
    unpacked: bool,
) -> NodeId {
    let _ = width;
    arena
        .alloc(SLTNode::Input {
            variable,
            signed: false,
            index: vec![crate::SLTIndex {
                node: loop_value,
                stride,
                kind: if unpacked {
                    crate::SLTIndexKind::Unpacked { element_width: 1 }
                } else {
                    crate::SLTIndexKind::Packed
                },
            }],
            access: BitAccess::new(0, 0),
        })
        .unwrap()
}

fn synthetic_or_scan_group(width: usize, mutation: ScanMutation) -> (SLTNodeArena<u32>, NodeId) {
    synthetic_or_scan_group_with_layout(width, mutation, false)
}

fn synthetic_or_scan_group_with_layout(
    width: usize,
    mutation: ScanMutation,
    unpacked: bool,
) -> (SLTNodeArena<u32>, NodeId) {
    let mut arena = SLTNodeArena::new();
    let loop_value = input(&mut arena, SCAN_LOOP, 64);
    let old_vector = input(&mut arena, SCAN_VECTOR_STATE, width);
    let old_found = input(&mut arena, SCAN_FOUND_STATE, 1);
    let source = scan_dynamic_bit(
        &mut arena,
        SCAN_SOURCE,
        loop_value,
        width,
        if matches!(mutation, ScanMutation::NonIdentityInputStride) {
            2
        } else {
            1
        },
        unpacked,
    );
    let mask = scan_dynamic_bit(&mut arena, SCAN_MASK, loop_value, width, 1, unpacked);
    let bound = input(&mut arena, SCAN_BOUND, 8);
    let unmasked = input(&mut arena, SCAN_UNMASKED, 1);
    let mode = input(&mut arena, SCAN_MODE, 2);
    let guard = if matches!(mutation, ScanMutation::OverflowFalseGuard) {
        let one = constant(&mut arena, 1, 1);
        arena
            .alloc(SLTNode::Binary(one, BinaryOp::Add, one))
            .unwrap()
    } else {
        input(&mut arena, SCAN_GUARD, 1)
    };

    let lane_bits = slt_scan_lane_bits(width);
    let valid_lane_mask = (1u64 << lane_bits) - 1;
    let lane_mask = constant(
        &mut arena,
        if matches!(mutation, ScanMutation::NarrowLoopMask) {
            valid_lane_mask >> 1
        } else {
            0xff
        },
        64,
    );
    let truncated_loop = arena
        .alloc(SLTNode::Binary(loop_value, BinaryOp::And, lane_mask))
        .unwrap();
    let in_range = arena
        .alloc(SLTNode::Binary(truncated_loop, BinaryOp::LtU, bound))
        .unwrap();
    let enabled = arena
        .alloc(SLTNode::Binary(unmasked, BinaryOp::LogicOr, mask))
        .unwrap();
    let active = arena
        .alloc(SLTNode::Binary(in_range, BinaryOp::LogicAnd, enabled))
        .unwrap();
    let found_next = arena
        .alloc(SLTNode::Binary(old_found, BinaryOp::LogicOr, source))
        .unwrap();
    let found_update = arena
        .alloc(SLTNode::Mux {
            cond: active,
            then_expr: found_next,
            else_expr: old_found,
        })
        .unwrap();

    let not_found = arena
        .alloc(SLTNode::Unary(UnaryOp::LogicNot, old_found))
        .unwrap();
    let not_source = arena
        .alloc(SLTNode::Unary(UnaryOp::LogicNot, source))
        .unwrap();
    let before = arena
        .alloc(SLTNode::Binary(
            not_found,
            BinaryOp::LogicAnd,
            if matches!(mutation, ScanMutation::WrongBeforeValue) {
                source
            } else {
                not_source
            },
        ))
        .unwrap();
    let first = arena
        .alloc(SLTNode::Binary(not_found, BinaryOp::LogicAnd, source))
        .unwrap();
    let one_mode = constant(&mut arena, 1, 2);
    let two_mode = constant(&mut arena, 2, 2);
    let is_before = arena
        .alloc(SLTNode::Binary(mode, BinaryOp::EqWildcard, one_mode))
        .unwrap();
    let is_first = arena
        .alloc(SLTNode::Binary(mode, BinaryOp::EqWildcard, two_mode))
        .unwrap();
    let first_or_through = arena
        .alloc(SLTNode::Mux {
            cond: is_first,
            then_expr: first,
            else_expr: not_found,
        })
        .unwrap();
    let selected = arena
        .alloc(SLTNode::Mux {
            cond: is_before,
            then_expr: before,
            else_expr: first_or_through,
        })
        .unwrap();

    let zero64 = constant(&mut arena, 0, 64);
    let one64 = constant(&mut arena, 1, 64);
    let scaled = arena
        .alloc(SLTNode::Binary(loop_value, BinaryOp::Mul, one64))
        .unwrap();
    let identity_offset = arena
        .alloc(SLTNode::Binary(zero64, BinaryOp::Add, scaled))
        .unwrap();
    let offset = if matches!(mutation, ScanMutation::NonIdentityOffset) {
        arena
            .alloc(SLTNode::Binary(identity_offset, BinaryOp::Add, one64))
            .unwrap()
    } else {
        identity_offset
    };
    let one = constant(&mut arena, 1, width);
    let bit_mask = arena
        .alloc(SLTNode::Binary(one, BinaryOp::Shl, offset))
        .unwrap();
    let inverted_mask = arena
        .alloc(SLTNode::Unary(UnaryOp::BitNot, bit_mask))
        .unwrap();
    let preserved = arena
        .alloc(SLTNode::Binary(old_vector, BinaryOp::And, inverted_mask))
        .unwrap();
    let extended = if width == 1 {
        selected
    } else {
        let zero = constant(&mut arena, 0, width - 1);
        arena
            .alloc(SLTNode::Concat(vec![(zero, width - 1), (selected, 1)]))
            .unwrap()
    };
    let shifted = arena
        .alloc(SLTNode::Binary(extended, BinaryOp::Shl, offset))
        .unwrap();
    let inserted_bit = arena
        .alloc(SLTNode::Binary(shifted, BinaryOp::And, bit_mask))
        .unwrap();
    let inserted = arena
        .alloc(SLTNode::Binary(preserved, BinaryOp::Or, inserted_bit))
        .unwrap();
    let vector_update = arena
        .alloc(SLTNode::Mux {
            cond: if matches!(mutation, ScanMutation::DifferentActive) {
                source
            } else {
                active
            },
            then_expr: inserted,
            else_expr: old_vector,
        })
        .unwrap();
    let initial_vector = input(&mut arena, SCAN_VECTOR_STATE, width);
    let initial_found = constant(&mut arena, 0, 1);
    let group = arena
        .alloc(SLTNode::ForFoldGroup {
            loop_var: SCAN_LOOP,
            loop_width: 64,
            loop_signed: false,
            start: BigInt::from(0u8),
            step: BigInt::from(1u8),
            trip_count: width,
            entry_guard: guard,
            states: vec![
                SLTForFoldGroupState {
                    target: VarAtomBase::new(SCAN_VECTOR_STATE, 0, width - 1),
                    initial: initial_vector,
                    update: vector_update,
                },
                SLTForFoldGroupState {
                    target: VarAtomBase::new(SCAN_FOUND_STATE, 0, 0),
                    initial: initial_found,
                    update: found_update,
                },
            ],
        })
        .unwrap();
    (arena, group)
}

fn lower_synthetic_scan(
    width: usize,
    mutation: ScanMutation,
    four_state: bool,
) -> (ExecutionUnit<u32>, RegisterId) {
    let (arena, group) = synthetic_or_scan_group(width, mutation);
    let mut builder = SIRBuilder::new();
    let result = SLTToSIRLowerer::new(four_state).lower(
        &mut builder,
        group,
        &arena,
        &mut crate::HashMap::default(),
    );
    (finish_lowering(builder), result)
}

fn scan_reference(
    width: usize,
    source: u64,
    mask: u64,
    old: u64,
    bound: u64,
    unmasked: bool,
    mode: u64,
    guard: bool,
) -> (u64, bool) {
    if !guard {
        return (old, false);
    }
    let mut result = old;
    let mut found = false;
    for lane in 0..width {
        let active = (lane as u64) < bound && (unmasked || (mask >> lane) & 1 != 0);
        if active {
            let bit = (source >> lane) & 1 != 0;
            let selected = match mode {
                1 => !found && !bit,
                2 => !found && bit,
                _ => !found,
            };
            let lane_mask = 1u64 << lane;
            result = if selected {
                result | lane_mask
            } else {
                result & !lane_mask
            };
            found |= bit;
        }
    }
    (result, found)
}

#[test]
fn exact_two_state_or_scan_lowers_without_a_runtime_loop() {
    let (eu, _) = lower_synthetic_scan(8, ScanMutation::None, false);
    assert_eq!(branch_count(&eu), 0);
    assert_eq!(
        instruction_count(&eu, |instruction| matches!(
            instruction,
            SIRInstruction::Unary(_, UnaryOp::Or, _)
        )),
        1
    );
}

#[test]
fn unpacked_bit_scan_uses_explicit_packed_elements_loads() {
    let width = 32;
    let (arena, group) = synthetic_or_scan_group_with_layout(width, ScanMutation::None, true);
    let mut builder = SIRBuilder::new();
    SLTToSIRLowerer::new(false).lower(&mut builder, group, &arena, &mut crate::HashMap::default());
    let eu = finish_lowering(builder);
    let packed_loads = eu
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            matches!(
                instruction,
                SIRInstruction::Load(
                    _,
                    SCAN_SOURCE | SCAN_MASK,
                    SIROffset::PackedElements {
                        bit_offset: 0,
                        element_width: 1
                    },
                    32
                )
            )
        })
        .count();

    assert_eq!(packed_loads, 2);
}

#[test]
fn scan_entry_guard_constant_evaluation_uses_bitvector_width() {
    let width = 4;
    let old = 0b1010u64;
    let (eu, result) = lower_synthetic_scan(width, ScanMutation::OverflowFalseGuard, false);
    let memory = crate::HashMap::from_iter([
        (
            SCAN_VECTOR_STATE,
            TestSIRValue {
                payload: old.into(),
                mask: 0u8.into(),
            },
        ),
        (
            SCAN_SOURCE,
            TestSIRValue {
                payload: 0b1111u8.into(),
                mask: 0u8.into(),
            },
        ),
        (
            SCAN_MASK,
            TestSIRValue {
                payload: 0b1111u8.into(),
                mask: 0u8.into(),
            },
        ),
        (
            SCAN_BOUND,
            TestSIRValue {
                payload: width.into(),
                mask: 0u8.into(),
            },
        ),
        (
            SCAN_UNMASKED,
            TestSIRValue {
                payload: 1u8.into(),
                mask: 0u8.into(),
            },
        ),
        (
            SCAN_MODE,
            TestSIRValue {
                payload: 2u8.into(),
                mask: 0u8.into(),
            },
        ),
    ]);

    assert_eq!(branch_count(&eu), 0);
    assert_eq!(
        execute_fold_group_sir_with_memory(&eu, &memory)[&result].payload,
        BigUint::from(old << 1)
    );
}

#[test]
fn word_scan_matches_the_sequential_first_true_semantics_exhaustively() {
    for width in 1..=4 {
        let (eu, result) = lower_synthetic_scan(width, ScanMutation::None, false);
        let values = 1u64 << width;
        for source in 0..values {
            for mask in 0..values {
                for old in 0..values {
                    for bound in 0..=width as u64 {
                        for unmasked in [false, true] {
                            for mode in 1..=3 {
                                for guard in [false, true] {
                                    let memory = crate::HashMap::from_iter([
                                        (
                                            SCAN_VECTOR_STATE,
                                            TestSIRValue {
                                                payload: old.into(),
                                                mask: 0u8.into(),
                                            },
                                        ),
                                        (
                                            SCAN_SOURCE,
                                            TestSIRValue {
                                                payload: source.into(),
                                                mask: 0u8.into(),
                                            },
                                        ),
                                        (
                                            SCAN_MASK,
                                            TestSIRValue {
                                                payload: mask.into(),
                                                mask: 0u8.into(),
                                            },
                                        ),
                                        (
                                            SCAN_BOUND,
                                            TestSIRValue {
                                                payload: bound.into(),
                                                mask: 0u8.into(),
                                            },
                                        ),
                                        (
                                            SCAN_UNMASKED,
                                            TestSIRValue {
                                                payload: u8::from(unmasked).into(),
                                                mask: 0u8.into(),
                                            },
                                        ),
                                        (
                                            SCAN_MODE,
                                            TestSIRValue {
                                                payload: mode.into(),
                                                mask: 0u8.into(),
                                            },
                                        ),
                                        (
                                            SCAN_GUARD,
                                            TestSIRValue {
                                                payload: u8::from(guard).into(),
                                                mask: 0u8.into(),
                                            },
                                        ),
                                    ]);
                                    let actual = &execute_fold_group_sir_with_memory(&eu, &memory)
                                        [&result]
                                        .payload;
                                    let (expected_vector, expected_found) = scan_reference(
                                        width, source, mask, old, bound, unmasked, mode, guard,
                                    );
                                    let expected =
                                        (expected_vector << 1) | u64::from(expected_found);
                                    assert_eq!(
                                        actual,
                                        &BigUint::from(expected),
                                        "width={width} source={source:#x} mask={mask:#x} old={old:#x} bound={bound} unmasked={unmasked} mode={mode} guard={guard}",
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn or_scan_matcher_rejects_every_near_miss_and_four_state_mode() {
    for mutation in [
        ScanMutation::DifferentActive,
        ScanMutation::NonIdentityOffset,
        ScanMutation::NonIdentityInputStride,
        ScanMutation::NarrowLoopMask,
        ScanMutation::WrongBeforeValue,
    ] {
        let (eu, _) = lower_synthetic_scan(4, mutation, false);
        assert!(branch_count(&eu) > 0);
    }
    let (eu, _) = lower_synthetic_scan(4, ScanMutation::None, true);
    assert!(branch_count(&eu) > 0);
}
