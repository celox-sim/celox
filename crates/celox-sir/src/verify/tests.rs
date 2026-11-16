use crate::HashMap;
use crate::{SIRInstruction, SIRValue};

use super::*;

fn bit(width: usize) -> RegisterType {
    RegisterType::Bit {
        width,
        signed: false,
    }
}

fn signed_bit(width: usize) -> RegisterType {
    RegisterType::Bit {
        width,
        signed: true,
    }
}

fn logic(width: usize) -> RegisterType {
    RegisterType::Logic { width }
}

fn unit(
    blocks: impl IntoIterator<Item = BasicBlock<usize>>,
    registers: impl IntoIterator<Item = (RegisterId, RegisterType)>,
) -> ExecutionUnit<usize> {
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: blocks.into_iter().map(|b| (b.id, b)).collect(),
        register_map: registers.into_iter().collect::<HashMap<_, _>>(),
    }
}

/// A program whose process stores 32-bit `r0` into slot 0 of function
/// 0 and reads its slot 1, and whose function copies slot 0 to slot 1
/// through its frame slot addresses 100 and 101. `edit` changes it
/// before it is verified.
fn calls(edit: impl FnOnce(&mut crate::SirProgram<usize, usize>)) -> Result<(), SirVerifyError> {
    let call = SIRInstruction::Call {
        function: crate::FunctionId(0),
        frame: RegisterId(1),
        args: vec![(0, RegisterId(0))],
        outputs: vec![1],
        dst: Some(RegisterId(2)),
        effects: Default::default(),
    };
    let process = unit(
        [BasicBlock {
            id: BlockId(0),
            params: vec![],
            instructions: vec![
                SIRInstruction::Imm(RegisterId(0), SIRValue::new(3u8)),
                SIRInstruction::Imm(RegisterId(1), SIRValue::new(0u8)),
                call,
            ],
            terminator: SIRTerminator::Return,
        }],
        [
            (RegisterId(0), bit(32)),
            (RegisterId(1), bit(32)),
            (RegisterId(2), bit(32)),
        ],
    );
    let body = unit(
        [BasicBlock {
            id: BlockId(0),
            params: vec![],
            instructions: vec![
                SIRInstruction::Load(RegisterId(0), 100, SIROffset::Static(0), 32),
                SIRInstruction::Store(
                    101,
                    SIROffset::Static(0),
                    32,
                    RegisterId(0),
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            terminator: SIRTerminator::Return,
        }],
        [(RegisterId(0), bit(32))],
    );
    let slot = |addr, offset| crate::FrameSlot {
        addr,
        offset,
        width: 32,
        four_state: false,
        handle: false,
        element_width: None,
    };
    let mut program = crate::SirProgram {
        eval_comb: Vec::new(),
        eval_apply_ffs: HashMap::default(),
        eval_comb_apply_ffs: HashMap::default(),
        merge_comb_apply: false,
        eval_comb_stateful: None,
        eval_only_ffs: HashMap::default(),
        apply_ffs: HashMap::default(),
        parallel: None,
        processes: vec![process],
        functions: vec![crate::FunctionUnit {
            unit: body,
            frame: crate::FrameLayout {
                size: 8,
                slots: vec![slot(100, 0), slot(101, 4)],
            },
            effects: Default::default(),
        }],
    };
    program.summarize_calls();
    edit(&mut program);
    program.verify_functions()
}

fn invariant(result: Result<(), SirVerifyError>) -> &'static str {
    result.expect_err("an invalid program").invariant
}

fn process_call(program: &mut crate::SirProgram<usize, usize>) -> &mut SIRInstruction<usize> {
    &mut program.processes[0]
        .blocks
        .get_mut(&BlockId(0))
        .unwrap()
        .instructions[2]
}

#[test]
fn accepts_a_call_that_fills_its_callee_frame() {
    calls(|_| {}).unwrap();
}

#[test]
fn rejects_calls_that_do_not_match_the_callee_frame() {
    assert_eq!(
        invariant(calls(|program| {
            let SIRInstruction::Call { function, .. } = process_call(program) else {
                unreachable!()
            };
            *function = crate::FunctionId(1);
        })),
        "CALL.FUNCTION_EXISTS"
    );
    assert_eq!(
        invariant(calls(|program| {
            let SIRInstruction::Call { args, .. } = process_call(program) else {
                unreachable!()
            };
            args[0].0 = 2;
        })),
        "CALL.SLOT_EXISTS"
    );
    assert_eq!(
        invariant(calls(|program| {
            program.processes[0]
                .register_map
                .insert(RegisterId(0), bit(16));
        })),
        "CALL.ARGUMENT_WIDTH"
    );
    assert_eq!(
        invariant(calls(|program| {
            let SIRInstruction::Call { outputs, .. } = process_call(program) else {
                unreachable!()
            };
            outputs.push(0);
        })),
        "CALL.OUTPUT_WIDTH"
    );
}

#[test]
fn keeps_frame_slots_in_their_frame_and_function() {
    assert_eq!(
        invariant(calls(|program| program.functions[0].frame.size = 6)),
        "FRAME.SLOT_IN_FRAME"
    );
    assert_eq!(
        invariant(calls(
            |program| program.functions[0].frame.slots[1].addr = 100
        )),
        "FRAME.SLOT_ADDRESS_UNIQUE"
    );
    assert_eq!(
        invariant(calls(|program| {
            program.processes[0]
                .blocks
                .get_mut(&BlockId(0))
                .unwrap()
                .instructions
                .push(SIRInstruction::Load(
                    RegisterId(2),
                    100,
                    SIROffset::Static(0),
                    32,
                ));
        })),
        "FRAME.SLOT_OWN_FUNCTION"
    );
    assert_eq!(
        invariant(calls(|program| {
            let body = program.functions[0]
                .unit
                .blocks
                .get_mut(&BlockId(0))
                .unwrap();
            body.instructions[0] =
                SIRInstruction::Load(RegisterId(0), 100, SIROffset::Static(8), 32);
        })),
        "FRAME.ACCESS_IN_SLOT"
    );
    assert_eq!(
        invariant(calls(|program| {
            let body = program.functions[0]
                .unit
                .blocks
                .get_mut(&BlockId(0))
                .unwrap();
            body.instructions[0] =
                SIRInstruction::Load(RegisterId(0), 100, SIROffset::Dynamic(RegisterId(0)), 32);
        })),
        "FRAME.STATIC_OFFSET"
    );
}

#[test]
fn array_slots_are_accessed_within_an_element() {
    // A slot holding an unpacked array of one 32-bit element.
    let dynamic = |offset: SIROffset, width: usize| {
        move |program: &mut crate::SirProgram<usize, usize>| {
            program.functions[0].frame.slots[0].element_width = Some(32);
            let body = program.functions[0]
                .unit
                .blocks
                .get_mut(&BlockId(0))
                .unwrap();
            body.instructions[0] = SIRInstruction::Load(RegisterId(0), 100, offset, width);
            program.summarize_calls();
        }
    };
    calls(dynamic(SIROffset::Dynamic(RegisterId(0)), 32)).unwrap();
    calls(dynamic(
        SIROffset::Element {
            index: RegisterId(0),
            element_width: 32,
            bit_offset: 0,
            dynamic_bit_offset: None,
        },
        32,
    ))
    .unwrap();
    // Elements of another width, and accesses wider than an element.
    assert_eq!(
        invariant(calls(dynamic(
            SIROffset::Element {
                index: RegisterId(0),
                element_width: 16,
                bit_offset: 0,
                dynamic_bit_offset: None,
            },
            16,
        ))),
        "FRAME.STATIC_OFFSET"
    );
    assert_eq!(
        invariant(calls(dynamic(SIROffset::Dynamic(RegisterId(0)), 33))),
        "FRAME.STATIC_OFFSET"
    );
}

#[test]
fn rejects_call_effects_that_are_not_current() {
    // A store the callee gained since its effects were computed.
    assert_eq!(
        invariant(calls(|program| {
            let body = program.functions[0]
                .unit
                .blocks
                .get_mut(&BlockId(0))
                .unwrap();
            body.instructions.push(SIRInstruction::Store(
                5,
                SIROffset::Static(0),
                32,
                RegisterId(0),
                Vec::new(),
                Vec::new(),
            ));
        })),
        "CALL.EFFECTS_CURRENT"
    );
    assert_eq!(
        invariant(calls(|program| {
            let SIRInstruction::Call { effects, .. } = process_call(program) else {
                unreachable!()
            };
            *effects = Default::default();
            program.functions[0].effects = std::sync::Arc::new(crate::CallEffects {
                host: crate::HostEffects::ALL,
                ..Default::default()
            });
        })),
        "CALL.EFFECTS_CURRENT"
    );
}

#[test]
fn accepts_well_formed_branch_and_block_parameter() {
    let eu = unit(
        [
            BasicBlock {
                id: BlockId(0),
                params: vec![],
                instructions: vec![
                    SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                    SIRInstruction::Imm(RegisterId(1), SIRValue::new(7u8)),
                ],
                terminator: SIRTerminator::Jump(BlockId(1), vec![RegisterId(1)]),
            },
            BasicBlock {
                id: BlockId(1),
                params: vec![RegisterId(2)],
                instructions: vec![SIRInstruction::Unary(
                    RegisterId(3),
                    UnaryOp::Ident,
                    RegisterId(2),
                )],
                terminator: SIRTerminator::Return,
            },
        ],
        [
            (RegisterId(0), bit(1)),
            (RegisterId(1), bit(8)),
            (RegisterId(2), bit(8)),
            (RegisterId(3), bit(8)),
        ],
    );
    assert_eq!(eu.verify_result(), Ok(()));
}

#[test]
fn rejects_use_not_dominated_by_definition() {
    let eu = unit(
        [
            BasicBlock {
                id: BlockId(0),
                params: vec![],
                instructions: vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8))],
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), vec![]),
                    false_block: (BlockId(2), vec![]),
                },
            },
            BasicBlock {
                id: BlockId(1),
                params: vec![],
                instructions: vec![SIRInstruction::Imm(RegisterId(1), SIRValue::new(3u8))],
                terminator: SIRTerminator::Jump(BlockId(2), vec![]),
            },
            BasicBlock {
                id: BlockId(2),
                params: vec![],
                instructions: vec![SIRInstruction::Unary(
                    RegisterId(2),
                    UnaryOp::Ident,
                    RegisterId(1),
                )],
                terminator: SIRTerminator::Return,
            },
        ],
        [
            (RegisterId(0), bit(1)),
            (RegisterId(1), bit(8)),
            (RegisterId(2), bit(8)),
        ],
    );
    assert_eq!(
        eu.verify_result().unwrap_err().invariant,
        "SSA.DEFINITION_DOMINATES_USE"
    );
}

