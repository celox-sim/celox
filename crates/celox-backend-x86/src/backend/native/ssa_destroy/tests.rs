use super::*;
use crate::native::emit::{self, EmitError};
use crate::native::mir::{BaseReg, MBlock, MInst, OpSize, PhiNode, SpillDesc, VRegAllocator};

fn register_copy(
    destination_value: u32,
    source_value: u32,
    destination: PhysReg,
    source: PhysReg,
) -> ParallelCopy {
    ParallelCopy {
        phi_destination: VReg(destination_value),
        source_value: VReg(source_value),
        destination: ParallelCopyDestination::Register(destination),
        source: ParallelCopySource::Register(source),
    }
}

#[test]
fn resolver_emits_acyclic_chain_in_safe_reverse_order_without_temporary() {
    let rows = vec![
        register_copy(2, 0, PhysReg::RAX, PhysReg::RDX),
        register_copy(3, 1, PhysReg::RSI, PhysReg::RAX),
    ];

    let (operations, work) = resolve_parallel_copies(BlockId(0), BlockId(1), &rows).unwrap();

    assert_eq!(
        operations,
        vec![
            ParallelCopyOperation::Move {
                destination: ParallelCopyDestination::Register(PhysReg::RSI),
                source: ParallelCopySource::Register(PhysReg::RAX),
            },
            ParallelCopyOperation::Move {
                destination: ParallelCopyDestination::Register(PhysReg::RAX),
                source: ParallelCopySource::Register(PhysReg::RDX),
            },
        ]
    );
    assert_eq!(work.effective_copies, 2);
    assert_eq!(work.direct_moves, 2);
    assert_eq!(work.cycle_breaks, 0);
}

#[test]
fn resolver_uses_one_swap_only_for_a_two_register_cycle() {
    let rows = vec![
        register_copy(2, 0, PhysReg::RAX, PhysReg::RDX),
        register_copy(3, 1, PhysReg::RDX, PhysReg::RAX),
    ];
    let (operations, work) = resolve_parallel_copies(BlockId(0), BlockId(1), &rows).unwrap();
    assert_eq!(work.cycle_breaks, 1);
    assert_eq!(work.temporary_cycle_breaks, 0);
    assert_eq!(work.register_swaps, 1);
    assert_eq!(operations.len(), 1);
    assert!(matches!(
        operations[0],
        ParallelCopyOperation::SwapRegisters { .. }
    ));
}

#[test]
fn resolver_uses_a_temporary_for_long_register_cycles() {
    let rows = vec![
        register_copy(3, 0, PhysReg::RAX, PhysReg::RDX),
        register_copy(4, 1, PhysReg::RDX, PhysReg::RSI),
        register_copy(5, 2, PhysReg::RSI, PhysReg::RAX),
    ];
    let (operations, work) = resolve_parallel_copies(BlockId(0), BlockId(1), &rows).unwrap();
    assert_eq!(work.cycle_breaks, 1, "{operations:?}");
    assert_eq!(work.temporary_cycle_breaks, 1, "{operations:?}");
    assert_eq!(work.register_swaps, 0, "{operations:?}");
    assert_eq!(operations.len(), rows.len() + 1);
    assert!(matches!(
        operations.first(),
        Some(ParallelCopyOperation::SaveTemporary(_))
    ));
    assert!(matches!(
        operations.last(),
        Some(ParallelCopyOperation::RestoreTemporary(_))
    ));
}

#[test]
fn resolver_drains_cycle_fanout_before_register_swap() {
    let rows = vec![
        register_copy(3, 0, PhysReg::RAX, PhysReg::RDX),
        register_copy(4, 1, PhysReg::RDX, PhysReg::RAX),
        register_copy(5, 2, PhysReg::RSI, PhysReg::RAX),
    ];

    let (operations, work) = resolve_parallel_copies(BlockId(0), BlockId(1), &rows).unwrap();

    assert_eq!(
        operations.first(),
        Some(&ParallelCopyOperation::Move {
            destination: ParallelCopyDestination::Register(PhysReg::RSI),
            source: ParallelCopySource::Register(PhysReg::RAX),
        })
    );
    assert_eq!(work.cycle_breaks, 1);
    assert_eq!(work.register_swaps, 1);
    assert_eq!(work.temporary_cycle_breaks, 0);
    assert_eq!(work.effective_copies, 3);
}

