use super::*;
use celox_design::StateObjectId as VarId;

fn var_id(index: usize) -> VarId {
    VarId(index as u32)
}

fn address(index: usize) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr {
        region: STABLE_REGION,
        instance_id: InstanceId(0),
        var_id: var_id(index),
    }
}

fn register_map(registers: &[(usize, usize)]) -> HashMap<RegisterId, RegisterType> {
    registers
        .iter()
        .map(|&(register, width)| {
            (
                RegisterId(register),
                RegisterType::Bit {
                    width,
                    signed: false,
                },
            )
        })
        .collect()
}

fn single_block_unit(
    instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    registers: &[(usize, usize)],
) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let block = BasicBlock {
        id: BlockId(0),
        params: Vec::new(),
        instructions,
        terminator: SIRTerminator::Return,
    };
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [(BlockId(0), block)].into_iter().collect(),
        register_map: register_map(registers),
    };
    assert!(unit.verify_result().is_ok());
    unit
}

fn load(
    register: usize,
    address: RegionedAbsoluteAddr,
    width: usize,
) -> SIRInstruction<RegionedAbsoluteAddr> {
    SIRInstruction::Load(RegisterId(register), address, SIROffset::Static(0), width)
}

fn load_at(
    register: usize,
    address: RegionedAbsoluteAddr,
    offset: usize,
    width: usize,
) -> SIRInstruction<RegionedAbsoluteAddr> {
    SIRInstruction::Load(
        RegisterId(register),
        address,
        SIROffset::Static(offset),
        width,
    )
}

fn store(
    address: RegionedAbsoluteAddr,
    register: usize,
    width: usize,
) -> SIRInstruction<RegionedAbsoluteAddr> {
    SIRInstruction::Store(
        address,
        SIROffset::Static(0),
        width,
        RegisterId(register),
        Vec::new(),
        Vec::new(),
    )
}

fn metadata(entries: &[(RegionedAbsoluteAddr, usize)]) -> HashMap<AbsoluteAddr, AddressMetadata> {
    entries
        .iter()
        .map(|&(address, width)| {
            (
                address.absolute_addr(),
                AddressMetadata {
                    width,
                    is_4state: false,
                },
            )
        })
        .collect()
}

fn metadata_with_states(
    entries: &[(RegionedAbsoluteAddr, usize, bool)],
) -> HashMap<AbsoluteAddr, AddressMetadata> {
    entries
        .iter()
        .map(|&(address, width, is_4state)| {
            (
                address.absolute_addr(),
                AddressMetadata { width, is_4state },
            )
        })
        .collect()
}

fn aliases(
    units: &[ExecutionUnit<RegionedAbsoluteAddr>],
    metadata: &HashMap<AbsoluteAddr, AddressMetadata>,
) -> HashMap<AbsoluteAddr, AbsoluteAddr> {
    analyze_eval_comb_aliases(
        units,
        metadata,
        &HashSet::default(),
        &HashMap::default(),
        false,
    )
}

fn optimize_identity_stores(
    units: &mut [ExecutionUnit<RegionedAbsoluteAddr>],
    metadata: &HashMap<AbsoluteAddr, AddressMetadata>,
    existing_aliases: &HashMap<AbsoluteAddr, AbsoluteAddr>,
    four_state: bool,
) -> HashMap<AbsoluteAddr, AbsoluteAddr> {
    optimize_eval_comb_identity_stores(
        units,
        metadata,
        &HashSet::default(),
        &ExternalAccesses::default(),
        &HashMap::default(),
        existing_aliases,
        four_state,
        None,
    )
}

fn is_commit_from_to(
    instruction: &SIRInstruction<RegionedAbsoluteAddr>,
    source: RegionedAbsoluteAddr,
    destination: RegionedAbsoluteAddr,
    width: usize,
) -> bool {
    matches!(
        instruction,
        SIRInstruction::Commit(
            actual_source,
            actual_destination,
            SIROffset::Static(0),
            actual_width,
            triggers,
        ) if *actual_source == source
            && *actual_destination == destination
            && *actual_width == width
            && triggers.is_empty()
    )
}

/// Shape emitted for Heliodor's 32 x 3-bit store-funct3 array: two wide
/// overlapping source loads, one direct boundary chunk, and 30 shifted
/// chunks packed by a 32-input Concat.
fn heliodor_funct3_identity_unit(
    source: RegionedAbsoluteAddr,
    destination: RegionedAbsoluteAddr,
    second_destination: Option<RegionedAbsoluteAddr>,
) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let mut instructions = vec![
        load_at(0, source, 0, 64),
        load_at(1, source, 63, 3),
        load_at(2, source, 64, 64),
        SIRInstruction::Imm(RegisterId(3), SIRValue::new(7u8)),
    ];
    let mut registers = vec![(0, 64), (1, 3), (2, 64), (3, 3)];
    let mut next_register = 4usize;
    let mut chunks_low_to_high = Vec::new();

    for source_offset in (0..96).step_by(3) {
        if source_offset == 63 {
            chunks_low_to_high.push(RegisterId(1));
            continue;
        }

        let (load_register, load_offset) = if source_offset < 63 {
            (RegisterId(0), 0)
        } else {
            (RegisterId(2), 64)
        };
        let shift = source_offset - load_offset;
        let shifted = if shift == 0 {
            load_register
        } else {
            let shift_register = RegisterId(next_register);
            next_register += 1;
            instructions.push(SIRInstruction::Imm(
                shift_register,
                SIRValue::new(shift as u64),
            ));
            registers.push((shift_register.0, 7));

            let shifted = RegisterId(next_register);
            next_register += 1;
            instructions.push(SIRInstruction::Binary(
                shifted,
                load_register,
                BinaryOp::Shr,
                shift_register,
            ));
            registers.push((shifted.0, 64));
            shifted
        };

        let chunk = RegisterId(next_register);
        next_register += 1;
        instructions.push(SIRInstruction::Binary(
            chunk,
            shifted,
            BinaryOp::And,
            RegisterId(3),
        ));
        registers.push((chunk.0, 3));
        chunks_low_to_high.push(chunk);
    }

    let packed = RegisterId(next_register);
    instructions.push(SIRInstruction::Concat(
        packed,
        chunks_low_to_high.into_iter().rev().collect(),
    ));
    registers.push((packed.0, 96));
    instructions.push(store(destination, packed.0, 96));
    if let Some(second_destination) = second_destination {
        instructions.push(store(second_destination, packed.0, 96));
    }

    single_block_unit(instructions, &registers)
}

