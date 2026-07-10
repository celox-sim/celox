use super::analysis;
use super::assignment::*;
use super::*;
use crate::backend::native::mir::*;
use crate::backend::native::{emit, jit_mem};

#[test]
fn invalid_input_is_a_structured_error_not_a_panic() {
    let mut func = MFunction::new(VRegAllocator::new(), Vec::new());

    let error = match run_regalloc(&mut func) {
        Ok(_) => panic!("empty MIR must be rejected"),
        Err(error) => error,
    };

    assert_eq!(error.phase, "input MIR verification");
    assert_eq!(error.rule, "CFG.NON_EMPTY");
    assert_eq!(error.block, None);
}

#[test]
fn cssa_error_mapping_preserves_structured_context() {
    let source = VReg(7);
    let error = super::cssa::CssaError {
        rule: "CSSA.TEST_RULE",
        block: Some(BlockId(3)),
        instruction: Some(4),
        class: None,
        values: vec![source],
        message: "test CSSA failure".into(),
    };

    let mapped = super::cssa_error("CSSA normalization", error);

    assert_eq!(mapped.phase, "CSSA normalization");
    assert_eq!(mapped.rule, "CSSA.TEST_RULE");
    assert_eq!(mapped.block, Some(BlockId(3)));
    assert_eq!(mapped.instruction, Some(4));
    assert_eq!(mapped.values, vec![source]);
    assert_eq!(mapped.message, "test CSSA failure");
}

/// Build a simple MFunction with one block, run regalloc, verify.
fn run_and_verify(insts: Vec<MInst>, mut spill_descs: Vec<SpillDesc>) -> AssignmentMap {
    // Find the max VReg number used in instructions
    let mut max_vreg = spill_descs.len() as u32;
    for inst in &insts {
        if let Some(d) = inst.def() {
            max_vreg = max_vreg.max(d.0 + 1);
        }
        for u in inst.uses() {
            max_vreg = max_vreg.max(u.0 + 1);
        }
    }
    let mut vregs = VRegAllocator::new();
    while vregs.count() < max_vreg {
        vregs.alloc();
    }
    while spill_descs.len() < max_vreg as usize {
        spill_descs.push(SpillDesc::transient());
    }
    let mut func = MFunction::new(vregs, spill_descs);
    let mut block = MBlock::new(BlockId(0));
    for inst in insts {
        block.push(inst);
    }
    block.push(MInst::Return);
    func.push_block(block);

    let result = run_regalloc(&mut func).unwrap();

    // Re-verify on final instructions
    let analysis = analysis::analyze(&func);
    super::verify_assignment(&func, &analysis, &result.assignment).unwrap();

    result.assignment
}

fn emit_and_run_store0(func: &MFunction, assignment: &AssignmentMap) -> u64 {
    let emitted = emit::emit(func, assignment, 0).expect("emit failed");
    let jit = jit_mem::JitCode::new(&emitted.code).expect("jit allocation failed");
    let mut state = vec![0u8; 8];
    let status = unsafe { jit.call(&mut state) };
    assert_eq!(status, 0);
    u64::from_le_bytes(state[..8].try_into().unwrap())
}

