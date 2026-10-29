use super::analysis;
use super::assignment::*;
use super::*;
use crate::native::mir::*;
use crate::native::{emit, jit_mem};

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
fn allocation_boundary_removes_single_use_compare_results() {
    let mut vregs = VRegAllocator::new();
    let lhs = vregs.alloc();
    let rhs = vregs.alloc();
    let condition = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);

    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm { dst: lhs, value: 1 });
    entry.push(MInst::LoadImm { dst: rhs, value: 2 });
    entry.push(MInst::Cmp {
        dst: condition,
        lhs,
        rhs,
        kind: CmpKind::LtU,
    });
    entry.push(MInst::Branch {
        cond: condition,
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });
    let mut true_block = MBlock::new(BlockId(1));
    true_block.push(MInst::Return);
    let mut false_block = MBlock::new(BlockId(2));
    false_block.push(MInst::Return);
    func.blocks = vec![entry, true_block, false_block];

    run_regalloc(&mut func).unwrap();

    assert!(
        !func
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .any(|instruction| instruction.def() == Some(condition))
    );
    assert!(matches!(
        func.blocks[0].terminator(),
        Some(MInst::BranchPred {
            predicate: BranchPredicate::Compare {
                kind: CmpKind::LtU,
                ..
            },
            ..
        })
    ));
}

#[test]
fn trace_captures_regalloc_owned_memory_folds_before_scheduling() {
    let mut vregs = VRegAllocator::new();
    let loaded = vregs.alloc();
    let masked = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: loaded,
        base: BaseReg::SimState,
        offset: 37,
        size: OpSize::S8,
    });
    block.push(MInst::AndImm {
        dst: masked,
        src: loaded,
        imm: 0xfc,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 37,
        src: masked,
        size: OpSize::S8,
    });
    block.push(MInst::Return);
    func.push_block(block);
    let mut trace = RegallocTrace::default();

    run_regalloc_with_label_and_trace(&mut func, "late-fold-trace-test", Some(&mut trace)).unwrap();

    assert!(trace.mir_after_late_memory_folds.contains("and_store.i8"));
    assert!(!trace.mir_after_late_memory_folds.contains("load.i8"));
    assert!(!trace.mir_after_late_memory_folds.is_empty());
    assert!(!trace.mir_after_scheduling.is_empty());
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
    super::verify_assignment(&func, &result.assignment).unwrap();

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
        tracing::debug!("  [{ii:3}] {inst}  => {r:?}");
    }

    // Verify
    super::verify_assignment(&func, &assignment).unwrap();
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
    super::verify_assignment(&func, &result.assignment).unwrap();
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
    super::verify_assignment(&func, &result.assignment).unwrap();
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

#[test]
fn cancelled_allocation_reports_a_cancellation_error() {
    let mut func = MFunction::new(VRegAllocator::new(), Vec::new());
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::Return);
    func.push_block(entry);

    let error = match super::run_regalloc_for_codegen(
        &mut func,
        "cancelled_test",
        None,
        &crate::NativeDiagnostics::default(),
        false,
        super::RegallocStrategy::Optimizing,
        || true,
    ) {
        Ok(_) => panic!("a cancelled allocation must not return a result"),
        Err(error) => error,
    };

    assert!(super::is_cancellation(&error));
    assert_eq!(error.phase, "compilation cancelled");
}

#[test]
fn uncancelled_allocation_never_reports_cancellation() {
    let mut func = MFunction::new(VRegAllocator::new(), Vec::new());

    // An empty function is rejected for CFG reasons under verification, but
    // that rejection must not be classified as cancellation.
    let error = match super::run_regalloc_with_label_and_trace_and_diagnostics(
        &mut func,
        "uncancelled_test",
        None,
        &crate::NativeDiagnostics {
            verify_regalloc: true,
            ..crate::NativeDiagnostics::default()
        },
        false,
        || false,
    ) {
        Ok(_) => panic!("empty MIR is rejected"),
        Err(error) => error,
    };

    assert!(!super::is_cancellation(&error));
}

/// Run the optimizing allocator on `func` and require the interpreter's
/// effects from its machine code.
fn assert_optimizing_matches_reference(func: &MFunction, inputs: &[Vec<u8>], context: &str) {
    use super::test_programs::{allocate, execute, interpret};
    let (allocated, result) = allocate(func, RegallocStrategy::Optimizing);
    for input in inputs {
        assert_eq!(
            execute(&allocated, &result, input),
            (0, interpret(func, input)),
            "optimizing result differs ({context})\ninput MIR:\n{func}\nallocated MIR:\n{allocated}"
        );
    }
}