#[test]
fn identity_store_in_common_merge_block_is_must_execute() {
    let source = address(0);
    let destination = address(1);
    let entry = BasicBlock {
        id: BlockId(0),
        params: Vec::new(),
        instructions: vec![
            SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
            load(1, source, 8),
        ],
        terminator: SIRTerminator::Branch {
            cond: RegisterId(0),
            true_block: (BlockId(1), Vec::new()),
            false_block: (BlockId(2), Vec::new()),
        },
    };
    let true_block = BasicBlock {
        id: BlockId(1),
        params: Vec::new(),
        instructions: Vec::new(),
        terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
    };
    let false_block = BasicBlock {
        id: BlockId(2),
        params: Vec::new(),
        instructions: Vec::new(),
        terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
    };
    let merge = BasicBlock {
        id: BlockId(3),
        params: Vec::new(),
        instructions: vec![store(destination, 1, 8)],
        terminator: SIRTerminator::Return,
    };
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [entry, true_block, false_block, merge]
            .into_iter()
            .map(|block| (block.id, block))
            .collect(),
        register_map: register_map(&[(0, 1), (1, 8)]),
    };
    assert!(unit.verify_result().is_ok());

    assert_eq!(
        aliases(&[unit], &metadata(&[(source, 8), (destination, 8)]))
            .get(&destination.absolute_addr()),
        Some(&source.absolute_addr())
    );
}

#[test]
fn conditional_identity_store_is_not_an_alias() {
    let source = address(0);
    let destination = address(1);
    let entry = BasicBlock {
        id: BlockId(0),
        params: Vec::new(),
        instructions: vec![
            SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
            load(1, source, 8),
        ],
        terminator: SIRTerminator::Branch {
            cond: RegisterId(0),
            true_block: (BlockId(1), Vec::new()),
            false_block: (BlockId(2), Vec::new()),
        },
    };
    let true_block = BasicBlock {
        id: BlockId(1),
        params: Vec::new(),
        instructions: vec![store(destination, 1, 8)],
        terminator: SIRTerminator::Return,
    };
    let false_block = BasicBlock {
        id: BlockId(2),
        params: Vec::new(),
        instructions: Vec::new(),
        terminator: SIRTerminator::Return,
    };
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [entry, true_block, false_block]
            .into_iter()
            .map(|block| (block.id, block))
            .collect(),
        register_map: register_map(&[(0, 1), (1, 8)]),
    };
    assert!(unit.verify_result().is_ok());

    assert!(aliases(&[unit], &metadata(&[(source, 8), (destination, 8)])).is_empty());
}

#[test]
fn identity_copy_of_a_value_stored_earlier_in_the_unit_is_an_alias() {
    let source = address(0);
    let destination = address(1);
    let unit = single_block_unit(
        vec![
            SIRInstruction::Imm(RegisterId(0), SIRValue::new(5u8)),
            store(source, 0, 8),
            load(1, source, 8),
            store(destination, 1, 8),
        ],
        &[(0, 8), (1, 8)],
    );

    assert_eq!(
        aliases(&[unit], &metadata(&[(source, 8), (destination, 8)])),
        HashMap::from_iter([(destination.absolute_addr(), source.absolute_addr())])
    );
}

#[test]
fn identity_copy_of_a_value_stored_earlier_in_a_preceding_block_is_an_alias() {
    let source = address(0);
    let destination = address(1);
    let entry = BasicBlock {
        id: BlockId(0),
        params: Vec::new(),
        instructions: vec![
            SIRInstruction::Imm(RegisterId(0), SIRValue::new(5u8)),
            store(source, 0, 8),
        ],
        terminator: SIRTerminator::Jump(BlockId(1), Vec::new()),
    };
    let copy = BasicBlock {
        id: BlockId(1),
        params: Vec::new(),
        instructions: vec![load(1, source, 8), store(destination, 1, 8)],
        terminator: SIRTerminator::Return,
    };
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [entry, copy]
            .into_iter()
            .map(|block| (block.id, block))
            .collect(),
        register_map: register_map(&[(0, 8), (1, 8)]),
    };
    assert!(unit.verify_result().is_ok());

    assert_eq!(
        aliases(&[unit], &metadata(&[(source, 8), (destination, 8)])),
        HashMap::from_iter([(destination.absolute_addr(), source.absolute_addr())])
    );
}