fn select_store_function(cond_value: u64) -> MFunction {
    let mut vregs = VRegAllocator::new();
    while vregs.count() < 4 {
        vregs.alloc();
    }
    let spill_descs = vec![
        SpillDesc::remat(cond_value),
        SpillDesc::remat(42),
        SpillDesc::remat(99),
        SpillDesc::transient(),
    ];
    let mut func = MFunction::new(vregs, spill_descs);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm {
        dst: VReg(0),
        value: cond_value,
    });
    block.push(MInst::LoadImm {
        dst: VReg(1),
        value: 42,
    });
    block.push(MInst::LoadImm {
        dst: VReg(2),
        value: 99,
    });
    block.push(MInst::Select {
        dst: VReg(3),
        cond: VReg(0),
        true_val: VReg(1),
        false_val: VReg(2),
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 0,
        src: VReg(3),
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    func.push_block(block);
    func
}

#[test]
fn test_simple_add() {
    // v0 = imm 42
    // v1 = imm 10
    // v2 = add v0, v1
    // store [sim+0], v2
    // ret
    let insts = vec![
        MInst::LoadImm {
            dst: VReg(0),
            value: 42,
        },
        MInst::LoadImm {
            dst: VReg(1),
            value: 10,
        },
        MInst::Add {
            dst: VReg(2),
            lhs: VReg(0),
            rhs: VReg(1),
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: VReg(2),
            size: OpSize::S64,
        },
    ];
    let descs = vec![
        SpillDesc::remat(42),
        SpillDesc::remat(10),
        SpillDesc::transient(),
    ];
    let asgn = run_and_verify(insts, descs);
    // All 3 VRegs should have assignments
    assert!(asgn.get(VReg(0)).is_some());
    assert!(asgn.get(VReg(1)).is_some());
    assert!(asgn.get(VReg(2)).is_some());
    // No two should share a register (guaranteed by verifier, but check)
    let r0 = asgn.get(VReg(0)).unwrap();
    let r1 = asgn.get(VReg(1)).unwrap();
    let _r2 = asgn.get(VReg(2)).unwrap();
    assert_ne!(r0, r1);
    // v0 dies after add, v2 can reuse v0's register
}

#[test]
fn test_shift_rcx_constraint() {
    // v0 = imm 100
    // v1 = imm 3
    // v2 = shl v0, v1   ← v1 must be in RCX (Fixed constraint)
    // store [sim+0], v2
    let insts = vec![
        MInst::LoadImm {
            dst: VReg(0),
            value: 100,
        },
        MInst::LoadImm {
            dst: VReg(1),
            value: 3,
        },
        MInst::Shl {
            dst: VReg(2),
            lhs: VReg(0),
            rhs: VReg(1),
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: VReg(2),
            size: OpSize::S64,
        },
    ];
    let descs = vec![
        SpillDesc::remat(100),
        SpillDesc::remat(3),
        SpillDesc::transient(),
    ];
    let _asgn = run_and_verify(insts, descs);
    // Verifier passing = no conflicts. Unified pass may create a
    // fresh copy for the shift rhs, so v1 itself may not be in RCX.
}

#[test]
fn test_high_pressure_spill() {
    // Create 14 live VRegs (> 13 registers) to force spilling
    let mut insts = Vec::new();
    let mut descs = Vec::new();
    for i in 0..14 {
        insts.push(MInst::LoadImm {
            dst: VReg(i),
            value: i as u64,
        });
        descs.push(SpillDesc::remat(i as u64));
    }
    // Use all 14 in a chain of adds
    let mut acc = VReg(0);
    for i in 1..14 {
        let dst = VReg(14 + i);
        insts.push(MInst::Add {
            dst,
            lhs: acc,
            rhs: VReg(i),
        });
        descs.push(SpillDesc::transient());
        acc = dst;
    }
    insts.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 0,
        src: acc,
        size: OpSize::S64,
    });

    // This should NOT panic — spilling handles the pressure
    let _asgn = run_and_verify(insts, descs);
}

#[test]
fn test_shift_with_pressure() {
    // Many live VRegs + shift instruction = tests RCX blocked set + spilling
    let mut insts = Vec::new();
    let mut descs = Vec::new();
    // Create 10 live values (leaving room for shift overhead)
    for i in 0..10u32 {
        insts.push(MInst::LoadImm {
            dst: VReg(i),
            value: i as u64,
        });
        descs.push(SpillDesc::remat(i as u64));
    }
    // Shift using v0 (lhs) and v1 (rhs → RCX)
    let shift_dst = VReg(10);
    insts.push(MInst::Shl {
        dst: shift_dst,
        lhs: VReg(0),
        rhs: VReg(1),
    });
    descs.push(SpillDesc::transient());

    // Use remaining values after the shift (so they're live across it)
    let mut acc = shift_dst;
    for i in 2..10u32 {
        let dst = VReg(11 + i);
        insts.push(MInst::Add {
            dst,
            lhs: acc,
            rhs: VReg(i),
        });
        descs.push(SpillDesc::transient());
        acc = dst;
    }
    insts.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 0,
        src: acc,
        size: OpSize::S64,
    });

    // Build function. Ensure VReg counter is past all used VRegs.
    let max_vreg = 21u32; // v0..v20 used in the test
    let mut vregs = VRegAllocator::new();
    while (vregs.count()) < max_vreg {
        vregs.alloc();
    }
    while descs.len() < max_vreg as usize {
        descs.push(SpillDesc::transient());
    }
    let mut func = MFunction::new(vregs, descs);
    let mut block = MBlock::new(BlockId(0));
    for inst in insts {
        block.push(inst);
    }
    block.push(MInst::Return);
    func.push_block(block);

    let analysis_pre = analysis::analyze(&func);
    let (assignment, _) = super::unified::unified_alloc(&mut func, &analysis_pre);

    // Dump
    for (ii, inst) in func.blocks[0].insts.iter().enumerate() {
        let r = inst.def().and_then(|d| assignment.get(d));
        eprintln!("  [{ii:3}] {inst}  => {r:?}");
    }

    // Verify
    let analysis = analysis::analyze(&func);
    super::verify_assignment(&func, &analysis, &assignment).unwrap();
}