/// `v18` reads the header phi `v20` on the backedge while `v20`'s own row
/// writes its next value. With `v20` stack-resident, the edge stored the
/// next `v20` into its home before reloading the current one for `v18`.
/// Minimized from random program 32 of an earlier generator; the dead
/// definitions supply the register pressure that spills `v20`.
const BACKEDGE_PHI_READ_AFTER_WRITE: &str = "
    bb0:
      v0 = load.i64 [sim + 0]
      v2 = load.i64 [sim + 16]
      v3 = load.i64 [sim + 24]
      v4 = imm 0x1
      v6 = load.i64 [sim + 48]
      v7 = load.i64 [sim + 56]
      v8 = imm 0x0
      jmp bb1
    bb1:
      v9 = phi(bb0: v8, bb12: v242)
      v10 = phi(bb0: v6, bb12: v67)
      jmp bb2
    bb2:
      v11 = or v7, 0x1
      v12 = udiv v6, v11
      v13 = add v12, 4660
      v14 = mul.w64 v10, v0
      v16 = imm 0x0
      jmp bb4
    bb4:
      v17 = phi(bb2: v16, bb9: v68)
      v18 = phi(bb2: v14, bb9: v20)
      v19 = phi(bb2: v6, bb9: v29)
      v20 = phi(bb2: v3, bb9: v30)
      v21 = phi(bb2: v13, bb9: v30)
      jmp bb5
    bb5:
      v24 = mov.w64 v6
      v26 = mov.w64 v17
      v27 = and.w64 v7, v3
      v28 = imm 0xbd7068da3e930e65
      v29 = imm 0x1
      v30 = add.w64 v17, v26
      v34 = add v19, 4660
      v36 = cmp.LtU v10, v0
      v37 = select v36, v10, v18
      v40 = xor.w64 v34, v9
      v46 = and.w64 v29, v13
      v47 = and.w64 v2, 0x1
      br v47, bb7, bb8
    bb7:
      v51 = imm 0x1
      v52 = select v51, v4, v14
      jmp bb9
    bb8:
      jmp bb9
    bb9:
      v65 = phi(bb7: v21, bb8: v21)
      v67 = phi(bb7: v28, bb8: v28)
      v68 = add v17, 1
      v69 = cmp.LtU v68, 4
      br v69, bb4, bb6
    bb6:
      v70 = and.w64 v21, 0x1
      br v70, bb10, bb11
    bb10:
      v71 = and.w64 v3, 0x1
      br v71, bb13, bb14
    bb13:
      jmp bb15
    bb14:
      jmp bb15
    bb15:
      v152 = and.w64 v37, 0x1
      br v152, bb16, bb17
    bb16:
      jmp bb18
    bb17:
      jmp bb18
    bb18:
      jmp bb12
    bb11:
      jmp bb12
    bb12:
      v242 = add v9, 1
      v243 = cmp.LtU v242, 1
      br v243, bb1, bb3
    bb3:
      store.i64 [sim + 80], v37
      ret
";

/// A three-phi rotation (`v37 <- v48`, `v39 <- v40`, `v40 <- v37`) on an
/// inner-loop backedge, minimized from random program 16.
const BACKEDGE_PHI_ROTATION: &str = "
    bb0:
      v0 = imm 0x1
      v1 = imm 0x1
      v2 = imm 0x1
      v3 = imm 0x1
      v5 = imm 0x1
      v8 = imm 0x0
      jmp bb1
    bb1:
      v9 = phi(bb0: v8, bb6: v82)
      jmp bb2
    bb2:
      v12 = imm 0x1
      v13 = imm 0x1
      v14 = imm 0x1
      v15 = imm 0x1
      v18 = imm 0x1
      v20 = imm 0x1
      v24 = imm 0x1
      v25 = not v24
      v26 = imm 0x0
      jmp bb4
    bb4:
      v27 = phi(bb2: v26, bb9: v80)
      jmp bb5
    bb5:
      v34 = imm 0x1
      v35 = imm 0x0
      jmp bb7
    bb7:
      v36 = phi(bb5: v35, bb8: v78)
      v37 = phi(bb5: v3, bb8: v48)
      v39 = phi(bb5: v0, bb8: v40)
      v40 = phi(bb5: v15, bb8: v37)
      jmp bb8
    bb8:
      v42 = sub.w64 v25, v37
      v48 = mov.w64 v42
      v51 = or.w64 v39, v1
      v58 = imm 0x1
      v59 = sar v5, v58
      v60 = mov.w64 v14
      v61 = or.w64 v48, v13
      v67 = imm 0x1
      v68 = shr v34, v67
      v69 = and.w64 v2, 0x3f
      v71 = mov.w64 v12
      v72 = and.w64 v20, 0x3f
      v74 = add v18, 4660
      v75 = shr v27, 7
      v78 = add v36, 1
      v79 = cmp.LtU v78, 4
      br v79, bb7, bb9
    bb9:
      v80 = add v27, 1
      v81 = cmp.LtU v80, 2
      br v81, bb4, bb6
    bb6:
      v82 = add v9, 1
      v83 = cmp.LtU v82, 3
      br v83, bb1, bb3
    bb3:
      store.i64 [sim + 88], v51
      ret
";

fn assert_fixed_program(text: &str, seed: u64, context: &str) {
    use super::test_programs::{parse_mir, random_inputs};
    use crate::native::features::X86Features;
    for bmi2 in [true, false] {
        let func = parse_mir(text, X86Features::for_test(bmi2));
        assert_optimizing_matches_reference(&func, &random_inputs(seed), context);
    }
}

#[test]
fn optimizing_backedge_phi_reads_the_current_value() {
    assert_fixed_program(BACKEDGE_PHI_READ_AFTER_WRITE, 32, "read after write");
}

#[test]
fn optimizing_backedge_phi_rotation_is_parallel() {
    assert_fixed_program(BACKEDGE_PHI_ROTATION, 16, "rotation");
}

#[test]
fn random_programs_match_reference_semantics_with_the_optimizing_allocator() {
    use super::test_programs::{random_function, random_inputs};
    use crate::native::features::X86Features;
    for seed in 0..120 {
        for bmi2 in [true, false] {
            let func = random_function(seed, X86Features::for_test(bmi2));
            assert_optimizing_matches_reference(
                &func,
                &random_inputs(seed),
                &format!("seed {seed}, bmi2 {bmi2}"),
            );
        }
    }
}
