use super::*;
use crate::mir::{BlockId, MBlock};

fn function(instructions: Vec<MInst>) -> MFunction {
    let mut block = MBlock::new(BlockId(0));
    block.insts = instructions;
    MFunction::new(vec![block], Vec::new())
}

#[test]
fn folds_single_use_compare_into_flag_branch() {
    let mut function = function(vec![
        MInst::Cmp {
            dst: VReg(2),
            lhs: VReg(0),
            rhs: VReg(1),
            kind: CmpKind::LtU,
        },
        MInst::Branch {
            cond: VReg(2),
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        },
    ]);

    optimize(&mut function);

    assert_eq!(
        function.blocks[0].insts,
        vec![MInst::BranchPred {
            predicate: BranchPredicate::Compare {
                lhs: VReg(0),
                rhs: VReg(1),
                kind: CmpKind::LtU,
            },
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        }]
    );
}

#[test]
fn folds_single_use_load_into_memory_branch() {
    let mut function = function(vec![
        MInst::Load {
            dst: VReg(0),
            base: crate::mir::BaseReg::SimState,
            offset: 24,
            size: crate::mir::OpSize::S32,
        },
        MInst::Branch {
            cond: VReg(0),
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        },
    ]);

    optimize(&mut function);

    assert!(matches!(
        function.blocks[0].insts.as_slice(),
        [MInst::BranchPred {
            predicate: BranchPredicate::MemoryNonZero {
                base: crate::mir::BaseReg::SimState,
                offset: 24,
                size: crate::mir::OpSize::S32,
            },
            ..
        }]
    ));
}

#[test]
fn keeps_compare_result_with_multiple_uses() {
    let original = vec![
        MInst::CmpImm {
            dst: VReg(1),
            lhs: VReg(0),
            imm: 0,
            kind: CmpKind::Ne,
        },
        MInst::Store {
            base: crate::mir::BaseReg::SimState,
            offset: 0,
            src: VReg(1),
            size: crate::mir::OpSize::S8,
        },
        MInst::Branch {
            cond: VReg(1),
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        },
    ];
    let mut function = function(original.clone());

    optimize(&mut function);

    assert_eq!(function.blocks[0].insts, original);
}

#[test]
fn folds_single_use_constants_into_machine_operations() {
    let mut function = function(vec![
        MInst::LoadImm {
            dst: VReg(0),
            value: 32,
        },
        MInst::Mul {
            dst: VReg(1),
            lhs: VReg(2),
            rhs: VReg(0),
        },
        MInst::LoadImm {
            dst: VReg(3),
            value: 64,
        },
        MInst::Cmp {
            dst: VReg(4),
            lhs: VReg(1),
            rhs: VReg(3),
            kind: CmpKind::Eq,
        },
        MInst::Store {
            base: crate::mir::BaseReg::SimState,
            offset: 0,
            src: VReg(4),
            size: crate::mir::OpSize::S8,
        },
        MInst::Return,
    ]);

    optimize(&mut function);

    assert!(matches!(
        function.blocks[0].insts[0],
        MInst::ShlImm {
            dst: VReg(1),
            src: VReg(2),
            imm: 5
        }
    ));
    assert!(matches!(
        function.blocks[0].insts[1],
        MInst::CmpImm {
            dst: VReg(4),
            lhs: VReg(1),
            imm: 64,
            kind: CmpKind::Eq
        }
    ));
}

#[test]
fn removes_identity_machine_operations() {
    let mut function = function(vec![
        MInst::LoadImm {
            dst: VReg(0),
            value: 0,
        },
        MInst::Shr {
            dst: VReg(2),
            lhs: VReg(1),
            rhs: VReg(0),
        },
        MInst::OrImm {
            dst: VReg(3),
            src: VReg(2),
            imm: 0,
        },
        MInst::AddImm {
            dst: VReg(4),
            src: VReg(3),
            imm: 0,
        },
        MInst::AndImm {
            dst: VReg(5),
            src: VReg(4),
            imm: u64::MAX,
        },
        MInst::Store {
            base: crate::mir::BaseReg::SimState,
            offset: 0,
            src: VReg(5),
            size: crate::mir::OpSize::S64,
        },
        MInst::AndImm32 {
            dst: VReg(6),
            src: VReg(7),
            imm: u32::MAX,
        },
        MInst::Store {
            base: crate::mir::BaseReg::SimState,
            offset: 8,
            src: VReg(6),
            size: crate::mir::OpSize::S64,
        },
        MInst::Return,
    ]);

    optimize(&mut function);

    assert_eq!(
        function.blocks[0].insts,
        vec![
            MInst::Store {
                base: crate::mir::BaseReg::SimState,
                offset: 0,
                src: VReg(1),
                size: crate::mir::OpSize::S64,
            },
            MInst::Mov32 {
                dst: VReg(6),
                src: VReg(7),
            },
            MInst::Store {
                base: crate::mir::BaseReg::SimState,
                offset: 8,
                src: VReg(6),
                size: crate::mir::OpSize::S64,
            },
            MInst::Return,
        ]
    );
}

