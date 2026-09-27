use super::*;

#[test]
fn cheap_mux_stays_branchless() {
    let mut arena = SLTNodeArena::new();
    let cond = input(&mut arena, 0, 1);
    let then_expr = input(&mut arena, 1, 8);
    let else_expr = input(&mut arena, 2, 8);
    let mux = arena
        .alloc(SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        })
        .unwrap();
    let mut builder = SIRBuilder::new();
    SLTToSIRLowerer::new(false).lower(&mut builder, mux, &arena, &mut crate::HashMap::default());
    let eu = finish_lowering(builder);

    assert_eq!(branch_count(&eu), 0);
    assert_eq!(
        instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Mux(..))),
        1
    );
}

#[test]
fn expected_cost_uses_static_equality_probability() {
    let even = StaticBranchProbability::EVEN;
    let equality = StaticBranchProbability {
        true_weight: 1,
        total_weight: 5,
    };

    // With a 50/50 prior, ten units in the true arm cannot repay the
    // expected branch miss.  When equality is predicted false, 80% of that
    // arm is skipped and the same transformation is profitable.
    assert!(!SLTToSIRLowerer::mux_cfg_is_profitable(10, 0, 64, even));
    assert!(SLTToSIRLowerer::mux_cfg_is_profitable(10, 0, 64, equality));
    assert!(!SLTToSIRLowerer::mux_cfg_is_profitable(
        10,
        0,
        64,
        equality.inverted(),
    ));
}

#[test]
fn wildcard_equality_uses_the_decoder_bias() {
    let mut arena = SLTNodeArena::new();
    let selector = input(&mut arena, 0, 8);
    let opcode = constant(&mut arena, 0x13, 8);
    let eq = arena
        .alloc(SLTNode::Binary(selector, BinaryOp::EqWildcard, opcode))
        .unwrap();
    let ne = arena
        .alloc(SLTNode::Binary(selector, BinaryOp::NeWildcard, opcode))
        .unwrap();

    let eq_probability = SLTToSIRLowerer::static_true_probability(eq, &arena);
    let ne_probability = SLTToSIRLowerer::static_true_probability(ne, &arena);
    assert_eq!(
        (eq_probability.true_weight, eq_probability.total_weight),
        (1, 5)
    );
    assert_eq!(
        (ne_probability.true_weight, ne_probability.total_weight),
        (4, 5)
    );
}

#[test]
fn expensive_mux_preserves_control_flow_and_verifies() {
    let mut arena = SLTNodeArena::new();
    let cond = input(&mut arena, 0, 1);
    let then_input = input(&mut arena, 1, 64);
    let else_input = input(&mut arena, 2, 64);
    let then_expr = operation_chain(&mut arena, then_input, BinaryOp::Add, 8, 10, 64);
    let else_expr = operation_chain(&mut arena, else_input, BinaryOp::Xor, 12, 100, 64);
    let mux = arena
        .alloc(SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        })
        .unwrap();
    let mut builder = SIRBuilder::new();
    SLTToSIRLowerer::new(false).lower(&mut builder, mux, &arena, &mut crate::HashMap::default());
    let eu = finish_lowering(builder);

    assert_eq!(branch_count(&eu), 1);
    assert_eq!(eu.blocks.len(), 4);
    assert_eq!(
        instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Mux(..))),
        0
    );
}

#[test]
fn shared_arm_dag_is_hoisted_once() {
    let mut arena = SLTNodeArena::new();
    let cond = input(&mut arena, 0, 1);
    let source = input(&mut arena, 1, 64);
    let shared = operation_chain(&mut arena, source, BinaryOp::Mul, 3, 3, 64);
    let then_source = input(&mut arena, 2, 64);
    let else_source = input(&mut arena, 3, 64);
    let then_unique = operation_chain(&mut arena, then_source, BinaryOp::Add, 5, 20, 64);
    let else_unique = operation_chain(&mut arena, else_source, BinaryOp::Sub, 5, 40, 64);
    let then_expr = arena
        .alloc(SLTNode::Binary(shared, BinaryOp::Add, then_unique))
        .unwrap();
    let else_expr = arena
        .alloc(SLTNode::Binary(shared, BinaryOp::Sub, else_unique))
        .unwrap();
    let mux = arena
        .alloc(SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        })
        .unwrap();
    let mut builder = SIRBuilder::new();
    SLTToSIRLowerer::new(false).lower(&mut builder, mux, &arena, &mut crate::HashMap::default());
    let eu = finish_lowering(builder);

    assert_eq!(branch_count(&eu), 1);
    assert_eq!(
        instruction_count(&eu, |inst| matches!(
            inst,
            SIRInstruction::Binary(_, _, BinaryOp::Mul, _)
        )),
        3,
    );
    let entry = &eu.blocks[&BlockId(0)];
    assert_eq!(
        entry
            .instructions
            .iter()
            .filter(|inst| matches!(inst, SIRInstruction::Binary(_, _, BinaryOp::Mul, _)))
            .count(),
        3,
    );
}