fn one_edge_function(phi_sources: &[(VReg, VReg)]) -> MFunction {
    let max_vreg = phi_sources
        .iter()
        .flat_map(|(destination, source)| [destination.0, source.0])
        .max()
        .map_or(0, |maximum| maximum + 1);
    let mut vregs = VRegAllocator::new();
    let mut spill_descs = Vec::new();
    while vregs.count() < max_vreg {
        vregs.alloc();
        spill_descs.push(SpillDesc::transient());
    }
    let mut function = MFunction::new(vregs, spill_descs);
    let mut predecessor = MBlock::new(BlockId(0));
    predecessor.push(MInst::Jump { target: BlockId(1) });
    let mut successor = MBlock::new(BlockId(1));
    successor.phis = phi_sources
        .iter()
        .map(|&(destination, source)| PhiNode {
            dst: destination,
            sources: vec![(BlockId(0), source)],
        })
        .collect();
    successor.push(MInst::Return);
    function.push_block(predecessor);
    function.push_block(successor);
    function
}

#[test]
fn missing_phi_source_location_is_a_structured_plan_error() {
    let function = one_edge_function(&[(VReg(1), VReg(0))]);
    let mut assignment = AssignmentMap::default();
    assignment.set(VReg(1), PhysReg::RAX);

    let error = SsaDestructionPlan::build(&function, &assignment).unwrap_err();

    assert_eq!(error.rule, "SSA_DEST.SOURCE_LOCATION");
    assert_eq!(error.predecessor, Some(BlockId(0)));
    assert_eq!(error.successor, Some(BlockId(1)));
    assert_eq!(error.phi_destination, Some(VReg(1)));
    assert_eq!(error.source_value, Some(VReg(0)));
}

#[test]
fn identity_rows_are_counted_but_emit_no_effective_copy() {
    let function = one_edge_function(&[(VReg(1), VReg(0))]);
    let mut assignment = AssignmentMap::default();
    assignment.set(VReg(0), PhysReg::RAX);
    assignment.set(VReg(1), PhysReg::RAX);

    let plan = SsaDestructionPlan::build(&function, &assignment).unwrap();
    plan.verify(&function, &assignment, 0).unwrap();
    let stats = plan.stats();

    assert_eq!(stats.edges, 1);
    assert_eq!(stats.rows, 1);
    assert_eq!(stats.identity_rows, 1);
    assert_eq!(stats.effective_copies, 0);
    assert_eq!(stats.identity_only_edges, 1);
    assert!(
        !plan
            .edge(BlockId(0), BlockId(1))
            .unwrap()
            .has_effective_copies()
    );
}

#[test]
fn trailing_copy_alias_eliminates_a_false_register_cycle() {
    let mut vregs = VRegAllocator::new();
    let original = vregs.alloc();
    let copied = vregs.alloc();
    let first_destination = vregs.alloc();
    let second_destination = vregs.alloc();
    let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);

    let mut predecessor = MBlock::new(BlockId(0));
    predecessor.push(MInst::LoadImm {
        dst: original,
        value: 7,
    });
    predecessor.push(MInst::Mov {
        dst: copied,
        src: original,
    });
    predecessor.push(MInst::Jump { target: BlockId(1) });
    function.push_block(predecessor);

    let mut successor = MBlock::new(BlockId(1));
    successor.phis.push(PhiNode {
        dst: first_destination,
        sources: vec![(BlockId(0), copied)],
    });
    successor.phis.push(PhiNode {
        dst: second_destination,
        sources: vec![(BlockId(0), original)],
    });
    successor.push(MInst::Return);
    function.push_block(successor);

    let mut assignment = AssignmentMap::default();
    assignment.set(original, PhysReg::RAX);
    assignment.set(copied, PhysReg::RDX);
    assignment.set(first_destination, PhysReg::RAX);
    assignment.set(second_destination, PhysReg::RDX);

    let plan = SsaDestructionPlan::build(&function, &assignment).unwrap();
    plan.verify(&function, &assignment, 0).unwrap();
    let edge = plan.edge(BlockId(0), BlockId(1)).unwrap();

    assert!(edge.operations.is_empty());
    assert_eq!(edge.work.effective_copies, 0);
    assert_eq!(edge.work.register_swaps, 0);
}

#[test]
fn semantic_only_phi_retains_ssa_row_without_a_machine_copy() {
    let function = one_edge_function(&[(VReg(1), VReg(0))]);
    let mut assignment = AssignmentMap::default();
    assignment.set_semantic_phi_definition(VReg(1));

    let plan = SsaDestructionPlan::build(&function, &assignment).unwrap();
    plan.verify(&function, &assignment, 0).unwrap();

    let edge = plan.edge(BlockId(0), BlockId(1)).unwrap();
    assert!(edge.rows.is_empty());
    assert!(edge.operations.is_empty());
    assert_eq!(plan.stats().edges, 1);
    assert_eq!(plan.stats().rows, 0);
    assert_eq!(function.blocks[1].phis.len(), 1);
}

