#[test]
fn canonical_identity_is_implicit_and_rollback_restores_aliases() {
    use super::{GvnState, resolve_canonical};
    use crate::ir::RegisterId;
    let mut state = GvnState::default();
    for id in 0..1024 {
        state.set_canonical(RegisterId(id), RegisterId(id));
    }
    assert!(state.canonical.is_empty());
    assert!(state.canonical_changes.is_empty());
    let first = state.checkpoint();
    state.set_canonical(RegisterId(5), RegisterId(2));
    let alias = state.checkpoint();
    state.set_canonical(RegisterId(5), RegisterId(5));
    assert_eq!(
        resolve_canonical(RegisterId(5), &state.canonical),
        RegisterId(5)
    );
    state.rollback(alias);
    assert_eq!(
        resolve_canonical(RegisterId(5), &state.canonical),
        RegisterId(2)
    );
    state.rollback(first);
    assert!(state.canonical.is_empty());
    assert!(state.canonical_changes.is_empty());
}
use super::*;
use crate::ir::InstanceId;
use celox_design::StateObjectId as VarId;
use num_bigint::BigUint;

fn bit(width: usize) -> RegisterType {
    RegisterType::Bit {
        width,
        signed: false,
    }
}

fn address(instance: usize) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr {
        region: 0,
        instance_id: InstanceId(instance),
        var_id: VarId::default(),
    }
}

#[allow(clippy::too_many_arguments)]
fn loop_unit(
    entry_instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    entry_args: Vec<RegisterId>,
    body_params: Vec<RegisterId>,
    body_instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    loop_cond: RegisterId,
    backedge_args: Vec<RegisterId>,
    register_types: Vec<(RegisterId, RegisterType)>,
) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let entry_id = BlockId(0);
    let body_id = BlockId(1);
    let exit_id = BlockId(2);
    let unit = ExecutionUnit {
        entry_block_id: entry_id,
        blocks: [
            BasicBlock {
                id: entry_id,
                params: Vec::new(),
                instructions: entry_instructions,
                terminator: SIRTerminator::Jump(body_id, entry_args),
            },
            BasicBlock {
                id: body_id,
                params: body_params,
                instructions: body_instructions,
                terminator: SIRTerminator::Branch {
                    cond: loop_cond,
                    true_block: (body_id, backedge_args),
                    false_block: (exit_id, Vec::new()),
                },
            },
            BasicBlock {
                id: exit_id,
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Return,
            },
        ]
        .into_iter()
        .map(|block| (block.id, block))
        .collect(),
        register_map: register_types.into_iter().collect(),
    };
    unit.verify_result().unwrap();
    unit
}

#[test]
fn removes_redundant_definition_after_rewriting_uses() {
    let r0 = RegisterId(0);
    let r1 = RegisterId(1);
    let r2 = RegisterId(2);
    let block_id = BlockId(0);
    let mut unit = ExecutionUnit {
        entry_block_id: block_id,
        blocks: std::iter::once((
            block_id,
            BasicBlock {
                id: block_id,
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Imm(r0, SIRValue::new(1u8)),
                    SIRInstruction::Imm(r1, SIRValue::new(1u8)),
                    SIRInstruction::Binary(r2, r1, BinaryOp::Add, r1),
                ],
                terminator: SIRTerminator::Return,
            },
        ))
        .collect(),
        register_map: [
            (
                r0,
                RegisterType::Bit {
                    width: 8,
                    signed: false,
                },
            ),
            (
                r1,
                RegisterType::Bit {
                    width: 8,
                    signed: false,
                },
            ),
            (
                r2,
                RegisterType::Bit {
                    width: 8,
                    signed: false,
                },
            ),
        ]
        .into_iter()
        .collect(),
    };

    GvnPass.run(&mut unit, &PassOptions::default());

    assert_eq!(
        unit.blocks[&block_id].instructions,
        vec![
            SIRInstruction::Imm(r0, SIRValue::new(1u8)),
            SIRInstruction::Binary(r2, r0, BinaryOp::Add, r0),
        ]
    );
    assert!(!unit.register_map.contains_key(&r1));
    unit.verify_result().unwrap();
}