#[test]
fn rejects_edge_type_mismatch() {
    let eu = unit(
        [
            BasicBlock {
                id: BlockId(0),
                params: vec![],
                instructions: vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8))],
                terminator: SIRTerminator::Jump(BlockId(1), vec![RegisterId(0)]),
            },
            BasicBlock {
                id: BlockId(1),
                params: vec![RegisterId(1)],
                instructions: vec![],
                terminator: SIRTerminator::Return,
            },
        ],
        [(RegisterId(0), bit(1)), (RegisterId(1), bit(2))],
    );
    assert_eq!(
        eu.verify_result().unwrap_err().invariant,
        "TYPE.EDGE_ARGUMENT"
    );
}

fn extern_call(args: usize, argument_type: RegisterType) -> ExecutionUnit<usize> {
    let args = (0..args).map(RegisterId).collect::<Vec<_>>();
    unit(
        [BasicBlock {
            id: BlockId(0),
            params: vec![],
            instructions: args
                .iter()
                .map(|&arg| SIRInstruction::Imm(arg, SIRValue::new(1u8)))
                .chain([SIRInstruction::ExternCall {
                    dst: None,
                    func: 0,
                    args: args.clone(),
                }])
                .collect(),
            terminator: SIRTerminator::Return,
        }],
        args.iter().map(|&arg| (arg, argument_type.clone())),
    )
}