#[test]
fn identity_stack_row_still_requires_a_valid_frame_slot() {
    let function = one_edge_function(&[(VReg(1), VReg(0))]);
    let mut assignment = AssignmentMap::default();
    assignment.set_edge_location(BlockId(0), VReg(0), EdgeLocation::Stack(1));
    assignment.set_edge_spill_slot(VReg(1), 1);

    let plan = SsaDestructionPlan::build(&function, &assignment).unwrap();
    let error = plan.verify(&function, &assignment, 16).unwrap_err();

    assert_eq!(error.rule, "SSA_DEST.STACK_SLOT");
    assert_eq!(error.predecessor, Some(BlockId(0)));
    assert_eq!(error.successor, Some(BlockId(1)));
}

#[test]
fn missing_instruction_assignment_is_rejected_before_encoding() {
    let mut vregs = VRegAllocator::new();
    let value = vregs.alloc();
    let mut function = MFunction::new(vregs, vec![SpillDesc::transient()]);
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: value,
        value: 1,
    });
    entry.push(MInst::Return);
    function.push_block(entry);

    let error = match emit::emit(&function, &AssignmentMap::default(), 0) {
        Ok(_) => panic!("missing assignment must be rejected before encoding"),
        Err(error) => error,
    };

    let EmitError::Input(error) = error else {
        panic!("expected emission-input error, got {error}");
    };
    assert_eq!(error.rule, "EMIT.ASSIGNMENT_COMPLETE");
    assert_eq!(error.block, Some(BlockId(0)));
    assert_eq!(error.instruction, Some(0));
    assert_eq!(error.value, Some(value));
}

#[test]
fn ordinary_stack_access_must_be_aligned_and_inside_the_spill_frame() {
    for offset in [-8, 4, 16] {
        let mut vregs = VRegAllocator::new();
        let value = vregs.alloc();
        let mut function = MFunction::new(vregs, vec![SpillDesc::transient()]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::Load {
            dst: value,
            base: BaseReg::StackFrame,
            offset,
            size: OpSize::S64,
        });
        entry.push(MInst::Return);
        function.push_block(entry);
        let mut assignment = AssignmentMap::default();
        assignment.set(value, PhysReg::RAX);

        let error = match emit::emit(&function, &assignment, 16) {
            Ok(_) => panic!("invalid stack-frame access must be rejected"),
            Err(error) => error,
        };
        let EmitError::Input(error) = error else {
            panic!("expected stack-frame input error, got {error}");
        };
        assert_eq!(error.rule, "EMIT.STACK_FRAME_ACCESS");
        assert_eq!(error.block, Some(BlockId(0)));
        assert_eq!(error.instruction, Some(0));
    }
}

#[test]
fn unencodable_frame_size_is_rejected_before_encoding() {
    let mut function = MFunction::new(VRegAllocator::new(), Vec::new());
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::Return);
    function.push_block(entry);

    let error = match emit::emit(&function, &AssignmentMap::default(), i32::MAX as u32) {
        Ok(_) => panic!("unencodable frame must be rejected before encoding"),
        Err(error) => error,
    };

    let EmitError::Input(error) = error else {
        panic!("expected emission-input error, got {error}");
    };
    assert_eq!(error.rule, "EMIT.FRAME_SIZE_RANGE");
}

#[cfg(target_arch = "x86_64")]
fn execute_single_stack_destination(source: EdgeLocation, expected: u64) -> u64 {
    use crate::native::jit_mem::JitCode;

    let mut vregs = VRegAllocator::new();
    let source_value = vregs.alloc();
    let destination = vregs.alloc();
    let observed = vregs.alloc();
    let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
    let mut predecessor = MBlock::new(BlockId(0));
    predecessor.push(MInst::LoadImm {
        dst: source_value,
        value: expected,
    });
    if matches!(source, EdgeLocation::Stack(_)) {
        predecessor.push(MInst::Store {
            base: BaseReg::StackFrame,
            offset: 0,
            src: source_value,
            size: OpSize::S64,
        });
    }
    predecessor.push(MInst::Jump { target: BlockId(1) });
    let mut successor = MBlock::new(BlockId(1));
    successor.phis.push(PhiNode {
        dst: destination,
        sources: vec![(BlockId(0), source_value)],
    });
    successor.push(MInst::Load {
        dst: observed,
        base: BaseReg::StackFrame,
        offset: 8,
        size: OpSize::S64,
    });
    successor.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 0,
        src: observed,
        size: OpSize::S64,
    });
    successor.push(MInst::Return);
    function.blocks = vec![predecessor, successor];

    let mut assignment = AssignmentMap::default();
    assignment.set(source_value, PhysReg::RAX);
    assignment.set(observed, PhysReg::RDX);
    assignment.set_edge_location(BlockId(0), source_value, source);
    assignment.set_edge_spill_slot(destination, 8);
    let emitted = emit::emit(&function, &assignment, 16).unwrap();
    let jit = JitCode::new(&emitted.code).unwrap();
    let mut state = [0_u8; 8];
    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    u64::from_le_bytes(state)
}