#[test]
fn reuses_cross_type_concat_bits_only_in_two_state_mode() {
    let r0 = RegisterId(0);
    let r1 = RegisterId(1);
    let packed_bit = RegisterId(2);
    let packed_logic = RegisterId(3);
    let block_id = BlockId(0);
    let fixture = ExecutionUnit {
        entry_block_id: block_id,
        blocks: std::iter::once((
            block_id,
            BasicBlock {
                id: block_id,
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Imm(r0, SIRValue::new(0u8)),
                    SIRInstruction::Imm(r1, SIRValue::new(1u8)),
                    SIRInstruction::Concat(packed_bit, vec![r1, r0]),
                    SIRInstruction::Store(
                        address(0),
                        SIROffset::Static(0),
                        2,
                        packed_bit,
                        Vec::new(),
                        Vec::new(),
                    ),
                    SIRInstruction::Concat(packed_logic, vec![r1, r0]),
                    SIRInstruction::Store(
                        address(1),
                        SIROffset::Static(0),
                        2,
                        packed_logic,
                        Vec::new(),
                        Vec::new(),
                    ),
                ],
                terminator: SIRTerminator::Return,
            },
        ))
        .collect(),
        register_map: [
            (r0, bit(1)),
            (r1, bit(1)),
            (packed_bit, bit(2)),
            (packed_logic, RegisterType::Logic { width: 2 }),
        ]
        .into_iter()
        .collect(),
    };

    let mut two_state = fixture.clone();
    GvnPass.run(&mut two_state, &PassOptions::default());
    assert!(matches!(
        two_state.blocks[&block_id].instructions[4],
        SIRInstruction::Unary(dst, UnaryOp::Ident, source)
            if dst == packed_logic && source == packed_bit
    ));
    two_state.verify_result().unwrap();

    let mut four_state = fixture;
    GvnPass.run(
        &mut four_state,
        &PassOptions {
            four_state: true,
            ..PassOptions::default()
        },
    );
    assert!(matches!(
        four_state.blocks[&block_id].instructions[4],
        SIRInstruction::Concat(dst, _)
            if dst == packed_logic
    ));
    four_state.verify_result().unwrap();
}

#[test]
fn loop_block_cses_param_expressions_and_reuses_dominating_values() {
    let registers = (0..12)
        .map(|index| {
            (
                RegisterId(index),
                if index == 0 || index == 4 {
                    bit(1)
                } else {
                    bit(8)
                },
            )
        })
        .collect();
    let mut unit = loop_unit(
        vec![
            SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
            SIRInstruction::Imm(RegisterId(1), SIRValue::new(2u8)),
            SIRInstruction::Imm(RegisterId(2), SIRValue::new(3u8)),
            SIRInstruction::Binary(RegisterId(3), RegisterId(1), BinaryOp::Add, RegisterId(2)),
        ],
        vec![RegisterId(0), RegisterId(1), RegisterId(2)],
        vec![RegisterId(4), RegisterId(5), RegisterId(6)],
        vec![
            SIRInstruction::Binary(RegisterId(7), RegisterId(5), BinaryOp::Add, RegisterId(6)),
            SIRInstruction::Binary(RegisterId(8), RegisterId(5), BinaryOp::Add, RegisterId(6)),
            SIRInstruction::Binary(RegisterId(9), RegisterId(8), BinaryOp::Xor, RegisterId(5)),
            // These operands are loop-invariant SSA values defined in the
            // preheader, so the dominating r3 definition is available in
            // every iteration. The block parameters above remain distinct.
            SIRInstruction::Binary(RegisterId(10), RegisterId(1), BinaryOp::Add, RegisterId(2)),
            SIRInstruction::Binary(RegisterId(11), RegisterId(1), BinaryOp::Add, RegisterId(2)),
        ],
        RegisterId(4),
        vec![RegisterId(4), RegisterId(5), RegisterId(6)],
        registers,
    );

    GvnPass.run(&mut unit, &PassOptions::default());

    let body = &unit.blocks[&BlockId(1)];
    assert!(
        body.instructions
            .iter()
            .any(|inst| { matches!(inst, SIRInstruction::Binary(RegisterId(7), ..)) })
    );
    assert!(body.instructions.iter().any(|inst| {
        matches!(
            inst,
            SIRInstruction::Binary(RegisterId(9), RegisterId(7), BinaryOp::Xor, RegisterId(5))
        )
    }));
    assert!(!unit.register_map.contains_key(&RegisterId(8)));
    assert!(!unit.register_map.contains_key(&RegisterId(10)));
    assert!(!unit.register_map.contains_key(&RegisterId(11)));
    assert!(unit.register_map.contains_key(&RegisterId(3)));
    unit.verify_result().unwrap();
}

