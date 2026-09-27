use super::*;

#[test]
fn signed_inputs_report_signedness() {
    let mut arena = SLTNodeArena::<u32>::new();
    let node = arena
        .alloc(SLTNode::Input {
            variable: 0,
            signed: true,
            index: vec![],
            access: BitAccess::new(0, 7),
        })
        .unwrap();
    let lowerer = SLTToSIRLowerer::new(false);
    assert!(lowerer.get_bound_signed(node, &arena));
}

#[test]
fn unsigned_inputs_report_unsignedness() {
    let mut arena = SLTNodeArena::<u32>::new();
    let node = arena
        .alloc(SLTNode::Input {
            variable: 0,
            signed: false,
            index: vec![],
            access: BitAccess::new(0, 7),
        })
        .unwrap();
    let lowerer = SLTToSIRLowerer::new(false);
    assert!(!lowerer.get_bound_signed(node, &arena));
}

#[test]
fn bit_count_results_are_unsigned_even_for_signed_inputs() {
    let mut arena = SLTNodeArena::<u32>::new();
    let input = arena
        .alloc(SLTNode::Input {
            variable: 0,
            signed: true,
            index: vec![],
            access: BitAccess::new(0, 7),
        })
        .unwrap();
    let lowerer = SLTToSIRLowerer::new(false);

    for op in [
        UnaryOp::PopCount,
        UnaryOp::CountLeadingZeros,
        UnaryOp::CountTrailingZeros,
    ] {
        let node = arena.alloc(SLTNode::Unary(op, input)).unwrap();
        assert!(!lowerer.get_bound_signed(node, &arena));
    }
}

#[test]
fn unary_value_operators_preserve_operand_expression_signedness() {
    let mut arena = SLTNodeArena::<u32>::new();
    let signed = arena
        .alloc(SLTNode::Input {
            variable: 0,
            signed: true,
            index: vec![],
            access: BitAccess::new(0, 7),
        })
        .unwrap();
    let unsigned = arena
        .alloc(SLTNode::Input {
            variable: 1,
            signed: false,
            index: vec![],
            access: BitAccess::new(0, 7),
        })
        .unwrap();
    let lowerer = SLTToSIRLowerer::new(false);

    for op in [
        UnaryOp::Ident,
        UnaryOp::ToTwoState,
        UnaryOp::Minus,
        UnaryOp::BitNot,
    ] {
        let signed_result = arena.alloc(SLTNode::Unary(op, signed)).unwrap();
        let unsigned_result = arena.alloc(SLTNode::Unary(op, unsigned)).unwrap();
        assert!(lowerer.get_bound_signed(signed_result, &arena), "{op}");
        assert!(!lowerer.get_bound_signed(unsigned_result, &arena), "{op}");
    }
}

#[test]
fn width_materialization_preserves_four_state_register_kind() {
    let lowerer = SLTToSIRLowerer::new(true);
    let mut builder = SIRBuilder::<usize>::new();
    let source = builder.alloc_logic(5);
    builder.emit(SIRInstruction::Imm(
        source,
        SIRValue::new_four_state(0x11u8, 0x10u8),
    ));

    let widened = lowerer.cast_reg_width_ext(&mut builder, source, 8, true);
    let narrowed = lowerer.cast_reg_width_ext(&mut builder, widened, 4, true);

    assert!(matches!(
        builder.register(&widened),
        RegisterType::Logic { width: 8 }
    ));
    assert!(matches!(
        builder.register(&narrowed),
        RegisterType::Logic { width: 4 }
    ));
}

#[test]
fn mixed_sign_subtraction_bound_is_unsigned() {
    let mut arena = SLTNodeArena::<u32>::new();
    let lhs = arena
        .alloc(SLTNode::Constant(1u8.into(), 0u8.into(), 8, false))
        .unwrap();
    let rhs = arena
        .alloc(SLTNode::Input {
            variable: 0,
            signed: true,
            index: vec![],
            access: BitAccess::new(0, 7),
        })
        .unwrap();
    let node = arena
        .alloc(SLTNode::Binary(lhs, BinaryOp::Sub, rhs))
        .unwrap();
    let lowerer = SLTToSIRLowerer::new(false);
    assert!(!lowerer.get_bound_signed(node, &arena));
}

#[test]
fn mixed_sign_mux_bound_is_unsigned() {
    let mut arena = SLTNodeArena::<u32>::new();
    let cond = arena
        .alloc(SLTNode::Constant(1u8.into(), 0u8.into(), 1, false))
        .unwrap();
    let then_expr = arena
        .alloc(SLTNode::Input {
            variable: 0,
            signed: true,
            index: vec![],
            access: BitAccess::new(0, 7),
        })
        .unwrap();
    let else_expr = arena
        .alloc(SLTNode::Input {
            variable: 1,
            signed: false,
            index: vec![],
            access: BitAccess::new(0, 7),
        })
        .unwrap();
    let node = arena
        .alloc(SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        })
        .unwrap();
    let lowerer = SLTToSIRLowerer::new(false);
    assert!(!lowerer.get_bound_signed(node, &arena));
}

#[test]
fn comparison_bound_is_not_signed() {
    let mut arena = SLTNodeArena::<u32>::new();
    let lhs = arena
        .alloc(SLTNode::Input {
            variable: 0,
            signed: false,
            index: vec![],
            access: BitAccess::new(0, 7),
        })
        .unwrap();
    let rhs = arena
        .alloc(SLTNode::Input {
            variable: 1,
            signed: true,
            index: vec![],
            access: BitAccess::new(0, 7),
        })
        .unwrap();
    let node = arena
        .alloc(SLTNode::Binary(lhs, BinaryOp::LtS, rhs))
        .unwrap();
    let lowerer = SLTToSIRLowerer::new(false);
    assert!(!lowerer.get_bound_signed(node, &arena));
}

#[test]
fn unsigned_target_bound_zero_extends_signed_slice_without_losing_state_kind() {
    let mut arena = SLTNodeArena::<u32>::new();
    let inner = arena
        .alloc(SLTNode::Input {
            variable: 0,
            signed: true,
            index: vec![],
            access: BitAccess::new(0, 15),
        })
        .unwrap();
    let casted = arena
        .alloc(SLTNode::Slice {
            expr: inner,
            access: BitAccess::new(0, 7),
        })
        .unwrap();
    let mut builder = SIRBuilder::<u32>::new();
    let mut cache = crate::HashMap::default();
    let lowerer = SLTToSIRLowerer::new(false);
    let reg = lowerer.lower_bound(
        &mut builder,
        &SLTLoopBound::Expr(casted),
        8,
        9,
        false,
        &arena,
        &mut cache,
        None,
    );
    assert!(matches!(
        builder.register(&reg),
        RegisterType::Logic { width: 9 }
    ));
    assert!(!lowerer.get_bound_signed(casted, &arena));
}
