use super::*;

#[test]
fn exact_zero_analysis_collapses_repeated_concat_dependencies() {
    let zero = RegisterId(0);
    let wide_zero = RegisterId(1);
    let sliced_zero = RegisterId(2);
    let nonzero = RegisterId(3);
    let mixed = RegisterId(4);
    let eu: ExecutionUnit<()> = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![],
                instructions: vec![
                    SIRInstruction::Imm(zero, SIRValue::new(0u8)),
                    SIRInstruction::Concat(wide_zero, vec![zero; 4096]),
                    SIRInstruction::Slice(sliced_zero, wide_zero, 0, 64),
                    SIRInstruction::Imm(nonzero, SIRValue::new(1u8)),
                    SIRInstruction::Concat(mixed, vec![zero, nonzero]),
                ],
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map: HashMap::default(),
    };

    let zeros = collect_exact_zero_registers(&eu, [sliced_zero, mixed]);
    assert!(zeros.contains(&zero));
    assert!(zeros.contains(&wide_zero));
    assert!(zeros.contains(&sliced_zero));
    assert!(!zeros.contains(&nonzero));
    assert!(!zeros.contains(&mixed));
}

#[test]
fn test_sirvalue_display() {
    let val = SIRValue::new(42u64);
    let display = format!("{}", val);
    assert!(display.contains("SIRValue"));
    assert!(display.contains("0x2a")); // 42 in hex
}

#[test]
fn test_absoluteaddr_display() {
    let addr = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: celox_design::StateObjectId(0),
    };
    let display = format!("{}", addr);
    assert!(display.contains("AbsoluteAddr"));
    assert!(display.contains("inst0"));
    assert!(display.contains("state0"));
}

#[test]
fn test_glueaddr_display() {
    let parent_addr = celox_frontend_veryl::GlueAddr::Parent(veryl_analyzer::ir::VarId::default());
    let parent_display = format!("{}", parent_addr);
    assert!(parent_display.contains("GlueAddr::Parent"));
    assert!(parent_display.contains("var0"));

    let child_addr = celox_frontend_veryl::GlueAddr::Child(veryl_analyzer::ir::VarId::default());
    let child_display = format!("{}", child_addr);
    assert!(child_display.contains("GlueAddr::Child"));
    assert!(child_display.contains("var0"));
}

#[test]
fn test_instanceid_display() {
    let id = InstanceId(42);
    let display = format!("{}", id);
    assert_eq!(display, "inst42");
}

#[test]
fn test_binaryop_display() {
    assert_eq!(format!("{}", BinaryOp::Add), "Add");
    assert_eq!(format!("{}", BinaryOp::Sub), "Sub");
    assert_eq!(format!("{}", BinaryOp::Mul), "Mul");
    assert_eq!(format!("{}", BinaryOp::Xor), "Xor");
}

#[test]
fn test_unaryop_display() {
    assert_eq!(format!("{}", UnaryOp::Minus), "Minus");
    assert_eq!(format!("{}", UnaryOp::LogicNot), "LogicNot");
    assert_eq!(format!("{}", UnaryOp::BitNot), "BitNot");
    assert_eq!(format!("{}", UnaryOp::PopCount), "PopCount");
    assert_eq!(
        format!("{}", UnaryOp::CountLeadingZeros),
        "CountLeadingZeros"
    );
    assert_eq!(
        format!("{}", UnaryOp::CountTrailingZeros),
        "CountTrailingZeros"
    );
}

#[test]
fn bit_count_result_width_represents_operand_width() {
    for (operand_width, expected) in [
        (0, 0),
        (1, 1),
        (2, 2),
        (3, 2),
        (8, 4),
        (usize::MAX, usize::BITS as usize),
    ] {
        for op in [
            UnaryOp::PopCount,
            UnaryOp::CountLeadingZeros,
            UnaryOp::CountTrailingZeros,
        ] {
            assert_eq!(op.result_width(operand_width), expected, "{op}");
        }
    }
}

#[test]
fn bit_count_unary_ops_roundtrip_through_serde() {
    for op in [
        UnaryOp::PopCount,
        UnaryOp::CountLeadingZeros,
        UnaryOp::CountTrailingZeros,
    ] {
        let encoded = serde_json::to_string(&op).unwrap();
        let decoded: UnaryOp = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, op);
    }
}