#[test]
fn checks_extern_call_argument_count_and_types() {
    assert_eq!(
        extern_call(crate::MAX_EXTERN_CALL_ARGUMENTS, signed_bit(32)).verify_result(),
        Ok(())
    );
    assert_eq!(extern_call(1, logic(1)).verify_result(), Ok(()));
    assert_eq!(
        extern_call(crate::MAX_EXTERN_CALL_ARGUMENTS + 1, bit(32))
            .verify_result()
            .unwrap_err()
            .invariant,
        "TYPE.EXTERN_CALL_ARGUMENT_COUNT"
    );
    for argument_type in [bit(7), logic(2)] {
        assert_eq!(
            extern_call(1, argument_type)
                .verify_result()
                .unwrap_err()
                .invariant,
            "TYPE.EXTERN_CALL_C_INTEGER"
        );
    }
}

#[test]
fn rejects_mux_arm_width_mismatch() {
    let eu = unit(
        [BasicBlock {
            id: BlockId(0),
            params: vec![],
            instructions: vec![SIRInstruction::Mux(
                RegisterId(0),
                RegisterId(1),
                RegisterId(2),
                RegisterId(3),
            )],
            terminator: SIRTerminator::Return,
        }],
        [
            (RegisterId(0), bit(8)),
            (RegisterId(1), bit(1)),
            (RegisterId(2), bit(8)),
            (RegisterId(3), bit(7)),
        ],
    );
    assert_eq!(
        eu.verify_result().unwrap_err().invariant,
        "TYPE.MUX_ELSE_WIDTH"
    );
}