#[test]
fn identity_copy_taken_before_the_source_store_is_not_an_alias() {
    let source = address(0);
    let destination = address(1);
    let unit = single_block_unit(
        vec![
            load(1, source, 8),
            store(destination, 1, 8),
            SIRInstruction::Imm(RegisterId(0), SIRValue::new(5u8)),
            store(source, 0, 8),
        ],
        &[(0, 8), (1, 8)],
    );

    assert!(aliases(&[unit], &metadata(&[(source, 8), (destination, 8)])).is_empty());
}

#[test]
fn identity_copy_of_a_value_a_later_unit_stores_is_not_an_alias() {
    let source = address(0);
    let destination = address(1);
    let copy = single_block_unit(
        vec![load(1, source, 8), store(destination, 1, 8)],
        &[(1, 8)],
    );
    let writer = single_block_unit(
        vec![
            SIRInstruction::Imm(RegisterId(0), SIRValue::new(5u8)),
            store(source, 0, 8),
        ],
        &[(0, 8)],
    );

    assert!(aliases(&[copy, writer], &metadata(&[(source, 8), (destination, 8)])).is_empty());
}

#[test]
fn identity_copy_in_a_loop_with_its_source_store_is_not_an_alias() {
    let source = address(0);
    let destination = address(1);
    let entry = BasicBlock {
        id: BlockId(0),
        params: Vec::new(),
        instructions: vec![
            SIRInstruction::Imm(RegisterId(0), SIRValue::new(5u8)),
            store(source, 0, 8),
        ],
        terminator: SIRTerminator::Jump(BlockId(1), Vec::new()),
    };
    let copy = BasicBlock {
        id: BlockId(1),
        params: Vec::new(),
        instructions: vec![
            load(1, source, 8),
            store(destination, 1, 8),
            SIRInstruction::Imm(RegisterId(2), SIRValue::new(1u8)),
        ],
        terminator: SIRTerminator::Branch {
            cond: RegisterId(2),
            true_block: (BlockId(0), Vec::new()),
            false_block: (BlockId(2), Vec::new()),
        },
    };
    let exit = BasicBlock {
        id: BlockId(2),
        params: Vec::new(),
        instructions: Vec::new(),
        terminator: SIRTerminator::Return,
    };
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [entry, copy, exit]
            .into_iter()
            .map(|block| (block.id, block))
            .collect(),
        register_map: register_map(&[(0, 8), (1, 8), (2, 1)]),
    };
    assert!(unit.verify_result().is_ok());

    assert!(aliases(&[unit], &metadata(&[(source, 8), (destination, 8)])).is_empty());
}

#[test]
fn second_unit_load_blocks_identity_alias() {
    let source = address(0);
    let destination = address(1);
    let writer = single_block_unit(
        vec![load(0, source, 8), store(destination, 0, 8)],
        &[(0, 8)],
    );
    let reader = single_block_unit(vec![load(0, destination, 8)], &[(0, 8)]);

    assert!(
        aliases(
            &[writer, reader],
            &metadata(&[(source, 8), (destination, 8)])
        )
        .is_empty()
    );
}

#[test]
fn wide_read_destination_uses_commit_and_post_bypass_dce_removes_the_value_dag() {
    use super::super::pass_loop_idiom::LoopIdiomPass;
    use super::super::pass_manager::ExecutionUnitPass;
    use crate::PassOptions;

    let source = address(0);
    let destination = address(1);
    let mut instructions = Vec::new();
    let mut registers = Vec::new();
    for register in 0..32 {
        instructions.push(load_at(register, source, (31 - register) * 64, 64));
        registers.push((register, 64));
    }
    instructions.push(SIRInstruction::Concat(
        RegisterId(32),
        (0..32).map(RegisterId).collect(),
    ));
    instructions.push(store(destination, 32, 2048));
    registers.push((32, 2048));
    let writer = single_block_unit(instructions, &registers);
    let reader = single_block_unit(vec![load(0, destination, 2048)], &[(0, 2048)]);
    let mut units = vec![writer, reader];

    let selected = optimize_identity_stores(
        &mut units,
        &metadata(&[(source, 2048), (destination, 2048)]),
        &HashMap::default(),
        false,
    );
    assert!(selected.is_empty(), "a read destination cannot be aliased");
    assert!(is_commit_from_to(
        units[0].blocks[&BlockId(0)].instructions.last().unwrap(),
        source,
        destination,
        2048,
    ));

    // This is the existing post-IdentityStoreBypass sweep in the program
    // pipeline. Since Commit reads memory directly, the old Load/Concat
    // register DAG has no remaining use and must disappear.
    LoopIdiomPass.run(&mut units[0], &PassOptions::default());
    let instructions = &units[0].blocks[&BlockId(0)].instructions;
    assert_eq!(instructions.len(), 1);
    assert!(is_commit_from_to(
        &instructions[0],
        source,
        destination,
        2048,
    ));
    units[0].verify_result().unwrap();
}

#[test]
fn direct_small_identity_copy_is_not_replaced_by_more_expensive_commit() {
    let source = address(0);
    let destination = address(1);
    let writer = single_block_unit(
        vec![load(0, source, 8), store(destination, 0, 8)],
        &[(0, 8)],
    );
    let reader = single_block_unit(vec![load(0, destination, 8)], &[(0, 8)]);
    let mut units = vec![writer, reader];

    assert!(
        optimize_identity_stores(
            &mut units,
            &metadata(&[(source, 8), (destination, 8)]),
            &HashMap::default(),
            false,
        )
        .is_empty()
    );
    assert!(matches!(
        units[0].blocks[&BlockId(0)].instructions[1],
        SIRInstruction::Store(..)
    ));
}