#[cfg(target_arch = "x86_64")]
#[test]
fn single_stack_to_stack_copy_uses_the_original_frame_offsets() {
    assert_eq!(
        execute_single_stack_destination(EdgeLocation::Stack(0), 0x1122_3344_5566_7788),
        0x1122_3344_5566_7788
    );
}

#[cfg(target_arch = "x86_64")]
#[test]
fn single_immediate_to_stack_copy_preserves_all_64_bits() {
    assert_eq!(
        execute_single_stack_destination(
            EdgeLocation::Immediate(0x8877_6655_4433_2211),
            0x8877_6655_4433_2211,
        ),
        0x8877_6655_4433_2211
    );
}

#[cfg(target_arch = "x86_64")]
fn execute_register_parallel_copy(
    first_destination: PhysReg,
    second_destination: PhysReg,
) -> (Vec<iced_x86::Mnemonic>, ParallelCopyWork, [u64; 2]) {
    use crate::native::jit_mem::JitCode;
    use iced_x86::{Decoder, DecoderOptions};

    let mut vregs = VRegAllocator::new();
    let first_source = vregs.alloc();
    let second_source = vregs.alloc();
    let first_result = vregs.alloc();
    let second_result = vregs.alloc();
    let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);
    let mut predecessor = MBlock::new(BlockId(0));
    predecessor.push(MInst::LoadImm {
        dst: first_source,
        value: 11,
    });
    predecessor.push(MInst::LoadImm {
        dst: second_source,
        value: 22,
    });
    predecessor.push(MInst::Jump { target: BlockId(1) });
    let mut successor = MBlock::new(BlockId(1));
    successor.phis = vec![
        PhiNode {
            dst: first_result,
            sources: vec![(BlockId(0), first_source)],
        },
        PhiNode {
            dst: second_result,
            sources: vec![(BlockId(0), second_source)],
        },
    ];
    successor.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 0,
        src: first_result,
        size: OpSize::S64,
    });
    successor.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 8,
        src: second_result,
        size: OpSize::S64,
    });
    successor.push(MInst::Return);
    function.blocks = vec![predecessor, successor];

    let mut assignment = AssignmentMap::default();
    assignment.set(first_source, PhysReg::RDX);
    assignment.set(second_source, PhysReg::RAX);
    assignment.set(first_result, first_destination);
    assignment.set(second_result, second_destination);
    assignment.set_edge_location(
        BlockId(0),
        first_source,
        EdgeLocation::Register(PhysReg::RDX),
    );
    assignment.set_edge_location(
        BlockId(0),
        second_source,
        EdgeLocation::Register(PhysReg::RAX),
    );

    let plan = SsaDestructionPlan::build(&function, &assignment).unwrap();
    plan.verify(&function, &assignment, 0).unwrap();
    let work = plan.edge(BlockId(0), BlockId(1)).unwrap().work;
    let emitted = emit::emit(&function, &assignment, 0).unwrap();
    let start = emitted
        .block_offsets
        .iter()
        .find(|(block, _)| *block == BlockId(0))
        .unwrap()
        .1 as usize;
    let end = emitted
        .block_offsets
        .iter()
        .find(|(block, _)| *block == BlockId(1))
        .unwrap()
        .1 as usize;
    let mut decoder = Decoder::with_ip(
        64,
        &emitted.code[start..end],
        start as u64,
        DecoderOptions::NONE,
    );
    let mut mnemonics = Vec::new();
    while decoder.can_decode() {
        mnemonics.push(decoder.decode().mnemonic());
    }

    let jit = JitCode::new(&emitted.code).unwrap();
    let mut state = [0_u8; 16];
    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    let values = [
        u64::from_le_bytes(state[..8].try_into().unwrap()),
        u64::from_le_bytes(state[8..].try_into().unwrap()),
    ];
    (mnemonics, work, values)
}

#[cfg(target_arch = "x86_64")]
#[test]
fn acyclic_register_copies_execute_without_stack_temporary() {
    use iced_x86::Mnemonic;

    let (mnemonics, work, values) = execute_register_parallel_copy(PhysReg::RAX, PhysReg::RSI);

    assert_eq!(values, [11, 22]);
    assert_eq!(work.cycle_breaks, 0);
    assert_eq!(work.direct_moves, 2);
    assert!(!mnemonics.contains(&Mnemonic::Push), "{mnemonics:?}");
    assert!(!mnemonics.contains(&Mnemonic::Pop), "{mnemonics:?}");
}