#[test]
fn rejects_non_boolean_operator_results() {
    let comparison = unit(
        [BasicBlock {
            id: BlockId(0),
            params: vec![],
            instructions: vec![SIRInstruction::Binary(
                RegisterId(0),
                RegisterId(1),
                BinaryOp::LtU,
                RegisterId(2),
            )],
            terminator: SIRTerminator::Return,
        }],
        [
            (RegisterId(0), bit(8)),
            (RegisterId(1), bit(8)),
            (RegisterId(2), bit(8)),
        ],
    );
    assert_eq!(
        comparison.verify_result().unwrap_err().invariant,
        "TYPE.COMPARISON_RESULT_WIDTH"
    );

    let reduction = unit(
        [BasicBlock {
            id: BlockId(0),
            params: vec![],
            instructions: vec![SIRInstruction::Unary(
                RegisterId(0),
                UnaryOp::Or,
                RegisterId(1),
            )],
            terminator: SIRTerminator::Return,
        }],
        [(RegisterId(0), bit(8)), (RegisterId(1), bit(8))],
    );
    assert_eq!(
        reduction.verify_result().unwrap_err().invariant,
        "TYPE.REDUCTION_RESULT_WIDTH"
    );
}

#[test]
fn rejects_width_changing_two_state_conversion() {
    let eu = unit(
        [BasicBlock {
            id: BlockId(0),
            params: vec![],
            instructions: vec![SIRInstruction::Unary(
                RegisterId(0),
                UnaryOp::ToTwoState,
                RegisterId(1),
            )],
            terminator: SIRTerminator::Return,
        }],
        [(RegisterId(0), bit(8)), (RegisterId(1), bit(4))],
    );
    assert_eq!(
        eu.verify_result().unwrap_err().invariant,
        "TYPE.TWO_STATE_WIDTH"
    );
}

#[test]
fn rejects_non_bit_two_state_destination() {
    let eu = unit(
        [BasicBlock {
            id: BlockId(0),
            params: vec![],
            instructions: vec![SIRInstruction::Unary(
                RegisterId(0),
                UnaryOp::ToTwoState,
                RegisterId(1),
            )],
            terminator: SIRTerminator::Return,
        }],
        [(RegisterId(0), logic(8)), (RegisterId(1), logic(8))],
    );
    assert_eq!(
        eu.verify_result().unwrap_err().invariant,
        "TYPE.TWO_STATE_DESTINATION"
    );
}

#[test]
fn rejects_width_changing_value_unary() {
    for op in [UnaryOp::Minus, UnaryOp::BitNot] {
        let eu = unit(
            [BasicBlock {
                id: BlockId(0),
                params: vec![],
                instructions: vec![SIRInstruction::Unary(RegisterId(0), op, RegisterId(1))],
                terminator: SIRTerminator::Return,
            }],
            [(RegisterId(0), bit(8)), (RegisterId(1), bit(4))],
        );
        assert_eq!(
            eu.verify_result().unwrap_err().invariant,
            "TYPE.UNARY_VALUE_WIDTH"
        );
    }
}

#[test]
fn rejects_signed_bit_boolean_result() {
    let eu = unit(
        [BasicBlock {
            id: BlockId(0),
            params: vec![],
            instructions: vec![SIRInstruction::Binary(
                RegisterId(0),
                RegisterId(1),
                BinaryOp::Eq,
                RegisterId(2),
            )],
            terminator: SIRTerminator::Return,
        }],
        [
            (RegisterId(0), signed_bit(1)),
            (RegisterId(1), bit(8)),
            (RegisterId(2), bit(8)),
        ],
    );
    assert_eq!(
        eu.verify_result().unwrap_err().invariant,
        "TYPE.BOOLEAN_RESULT_UNSIGNED"
    );
}