#[test]
fn heliodor_96_bit_chunk_assembly_remains_profitable_as_commit() {
    let source = address(0);
    let destination = address(1);
    let writer = heliodor_funct3_identity_unit(source, destination, None);
    let reader = single_block_unit(vec![load(0, destination, 96)], &[(0, 96)]);
    let mut units = vec![writer, reader];

    assert!(
        optimize_identity_stores(
            &mut units,
            &metadata(&[(source, 96), (destination, 96)]),
            &HashMap::default(),
            false,
        )
        .is_empty()
    );
    assert!(is_commit_from_to(
        units[0].blocks[&BlockId(0)].instructions.last().unwrap(),
        source,
        destination,
        96,
    ));
}

#[test]
fn shared_96_bit_identity_dag_is_not_charged_as_removable() {
    let source = address(0);
    let first_destination = address(1);
    let second_destination = address(2);
    let writer = heliodor_funct3_identity_unit(source, first_destination, Some(second_destination));
    let first_reader = single_block_unit(vec![load(0, first_destination, 96)], &[(0, 96)]);
    let second_reader = single_block_unit(vec![load(0, second_destination, 96)], &[(0, 96)]);
    let mut units = vec![writer, first_reader, second_reader];

    assert!(
        optimize_identity_stores(
            &mut units,
            &metadata(&[
                (source, 96),
                (first_destination, 96),
                (second_destination, 96),
            ]),
            &HashMap::default(),
            false,
        )
        .is_empty()
    );
    let instructions = &units[0].blocks[&BlockId(0)].instructions;
    assert!(matches!(
        instructions[instructions.len() - 2],
        SIRInstruction::Store(..)
    ));
    assert!(matches!(
        instructions[instructions.len() - 1],
        SIRInstruction::Store(..)
    ));
}

#[test]
fn removable_cost_counts_reconvergent_definition_once() {
    let source = address(0);
    let destination = address(1);
    let unit = single_block_unit(
        vec![
            load(0, source, 8),
            SIRInstruction::Unary(RegisterId(1), UnaryOp::Ident, RegisterId(0)),
            SIRInstruction::Unary(RegisterId(2), UnaryOp::Ident, RegisterId(0)),
            SIRInstruction::Binary(RegisterId(3), RegisterId(1), BinaryOp::And, RegisterId(2)),
            store(destination, 3, 8),
        ],
        &[(0, 8), (1, 8), (2, 8), (3, 8)],
    );
    let defs = unit
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| def_reg(instruction).map(|dst| (dst, instruction)))
        .collect::<HashMap<_, _>>();
    let use_counts = register_use_counts(&unit);
    let expected = unit.blocks[&BlockId(0)].instructions[..4]
        .iter()
        .map(|instruction| estimate_clif_cost(instruction, &unit.register_map, false))
        .fold(0usize, usize::saturating_add);

    assert_eq!(
        removable_definition_cost_after_removing_use(
            RegisterId(3),
            &defs,
            &use_counts,
            &unit.register_map,
            false,
        ),
        expected,
    );
}

#[test]
fn source_written_anywhere_rejects_identity_commit() {
    let source = address(0);
    let destination = address(1);
    let unit = single_block_unit(
        vec![
            load(0, source, 8),
            store(destination, 0, 8),
            SIRInstruction::Imm(RegisterId(1), SIRValue::new(7u8)),
            store(source, 1, 8),
        ],
        &[(0, 8), (1, 8)],
    );
    let mut units = vec![unit];

    assert!(
        optimize_identity_stores(
            &mut units,
            &metadata(&[(source, 8), (destination, 8)]),
            &HashMap::default(),
            false,
        )
        .is_empty()
    );
    assert!(matches!(
        units[0].blocks[&BlockId(0)].instructions[1],
        SIRInstruction::Store(..)
    ));
}

#[test]
fn four_state_metadata_mismatch_rejects_identity_commit() {
    let source = address(0);
    let destination = address(1);
    let writer = single_block_unit(
        vec![load(0, source, 8), store(destination, 0, 8)],
        &[(0, 8)],
    );
    let mut units = vec![writer];

    assert!(
        optimize_identity_stores(
            &mut units,
            &metadata_with_states(&[(source, 8, true), (destination, 8, false)]),
            &HashMap::default(),
            true,
        )
        .is_empty()
    );
    assert!(matches!(
        units[0].blocks[&BlockId(0)].instructions[1],
        SIRInstruction::Store(..)
    ));
}

#[test]
fn four_state_storage_rejects_identity_commit_even_when_both_sides_match() {
    let source = address(0);
    let destination = address(1);
    let writer = single_block_unit(
        vec![load(0, source, 8), store(destination, 0, 8)],
        &[(0, 8)],
    );
    let reader = single_block_unit(vec![load(0, destination, 8)], &[(0, 8)]);
    let mut units = vec![writer, reader];

    assert!(
        optimize_identity_stores(
            &mut units,
            &metadata_with_states(&[(source, 8, true), (destination, 8, true)]),
            &HashMap::default(),
            true,
        )
        .is_empty()
    );
    assert!(matches!(
        units[0].blocks[&BlockId(0)].instructions[1],
        SIRInstruction::Store(..)
    ));
}

#[test]
fn source_width_mismatch_rejects_identity_commit_in_two_state_mode() {
    let source = address(0);
    let destination = address(1);
    let writer = single_block_unit(
        vec![load(0, source, 8), store(destination, 0, 8)],
        &[(0, 8)],
    );
    let reader = single_block_unit(vec![load(0, destination, 8)], &[(0, 8)]);
    let mut units = vec![writer, reader];

    assert!(
        optimize_identity_stores(
            &mut units,
            &metadata(&[(source, 16), (destination, 8)]),
            &HashMap::default(),
            false,
        )
        .is_empty()
    );
    assert!(matches!(
        units[0].blocks[&BlockId(0)].instructions[1],
        SIRInstruction::Store(..)
    ));
}