#[test]
fn loop_block_cses_dynamic_loads_after_offset_aliasing() {
    let mut unit = loop_unit(
        vec![
            SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
            SIRInstruction::Imm(RegisterId(1), SIRValue::new(4u8)),
        ],
        vec![RegisterId(0), RegisterId(1)],
        vec![RegisterId(2), RegisterId(3)],
        vec![
            SIRInstruction::Unary(RegisterId(4), UnaryOp::Ident, RegisterId(3)),
            SIRInstruction::Unary(RegisterId(5), UnaryOp::Ident, RegisterId(3)),
            SIRInstruction::Load(
                RegisterId(6),
                address(0),
                SIROffset::Dynamic(RegisterId(4)),
                8,
            ),
            SIRInstruction::Load(
                RegisterId(7),
                address(0),
                SIROffset::Dynamic(RegisterId(5)),
                8,
            ),
        ],
        RegisterId(2),
        vec![RegisterId(2), RegisterId(3)],
        vec![
            (RegisterId(0), bit(1)),
            (RegisterId(1), bit(64)),
            (RegisterId(2), bit(1)),
            (RegisterId(3), bit(64)),
            (RegisterId(4), bit(64)),
            (RegisterId(5), bit(64)),
            (RegisterId(6), RegisterType::Logic { width: 8 }),
            (RegisterId(7), RegisterType::Logic { width: 8 }),
        ],
    );

    GvnPass.run(&mut unit, &PassOptions::default());

    let body = &unit.blocks[&BlockId(1)];
    assert_eq!(
        body.instructions
            .iter()
            .filter(|inst| matches!(inst, SIRInstruction::Load(..)))
            .count(),
        1
    );
    assert!(body.instructions.iter().any(|inst| {
        matches!(
            inst,
            SIRInstruction::Load(RegisterId(6), _, SIROffset::Dynamic(RegisterId(4)), 8)
        )
    }));
    assert!(!unit.register_map.contains_key(&RegisterId(5)));
    assert!(!unit.register_map.contains_key(&RegisterId(7)));
    unit.verify_result().unwrap();
}

#[test]
fn loop_store_is_a_load_cse_barrier_and_rewrites_dynamic_offset() {
    let addr = address(0);
    let mut unit = loop_unit(
        vec![
            SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
            SIRInstruction::Imm(RegisterId(1), SIRValue::new(4u8)),
            SIRInstruction::Imm(RegisterId(2), SIRValue::new(9u8)),
        ],
        vec![RegisterId(0), RegisterId(1), RegisterId(2)],
        vec![RegisterId(3), RegisterId(4), RegisterId(5)],
        vec![
            SIRInstruction::Unary(RegisterId(6), UnaryOp::Ident, RegisterId(4)),
            SIRInstruction::Unary(RegisterId(7), UnaryOp::Ident, RegisterId(4)),
            SIRInstruction::Load(RegisterId(8), addr, SIROffset::Dynamic(RegisterId(6)), 8),
            SIRInstruction::Store(
                addr,
                SIROffset::Dynamic(RegisterId(7)),
                8,
                RegisterId(5),
                Vec::new(),
                Vec::new(),
            ),
            SIRInstruction::Load(RegisterId(9), addr, SIROffset::Dynamic(RegisterId(7)), 8),
        ],
        RegisterId(3),
        vec![RegisterId(3), RegisterId(4), RegisterId(5)],
        vec![
            (RegisterId(0), bit(1)),
            (RegisterId(1), bit(64)),
            (RegisterId(2), bit(8)),
            (RegisterId(3), bit(1)),
            (RegisterId(4), bit(64)),
            (RegisterId(5), bit(8)),
            (RegisterId(6), bit(64)),
            (RegisterId(7), bit(64)),
            (RegisterId(8), RegisterType::Logic { width: 8 }),
            (RegisterId(9), RegisterType::Logic { width: 8 }),
        ],
    );

    GvnPass.run(&mut unit, &PassOptions::default());

    let body = &unit.blocks[&BlockId(1)];
    assert_eq!(
        body.instructions
            .iter()
            .filter(|inst| matches!(inst, SIRInstruction::Load(..)))
            .count(),
        2
    );
    assert!(body.instructions.iter().all(|inst| match inst {
        SIRInstruction::Load(_, _, SIROffset::Dynamic(offset), _)
        | SIRInstruction::Store(_, SIROffset::Dynamic(offset), ..) => {
            *offset == RegisterId(6)
        }
        _ => true,
    }));
    assert!(!unit.register_map.contains_key(&RegisterId(7)));
    unit.verify_result().unwrap();
}