#[test]
fn test_sirinstruction_display() {
    // Test Imm instruction
    let imm: SIRInstruction<i32> = SIRInstruction::Imm(RegisterId(0), SIRValue::new(42u64));
    let imm_display = format!("{}", imm);
    assert!(imm_display.contains("r0"));
    assert!(imm_display.contains("SIRValue"));

    // Test Binary instruction
    let binary: SIRInstruction<i32> =
        SIRInstruction::Binary(RegisterId(0), RegisterId(1), BinaryOp::Add, RegisterId(2));
    let binary_display = format!("{}", binary);
    assert!(binary_display.contains("r0"));
    assert!(binary_display.contains("r1"));
    assert!(binary_display.contains("r2"));
    assert!(binary_display.contains("Add"));

    // Test Unary instruction
    let unary: SIRInstruction<i32> =
        SIRInstruction::Unary(RegisterId(0), UnaryOp::Minus, RegisterId(1));
    let unary_display = format!("{}", unary);
    assert!(unary_display.contains("r0"));
    assert!(unary_display.contains("r1"));
    assert!(unary_display.contains("Minus"));
}

#[test]
fn test_sirterminator_display() {
    // Test Jump
    let jump = SIRTerminator::Jump(BlockId(1), vec![RegisterId(0), RegisterId(1)]);
    let jump_display = format!("{}", jump);
    assert!(jump_display.contains("Jump"));
    assert!(jump_display.contains("b1"));

    // Test Return
    let ret = SIRTerminator::Return;
    let ret_display = format!("{}", ret);
    assert_eq!(ret_display, "Return");

    // Test Branch
    let branch = SIRTerminator::Branch {
        cond: RegisterId(0),
        true_block: (BlockId(1), vec![]),
        false_block: (BlockId(2), vec![]),
    };
    let branch_display = format!("{}", branch);
    assert!(branch_display.contains("Branch"));
    assert!(branch_display.contains("b1"));
    assert!(branch_display.contains("b2"));
}

#[test]
fn test_basicblock_display() {
    let _block: BasicBlock<i32> = BasicBlock {
        id: BlockId(0),
        params: vec![RegisterId(0), RegisterId(1)],
        instructions: vec![
            SIRInstruction::Imm(RegisterId(2), SIRValue::new(42u64)),
            SIRInstruction::Binary(RegisterId(3), RegisterId(0), BinaryOp::Add, RegisterId(2)),
        ],
        terminator: SIRTerminator::Return,
    };

    let block_display = format!("{}", _block);
    assert!(block_display.contains("b0:"));
    assert!(block_display.contains("params:"));
    assert!(block_display.contains("r0"));
    assert!(block_display.contains("r1"));
    assert!(block_display.contains("Add"));
    assert!(block_display.contains("Return"));
}

#[test]
fn single_predecessor_inlining_rewrites_dominated_parameter_uses() {
    let mut eu: ExecutionUnit<()> = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0)],
                instructions: Vec::new(),
                terminator: SIRTerminator::Jump(BlockId(1), vec![RegisterId(0)]),
            },
            BasicBlock {
                id: BlockId(1),
                params: vec![RegisterId(1)],
                instructions: Vec::new(),
                terminator: SIRTerminator::Jump(BlockId(2), Vec::new()),
            },
            BasicBlock {
                id: BlockId(2),
                params: Vec::new(),
                instructions: vec![SIRInstruction::Unary(
                    RegisterId(2),
                    UnaryOp::Ident,
                    RegisterId(1),
                )],
                terminator: SIRTerminator::Return,
            },
        ]
        .into_iter()
        .map(|block| (block.id, block))
        .collect(),
        register_map: (0..3)
            .map(|register| {
                (
                    RegisterId(register),
                    RegisterType::Bit {
                        width: 8,
                        signed: false,
                    },
                )
            })
            .collect(),
    };
    eu.verify_result().unwrap();

    assert!(inline_single_predecessor_jumps(&mut eu).unwrap());
    eu.verify_result().unwrap();
    assert_eq!(eu.blocks.len(), 1);
    assert!(matches!(
        eu.blocks[&BlockId(0)].instructions.as_slice(),
        [SIRInstruction::Unary(
            RegisterId(2),
            UnaryOp::Ident,
            RegisterId(0)
        )]
    ));
}

#[test]
fn single_predecessor_inlining_handles_deep_linear_cfg() {
    const BLOCK_COUNT: usize = 20_000;

    let mut eu: ExecutionUnit<()> = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: (0..BLOCK_COUNT)
            .map(|index| {
                let id = BlockId(index);
                let terminator = if index + 1 == BLOCK_COUNT {
                    SIRTerminator::Return
                } else {
                    SIRTerminator::Jump(BlockId(index + 1), Vec::new())
                };
                (
                    id,
                    BasicBlock {
                        id,
                        params: Vec::new(),
                        instructions: Vec::new(),
                        terminator,
                    },
                )
            })
            .collect(),
        register_map: crate::HashMap::default(),
    };
    eu.verify_result().unwrap();

    assert!(inline_single_predecessor_jumps(&mut eu).unwrap());
    assert_eq!(eu.blocks.len(), 1);
    assert_eq!(eu.blocks[&BlockId(0)].terminator, SIRTerminator::Return);
    eu.verify_result().unwrap();
}