#[test]
fn malformed_chunk_result_width_rejects_identity_commit() {
    let source = address(0);
    let destination = address(1);
    let writer = single_block_unit(
        vec![
            load(0, source, 8),
            SIRInstruction::Imm(RegisterId(1), SIRValue::new(4u8)),
            SIRInstruction::Binary(RegisterId(2), RegisterId(0), BinaryOp::Shr, RegisterId(1)),
            SIRInstruction::Imm(RegisterId(3), SIRValue::new(0xfu8)),
            SIRInstruction::Binary(RegisterId(4), RegisterId(2), BinaryOp::And, RegisterId(3)),
            SIRInstruction::Imm(RegisterId(5), SIRValue::new(0xfu8)),
            SIRInstruction::Binary(RegisterId(6), RegisterId(0), BinaryOp::And, RegisterId(5)),
            SIRInstruction::Concat(RegisterId(7), vec![RegisterId(4), RegisterId(6)]),
            store(destination, 7, 8),
        ],
        &[
            (0, 8),
            (1, 3),
            (2, 8),
            (3, 4),
            (4, 2),
            (5, 4),
            (6, 6),
            (7, 8),
        ],
    );
    let reader = single_block_unit(vec![load(0, destination, 8)], &[(0, 8)]);
    let mut units = vec![writer, reader];

    assert!(
        optimize_identity_stores(
            &mut units,
            &metadata(&[(source, 8), (destination, 8)]),
            &HashMap::default(),
            false,
        )
        .is_empty()
    );
    assert!(matches!(
        units[0].blocks[&BlockId(0)].instructions.last(),
        Some(SIRInstruction::Store(..))
    ));
}

#[test]
fn truncated_shift_result_rejects_identity_commit() {
    let source = address(0);
    let destination = address(1);
    let writer = single_block_unit(
        vec![
            load(0, source, 8),
            SIRInstruction::Imm(RegisterId(1), SIRValue::new(4u8)),
            SIRInstruction::Binary(RegisterId(2), RegisterId(0), BinaryOp::Shr, RegisterId(1)),
            SIRInstruction::Imm(RegisterId(3), SIRValue::new(0xfu8)),
            SIRInstruction::Binary(RegisterId(4), RegisterId(2), BinaryOp::And, RegisterId(3)),
            SIRInstruction::Imm(RegisterId(5), SIRValue::new(0xfu8)),
            SIRInstruction::Binary(RegisterId(6), RegisterId(0), BinaryOp::And, RegisterId(5)),
            SIRInstruction::Concat(RegisterId(7), vec![RegisterId(4), RegisterId(6)]),
            store(destination, 7, 8),
        ],
        &[
            (0, 8),
            (1, 3),
            (2, 2),
            (3, 4),
            (4, 4),
            (5, 4),
            (6, 4),
            (7, 8),
        ],
    );
    let reader = single_block_unit(vec![load(0, destination, 8)], &[(0, 8)]);
    let mut units = vec![writer, reader];

    assert!(
        optimize_identity_stores(
            &mut units,
            &metadata(&[(source, 8), (destination, 8)]),
            &HashMap::default(),
            false,
        )
        .is_empty()
    );
    assert!(matches!(
        units[0].blocks[&BlockId(0)].instructions.last(),
        Some(SIRInstruction::Store(..))
    ));
}

#[test]
fn existing_alias_involvement_rejects_identity_commit() {
    let source = address(0);
    let destination = address(1);
    let unrelated = address(2);
    let writer = single_block_unit(
        vec![load(0, source, 8), store(destination, 0, 8)],
        &[(0, 8)],
    );
    let reader = single_block_unit(vec![load(0, destination, 8)], &[(0, 8)]);
    let mut units = vec![writer, reader];
    let existing_aliases = [(unrelated.absolute_addr(), source.absolute_addr())]
        .into_iter()
        .collect();

    assert!(
        optimize_identity_stores(
            &mut units,
            &metadata(&[(source, 8), (destination, 8), (unrelated, 8)]),
            &existing_aliases,
            false,
        )
        .is_empty()
    );
    assert!(matches!(
        units[0].blocks[&BlockId(0)].instructions[1],
        SIRInstruction::Store(..)
    ));
}