#[test]
fn test_select_aliasing() {
    // Test Select instruction with dst == true_val register aliasing
    // This was a known emit bug (cmove instead of cmovne)
    let insts = vec![
        MInst::LoadImm {
            dst: VReg(0),
            value: 1,
        }, // cond
        MInst::LoadImm {
            dst: VReg(1),
            value: 42,
        }, // true_val
        MInst::LoadImm {
            dst: VReg(2),
            value: 99,
        }, // false_val
        MInst::Select {
            dst: VReg(3),
            cond: VReg(0),
            true_val: VReg(1),
            false_val: VReg(2),
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: VReg(3),
            size: OpSize::S64,
        },
    ];
    let descs = vec![
        SpillDesc::remat(1),
        SpillDesc::remat(42),
        SpillDesc::remat(99),
        SpillDesc::transient(),
    ];
    let _asgn = run_and_verify(insts, descs);
}

#[test]
fn test_select_emit_with_dst_aliases_cond() {
    for (cond_value, expected) in [(0, 99), (1, 42)] {
        let func = select_store_function(cond_value);
        let mut assignment = AssignmentMap::default();
        assignment.set(VReg(0), PhysReg::RAX);
        assignment.set(VReg(1), PhysReg::RDX);
        assignment.set(VReg(2), PhysReg::RSI);
        assignment.set(VReg(3), PhysReg::RAX);
        assert_eq!(emit_and_run_store0(&func, &assignment), expected);
    }
}

#[test]
fn test_select_emit_with_dst_aliases_true_val() {
    for (cond_value, expected) in [(0, 99), (1, 42)] {
        let func = select_store_function(cond_value);
        let mut assignment = AssignmentMap::default();
        assignment.set(VReg(0), PhysReg::RAX);
        assignment.set(VReg(1), PhysReg::RDX);
        assignment.set(VReg(2), PhysReg::RSI);
        assignment.set(VReg(3), PhysReg::RDX);
        assert_eq!(emit_and_run_store0(&func, &assignment), expected);
    }
}

#[test]
fn test_select_emit_with_dst_aliases_false_val() {
    for (cond_value, expected) in [(0, 99), (1, 42)] {
        let func = select_store_function(cond_value);
        let mut assignment = AssignmentMap::default();
        assignment.set(VReg(0), PhysReg::RAX);
        assignment.set(VReg(1), PhysReg::RDX);
        assignment.set(VReg(2), PhysReg::RSI);
        assignment.set(VReg(3), PhysReg::RSI);
        assert_eq!(emit_and_run_store0(&func, &assignment), expected);
    }
}

