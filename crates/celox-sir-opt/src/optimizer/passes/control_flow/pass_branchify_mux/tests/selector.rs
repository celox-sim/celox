use super::*;

fn selector_predicate_unit(
    duplicate_last_selector: bool,
    add_store: bool,
) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let mut instructions = vec![imm(3, 0), imm(4, 1), imm(5, 2)];
    if duplicate_last_selector {
        instructions[2] = imm(5, 1);
    }
    instructions.extend([
        SIRInstruction::Binary(
            RegisterId(6),
            RegisterId(1),
            crate::ir::BinaryOp::Eq,
            RegisterId(3),
        ),
        SIRInstruction::Binary(
            RegisterId(7),
            RegisterId(1),
            crate::ir::BinaryOp::Eq,
            RegisterId(4),
        ),
        SIRInstruction::Binary(
            RegisterId(8),
            RegisterId(1),
            crate::ir::BinaryOp::Eq,
            RegisterId(5),
        ),
        SIRInstruction::Load(RegisterId(9), addr(10), SIROffset::Static(0), 64),
        SIRInstruction::Load(RegisterId(10), addr(11), SIROffset::Static(0), 64),
        SIRInstruction::Load(RegisterId(11), addr(12), SIROffset::Static(0), 64),
        SIRInstruction::Binary(
            RegisterId(15),
            RegisterId(9),
            crate::ir::BinaryOp::Eq,
            RegisterId(12),
        ),
        SIRInstruction::Binary(
            RegisterId(16),
            RegisterId(10),
            crate::ir::BinaryOp::Eq,
            RegisterId(13),
        ),
        SIRInstruction::Binary(
            RegisterId(17),
            RegisterId(11),
            crate::ir::BinaryOp::Eq,
            RegisterId(14),
        ),
        SIRInstruction::Binary(
            RegisterId(18),
            RegisterId(6),
            crate::ir::BinaryOp::LogicAnd,
            RegisterId(15),
        ),
        SIRInstruction::Binary(
            RegisterId(19),
            RegisterId(7),
            crate::ir::BinaryOp::LogicAnd,
            RegisterId(16),
        ),
        SIRInstruction::Binary(
            RegisterId(20),
            RegisterId(8),
            crate::ir::BinaryOp::LogicAnd,
            RegisterId(17),
        ),
        SIRInstruction::Binary(
            RegisterId(21),
            RegisterId(18),
            crate::ir::BinaryOp::LogicOr,
            RegisterId(19),
        ),
        SIRInstruction::Binary(
            RegisterId(22),
            RegisterId(21),
            crate::ir::BinaryOp::LogicOr,
            RegisterId(20),
        ),
        SIRInstruction::Binary(
            RegisterId(23),
            RegisterId(0),
            crate::ir::BinaryOp::LogicAnd,
            RegisterId(22),
        ),
        SIRInstruction::Unary(
            RegisterId(24),
            crate::ir::UnaryOp::ToTwoState,
            RegisterId(23),
        ),
    ]);
    if add_store {
        instructions.push(store(20, 12));
    }
    cfg_unit(
        25,
        &[0, 6, 7, 8, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24],
        vec![
            BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instructions,
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(24),
                    true_block: (BlockId(1), Vec::new()),
                    false_block: (BlockId(2), Vec::new()),
                },
            },
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Return,
            },
            BasicBlock {
                id: BlockId(2),
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Return,
            },
        ],
    )
}

#[test]
fn selector_disjoint_payload_loads_become_control_dependent() {
    let mut eu = selector_predicate_unit(false, false);
    let mut next_block_id = 3;
    let mut reg_counter = 24;

    assert_eq!(
        branchify_selector_guarded_predicates(&mut eu, &mut next_block_id, &mut reg_counter,),
        1
    );

    let head = &eu.blocks[&BlockId(0)];
    assert!(
        !head
            .instructions
            .iter()
            .any(|instruction| matches!(instruction, SIRInstruction::Load(..)))
    );
    let SIRTerminator::Branch {
        cond,
        true_block,
        false_block,
    } = &head.terminator
    else {
        panic!("expected common guard branch");
    };
    assert_eq!(*cond, RegisterId(0));
    assert_eq!(true_block.0, BlockId(3));
    assert_eq!(false_block.0, BlockId(2));

    let mut decision = true_block.0;
    let mut payload_loads = Vec::new();
    for expected_selector in [RegisterId(6), RegisterId(7), RegisterId(8)] {
        let block = &eu.blocks[&decision];
        let SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } = &block.terminator
        else {
            panic!("expected selector decision");
        };
        assert_eq!(*cond, expected_selector);
        let payload = &eu.blocks[&true_block.0];
        payload_loads.extend(payload.instructions.iter().filter_map(|instruction| {
            let SIRInstruction::Load(dst, ..) = instruction else {
                return None;
            };
            Some(*dst)
        }));
        decision = false_block.0;
    }
    assert_eq!(
        payload_loads,
        vec![RegisterId(9), RegisterId(10), RegisterId(11)]
    );
    assert_eq!(decision, BlockId(2));
}

#[test]
fn selector_dispatch_rejects_overlapping_selector_values() {
    let mut eu = selector_predicate_unit(true, false);
    let mut next_block_id = 3;
    let mut reg_counter = 24;

    assert_eq!(
        branchify_selector_guarded_predicates(&mut eu, &mut next_block_id, &mut reg_counter,),
        0
    );
    assert_eq!(eu.blocks.len(), 3);
    assert_eq!(
        eu.blocks[&BlockId(0)]
            .instructions
            .iter()
            .filter(|instruction| matches!(instruction, SIRInstruction::Load(..)))
            .count(),
        3
    );
}

#[test]
fn selector_dispatch_normalizes_each_logic_condition() {
    let mut eu = selector_predicate_unit(false, false);
    for register in [0, 6, 7, 8, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24] {
        eu.register_map
            .insert(RegisterId(register), RegisterType::Logic { width: 1 });
    }
    let mut next_block_id = 3;
    let mut reg_counter = 24;

    assert_eq!(
        branchify_selector_guarded_predicates(&mut eu, &mut next_block_id, &mut reg_counter,),
        1
    );
    for block in eu.blocks.values() {
        let SIRTerminator::Branch { cond, .. } = block.terminator else {
            continue;
        };
        assert_eq!(
            eu.register_map[&cond],
            RegisterType::Bit {
                width: 1,
                signed: false,
            }
        );
    }
}

#[test]
fn selector_dispatch_does_not_delay_loads_across_a_store() {
    let mut eu = selector_predicate_unit(false, true);
    let mut next_block_id = 3;
    let mut reg_counter = 24;

    assert_eq!(
        branchify_selector_guarded_predicates(&mut eu, &mut next_block_id, &mut reg_counter,),
        0
    );
    assert_eq!(eu.blocks.len(), 3);
}
