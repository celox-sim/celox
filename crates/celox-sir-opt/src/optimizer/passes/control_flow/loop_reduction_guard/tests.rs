use super::*;
use crate::ir::{InstanceId, STABLE_REGION};
use celox_design::StateObjectId;

fn addr() -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr {
        region: STABLE_REGION,
        instance_id: InstanceId(0),
        var_id: StateObjectId(0),
    }
}

fn unit() -> ExecutionUnit<RegionedAbsoluteAddr> {
    let r = RegisterId;
    let mut types = HashMap::default();
    for id in 0..38 {
        types.insert(
            r(id),
            RegisterType::Bit {
                width: if matches!(id, 5 | 6 | 13..=31 | 35) {
                    1
                } else {
                    8
                },
                signed: false,
            },
        );
    }
    let entry = BasicBlock {
        id: BlockId(0),
        params: vec![],
        instructions: vec![
            SIRInstruction::Imm(r(0), SIRValue::new(0u8)),
            SIRInstruction::Imm(r(1), SIRValue::new(1u8)),
            SIRInstruction::Imm(r(2), SIRValue::new(32u8)),
            SIRInstruction::Load(r(5), addr(), SIROffset::Static(0), 1),
            SIRInstruction::Load(r(6), addr(), SIROffset::Static(1), 1),
            SIRInstruction::Load(r(7), addr(), SIROffset::Static(8), 8),
            SIRInstruction::Load(r(8), addr(), SIROffset::Static(16), 8),
        ],
        terminator: SIRTerminator::Jump(BlockId(1), vec![r(2), r(7), r(8)]),
    };
    let mut instructions = vec![SIRInstruction::Load(
        r(13),
        addr(),
        SIROffset::Dynamic(r(10)),
        1,
    )];
    for id in 14..30 {
        instructions.push(SIRInstruction::Unary(r(id), UnaryOp::Ident, r(id - 1)));
    }
    instructions.extend([
        SIRInstruction::Binary(r(30), r(5), BinaryOp::LogicAnd, r(29)),
        SIRInstruction::Binary(r(31), r(6), BinaryOp::And, r(29)),
        SIRInstruction::Mux(r(32), r(30), r(10), r(11)),
        SIRInstruction::Mux(r(33), r(31), r(2), r(12)),
        SIRInstruction::Binary(r(34), r(10), BinaryOp::Sub, r(1)),
        SIRInstruction::Binary(r(35), r(34), BinaryOp::Ne, r(0)),
    ]);
    let scan = BasicBlock {
        id: BlockId(1),
        params: vec![r(10), r(11), r(12)],
        instructions,
        terminator: SIRTerminator::Branch {
            cond: r(35),
            true_block: (BlockId(1), vec![r(34), r(32), r(33)]),
            false_block: (BlockId(2), vec![r(32), r(33)]),
        },
    };
    let exit = BasicBlock {
        id: BlockId(2),
        params: vec![r(36), r(37)],
        instructions: vec![
            SIRInstruction::Store(addr(), SIROffset::Static(32), 8, r(36), vec![], vec![]),
            SIRInstruction::Store(addr(), SIROffset::Static(40), 8, r(37), vec![], vec![]),
        ],
        terminator: SIRTerminator::Return,
    };
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [entry, scan, exit].into_iter().map(|b| (b.id, b)).collect(),
        register_map: types,
    }
}

#[test]
fn guards_all_results_and_preserves_nonzero_initial_values() {
    let mut eu = unit();
    eu.verify();
    assert_eq!(run(&mut eu), 1);
    eu.verify();
    let preheader = &eu.blocks[&BlockId(0)];
    assert!(matches!(
        preheader.instructions.last(),
        Some(SIRInstruction::Binary(
            _,
            RegisterId(5),
            BinaryOp::LogicOr,
            RegisterId(6)
        ))
    ));
    let SIRTerminator::Branch {
        true_block,
        false_block,
        ..
    } = &preheader.terminator
    else {
        panic!("missing loop guard")
    };
    assert_eq!(
        true_block,
        &(
            BlockId(1),
            vec![RegisterId(2), RegisterId(7), RegisterId(8)]
        )
    );
    assert_eq!(
        false_block,
        &(BlockId(2), vec![RegisterId(7), RegisterId(8)])
    );
    assert_eq!(
        run(&mut eu),
        0,
        "do not guard an already guarded incoming edge"
    );
}

#[test]
fn rejects_effects_uncovered_conditions_and_external_definitions() {
    let mut variants = vec![unit(), unit(), unit(), unit()];
    variants[0]
        .blocks
        .get_mut(&BlockId(1))
        .unwrap()
        .instructions
        .push(SIRInstruction::RuntimeEvent {
            site_id: 0,
            args: vec![],
        });
    variants[1]
        .blocks
        .get_mut(&BlockId(1))
        .unwrap()
        .instructions[17] = SIRInstruction::Binary(
        RegisterId(30),
        RegisterId(5),
        BinaryOp::LogicOr,
        RegisterId(29),
    );
    variants[2]
        .blocks
        .get_mut(&BlockId(2))
        .unwrap()
        .instructions
        .push(SIRInstruction::Store(
            addr(),
            SIROffset::Static(48),
            8,
            RegisterId(34),
            vec![],
            vec![],
        ));
    if let SIRTerminator::Branch { true_block, .. } =
        &mut variants[3].blocks.get_mut(&BlockId(1)).unwrap().terminator
    {
        true_block.1[1] = RegisterId(10); // The next iteration overwrites the accumulator without its guard.
    }
    for mut eu in variants {
        eu.verify();
        let before = eu.clone();
        assert_eq!(run(&mut eu), 0);
        assert_eq!(eu, before);
    }
}