#[test]
fn loop_load_key_separates_address_offset_width_and_result_type() {
    let mut unit = loop_unit(
        vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8))],
        vec![RegisterId(0)],
        vec![RegisterId(1)],
        vec![
            SIRInstruction::Load(RegisterId(2), address(0), SIROffset::Static(0), 8),
            SIRInstruction::Load(RegisterId(3), address(1), SIROffset::Static(0), 8),
            SIRInstruction::Load(RegisterId(4), address(0), SIROffset::Static(0), 16),
            SIRInstruction::Load(RegisterId(5), address(0), SIROffset::Static(1), 8),
            SIRInstruction::Load(RegisterId(6), address(0), SIROffset::Static(0), 8),
        ],
        RegisterId(1),
        vec![RegisterId(1)],
        vec![
            (RegisterId(0), bit(1)),
            (RegisterId(1), bit(1)),
            (RegisterId(2), RegisterType::Logic { width: 8 }),
            (RegisterId(3), RegisterType::Logic { width: 8 }),
            (RegisterId(4), RegisterType::Logic { width: 16 }),
            (RegisterId(5), RegisterType::Logic { width: 8 }),
            (RegisterId(6), bit(8)),
        ],
    );

    GvnPass.run(&mut unit, &PassOptions::default());

    assert_eq!(
        unit.blocks[&BlockId(1)]
            .instructions
            .iter()
            .filter(|inst| matches!(inst, SIRInstruction::Load(..)))
            .count(),
        5
    );
    for register in 2..=6 {
        assert!(unit.register_map.contains_key(&RegisterId(register)));
    }
    unit.verify_result().unwrap();
}

#[test]
fn loop_pure_key_separates_logic_bit_and_signed_bit_results() {
    let mut unit = loop_unit(
        vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8))],
        vec![RegisterId(0)],
        vec![RegisterId(1)],
        vec![
            SIRInstruction::Imm(RegisterId(2), SIRValue::new(5u8)),
            SIRInstruction::Imm(RegisterId(3), SIRValue::new(5u8)),
            SIRInstruction::Imm(RegisterId(4), SIRValue::new(5u8)),
            SIRInstruction::Imm(RegisterId(5), SIRValue::new(5u8)),
        ],
        RegisterId(1),
        vec![RegisterId(1)],
        vec![
            (RegisterId(0), bit(1)),
            (RegisterId(1), bit(1)),
            (RegisterId(2), RegisterType::Logic { width: 8 }),
            (RegisterId(3), bit(8)),
            (
                RegisterId(4),
                RegisterType::Bit {
                    width: 8,
                    signed: true,
                },
            ),
            (RegisterId(5), RegisterType::Logic { width: 8 }),
        ],
    );

    GvnPass.run(&mut unit, &PassOptions::default());

    for register in 2..=4 {
        assert!(unit.register_map.contains_key(&RegisterId(register)));
    }
    assert!(!unit.register_map.contains_key(&RegisterId(5)));
    assert_eq!(unit.blocks[&BlockId(1)].instructions.len(), 3);
    unit.verify_result().unwrap();
}

#[test]
fn loop_alias_rewrites_a_dominated_cross_block_use() {
    let entry_id = BlockId(0);
    let body_id = BlockId(1);
    let inner_id = BlockId(2);
    let exit_id = BlockId(3);
    let mut unit = ExecutionUnit {
        entry_block_id: entry_id,
        blocks: [
            BasicBlock {
                id: entry_id,
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                    SIRInstruction::Imm(RegisterId(1), SIRValue::new(7u8)),
                ],
                terminator: SIRTerminator::Jump(body_id, vec![RegisterId(0), RegisterId(1)]),
            },
            BasicBlock {
                id: body_id,
                params: vec![RegisterId(2), RegisterId(3)],
                instructions: vec![
                    SIRInstruction::Unary(RegisterId(4), UnaryOp::Ident, RegisterId(3)),
                    SIRInstruction::Unary(RegisterId(5), UnaryOp::Ident, RegisterId(3)),
                ],
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(2),
                    true_block: (inner_id, Vec::new()),
                    false_block: (exit_id, Vec::new()),
                },
            },
            BasicBlock {
                id: inner_id,
                params: Vec::new(),
                instructions: vec![SIRInstruction::Binary(
                    RegisterId(6),
                    RegisterId(5),
                    BinaryOp::Add,
                    RegisterId(3),
                )],
                terminator: SIRTerminator::Jump(body_id, vec![RegisterId(2), RegisterId(3)]),
            },
            BasicBlock {
                id: exit_id,
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Return,
            },
        ]
        .into_iter()
        .map(|block| (block.id, block))
        .collect(),
        register_map: vec![
            (RegisterId(0), bit(1)),
            (RegisterId(1), bit(8)),
            (RegisterId(2), bit(1)),
            (RegisterId(3), bit(8)),
            (RegisterId(4), bit(8)),
            (RegisterId(5), bit(8)),
            (RegisterId(6), bit(8)),
        ]
        .into_iter()
        .collect(),
    };
    unit.verify_result().unwrap();

    GvnPass.run(&mut unit, &PassOptions::default());

    assert!(!unit.register_map.contains_key(&RegisterId(5)));
    assert_eq!(unit.blocks[&body_id].instructions.len(), 1);
    assert!(matches!(
        unit.blocks[&inner_id].instructions[0],
        SIRInstruction::Binary(_, RegisterId(4), BinaryOp::Add, RegisterId(3))
    ));
    unit.verify_result().unwrap();
}