#[test]
fn fuses_single_use_compare_and_select() {
    let mut function = function(vec![
        MInst::LoadImm {
            dst: VReg(0),
            value: 7,
        },
        MInst::Cmp {
            dst: VReg(1),
            lhs: VReg(2),
            rhs: VReg(0),
            kind: CmpKind::LtU,
        },
        MInst::Select {
            dst: VReg(3),
            cond: VReg(1),
            true_val: VReg(4),
            false_val: VReg(5),
        },
        MInst::Store {
            base: crate::mir::BaseReg::SimState,
            offset: 0,
            src: VReg(3),
            size: crate::mir::OpSize::S64,
        },
        MInst::Return,
    ]);

    optimize(&mut function);

    assert!(matches!(
        function.blocks[0].insts[0],
        MInst::CmpImmSelect {
            dst: VReg(3),
            lhs: VReg(2),
            imm: 7,
            kind: CmpKind::LtU,
            true_val: VReg(4),
            false_val: VReg(5)
        }
    ));
}

#[test]
fn propagates_mov32_only_when_the_source_is_already_zero_extended() {
    let mut function = function(vec![
        MInst::Load {
            dst: VReg(0),
            base: crate::mir::BaseReg::SimState,
            offset: 0,
            size: crate::mir::OpSize::S32,
        },
        MInst::Mov32 {
            dst: VReg(1),
            src: VReg(0),
        },
        MInst::Load {
            dst: VReg(2),
            base: crate::mir::BaseReg::SimState,
            offset: 8,
            size: crate::mir::OpSize::S64,
        },
        MInst::Mov32 {
            dst: VReg(3),
            src: VReg(2),
        },
        MInst::Store {
            base: crate::mir::BaseReg::SimState,
            offset: 16,
            src: VReg(1),
            size: crate::mir::OpSize::S64,
        },
        MInst::Store {
            base: crate::mir::BaseReg::SimState,
            offset: 24,
            src: VReg(3),
            size: crate::mir::OpSize::S64,
        },
        MInst::Return,
    ]);

    optimize(&mut function);

    assert!(
        !function.blocks[0]
            .insts
            .iter()
            .any(|instruction| matches!(instruction, MInst::Mov32 { dst: VReg(1), .. }))
    );
    assert!(
        function.blocks[0]
            .insts
            .iter()
            .any(|instruction| matches!(instruction, MInst::Mov32 { dst: VReg(3), .. }))
    );
    assert!(function.blocks[0].insts.iter().any(|instruction| matches!(
        instruction,
        MInst::Store {
            src: VReg(0),
            offset: 16,
            ..
        }
    )));
}

#[test]
fn preserves_mov32_after_unchecked_bsr() {
    let mut function = function(vec![
        MInst::Bsr {
            dst: VReg(1),
            src: VReg(0),
        },
        MInst::Mov32 {
            dst: VReg(2),
            src: VReg(1),
        },
        MInst::Store {
            base: crate::mir::BaseReg::SimState,
            offset: 0,
            src: VReg(2),
            size: crate::mir::OpSize::S64,
        },
        MInst::Return,
    ]);

    optimize(&mut function);

    assert!(function.blocks[0].insts.iter().any(|instruction| matches!(
        instruction,
        MInst::Mov32 {
            dst: VReg(2),
            src: VReg(1)
        }
    )));
}

#[test]
fn reuses_nearby_common_expressions() {
    let mut function = function(vec![
        MInst::ShlImm {
            dst: VReg(1),
            src: VReg(0),
            imm: 5,
        },
        MInst::AddImm {
            dst: VReg(2),
            src: VReg(1),
            imm: 12,
        },
        MInst::ShlImm {
            dst: VReg(3),
            src: VReg(0),
            imm: 5,
        },
        MInst::AddImm {
            dst: VReg(4),
            src: VReg(3),
            imm: 14,
        },
        MInst::Store {
            base: crate::mir::BaseReg::SimState,
            offset: 0,
            src: VReg(2),
            size: crate::mir::OpSize::S64,
        },
        MInst::Store {
            base: crate::mir::BaseReg::SimState,
            offset: 8,
            src: VReg(4),
            size: crate::mir::OpSize::S64,
        },
        MInst::Return,
    ]);

    optimize(&mut function);

    assert_eq!(
        function.blocks[0]
            .insts
            .iter()
            .filter(|instruction| matches!(instruction, MInst::ShlImm { .. }))
            .count(),
        1
    );
    assert!(function.blocks[0].insts.iter().any(|instruction| matches!(
        instruction,
        MInst::AddImm {
            src: VReg(1),
            imm: 14,
            ..
        }
    )));
}

#[test]
fn removes_only_proven_redundant_low_masks() {
    let mut function = function(vec![
        MInst::Load {
            dst: VReg(0),
            base: crate::mir::BaseReg::SimState,
            offset: 0,
            size: crate::mir::OpSize::S8,
        },
        MInst::AndImm32 {
            dst: VReg(1),
            src: VReg(0),
            imm: 0xff,
        },
        MInst::AndImm32 {
            dst: VReg(2),
            src: VReg(0),
            imm: 0x7f,
        },
        MInst::Store {
            base: crate::mir::BaseReg::SimState,
            offset: 8,
            src: VReg(1),
            size: crate::mir::OpSize::S64,
        },
        MInst::Store {
            base: crate::mir::BaseReg::SimState,
            offset: 16,
            src: VReg(2),
            size: crate::mir::OpSize::S64,
        },
        MInst::Return,
    ]);

    optimize(&mut function);

    assert!(function.blocks[0].insts.iter().any(|instruction| matches!(
        instruction,
        MInst::Store {
            src: VReg(0),
            offset: 8,
            ..
        }
    )));
    assert!(function.blocks[0].insts.iter().any(|instruction| matches!(
        instruction,
        MInst::AndImm32 {
            dst: VReg(2),
            src: VReg(0),
            imm: 0x7f
        }
    )));
}