/// Only the settle's change snapshot (`NeCase` into a branch that only
/// publishes flags) may read an alias before its copy; a case
/// comparison that controls design logic is an ordinary read.
#[test]
fn only_settle_snapshot_compares_are_compare_only() {
    let flags = address(9);
    let block = |id: usize,
                 instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
                 terminator: SIRTerminator| {
        (
            BlockId(id),
            BasicBlock {
                id: BlockId(id),
                params: Vec::new(),
                instructions,
                terminator,
            },
        )
    };
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [
            // Settle snapshot of object 1: r1 old, r2 new.
            block(
                0,
                vec![
                    load(1, address(1), 8),
                    load(2, address(1), 8),
                    SIRInstruction::Binary(
                        RegisterId(3),
                        RegisterId(1),
                        BinaryOp::NeCase,
                        RegisterId(2),
                    ),
                ],
                SIRTerminator::Branch {
                    cond: RegisterId(3),
                    true_block: (BlockId(1), Vec::new()),
                    false_block: (BlockId(2), Vec::new()),
                },
            ),
            block(
                1,
                vec![
                    SIRInstruction::Imm(RegisterId(4), SIRValue::new(1u8)),
                    store(flags, 4, 8),
                ],
                SIRTerminator::Jump(BlockId(2), Vec::new()),
            ),
            // Design `a === b` deciding a design store.
            block(
                2,
                vec![
                    load(5, address(2), 8),
                    load(6, address(3), 8),
                    SIRInstruction::Binary(
                        RegisterId(7),
                        RegisterId(5),
                        BinaryOp::EqCase,
                        RegisterId(6),
                    ),
                ],
                SIRTerminator::Branch {
                    cond: RegisterId(7),
                    true_block: (BlockId(3), Vec::new()),
                    false_block: (BlockId(4), Vec::new()),
                },
            ),
            block(
                3,
                vec![store(address(4), 5, 8)],
                SIRTerminator::Jump(BlockId(4), Vec::new()),
            ),
            block(4, Vec::new(), SIRTerminator::Return),
        ]
        .into_iter()
        .collect(),
        register_map: register_map(&[(1, 8), (2, 8), (3, 1), (4, 8), (5, 8), (6, 8), (7, 1)]),
    };
    assert!(unit.verify_result().is_ok());
    let compared = compare_only_registers(&unit, flags.absolute_addr());
    assert!(compared.contains(&RegisterId(1)));
    assert!(compared.contains(&RegisterId(2)));
    assert!(!compared.contains(&RegisterId(5)));
    assert!(!compared.contains(&RegisterId(6)));
}

#[test]
fn conditional_identity_store_is_not_replaced_by_commit() {
    let source = address(0);
    let destination = address(1);
    let entry = BasicBlock {
        id: BlockId(0),
        params: Vec::new(),
        instructions: vec![
            SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
            load(1, source, 8),
        ],
        terminator: SIRTerminator::Branch {
            cond: RegisterId(0),
            true_block: (BlockId(1), Vec::new()),
            false_block: (BlockId(2), Vec::new()),
        },
    };
    let true_block = BasicBlock {
        id: BlockId(1),
        params: Vec::new(),
        instructions: vec![store(destination, 1, 8)],
        terminator: SIRTerminator::Return,
    };
    let false_block = BasicBlock {
        id: BlockId(2),
        params: Vec::new(),
        instructions: Vec::new(),
        terminator: SIRTerminator::Return,
    };
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [entry, true_block, false_block]
            .into_iter()
            .map(|block| (block.id, block))
            .collect(),
        register_map: register_map(&[(0, 1), (1, 8)]),
    };
    unit.verify_result().unwrap();
    let mut units = vec![unit];

    assert!(
        optimize_identity_stores(
            &mut units,
            &metadata(&[(source, 8), (destination, 8)]),
            &HashMap::default(),
            false,
        )
        .is_empty()
    );
    assert!(matches!(
        units[0].blocks[&BlockId(1)].instructions[0],
        SIRInstruction::Store(..)
    ));
}

#[test]
fn effectful_identity_store_is_not_replaced_by_commit() {
    let source = address(0);
    let destination = address(1);
    let mut effectful_store = store(destination, 0, 8);
    let SIRInstruction::Store(_, _, _, _, triggers, _) = &mut effectful_store else {
        unreachable!()
    };
    triggers.push(TriggerIdWithKind {
        kind: DomainKind::ClockPosedge,
        id: 0,
    });
    let unit = single_block_unit(vec![load(0, source, 8), effectful_store], &[(0, 8)]);
    let mut units = vec![unit];

    assert!(
        optimize_identity_stores(
            &mut units,
            &metadata(&[(source, 8), (destination, 8)]),
            &HashMap::default(),
            false,
        )
        .is_empty()
    );
    assert!(matches!(
        units[0].blocks[&BlockId(0)].instructions[1],
        SIRInstruction::Store(..)
    ));
}

#[test]
fn selected_alias_keeps_the_store_instead_of_replacing_it_with_commit() {
    let source = address(0);
    let destination = address(1);
    let writer = single_block_unit(
        vec![load(0, source, 8), store(destination, 0, 8)],
        &[(0, 8)],
    );
    let mut units = vec![writer];

    let selected = optimize_identity_stores(
        &mut units,
        &metadata(&[(source, 8), (destination, 8)]),
        &HashMap::default(),
        false,
    );
    assert_eq!(
        selected.get(&destination.absolute_addr()),
        Some(&source.absolute_addr())
    );
    assert!(matches!(
        units[0].blocks[&BlockId(0)].instructions[1],
        SIRInstruction::Store(..)
    ));
}

#[test]
fn second_unit_write_blocks_global_unique_store_proof() {
    let source = address(0);
    let destination = address(1);
    let identity_writer = single_block_unit(
        vec![load(0, source, 8), store(destination, 0, 8)],
        &[(0, 8)],
    );
    let other_writer = single_block_unit(
        vec![
            SIRInstruction::Imm(RegisterId(0), SIRValue::new(7u8)),
            store(destination, 0, 8),
        ],
        &[(0, 8)],
    );

    assert!(
        aliases(
            &[identity_writer, other_writer],
            &metadata(&[(source, 8), (destination, 8)])
        )
        .is_empty()
    );
}