#[cfg(target_arch = "x86_64")]
#[test]
fn register_cycle_executes_with_one_exchange_and_no_stack_temporary() {
    use iced_x86::Mnemonic;

    let (mnemonics, work, values) = execute_register_parallel_copy(PhysReg::RAX, PhysReg::RDX);

    assert_eq!(values, [11, 22]);
    assert_eq!(work.cycle_breaks, 1);
    assert_eq!(work.register_swaps, 1);
    assert_eq!(work.temporary_cycle_breaks, 0);
    assert_eq!(
        mnemonics
            .iter()
            .filter(|mnemonic| **mnemonic == Mnemonic::Xchg)
            .count(),
        1,
        "{mnemonics:?}"
    );
    assert!(!mnemonics.contains(&Mnemonic::Push), "{mnemonics:?}");
    assert!(!mnemonics.contains(&Mnemonic::Pop), "{mnemonics:?}");
}

#[cfg(target_arch = "x86_64")]
#[test]
fn identity_only_branch_edges_keep_flag_predicate_and_false_fallthrough() {
    use iced_x86::{Decoder, DecoderOptions, Mnemonic};

    let mut vregs = VRegAllocator::new();
    let lhs = vregs.alloc();
    let rhs = vregs.alloc();
    let value = vregs.alloc();
    let condition = vregs.alloc();
    let false_value = vregs.alloc();
    let true_value = vregs.alloc();
    let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 6]);
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm { dst: lhs, value: 1 });
    entry.push(MInst::LoadImm { dst: rhs, value: 1 });
    entry.push(MInst::LoadImm {
        dst: value,
        value: 99,
    });
    entry.push(MInst::Cmp {
        dst: condition,
        lhs,
        rhs,
        kind: crate::native::mir::CmpKind::Eq,
    });
    entry.push(MInst::Branch {
        cond: condition,
        true_bb: BlockId(2),
        false_bb: BlockId(1),
    });
    let mut false_block = MBlock::new(BlockId(1));
    false_block.phis.push(PhiNode {
        dst: false_value,
        sources: vec![(BlockId(0), value)],
    });
    false_block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 0,
        src: false_value,
        size: OpSize::S64,
    });
    false_block.push(MInst::Return);
    let mut true_block = MBlock::new(BlockId(2));
    true_block.phis.push(PhiNode {
        dst: true_value,
        sources: vec![(BlockId(0), value)],
    });
    true_block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 0,
        src: true_value,
        size: OpSize::S64,
    });
    true_block.push(MInst::Return);
    function.blocks = vec![entry, false_block, true_block];

    let mut assignment = AssignmentMap::default();
    for (vreg, register) in [
        (lhs, PhysReg::RAX),
        (rhs, PhysReg::RDX),
        (value, PhysReg::RSI),
        (condition, PhysReg::RDI),
        (false_value, PhysReg::RSI),
        (true_value, PhysReg::RSI),
    ] {
        assignment.set(vreg, register);
    }
    assignment.set_edge_location(BlockId(0), value, EdgeLocation::Register(PhysReg::RSI));

    assert_eq!(
        crate::native::mir_opt::fold_register_branch_predicates(&mut function),
        1
    );
    let plan = SsaDestructionPlan::build(&function, &assignment).unwrap();
    assert_eq!(plan.stats().effective_copies, 0);
    let emitted = emit::emit(&function, &assignment, 0).unwrap();
    let start = emitted.block_offsets[0].1 as usize;
    let end = emitted.block_offsets[1].1 as usize;
    let mut decoder = Decoder::with_ip(
        64,
        &emitted.code[start..end],
        start as u64,
        DecoderOptions::NONE,
    );
    let mut mnemonics = Vec::new();
    while decoder.can_decode() {
        mnemonics.push(decoder.decode().mnemonic());
    }
    assert!(mnemonics.contains(&Mnemonic::Cmp), "{mnemonics:?}");
    assert_eq!(
        mnemonics
            .iter()
            .filter(|mnemonic| matches!(mnemonic, Mnemonic::Je | Mnemonic::Jne))
            .count(),
        1,
        "{mnemonics:?}"
    );
    assert!(!mnemonics.contains(&Mnemonic::Jmp), "{mnemonics:?}");
    assert!(
        !mnemonics
            .iter()
            .any(|mnemonic| matches!(mnemonic, Mnemonic::Sete | Mnemonic::Setne))
    );
}