#[test]
fn nested_cost_directed_muxes_form_valid_ssa() {
    let mut arena = SLTNodeArena::new();
    let outer_cond = input(&mut arena, 0, 1);
    let inner_cond = input(&mut arena, 1, 1);
    let a = input(&mut arena, 2, 64);
    let b = input(&mut arena, 3, 64);
    let c = input(&mut arena, 4, 64);
    let inner_then = operation_chain(&mut arena, a, BinaryOp::Add, 8, 10, 64);
    let inner_else = operation_chain(&mut arena, b, BinaryOp::Sub, 8, 30, 64);
    let inner = arena
        .alloc(SLTNode::Mux {
            cond: inner_cond,
            then_expr: inner_then,
            else_expr: inner_else,
        })
        .unwrap();
    let outer_else = operation_chain(&mut arena, c, BinaryOp::Xor, 16, 70, 64);
    let outer = arena
        .alloc(SLTNode::Mux {
            cond: outer_cond,
            then_expr: inner,
            else_expr: outer_else,
        })
        .unwrap();
    let mut builder = SIRBuilder::new();
    SLTToSIRLowerer::new(false).lower(&mut builder, outer, &arena, &mut crate::HashMap::default());
    let eu = finish_lowering(builder);

    assert_eq!(branch_count(&eu), 2);
}

#[test]
fn deep_division_forces_cfg_and_casts_merge_width() {
    let mut arena = SLTNodeArena::new();
    let cond = input(&mut arena, 0, 1);
    let narrow = input(&mut arena, 1, 8);
    let numerator = input(&mut arena, 2, 16);
    let denominator = input(&mut arena, 3, 16);
    let quotient = arena
        .alloc(SLTNode::Binary(numerator, BinaryOp::DivU, denominator))
        .unwrap();
    let one = constant(&mut arena, 1, 16);
    let deep_division = arena
        .alloc(SLTNode::Binary(quotient, BinaryOp::Add, one))
        .unwrap();
    let mux = arena
        .alloc(SLTNode::Mux {
            cond,
            then_expr: narrow,
            else_expr: deep_division,
        })
        .unwrap();
    let mut builder = SIRBuilder::new();
    SLTToSIRLowerer::new(false).lower(&mut builder, mux, &arena, &mut crate::HashMap::default());
    let eu = finish_lowering(builder);

    assert_eq!(branch_count(&eu), 1);
    let merge = eu
        .blocks
        .values()
        .find(|block| !block.params.is_empty())
        .unwrap();
    assert_eq!(eu.register_map[&merge.params[0]].width(), 16);
}

#[test]
fn four_state_expensive_mux_keeps_xz_select_semantics() {
    let mut arena = SLTNodeArena::new();
    let cond = input(&mut arena, 0, 1);
    let then_input = input(&mut arena, 1, 64);
    let else_input = input(&mut arena, 2, 64);
    let then_expr = operation_chain(&mut arena, then_input, BinaryOp::Add, 10, 10, 64);
    let else_expr = operation_chain(&mut arena, else_input, BinaryOp::Sub, 10, 30, 64);
    let mux = arena
        .alloc(SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        })
        .unwrap();
    let mut builder = SIRBuilder::new();
    SLTToSIRLowerer::new(true).lower(&mut builder, mux, &arena, &mut crate::HashMap::default());
    let eu = finish_lowering(builder);

    assert_eq!(branch_count(&eu), 0);
    assert_eq!(
        instruction_count(&eu, |inst| matches!(inst, SIRInstruction::Mux(..))),
        1
    );
}

#[test]
fn for_fold_is_not_a_pure_mux_arm() {
    let mut arena = SLTNodeArena::new();
    let initial = input(&mut arena, 0, 8);
    let update = input(&mut arena, 1, 8);
    let continue_cond = constant(&mut arena, 1, 1);
    let target = VarAtomBase::new(2, 0, 7);
    let fold = arena
        .alloc(SLTNode::ForFold {
            loop_var: 3,
            loop_width: 8,
            loop_signed: false,
            start: SLTLoopBound::Const(0),
            end: SLTLoopBound::Const(2),
            inclusive: false,
            step: 1,
            step_op: SLTStepOp::Add,
            reverse: false,
            result: crate::SLTForFoldResult::State(target),
            initials: vec![crate::SLTForUpdate {
                target,
                expr: initial,
            }],
            updates: vec![crate::SLTForUpdate {
                target,
                expr: update,
            }],
            effects: vec![crate::SLTForEffect::Event {
                site_id: 1,
                guard: None,
                emit_on_true: true,
                args: vec![update],
                fatal_error_code: None,
            }],
            continue_cond,
        })
        .unwrap();

    assert!(!SLTToSIRLowerer::new(false).is_speculatable_pure(fold, &arena));
}