#[test]
fn same_register_number_in_different_units_is_not_a_duplicate_value() {
    let lhs = address(0);
    let rhs = address(1);
    let lhs_writer = single_block_unit(
        vec![
            SIRInstruction::Imm(RegisterId(0), SIRValue::new(3u8)),
            store(lhs, 0, 8),
        ],
        &[(0, 8)],
    );
    let rhs_writer = single_block_unit(
        vec![
            SIRInstruction::Imm(RegisterId(0), SIRValue::new(9u8)),
            store(rhs, 0, 8),
        ],
        &[(0, 8)],
    );

    assert!(aliases(&[lhs_writer, rhs_writer], &metadata(&[(lhs, 8), (rhs, 8)])).is_empty());
}

#[test]
fn duplicate_stores_in_different_blocks_are_not_assumed_lockstep() {
    let lhs = address(0);
    let rhs = address(1);
    let entry = BasicBlock {
        id: BlockId(0),
        params: Vec::new(),
        instructions: vec![
            SIRInstruction::Imm(RegisterId(0), SIRValue::new(3u8)),
            store(lhs, 0, 8),
        ],
        terminator: SIRTerminator::Jump(BlockId(1), Vec::new()),
    };
    let next = BasicBlock {
        id: BlockId(1),
        params: Vec::new(),
        instructions: vec![store(rhs, 0, 8)],
        terminator: SIRTerminator::Return,
    };
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [entry, next]
            .into_iter()
            .map(|block| (block.id, block))
            .collect(),
        register_map: register_map(&[(0, 8)]),
    };
    assert!(unit.verify_result().is_ok());

    assert!(aliases(&[unit], &metadata(&[(lhs, 8), (rhs, 8)])).is_empty());
}

#[test]
fn unconditional_duplicate_stores_alias_only_the_globally_unread_side() {
    let lhs = address(0);
    let rhs = address(1);
    let writer = single_block_unit(
        vec![
            SIRInstruction::Imm(RegisterId(0), SIRValue::new(3u8)),
            store(lhs, 0, 8),
            store(rhs, 0, 8),
        ],
        &[(0, 8)],
    );
    let rhs_reader = single_block_unit(vec![load(0, rhs, 8)], &[(0, 8)]);

    let result = aliases(&[writer, rhs_reader], &metadata(&[(lhs, 8), (rhs, 8)]));
    assert_eq!(result.get(&lhs.absolute_addr()), Some(&rhs.absolute_addr()));
    assert!(!result.contains_key(&rhs.absolute_addr()));
}

#[test]
fn conditional_duplicate_stores_are_not_aliased() {
    let lhs = address(0);
    let rhs = address(1);
    let entry = BasicBlock {
        id: BlockId(0),
        params: Vec::new(),
        instructions: vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8))],
        terminator: SIRTerminator::Branch {
            cond: RegisterId(0),
            true_block: (BlockId(1), Vec::new()),
            false_block: (BlockId(2), Vec::new()),
        },
    };
    let true_block = BasicBlock {
        id: BlockId(1),
        params: Vec::new(),
        instructions: vec![
            SIRInstruction::Imm(RegisterId(1), SIRValue::new(5u8)),
            store(lhs, 1, 8),
            store(rhs, 1, 8),
        ],
        terminator: SIRTerminator::Return,
    };
    let false_block = BasicBlock {
        id: BlockId(2),
        params: Vec::new(),
        instructions: Vec::new(),
        terminator: SIRTerminator::Return,
    };
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [entry, true_block, false_block]
            .into_iter()
            .map(|block| (block.id, block))
            .collect(),
        register_map: register_map(&[(0, 1), (1, 8)]),
    };
    assert!(unit.verify_result().is_ok());

    assert!(aliases(&[unit], &metadata(&[(lhs, 8), (rhs, 8)])).is_empty());
}

#[test]
fn partial_and_capture_enabled_stores_are_not_removable_aliases() {
    let source = address(0);
    let partial_destination = address(1);
    let captured_destination = address(2);
    let partial = single_block_unit(
        vec![load(0, source, 4), store(partial_destination, 0, 4)],
        &[(0, 4)],
    );
    let captured = single_block_unit(
        vec![
            load(0, source, 8),
            store(captured_destination, 0, 8),
            SIRInstruction::CombCaptureEnableIfChanged {
                old: RegisterId(0),
                new: RegisterId(0),
                sites: vec![1],
            },
        ],
        &[(0, 8)],
    );

    assert!(
        aliases(
            &[partial, captured],
            &metadata(&[
                (source, 8),
                (partial_destination, 8),
                (captured_destination, 8),
            ])
        )
        .is_empty()
    );
}

#[test]
fn final_alias_removal_names_the_reproved_identity_store_site() {
    let source = address(0);
    let destination = address(1);
    let mut units = vec![single_block_unit(
        vec![load(0, source, 8), store(destination, 0, 8)],
        &[(0, 8)],
    )];
    let aliases = [(destination.absolute_addr(), source.absolute_addr())]
        .into_iter()
        .collect();
    let metadata = metadata(&[(source, 8), (destination, 8)]);

    remove_proven_alias_stores(&mut units, &aliases, &metadata, false, None);

    assert_eq!(
        units[0].blocks[&BlockId(0)].instructions,
        vec![load(0, source, 8)]
    );
}

#[test]
fn final_alias_revalidation_preserves_a_copy_before_ff_publication() {
    let source = address(0);
    let destination = address(1);
    // The comb copy captures 42 before a fused FF update publishes 0.
    // Redirecting the final load to source would read the new FF value.
    let units = vec![single_block_unit(
        vec![
            load(0, source, 6),
            store(destination, 0, 6),
            SIRInstruction::Imm(RegisterId(1), SIRValue::new(0u8)),
            store(source, 1, 6),
            load(2, destination, 6),
        ],
        &[(0, 6), (1, 6), (2, 6)],
    )];
    let metadata = metadata(&[(source, 6), (destination, 6)]);
    let facts = collect_global_facts(&units, &metadata, false, None);
    assert!(
        proven_alias_store(&facts, destination.absolute_addr(), source.absolute_addr()).is_none()
    );
}