#[test]
fn immediate_key_keeps_payload_and_mask_structurally_separate() {
    // With the old `payload + [u64::MAX] + mask` encoding these two
    // distinct values both became [1, MAX, 2, MAX, 3].
    let limb = BigUint::from(1u8) << 64usize;
    let limb_2 = &limb << 64usize;
    let max = BigUint::from(u64::MAX);
    let value_a = SIRValue::new_four_state(
        BigUint::from(1u8),
        BigUint::from(2u8) + &max * &limb + BigUint::from(3u8) * &limb_2,
    );
    let value_b = SIRValue::new_four_state(
        BigUint::from(1u8) + &max * &limb + BigUint::from(2u8) * &limb_2,
        BigUint::from(3u8),
    );
    let block_id = BlockId(0);
    let mut unit = ExecutionUnit {
        entry_block_id: block_id,
        blocks: std::iter::once((
            block_id,
            BasicBlock {
                id: block_id,
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Imm(RegisterId(0), value_a),
                    SIRInstruction::Imm(RegisterId(1), value_b),
                    SIRInstruction::Binary(
                        RegisterId(2),
                        RegisterId(0),
                        BinaryOp::Xor,
                        RegisterId(1),
                    ),
                ],
                terminator: SIRTerminator::Return,
            },
        ))
        .collect(),
        register_map: (0..3)
            .map(|register| (RegisterId(register), RegisterType::Logic { width: 192 }))
            .collect(),
    };
    unit.verify_result().unwrap();

    GvnPass.run(&mut unit, &PassOptions::default());

    assert_eq!(unit.blocks[&block_id].instructions.len(), 3);
    assert!(unit.register_map.contains_key(&RegisterId(0)));
    assert!(unit.register_map.contains_key(&RegisterId(1)));
    unit.verify_result().unwrap();
}

#[test]
fn cfg_marks_exact_scc_members_instead_of_an_rpo_interval() {
    let entry = BlockId(0);
    let header = BlockId(1);
    let body = BlockId(2);
    let exit = BlockId(3);
    let unit = ExecutionUnit {
        entry_block_id: entry,
        blocks: [
            BasicBlock {
                id: entry,
                params: Vec::new(),
                instructions: vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8))],
                terminator: SIRTerminator::Jump(header, vec![RegisterId(0)]),
            },
            BasicBlock {
                id: header,
                params: vec![RegisterId(1)],
                instructions: Vec::new(),
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(1),
                    true_block: (body, Vec::new()),
                    false_block: (exit, Vec::new()),
                },
            },
            BasicBlock {
                id: body,
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Jump(header, vec![RegisterId(1)]),
            },
            BasicBlock {
                id: exit,
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Return,
            },
        ]
        .into_iter()
        .map(|block| (block.id, block))
        .collect(),
        register_map: [(RegisterId(0), bit(1)), (RegisterId(1), bit(1))]
            .into_iter()
            .collect(),
    };
    unit.verify_result().unwrap();

    let cfg = GvnCfg::new(&unit);
    let is_cyclic = |block| {
        let index = cfg.block_ids.iter().position(|id| *id == block).unwrap();
        cfg.cyclic[index]
    };
    assert!(!is_cyclic(entry));
    assert!(is_cyclic(header));
    assert!(is_cyclic(body));
    assert!(!is_cyclic(exit));
}

#[test]
fn cfg_marks_every_member_of_an_irreducible_multi_entry_scc() {
    let entry = BlockId(0);
    let a = BlockId(1);
    let b = BlockId(2);
    let c = BlockId(3);
    let d = BlockId(4);
    let exit = BlockId(5);
    let unit = ExecutionUnit {
        entry_block_id: entry,
        blocks: [
            BasicBlock {
                id: entry,
                params: Vec::new(),
                instructions: vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8))],
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (a, Vec::new()),
                    false_block: (b, Vec::new()),
                },
            },
            BasicBlock {
                id: a,
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Jump(c, Vec::new()),
            },
            BasicBlock {
                id: b,
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Jump(c, Vec::new()),
            },
            BasicBlock {
                id: c,
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (a, Vec::new()),
                    false_block: (d, Vec::new()),
                },
            },
            BasicBlock {
                id: d,
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (b, Vec::new()),
                    false_block: (exit, Vec::new()),
                },
            },
            BasicBlock {
                id: exit,
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Return,
            },
        ]
        .into_iter()
        .map(|block| (block.id, block))
        .collect(),
        register_map: [(RegisterId(0), bit(1))].into_iter().collect(),
    };
    unit.verify_result().unwrap();

    let cfg = GvnCfg::new(&unit);
    let is_cyclic = |block| {
        let index = cfg.block_ids.iter().position(|id| *id == block).unwrap();
        cfg.cyclic[index]
    };
    assert!(!is_cyclic(entry));
    for block in [a, b, c, d] {
        assert!(
            is_cyclic(block),
            "b{} must be in the irreducible SCC",
            block.0
        );
    }
    assert!(!is_cyclic(exit));
}