#[test]
fn consecutive_empty_fallthrough_blocks_alias_the_next_instruction() {
    use crate::HashMap;
    use crate::native::jit_mem::JitCode;
    use iced_x86::{Decoder, DecoderOptions, Mnemonic};

    let mut vregs = VRegAllocator::new();
    let condition = vregs.alloc();
    let true_value = vregs.alloc();
    let false_value = vregs.alloc();
    let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);

    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::Load {
        dst: condition,
        base: BaseReg::SimState,
        offset: 0,
        size: OpSize::S64,
    });
    entry.push(MInst::Branch {
        cond: condition,
        true_bb: BlockId(1),
        false_bb: BlockId(4),
    });

    let mut first_empty = MBlock::new(BlockId(1));
    first_empty.push(MInst::Jump { target: BlockId(2) });
    let mut second_empty = MBlock::new(BlockId(2));
    second_empty.push(MInst::Jump { target: BlockId(3) });

    let mut true_block = MBlock::new(BlockId(3));
    true_block.push(MInst::LoadImm {
        dst: true_value,
        value: 11,
    });
    true_block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 8,
        src: true_value,
        size: OpSize::S64,
    });
    true_block.push(MInst::Return);

    let mut false_block = MBlock::new(BlockId(4));
    false_block.push(MInst::LoadImm {
        dst: false_value,
        value: 22,
    });
    false_block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 8,
        src: false_value,
        size: OpSize::S64,
    });
    false_block.push(MInst::Return);

    function.blocks = vec![entry, first_empty, second_empty, true_block, false_block];
    let mut assignment = AssignmentMap::default();
    assignment.set(condition, PhysReg::RAX);
    assignment.set(true_value, PhysReg::RDX);
    assignment.set(false_value, PhysReg::RDX);

    let emitted = emit::emit(&function, &assignment, 0).unwrap();
    let offsets = emitted
        .block_offsets
        .iter()
        .copied()
        .collect::<HashMap<_, _>>();
    assert_eq!(offsets[&BlockId(1)], offsets[&BlockId(2)]);
    assert_eq!(offsets[&BlockId(2)], offsets[&BlockId(3)]);

    let mut decoder = Decoder::with_ip(64, &emitted.code, 0, DecoderOptions::NONE);
    while decoder.can_decode() {
        assert_ne!(decoder.decode().mnemonic(), Mnemonic::Nop);
    }

    let jit = JitCode::new(&emitted.code).unwrap();
    for (condition, expected) in [(0_u64, 22_u64), (1, 11)] {
        let mut state = [0_u8; 16];
        state[..8].copy_from_slice(&condition.to_le_bytes());
        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        assert_eq!(u64::from_le_bytes(state[8..].try_into().unwrap()), expected);
    }
}

#[test]
fn trailing_continuation_label_aliases_an_empty_fallthrough_chain() {
    use crate::HashMap;
    use crate::native::jit_mem::JitCode;
    use iced_x86::{Decoder, DecoderOptions, Mnemonic};

    let mut vregs = VRegAllocator::new();
    let source = vregs.alloc();
    let result = vregs.alloc();
    let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);

    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::Load {
        dst: source,
        base: BaseReg::SimState,
        offset: 0,
        size: OpSize::S64,
    });
    entry.push(MInst::BsrOr {
        dst: result,
        src: source,
        zero_value: 63,
    });
    entry.push(MInst::Jump { target: BlockId(1) });

    let mut first_empty = MBlock::new(BlockId(1));
    first_empty.push(MInst::Jump { target: BlockId(2) });
    let mut second_empty = MBlock::new(BlockId(2));
    second_empty.push(MInst::Jump { target: BlockId(3) });
    let mut exit = MBlock::new(BlockId(3));
    exit.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 8,
        src: result,
        size: OpSize::S64,
    });
    exit.push(MInst::Return);
    function.blocks = vec![entry, first_empty, second_empty, exit];

    let mut assignment = AssignmentMap::default();
    assignment.set(source, PhysReg::RAX);
    assignment.set(result, PhysReg::RDX);

    let emitted = emit::emit(&function, &assignment, 0).unwrap();
    let offsets = emitted
        .block_offsets
        .iter()
        .copied()
        .collect::<HashMap<_, _>>();
    assert_eq!(offsets[&BlockId(1)], offsets[&BlockId(2)]);
    assert_eq!(offsets[&BlockId(2)], offsets[&BlockId(3)]);

    let mut decoder = Decoder::with_ip(64, &emitted.code, 0, DecoderOptions::NONE);
    while decoder.can_decode() {
        assert_ne!(decoder.decode().mnemonic(), Mnemonic::Nop);
    }

    let jit = JitCode::new(&emitted.code).unwrap();
    for (source, expected) in [(0_u64, 63_u64), (8, 3)] {
        let mut state = [0_u8; 16];
        state[..8].copy_from_slice(&source.to_le_bytes());
        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        assert_eq!(u64::from_le_bytes(state[8..].try_into().unwrap()), expected);
    }
}