#[test]
fn final_duplicate_alias_revalidation_rejects_a_later_canonical_overwrite() {
    let source = address(0);
    let destination = address(1);
    let units = vec![single_block_unit(
        vec![
            SIRInstruction::Imm(RegisterId(0), SIRValue::new(42u8)),
            store(source, 0, 6),
            store(destination, 0, 6),
            SIRInstruction::Imm(RegisterId(1), SIRValue::new(0u8)),
            store(source, 1, 6),
            load(2, destination, 6),
        ],
        &[(0, 6), (1, 6), (2, 6)],
    )];
    let metadata = metadata(&[(source, 6), (destination, 6)]);
    let facts = collect_global_facts(&units, &metadata, false, None);
    assert!(
        proven_alias_store(&facts, destination.absolute_addr(), source.absolute_addr()).is_none()
    );
}

#[test]
fn final_alias_revalidation_rejects_an_additional_state_write() {
    let source = address(0);
    let destination = address(1);
    let working_destination = RegionedAbsoluteAddr {
        region: WORKING_REGION,
        ..destination
    };
    let units = vec![single_block_unit(
        vec![
            load(0, source, 8),
            store(destination, 0, 8),
            store(working_destination, 0, 8),
        ],
        &[(0, 8)],
    )];
    let metadata = metadata(&[(source, 8), (destination, 8)]);
    let facts = collect_global_facts(&units, &metadata, false, None);

    assert!(
        proven_alias_store(&facts, destination.absolute_addr(), source.absolute_addr()).is_none()
    );
}

fn call(
    reads: Vec<RegionedAbsoluteAddr>,
    writes: Vec<RegionedAbsoluteAddr>,
) -> SIRInstruction<RegionedAbsoluteAddr> {
    SIRInstruction::Call {
        function: celox_sir::FunctionId(0),
        frame: RegisterId(0),
        args: Vec::new(),
        outputs: Vec::new(),
        dst: None,
        effects: std::sync::Arc::new(celox_sir::CallEffects {
            reads,
            writes,
            ..Default::default()
        }),
    }
}

/// A call reading the copy reads it like a load, and one writing either
/// side writes it like a store: each keeps the copy distinct. A call
/// touching neither does not.
#[test]
fn a_call_reading_the_copy_or_writing_either_side_blocks_the_alias() {
    let source = address(0);
    let destination = address(1);
    let other = address(2);
    let metadata = metadata(&[(source, 8), (destination, 8), (other, 8)]);
    let with_call = |call: SIRInstruction<RegionedAbsoluteAddr>| {
        let unit = single_block_unit(
            vec![load(0, source, 8), store(destination, 0, 8), call],
            &[(0, 8)],
        );
        aliases(&[unit], &metadata)
    };
    assert!(with_call(call(vec![destination], Vec::new())).is_empty());
    assert!(with_call(call(Vec::new(), vec![destination])).is_empty());
    assert!(with_call(call(Vec::new(), vec![source])).is_empty());
    assert_eq!(
        with_call(call(vec![other], vec![other])),
        [(destination.absolute_addr(), source.absolute_addr())]
            .into_iter()
            .collect()
    );
}

/// Redirected accesses include those of function units, and calls'
/// effects follow their callees' redirected bodies.
#[test]
fn final_alias_redirection_rewrites_functions_and_call_effects() {
    let canonical = address(0);
    let alias = address(1);
    let caller = single_block_unit(
        vec![
            SIRInstruction::Imm(RegisterId(0), SIRValue::new(0u8)),
            call(vec![alias], Vec::new()),
        ],
        &[(0, 32)],
    );
    let callee = single_block_unit(vec![load(0, alias, 8)], &[(0, 8)]);
    let mut sir = SirProgram {
        eval_comb: vec![caller],
        eval_apply_ffs: HashMap::default(),
        eval_comb_apply_ffs: HashMap::default(),
        merge_comb_apply: false,
        eval_comb_stateful: None,
        eval_only_ffs: HashMap::default(),
        apply_ffs: HashMap::default(),
        parallel: None,
        processes: Vec::new(),
        functions: vec![celox_sir::FunctionUnit {
            unit: callee,
            frame: celox_sir::FrameLayout {
                size: 0,
                slots: Vec::new(),
            },
            effects: Default::default(),
        }],
    };
    sir.summarize_calls();
    let design = celox_design::ElaboratedDesign::default();
    let runtime_schema = celox_design::RuntimeSchema::default();
    let mut layout_requirements = celox_state_layout::LayoutRequirements::default();
    let mut program = OptimizationContext {
        sir: &mut sir,
        design: &design,
        runtime_schema: &runtime_schema,
        layout_requirements: &mut layout_requirements,
    };
    redirect_final_alias_accesses(
        &mut program,
        &[(alias.absolute_addr(), canonical.absolute_addr())]
            .into_iter()
            .collect(),
    );
    assert_eq!(
        sir.functions[0].unit.blocks[&BlockId(0)].instructions,
        [load(0, canonical, 8)]
    );
    let SIRInstruction::Call { effects, .. } =
        &sir.eval_comb[0].blocks[&BlockId(0)].instructions[1]
    else {
        unreachable!()
    };
    assert_eq!(effects.reads, [canonical]);
    assert!(sir.calls_summarized());
}