#[test]
fn rejects_comparison_operand_width_mismatch() {
    let eu = unit(
        [BasicBlock {
            id: BlockId(0),
            params: vec![],
            instructions: vec![SIRInstruction::Binary(
                RegisterId(0),
                RegisterId(1),
                BinaryOp::LtS,
                RegisterId(2),
            )],
            terminator: SIRTerminator::Return,
        }],
        [
            (RegisterId(0), bit(1)),
            (RegisterId(1), bit(8)),
            (RegisterId(2), bit(4)),
        ],
    );
    assert_eq!(
        eu.verify_result().unwrap_err().invariant,
        "TYPE.COMPARISON_OPERAND_WIDTH"
    );
}

#[test]
fn rejects_divrem_width_mismatch() {
    for op in [
        BinaryOp::DivU,
        BinaryOp::DivS,
        BinaryOp::RemU,
        BinaryOp::RemS,
    ] {
        let eu = unit(
            [BasicBlock {
                id: BlockId(0),
                params: vec![],
                instructions: vec![SIRInstruction::Binary(
                    RegisterId(0),
                    RegisterId(1),
                    op,
                    RegisterId(2),
                )],
                terminator: SIRTerminator::Return,
            }],
            [
                (RegisterId(0), bit(8)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), bit(4)),
            ],
        );
        assert_eq!(
            eu.verify_result().unwrap_err().invariant,
            "TYPE.DIVREM_RHS_WIDTH"
        );
    }
}

#[test]
fn rejects_masked_bit_immediate() {
    let eu = unit(
        [BasicBlock {
            id: BlockId(0),
            params: vec![],
            instructions: vec![SIRInstruction::Imm(
                RegisterId(0),
                SIRValue::new_four_state(1u8, 1u8),
            )],
            terminator: SIRTerminator::Return,
        }],
        [(RegisterId(0), bit(1))],
    );
    assert_eq!(
        eu.verify_result().unwrap_err().invariant,
        "TYPE.BIT_IMMEDIATE_MASK"
    );
}

#[test]
fn rejects_noncanonical_branch_condition() {
    for condition_type in [logic(1), bit(2), signed_bit(1)] {
        let eu = unit(
            [
                BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    instructions: vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(0u8))],
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(2), vec![]),
                    },
                },
                BasicBlock {
                    id: BlockId(1),
                    params: vec![],
                    instructions: vec![],
                    terminator: SIRTerminator::Return,
                },
                BasicBlock {
                    id: BlockId(2),
                    params: vec![],
                    instructions: vec![],
                    terminator: SIRTerminator::Return,
                },
            ],
            [(RegisterId(0), condition_type)],
        );
        assert_eq!(
            eu.verify_result().unwrap_err().invariant,
            "TYPE.BRANCH_CONDITION"
        );
    }
}

fn bit_count_unit(op: UnaryOp, source_width: usize, result_width: usize) -> ExecutionUnit<usize> {
    unit(
        [BasicBlock {
            id: BlockId(0),
            params: vec![],
            instructions: vec![
                SIRInstruction::Imm(RegisterId(0), SIRValue::new(0u8)),
                SIRInstruction::Unary(RegisterId(1), op, RegisterId(0)),
            ],
            terminator: SIRTerminator::Return,
        }],
        [
            (RegisterId(0), bit(source_width)),
            (RegisterId(1), bit(result_width)),
        ],
    )
}

#[test]
fn accepts_bit_count_result_that_can_represent_source_width() {
    for op in [
        UnaryOp::PopCount,
        UnaryOp::CountLeadingZeros,
        UnaryOp::CountTrailingZeros,
    ] {
        assert_eq!(bit_count_unit(op, 8, 4).verify_result(), Ok(()));
        assert_eq!(bit_count_unit(op, 8, 8).verify_result(), Ok(()));
    }
}

#[test]
fn rejects_bit_count_result_that_cannot_represent_source_width() {
    for op in [
        UnaryOp::PopCount,
        UnaryOp::CountLeadingZeros,
        UnaryOp::CountTrailingZeros,
    ] {
        let error = bit_count_unit(op, 8, 3).verify_result().unwrap_err();
        assert_eq!(error.invariant, "TYPE.BIT_COUNT_RESULT_WIDTH");
        assert_eq!(error.block, Some(BlockId(0)));
        assert_eq!(error.instruction, Some(1));
        assert!(error.message.contains("requires at least 4 result bits"));
    }
}