#[cfg(target_arch = "x86_64")]
#[test]
fn branch_executes_only_the_selected_edge_copy_plan() {
    use crate::native::jit_mem::JitCode;

    let mut vregs = VRegAllocator::new();
    let true_source = vregs.alloc();
    let false_source = vregs.alloc();
    let condition = vregs.alloc();
    let false_value = vregs.alloc();
    let true_value = vregs.alloc();
    let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 5]);
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: true_source,
        value: 11,
    });
    entry.push(MInst::LoadImm {
        dst: false_source,
        value: 22,
    });
    entry.push(MInst::Load {
        dst: condition,
        base: BaseReg::SimState,
        offset: 0,
        size: OpSize::S64,
    });
    entry.push(MInst::Branch {
        cond: condition,
        true_bb: BlockId(2),
        false_bb: BlockId(1),
    });
    let mut false_block = MBlock::new(BlockId(1));
    false_block.phis.push(PhiNode {
        dst: false_value,
        sources: vec![(BlockId(0), false_source)],
    });
    false_block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 8,
        src: false_value,
        size: OpSize::S64,
    });
    false_block.push(MInst::Return);
    let mut true_block = MBlock::new(BlockId(2));
    true_block.phis.push(PhiNode {
        dst: true_value,
        sources: vec![(BlockId(0), true_source)],
    });
    true_block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 8,
        src: true_value,
        size: OpSize::S64,
    });
    true_block.push(MInst::Return);
    function.blocks = vec![entry, false_block, true_block];

    let mut assignment = AssignmentMap::default();
    for (vreg, register) in [
        (true_source, PhysReg::RAX),
        (false_source, PhysReg::RDX),
        (condition, PhysReg::R8),
        (false_value, PhysReg::RDI),
        (true_value, PhysReg::RSI),
    ] {
        assignment.set(vreg, register);
    }
    assignment.set_edge_location(
        BlockId(0),
        true_source,
        EdgeLocation::Register(PhysReg::RAX),
    );
    assignment.set_edge_location(
        BlockId(0),
        false_source,
        EdgeLocation::Register(PhysReg::RDX),
    );
    let plan = SsaDestructionPlan::build(&function, &assignment).unwrap();
    assert_eq!(plan.stats().effective_copies, 2);
    let emitted = emit::emit(&function, &assignment, 0).unwrap();
    let jit = JitCode::new(&emitted.code).unwrap();

    for (condition, expected) in [(0_u64, 22_u64), (1, 11)] {
        let mut state = [0_u8; 16];
        state[..8].copy_from_slice(&condition.to_le_bytes());
        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        assert_eq!(u64::from_le_bytes(state[8..].try_into().unwrap()), expected);
    }
}

#[test]
fn verifier_rejects_two_phi_rows_with_one_physical_destination() {
    let function = one_edge_function(&[(VReg(2), VReg(0)), (VReg(3), VReg(1))]);
    let mut assignment = AssignmentMap::default();
    assignment.set(VReg(0), PhysReg::RAX);
    assignment.set(VReg(1), PhysReg::RDX);
    assignment.set(VReg(2), PhysReg::RSI);
    assignment.set(VReg(3), PhysReg::RDI);

    let mut plan = SsaDestructionPlan::build(&function, &assignment).unwrap();
    let edge = plan.edges.get_mut(&(BlockId(0), BlockId(1))).unwrap();
    edge.rows[1].destination = edge.rows[0].destination;
    let error = plan.verify(&function, &assignment, 0).unwrap_err();
    assert_eq!(error.rule, "SSA_DEST.NON_UNIQUE_DESTINATION");
    assert_eq!(error.predecessor, Some(BlockId(0)));
    assert_eq!(error.successor, Some(BlockId(1)));
}

#[test]
fn verifier_rejects_an_incomplete_materialized_edge_plan() {
    let function = one_edge_function(&[(VReg(2), VReg(0)), (VReg(3), VReg(1))]);
    let mut assignment = AssignmentMap::default();
    assignment.set(VReg(0), PhysReg::RAX);
    assignment.set(VReg(1), PhysReg::RDX);
    assignment.set(VReg(2), PhysReg::RSI);
    assignment.set(VReg(3), PhysReg::RDI);

    let mut plan = SsaDestructionPlan::build(&function, &assignment).unwrap();
    plan.edges
        .get_mut(&(BlockId(0), BlockId(1)))
        .unwrap()
        .rows
        .pop();
    let error = plan.verify(&function, &assignment, 0).unwrap_err();
    assert_eq!(error.rule, "SSA_DEST.INCOMPLETE_EDGE_ROWS");
}

