use super::analysis;
use super::assignment::*;
use super::*;
use crate::backend::native::mir::*;

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

    let result = run_regalloc(&mut func);

    // Re-verify on final instructions
    let analysis = analysis::analyze(&func);
    super::verify_assignment(&func, &analysis, &result.assignment);

    result.assignment
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
        let dst = VReg(14 + i as u32);
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
    super::verify_assignment(&func, &analysis, &assignment);
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