fn packed_unit() -> ExecutionUnit<RegionedAbsoluteAddr> {
    let mut eu = unit();
    for id in 38..42 {
        eu.register_map.insert(
            RegisterId(id),
            RegisterType::Bit {
                width: 8,
                signed: false,
            },
        );
    }
    if let SIRTerminator::Jump(_, args) = &mut eu.blocks.get_mut(&BlockId(0)).unwrap().terminator {
        args[1] = RegisterId(0);
    }
    let instructions = &mut eu.blocks.get_mut(&BlockId(1)).unwrap().instructions;
    let index = instructions
        .iter()
        .position(|inst| def_reg(inst) == Some(RegisterId(32)))
        .unwrap();
    instructions.splice(
        index..=index,
        [
            SIRInstruction::Binary(
                RegisterId(38),
                RegisterId(11),
                BinaryOp::And,
                RegisterId(10),
            ),
            SIRInstruction::Unary(RegisterId(39), UnaryOp::Ident, RegisterId(30)),
            SIRInstruction::Binary(
                RegisterId(40),
                RegisterId(39),
                BinaryOp::Shl,
                RegisterId(10),
            ),
            SIRInstruction::Binary(
                RegisterId(41),
                RegisterId(40),
                BinaryOp::And,
                RegisterId(10),
            ),
            SIRInstruction::Binary(RegisterId(32), RegisterId(38), BinaryOp::Or, RegisterId(41)),
        ],
    );
    eu
}

#[test]
fn guards_zero_seeded_packed_recurrence() {
    let mut eu = packed_unit();
    eu.verify();
    assert_eq!(run(&mut eu), 1);
    eu.verify();
    let SIRTerminator::Branch { false_block, .. } = &eu.blocks[&BlockId(0)].terminator else {
        panic!("missing guard")
    };
    assert_eq!(false_block.1, vec![RegisterId(0), RegisterId(8)]);
}

#[test]
fn normalizes_logic_guard_for_two_state_branch() {
    let mut eu = packed_unit();
    eu.register_map
        .insert(RegisterId(5), RegisterType::Logic { width: 1 });
    if let SIRTerminator::Branch { false_block, .. } =
        &mut eu.blocks.get_mut(&BlockId(1)).unwrap().terminator
    {
        false_block.1.truncate(1);
    }
    let exit = eu.blocks.get_mut(&BlockId(2)).unwrap();
    exit.params.truncate(1);
    exit.instructions.truncate(1);
    eu.verify();
    assert_eq!(run(&mut eu), 1);
    eu.verify();
    let entry = &eu.blocks[&BlockId(0)];
    assert!(matches!(
        entry.instructions.last(),
        Some(SIRInstruction::Unary(_, UnaryOp::ToTwoState, RegisterId(5)))
    ));
}

#[test]
fn rejects_nonzero_seed_and_unguarded_packed_payload() {
    let mut variants = [packed_unit(), packed_unit()];
    if let SIRTerminator::Jump(_, args) =
        &mut variants[0].blocks.get_mut(&BlockId(0)).unwrap().terminator
    {
        args[1] = RegisterId(7);
    }
    let instructions = &mut variants[1]
        .blocks
        .get_mut(&BlockId(1))
        .unwrap()
        .instructions;
    let index = instructions
        .iter()
        .position(|inst| def_reg(inst) == Some(RegisterId(30)))
        .unwrap();
    instructions[index] = SIRInstruction::Binary(
        RegisterId(30),
        RegisterId(5),
        BinaryOp::LogicOr,
        RegisterId(29),
    );
    for mut eu in variants {
        eu.verify();
        let before = eu.clone();
        assert_eq!(run(&mut eu), 0);
        assert_eq!(eu, before);
    }
}

#[test]
fn requires_a_finite_unit_stride_counter() {
    for step in [0u8, 2] {
        let mut eu = unit();
        eu.blocks.get_mut(&BlockId(0)).unwrap().instructions[1] =
            SIRInstruction::Imm(RegisterId(1), SIRValue::new(step));
        eu.verify();
        assert_eq!(run(&mut eu), 0);
    }
}

#[test]
fn four_state_pipeline_keeps_the_scan() {
    use super::super::pass_guarded_region_sinking::recover_merged_effect_regions;
    let mut eu = unit();
    let before = eu.clone();
    recover_merged_effect_regions(&mut eu, true);
    assert_eq!(eu, before);
}

#[test]
fn resizing_identity_is_not_a_preserved_value() {
    let param = RegisterId(0);
    let result = RegisterId(1);
    let inst = SIRInstruction::Unary(result, UnaryOp::Ident, param);
    let defs = [(result, &inst)].into_iter().collect();
    let local = [param, result].into_iter().collect();
    let types = [
        (
            param,
            RegisterType::Bit {
                width: 8,
                signed: false,
            },
        ),
        (
            result,
            RegisterType::Bit {
                width: 4,
                signed: false,
            },
        ),
    ]
    .into_iter()
    .collect();
    assert_eq!(preserved(result, &[param], &defs, &local, &types, 0), None);
}

#[test]
fn shared_predicate_dag_without_invariants_finishes() {
    let instructions = (1..4096)
        .map(|id| {
            SIRInstruction::Binary(
                RegisterId(id),
                RegisterId(id - 1),
                BinaryOp::LogicAnd,
                RegisterId(id - 1),
            )
        })
        .collect::<Vec<_>>();
    let defs = instructions
        .iter()
        .map(|inst| (def_reg(inst).unwrap(), inst))
        .collect();
    let local = (0..4096).map(RegisterId).collect();
    let types = (0..4096)
        .map(|id| {
            (
                RegisterId(id),
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            )
        })
        .collect();
    assert_eq!(false_guard(RegisterId(4095), &defs, &local, &types), None);
}