#[test]
fn load_availability_crosses_a_single_acyclic_edge() {
    let entry = BlockId(0);
    let next = BlockId(1);
    let addr = address(0);
    let mut unit = ExecutionUnit {
        entry_block_id: entry,
        blocks: [
            BasicBlock {
                id: entry,
                params: Vec::new(),
                instructions: vec![SIRInstruction::Load(
                    RegisterId(0),
                    addr,
                    SIROffset::Static(0),
                    8,
                )],
                terminator: SIRTerminator::Jump(next, Vec::new()),
            },
            BasicBlock {
                id: next,
                params: Vec::new(),
                instructions: vec![SIRInstruction::Load(
                    RegisterId(1),
                    addr,
                    SIROffset::Static(0),
                    8,
                )],
                terminator: SIRTerminator::Return,
            },
        ]
        .into_iter()
        .map(|block| (block.id, block))
        .collect(),
        register_map: [
            (RegisterId(0), RegisterType::Logic { width: 8 }),
            (RegisterId(1), RegisterType::Logic { width: 8 }),
        ]
        .into_iter()
        .collect(),
    };
    unit.verify_result().unwrap();

    GvnPass.run(&mut unit, &PassOptions::default());

    assert!(unit.blocks[&next].instructions.is_empty());
    assert!(!unit.register_map.contains_key(&RegisterId(1)));
    unit.verify_result().unwrap();
}

#[test]
fn same_state_version_load_crosses_a_diamond_with_a_disjoint_store() {
    let entry = BlockId(0);
    let left = BlockId(1);
    let right = BlockId(2);
    let join = BlockId(3);
    let addr = address(0);
    let other_addr = address(1);
    let mut unit = ExecutionUnit {
        entry_block_id: entry,
        blocks: [
            BasicBlock {
                id: entry,
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                    SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(0), 8),
                    SIRInstruction::Imm(RegisterId(3), SIRValue::new(7u8)),
                ],
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (left, Vec::new()),
                    false_block: (right, Vec::new()),
                },
            },
            BasicBlock {
                id: left,
                params: Vec::new(),
                instructions: vec![SIRInstruction::Store(
                    other_addr,
                    SIROffset::Static(0),
                    8,
                    RegisterId(3),
                    Vec::new(),
                    Vec::new(),
                )],
                terminator: SIRTerminator::Jump(join, Vec::new()),
            },
            BasicBlock {
                id: right,
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Jump(join, Vec::new()),
            },
            BasicBlock {
                id: join,
                params: Vec::new(),
                instructions: vec![SIRInstruction::Load(
                    RegisterId(2),
                    addr,
                    SIROffset::Static(0),
                    8,
                )],
                terminator: SIRTerminator::Return,
            },
        ]
        .into_iter()
        .map(|block| (block.id, block))
        .collect(),
        register_map: [
            (RegisterId(0), bit(1)),
            (RegisterId(1), RegisterType::Logic { width: 8 }),
            (RegisterId(2), RegisterType::Logic { width: 8 }),
            (RegisterId(3), RegisterType::Logic { width: 8 }),
        ]
        .into_iter()
        .collect(),
    };
    unit.verify_result().unwrap();

    GvnPass.run(&mut unit, &PassOptions::default());

    assert_eq!(unit.blocks[&entry].instructions.len(), 3);
    assert!(unit.blocks[&join].instructions.is_empty());
    assert!(!unit.register_map.contains_key(&RegisterId(2)));
    unit.verify_result().unwrap();
}

