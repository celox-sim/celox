use super::*;
use crate::decode::reduce_two_state_decodes;

fn arm(
    arena: &mut SLTNodeArena<u32>,
    selector: NodeId,
    label: u64,
    value: NodeId,
    tail: NodeId,
) -> NodeId {
    let label = constant(arena, label, crate::get_width(selector, arena));
    let cond = arena
        .alloc(SLTNode::Binary(selector, BinaryOp::Eq, label))
        .unwrap();
    arena
        .alloc(SLTNode::Mux {
            cond,
            then_expr: value,
            else_expr: tail,
        })
        .unwrap()
}

#[test]
fn decoder_groups_preserve_priority_and_the_default_for_every_selector() {
    let mut arena = SLTNodeArena::new();
    let selector = input(&mut arena, 0, 6);
    let values = [0, 11, 22, 33].map(|v| constant(&mut arena, v, 8));
    // An intervening default-valued row masks a duplicate label. The two
    // repeated nondefault groups exercise both a masked compare and a bitmap.
    let rows = [
        (0, 1),
        (1, 1),
        (2, 1),
        (3, 1),
        (4, 0),
        (4, 3),
        (7, 2),
        (18, 2),
        (63, 2),
        (12, 3),
    ];
    let mut root = values[0];
    for &(label, index) in rows.iter().rev() {
        root = arm(&mut arena, selector, label, values[index], root);
    }
    reduce_two_state_decodes(&mut arena, std::iter::once(&mut root)).unwrap();
    let nodes = crate::canon::reachable(&arena, [root]);
    assert_eq!(
        nodes
            .iter()
            .filter(|&&n| matches!(arena.get(n), SLTNode::Mux { .. }))
            .count(),
        3
    );
    let mut builder = SIRBuilder::new();
    let result =
        SLTToSIRLowerer::new(false).lower(&mut builder, root, &arena, &mut Default::default());
    let eu = finish_lowering(builder);
    for label in 0..64u64 {
        let memory = [(
            0,
            TestSIRValue {
                payload: label.into(),
                mask: 0u8.into(),
            },
        )]
        .into_iter()
        .collect();
        let out = execute_fold_group_sir_with_memory(&eu, &memory);
        let index = rows.iter().find(|&&(n, _)| label == n).map_or(0, |r| r.1);
        assert_eq!(
            out[&result].payload,
            BigUint::from([0u8, 11, 22, 33][index])
        );
    }
}

#[test]
fn shared_decode_suffix_remains_a_single_value() {
    let mut arena = SLTNodeArena::new();
    let selector = input(&mut arena, 0, 8);
    let a = input(&mut arena, 1, 8);
    let b = input(&mut arena, 2, 8);
    let inner = arm(&mut arena, selector, 2, a, b);
    let suffix = arm(&mut arena, selector, 1, a, inner);
    let outer = arm(&mut arena, selector, 0, b, suffix);
    let mut roots = [outer, suffix];
    reduce_two_state_decodes(&mut arena, &mut roots).unwrap();
    assert_ne!(roots[1], suffix);
    assert!(
        matches!(arena.get(roots[0]), SLTNode::Mux { else_expr, .. } if *else_expr == roots[1])
    );
}

#[test]
fn masked_literal_and_unequal_width_arms_keep_their_tests() {
    let mut arena = SLTNodeArena::new();
    let selector = input(&mut arena, 0, 2);
    let a = constant(&mut arena, 7, 8);
    let b = constant(&mut arena, 0, 8);
    let tail = arm(&mut arena, selector, 0, a, b);
    let wildcard = arena
        .alloc(SLTNode::Constant(0u8.into(), 1u8.into(), 2, false))
        .unwrap();
    let cmp = arena
        .alloc(SLTNode::Binary(selector, BinaryOp::EqWildcard, wildcard))
        .unwrap();
    let wide = constant(&mut arena, 0, 16);
    let unknown = arena
        .alloc(SLTNode::Mux {
            cond: cmp,
            then_expr: a,
            else_expr: tail,
        })
        .unwrap();
    let mixed = arm(&mut arena, selector, 1, wide, tail);
    for original in [unknown, mixed] {
        let mut root = original;
        reduce_two_state_decodes(&mut arena, std::iter::once(&mut root)).unwrap();
        assert_eq!(root, original);
    }
}

#[test]
fn four_state_transition_keeps_the_selector_bits_of_each_comparison() {
    use crate::next_state::{NextStateForm, Publication, ValueRoot, lower_transition};

    let mut arena = SLTNodeArena::new();
    let selector = input(&mut arena, 0, 2);
    let active = input(&mut arena, 1, 8);
    let idle = input(&mut arena, 2, 8);
    let tail = arm(&mut arena, selector, 0, active, idle);
    let value = arm(&mut arena, selector, 1, active, tail);
    let form = NextStateForm {
        roots: vec![ValueRoot {
            target: 3,
            access: BitAccess::new(0, 7),
            value,
        }],
        ports: vec![],
    };

    for (four_state, expected) in [(true, 2), (false, 1)] {
        let unit = lower_transition(
            &arena,
            &form,
            Publication::Direct,
            four_state,
            |id, _| id,
            &Default::default(),
        )
        .unwrap();
        assert_eq!(
            instruction_count(&unit, |i| matches!(
                i,
                SIRInstruction::Binary(_, _, BinaryOp::Eq, _)
            )),
            expected
        );
    }
}