#[test]
fn test_phi_dst_gets_register_under_entry_pressure() {
    let mut vregs = VRegAllocator::new();
    while vregs.count() < 15 {
        vregs.alloc();
    }

    let mut descs = Vec::new();
    for i in 0..15 {
        descs.push(if i == 13 {
            SpillDesc::transient()
        } else {
            SpillDesc::remat(i as u64)
        });
    }

    let mut func = MFunction::new(vregs, descs);

    let mut entry = MBlock::new(BlockId(0));
    for i in 0..13 {
        entry.push(MInst::LoadImm {
            dst: VReg(i),
            value: i as u64,
        });
    }
    entry.push(MInst::LoadImm {
        dst: VReg(14),
        value: 1,
    });
    entry.push(MInst::Branch {
        cond: VReg(14),
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });
    func.push_block(entry);

    let mut pass_through = MBlock::new(BlockId(1));
    pass_through.push(MInst::Jump { target: BlockId(2) });
    func.push_block(pass_through);

    let mut join = MBlock::new(BlockId(2));
    join.phis.push(PhiNode {
        dst: VReg(13),
        sources: vec![(BlockId(0), VReg(0)), (BlockId(1), VReg(1))],
    });
    let mut acc = VReg(13);
    for i in 0..13 {
        let dst = func.vregs.alloc();
        func.spill_descs.push(SpillDesc::transient());
        join.push(MInst::Add {
            dst,
            lhs: acc,
            rhs: VReg(i),
        });
        acc = dst;
    }
    join.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 0,
        src: acc,
        size: OpSize::S64,
    });
    join.push(MInst::Return);
    func.push_block(join);

    let result = run_regalloc(&mut func).unwrap();
    assert!(result.assignment.get(VReg(13)).is_some());
    let analysis = analysis::analyze(&func);
    super::verify_assignment(&func, &analysis, &result.assignment).unwrap();
}

#[test]
fn test_many_phi_edge_sources_are_materialized_without_pin_overflow() {
    const PHIS: u32 = 32;
    let mut vregs = VRegAllocator::new();
    let mut descs = Vec::new();
    let mut left_values = Vec::new();
    let mut right_values = Vec::new();
    let mut merged_values = Vec::new();
    for value in 0..PHIS {
        left_values.push(vregs.alloc());
        descs.push(SpillDesc::remat(value as u64 + 1));
    }
    for value in 0..PHIS {
        right_values.push(vregs.alloc());
        descs.push(SpillDesc::remat(value as u64 + 101));
    }
    for _ in 0..PHIS {
        merged_values.push(vregs.alloc());
        descs.push(SpillDesc::transient());
    }
    let condition = vregs.alloc();
    descs.push(SpillDesc::transient());
    let mut func = MFunction::new(vregs, descs);

    let mut entry = MBlock::new(BlockId(0));
    for (value, &destination) in left_values.iter().enumerate() {
        entry.push(MInst::LoadImm {
            dst: destination,
            value: value as u64 + 1,
        });
    }
    for (value, &destination) in right_values.iter().enumerate() {
        entry.push(MInst::LoadImm {
            dst: destination,
            value: value as u64 + 101,
        });
    }
    entry.push(MInst::Load {
        dst: condition,
        base: BaseReg::SimState,
        offset: 0,
        size: OpSize::S64,
    });
    entry.push(MInst::Branch {
        cond: condition,
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });
    func.push_block(entry);

    let mut left = MBlock::new(BlockId(1));
    left.push(MInst::Jump { target: BlockId(3) });
    func.push_block(left);
    let mut right = MBlock::new(BlockId(2));
    right.push(MInst::Jump { target: BlockId(3) });
    func.push_block(right);

    let mut join = MBlock::new(BlockId(3));
    for value in 0..PHIS as usize {
        join.phis.push(PhiNode {
            dst: merged_values[value],
            sources: vec![
                (BlockId(1), left_values[value]),
                (BlockId(2), right_values[value]),
            ],
        });
    }
    let mut sum = merged_values[0];
    for &value in &merged_values[1..] {
        let destination = func.vregs.alloc();
        func.spill_descs.push(SpillDesc::transient());
        join.push(MInst::Add {
            dst: destination,
            lhs: sum,
            rhs: value,
        });
        sum = destination;
    }
    join.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 8,
        src: sum,
        size: OpSize::S64,
    });
    join.push(MInst::Return);
    func.push_block(join);

    let result = run_regalloc(&mut func).unwrap();
    assert_eq!(func.verify_result(), Ok(()));
    let analysis = analysis::analyze(&func);
    super::verify_assignment(&func, &analysis, &result.assignment).unwrap();
    let emitted = emit::emit(&func, &result.assignment, result.spill_frame_size).unwrap();
    let jit = jit_mem::JitCode::new(&emitted.code).unwrap();

    let mut state = vec![0u8; 16];
    state[..8].copy_from_slice(&1u64.to_le_bytes());
    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    assert_eq!(u64::from_le_bytes(state[8..].try_into().unwrap()), 528);

    state[..8].copy_from_slice(&0u64.to_le_bytes());
    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    assert_eq!(u64::from_le_bytes(state[8..].try_into().unwrap()), 3_728);
}