#[test]
fn same_slot_store_on_one_arm_creates_a_join_version() {
    let entry = BlockId(0);
    let left = BlockId(1);
    let right = BlockId(2);
    let join = BlockId(3);
    let addr = address(0);
    let mut unit = ExecutionUnit {
        entry_block_id: entry,
        blocks: [
            BasicBlock {
                id: entry,
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Load(RegisterId(0), address(1), SIROffset::Static(0), 1),
                    SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(0), 8),
                    SIRInstruction::Imm(RegisterId(3), SIRValue::new(7u8)),
                ],
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (left, Vec::new()),
                    false_block: (right, Vec::new()),
                },
            },
            BasicBlock {
                id: left,
                params: Vec::new(),
                instructions: vec![SIRInstruction::Store(
                    addr,
                    SIROffset::Static(0),
                    8,
                    RegisterId(3),
                    Vec::new(),
                    Vec::new(),
                )],
                terminator: SIRTerminator::Jump(join, Vec::new()),
            },
            BasicBlock {
                id: right,
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Jump(join, Vec::new()),
            },
            BasicBlock {
                id: join,
                params: Vec::new(),
                instructions: vec![SIRInstruction::Load(
                    RegisterId(2),
                    addr,
                    SIROffset::Static(0),
                    8,
                )],
                terminator: SIRTerminator::Return,
            },
        ]
        .into_iter()
        .map(|block| (block.id, block))
        .collect(),
        register_map: [
            (RegisterId(0), bit(1)),
            (RegisterId(1), RegisterType::Logic { width: 8 }),
            (RegisterId(2), RegisterType::Logic { width: 8 }),
            (RegisterId(3), RegisterType::Logic { width: 8 }),
        ]
        .into_iter()
        .collect(),
    };
    unit.verify_result().unwrap();

    GvnPass.run(&mut unit, &PassOptions::default());

    assert!(matches!(
        unit.blocks[&join].instructions.as_slice(),
        [SIRInstruction::Load(RegisterId(2), ..)]
    ));
    assert!(unit.register_map.contains_key(&RegisterId(2)));
    unit.verify_result().unwrap();
}

#[test]
fn read_only_loop_reuses_a_dominating_preheader_load() {
    let addr = address(0);
    let mut unit = loop_unit(
        vec![
            SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
            SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(0), 8),
        ],
        vec![RegisterId(0)],
        vec![RegisterId(2)],
        vec![SIRInstruction::Load(
            RegisterId(3),
            addr,
            SIROffset::Static(0),
            8,
        )],
        RegisterId(2),
        vec![RegisterId(2)],
        vec![
            (RegisterId(0), bit(1)),
            (RegisterId(1), RegisterType::Logic { width: 8 }),
            (RegisterId(2), bit(1)),
            (RegisterId(3), RegisterType::Logic { width: 8 }),
        ],
    );

    GvnPass.run(&mut unit, &PassOptions::default());

    assert_eq!(
        unit.blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|inst| matches!(inst, SIRInstruction::Load(..)))
            .count(),
        1
    );
    assert!(!unit.register_map.contains_key(&RegisterId(3)));
    unit.verify_result().unwrap();
}

#[test]
fn loop_carried_same_slot_store_creates_a_header_version() {
    let addr = address(0);
    let mut unit = loop_unit(
        vec![
            SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
            SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(0), 8),
            SIRInstruction::Imm(RegisterId(4), SIRValue::new(7u8)),
        ],
        vec![RegisterId(0)],
        vec![RegisterId(2)],
        vec![
            SIRInstruction::Load(RegisterId(3), addr, SIROffset::Static(0), 8),
            SIRInstruction::Store(
                addr,
                SIROffset::Static(0),
                8,
                RegisterId(4),
                Vec::new(),
                Vec::new(),
            ),
        ],
        RegisterId(2),
        vec![RegisterId(2)],
        vec![
            (RegisterId(0), bit(1)),
            (RegisterId(1), RegisterType::Logic { width: 8 }),
            (RegisterId(2), bit(1)),
            (RegisterId(3), RegisterType::Logic { width: 8 }),
            (RegisterId(4), RegisterType::Logic { width: 8 }),
        ],
    );

    GvnPass.run(&mut unit, &PassOptions::default());

    assert_eq!(
        unit.blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|inst| matches!(inst, SIRInstruction::Load(..)))
            .count(),
        2
    );
    assert!(unit.register_map.contains_key(&RegisterId(3)));
    unit.verify_result().unwrap();
}