#[cfg(target_arch = "x86_64")]
#[test]
fn mixed_register_stack_immediate_cycles_preserve_parallel_semantics() {
    use crate::native::jit_mem::JitCode;

    let mut vregs = VRegAllocator::new();
    let values = (0..15).map(|_| vregs.alloc()).collect::<Vec<_>>();
    let [
        a,
        b,
        stack_zero,
        stack_eight,
        immediate,
        dst_a,
        dst_b,
        dst_stack_reg,
        dst_stack_eight,
        dst_stack_zero,
        dst_immediate_reg,
        dst_immediate_stack,
        load_zero,
        load_eight,
        load_sixteen,
    ] = values.as_slice()
    else {
        unreachable!()
    };
    let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 15]);
    let mut predecessor = MBlock::new(BlockId(0));
    predecessor.push(MInst::LoadImm { dst: *a, value: 11 });
    predecessor.push(MInst::LoadImm { dst: *b, value: 22 });
    predecessor.push(MInst::LoadImm {
        dst: *stack_zero,
        value: 33,
    });
    predecessor.push(MInst::Store {
        base: BaseReg::StackFrame,
        offset: 0,
        src: *stack_zero,
        size: OpSize::S64,
    });
    predecessor.push(MInst::LoadImm {
        dst: *stack_eight,
        value: 44,
    });
    predecessor.push(MInst::Store {
        base: BaseReg::StackFrame,
        offset: 8,
        src: *stack_eight,
        size: OpSize::S64,
    });
    predecessor.push(MInst::LoadImm {
        dst: *immediate,
        value: 55,
    });
    predecessor.push(MInst::Jump { target: BlockId(1) });

    let mut successor = MBlock::new(BlockId(1));
    successor.phis = vec![
        PhiNode {
            dst: *dst_a,
            sources: vec![(BlockId(0), *a)],
        },
        PhiNode {
            dst: *dst_b,
            sources: vec![(BlockId(0), *b)],
        },
        PhiNode {
            dst: *dst_stack_reg,
            sources: vec![(BlockId(0), *stack_zero)],
        },
        PhiNode {
            dst: *dst_stack_eight,
            sources: vec![(BlockId(0), *stack_zero)],
        },
        PhiNode {
            dst: *dst_stack_zero,
            sources: vec![(BlockId(0), *stack_eight)],
        },
        PhiNode {
            dst: *dst_immediate_reg,
            sources: vec![(BlockId(0), *immediate)],
        },
        PhiNode {
            dst: *dst_immediate_stack,
            sources: vec![(BlockId(0), *immediate)],
        },
    ];
    for (offset, source) in [
        (0, *dst_a),
        (8, *dst_b),
        (16, *dst_stack_reg),
        (24, *dst_immediate_reg),
    ] {
        successor.push(MInst::Store {
            base: BaseReg::SimState,
            offset,
            src: source,
            size: OpSize::S64,
        });
    }
    for (stack_offset, state_offset, destination) in [
        (0, 32, *load_zero),
        (8, 40, *load_eight),
        (16, 48, *load_sixteen),
    ] {
        successor.push(MInst::Load {
            dst: destination,
            base: BaseReg::StackFrame,
            offset: stack_offset,
            size: OpSize::S64,
        });
        successor.push(MInst::Store {
            base: BaseReg::SimState,
            offset: state_offset,
            src: destination,
            size: OpSize::S64,
        });
    }
    successor.push(MInst::Return);
    function.push_block(predecessor);
    function.push_block(successor);

    let mut assignment = AssignmentMap::default();
    for (value, register) in [
        (*a, PhysReg::RAX),
        (*b, PhysReg::RDX),
        (*stack_zero, PhysReg::RSI),
        (*stack_eight, PhysReg::RDI),
        (*immediate, PhysReg::R8),
        (*dst_a, PhysReg::RDX),
        (*dst_b, PhysReg::RAX),
        (*dst_stack_reg, PhysReg::R9),
        (*dst_immediate_reg, PhysReg::R10),
        (*load_zero, PhysReg::R11),
        (*load_eight, PhysReg::R11),
        (*load_sixteen, PhysReg::R11),
    ] {
        assignment.set(value, register);
    }
    assignment.set_edge_location(BlockId(0), *stack_zero, EdgeLocation::Stack(0));
    assignment.set_edge_location(BlockId(0), *stack_eight, EdgeLocation::Stack(8));
    assignment.set_edge_location(BlockId(0), *immediate, EdgeLocation::Immediate(77));
    assignment.set_edge_spill_slot(*dst_stack_eight, 8);
    assignment.set_edge_spill_slot(*dst_stack_zero, 0);
    assignment.set_edge_spill_slot(*dst_immediate_stack, 16);

    let emitted = emit::emit(&function, &assignment, 24).unwrap();
    let jit = JitCode::new(&emitted.code).unwrap();
    let mut state = vec![0_u8; 56];
    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    let actual = state
        .as_chunks::<8>()
        .0
        .iter()
        .map(|bytes| u64::from_le_bytes(*bytes))
        .collect::<Vec<_>>();
    assert_eq!(actual, [11, 22, 33, 77, 44, 33, 77]);
}