#[test]
fn full_width_labels_compare_all_64_bits() {
    let mut arena = SLTNodeArena::new();
    let selector = input(&mut arena, 0, 64);
    let yes = constant(&mut arena, 1, 1);
    let no = constant(&mut arena, 0, 1);
    let labels = [0, 1u64 << 63, u64::MAX];
    let mut root = no;
    for label in labels {
        root = arm(&mut arena, selector, label, yes, root);
    }
    reduce_two_state_decodes(&mut arena, [&mut root]).unwrap();
    let mut builder = SIRBuilder::new();
    let result =
        SLTToSIRLowerer::new(false).lower(&mut builder, root, &arena, &mut Default::default());
    let unit = finish_lowering(builder);
    for label in [0, 1, 1 << 63, (1 << 63) - 1, u64::MAX] {
        let memory = [(
            0,
            TestSIRValue {
                payload: label.into(),
                mask: 0u8.into(),
            },
        )]
        .into_iter()
        .collect();
        let values = execute_fold_group_sir_with_memory(&unit, &memory);
        assert_eq!(
            values[&result].payload,
            BigUint::from(u8::from(labels.contains(&label)))
        );
    }
}

#[test]
fn small_constant_lookup_keeps_its_individual_rows() {
    let mut arena = SLTNodeArena::new();
    let selector = input(&mut arena, 0, 2);
    let a = constant(&mut arena, 3, 4);
    let b = constant(&mut arena, 12, 4);
    let default = constant(&mut arena, 15, 4);
    let tail = arm(&mut arena, selector, 1, a, default);
    let tail = arm(&mut arena, selector, 2, b, tail);
    let original = arm(&mut arena, selector, 0, a, tail);
    let mut root = original;
    reduce_two_state_decodes(&mut arena, [&mut root]).unwrap();
    assert_eq!(root, original);
}

#[test]
fn state_table_matches_every_control_word_including_the_unlisted_state() {
    let mut arena = SLTNodeArena::new();
    let selector = input(&mut arena, 0, 3);
    let mut controls = [1, 2, 3, 4, 5, 6, 7].map(|address| input(&mut arena, address, 1));
    // A local enable is already an input. The other two factors can be
    // supplied together, keeping the eight raw controls within seven bits.
    let global_b = input(&mut arena, 8, 1);
    let enabled_b = arena
        .alloc(SLTNode::Binary(controls[0], BinaryOp::And, global_b))
        .unwrap();
    controls[6] = arena
        .alloc(SLTNode::Binary(controls[6], BinaryOp::And, enabled_b))
        .unwrap();
    let states = [0, 1, 2, 3, 4, 5, 6, 7].map(|state| constant(&mut arena, state, 3));
    let rest = arena
        .alloc(SLTNode::Binary(controls[1], BinaryOp::Or, controls[2]))
        .unwrap();
    let either = arena
        .alloc(SLTNode::Binary(controls[0], BinaryOp::Or, rest))
        .unwrap();
    let mut root = states[5];
    for state in (0..7).rev() {
        let other = (state + 1) % 7;
        let inner = arena
            .alloc(SLTNode::Mux {
                cond: controls[other],
                then_expr: states[(state + 2) % 8],
                else_expr: states[state],
            })
            .unwrap();
        let condition = if state == 6 { either } else { controls[state] };
        let next = arena
            .alloc(SLTNode::Mux {
                cond: condition,
                then_expr: states[(state + 3) % 8],
                else_expr: inner,
            })
            .unwrap();
        root = arm(&mut arena, selector, state as u64, next, root);
    }
    reduce_two_state_decodes(&mut arena, [&mut root]).unwrap();
    assert!(matches!(arena.get(root), SLTNode::Slice { .. }));
    let mut builder = SIRBuilder::new();
    let result =
        SLTToSIRLowerer::new(false).lower(&mut builder, root, &arena, &mut Default::default());
    let unit = finish_lowering(builder);
    let scalar = celox_sir::byte_lookup::scalarize(&unit, false);
    assert!(matches!(scalar, std::borrow::Cow::Owned(_)));
    assert!(scalar.register_map.values().all(|ty| ty.width() <= 64));
    scalar.verify();
    for control in 0..256usize {
        let flags = (control & 63) | (usize::from(control & 0xc1 == 0xc1) << 6);
        for state in 0..8usize {
            let memory = (0..9)
                .map(|i| {
                    let bits = if i == 0 {
                        state
                    } else {
                        (control >> (i - 1)) & 1
                    };
                    (
                        i as u32,
                        TestSIRValue {
                            payload: bits.into(),
                            mask: 0u8.into(),
                        },
                    )
                })
                .collect();
            let request = if state == 6 {
                flags & 7 != 0
            } else {
                flags & (1 << state) != 0
            };
            let expected = if state == 7 {
                5
            } else if request {
                (state + 3) % 8
            } else if flags & (1 << ((state + 1) % 7)) != 0 {
                (state + 2) % 8
            } else {
                state
            };
            for code in [&unit, scalar.as_ref()] {
                let values = execute_fold_group_sir_with_memory(code, &memory);
                assert_eq!(
                    values[&result].payload,
                    expected.into(),
                    "state {state}, controls {control}"
                );
            }
        }
    }
}