#[test]
fn observable_callbacks_split_loads_but_a_disjoint_commit_does_not() {
    let block = BlockId(0);
    let addr = address(0);
    let other_addr = address(1);
    let mut unit = ExecutionUnit {
        entry_block_id: block,
        blocks: std::iter::once((
            block,
            BasicBlock {
                id: block,
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                    SIRInstruction::Imm(RegisterId(1), SIRValue::new(2u8)),
                    SIRInstruction::Load(RegisterId(2), addr, SIROffset::Static(0), 8),
                    SIRInstruction::Commit(addr, other_addr, SIROffset::Static(0), 8, Vec::new()),
                    SIRInstruction::Load(RegisterId(3), addr, SIROffset::Static(0), 8),
                    SIRInstruction::RuntimeEvent {
                        site_id: 1,
                        args: vec![RegisterId(3)],
                    },
                    SIRInstruction::Load(RegisterId(4), addr, SIROffset::Static(0), 8),
                    SIRInstruction::CombCaptureEvent {
                        site_id: 2,
                        args: vec![RegisterId(4)],
                        fatal_error_code: None,
                        consume_enabled: false,
                    },
                    SIRInstruction::Load(RegisterId(5), addr, SIROffset::Static(0), 8),
                    SIRInstruction::CombCaptureEnableIfChanged {
                        old: RegisterId(0),
                        new: RegisterId(1),
                        sites: vec![2],
                    },
                    SIRInstruction::Load(RegisterId(6), addr, SIROffset::Static(0), 8),
                ],
                terminator: SIRTerminator::Return,
            },
        ))
        .collect(),
        register_map: [
            (RegisterId(0), bit(8)),
            (RegisterId(1), bit(8)),
            (RegisterId(2), RegisterType::Logic { width: 8 }),
            (RegisterId(3), RegisterType::Logic { width: 8 }),
            (RegisterId(4), RegisterType::Logic { width: 8 }),
            (RegisterId(5), RegisterType::Logic { width: 8 }),
            (RegisterId(6), RegisterType::Logic { width: 8 }),
        ]
        .into_iter()
        .collect(),
    };
    unit.verify_result().unwrap();

    GvnPass.run(&mut unit, &PassOptions::default());

    assert_eq!(
        unit.blocks[&block]
            .instructions
            .iter()
            .filter(|inst| matches!(inst, SIRInstruction::Load(..)))
            .count(),
        4
    );
    assert!(!unit.register_map.contains_key(&RegisterId(3)));
    for register in [2, 4, 5, 6] {
        assert!(unit.register_map.contains_key(&RegisterId(register)));
    }
    assert!(matches!(
        &unit.blocks[&block].instructions[4],
        SIRInstruction::RuntimeEvent { args, .. } if args == &vec![RegisterId(2)]
    ));
    unit.verify_result().unwrap();
}

#[test]
fn dominator_availability_rolls_back_before_a_sibling() {
    let entry = BlockId(0);
    let left = BlockId(1);
    let right = BlockId(2);
    let mut unit = ExecutionUnit {
        entry_block_id: entry,
        blocks: [
            BasicBlock {
                id: entry,
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                    SIRInstruction::Imm(RegisterId(1), SIRValue::new(7u8)),
                    SIRInstruction::Imm(RegisterId(2), SIRValue::new(9u8)),
                ],
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (left, Vec::new()),
                    false_block: (right, Vec::new()),
                },
            },
            BasicBlock {
                id: left,
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Binary(
                        RegisterId(3),
                        RegisterId(1),
                        BinaryOp::Add,
                        RegisterId(2),
                    ),
                    SIRInstruction::Store(
                        address(0),
                        SIROffset::Static(0),
                        8,
                        RegisterId(3),
                        Vec::new(),
                        Vec::new(),
                    ),
                ],
                terminator: SIRTerminator::Return,
            },
            BasicBlock {
                id: right,
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Binary(
                        RegisterId(4),
                        RegisterId(1),
                        BinaryOp::Add,
                        RegisterId(2),
                    ),
                    SIRInstruction::Store(
                        address(1),
                        SIROffset::Static(0),
                        8,
                        RegisterId(4),
                        Vec::new(),
                        Vec::new(),
                    ),
                ],
                terminator: SIRTerminator::Return,
            },
        ]
        .into_iter()
        .map(|block| (block.id, block))
        .collect(),
        register_map: [
            (RegisterId(0), bit(1)),
            (RegisterId(1), bit(8)),
            (RegisterId(2), bit(8)),
            (RegisterId(3), bit(8)),
            (RegisterId(4), bit(8)),
        ]
        .into_iter()
        .collect(),
    };
    unit.verify_result().unwrap();

    GvnPass.run(&mut unit, &PassOptions::default());

    assert!(matches!(
        unit.blocks[&left].instructions[1],
        SIRInstruction::Store(_, _, _, RegisterId(3), ..)
    ));
    assert!(matches!(
        unit.blocks[&right].instructions[1],
        SIRInstruction::Store(_, _, _, RegisterId(4), ..)
    ));
    assert!(unit.register_map.contains_key(&RegisterId(3)));
    assert!(unit.register_map.contains_key(&RegisterId(4)));
    unit.verify_result().unwrap();
}
