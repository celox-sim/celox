use super::*;

#[test]
fn folds_add_tree_of_bit_extracts_to_popcnt() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 8,
                size: OpSize::S64,
            },
            MInst::ShrImm {
                dst: VReg(1),
                src: VReg(0),
                imm: 0,
            },
            MInst::AndImm {
                dst: VReg(2),
                src: VReg(1),
                imm: 1,
            },
            MInst::ShrImm {
                dst: VReg(3),
                src: VReg(0),
                imm: 1,
            },
            MInst::AndImm {
                dst: VReg(4),
                src: VReg(3),
                imm: 1,
            },
            MInst::ShrImm {
                dst: VReg(5),
                src: VReg(0),
                imm: 2,
            },
            MInst::AndImm {
                dst: VReg(6),
                src: VReg(5),
                imm: 1,
            },
            MInst::Add {
                dst: VReg(7),
                lhs: VReg(2),
                rhs: VReg(4),
            },
            MInst::Add {
                dst: VReg(8),
                lhs: VReg(7),
                rhs: VReg(6),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: VReg(8),
                size: OpSize::S8,
            },
            MInst::Return,
        ],
        9,
    );

    optimize(&mut func);

    let insts = &func.blocks[0].insts;
    assert!(
        insts.iter().any(|inst| matches!(
            inst,
            MInst::Popcnt {
                dst: VReg(8),
                src: _
            }
        )),
        "{insts:#?}"
    );
}

#[test]
fn folds_chunk_deposit_chain_to_pdep() {
    if !crate::native::features::X86Features::detect().bmi2() {
        return;
    }

    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 8,
                size: OpSize::S64,
            },
            MInst::AndImm {
                dst: VReg(1),
                src: VReg(0),
                imm: 0xf,
            },
            MInst::ShlImm {
                dst: VReg(2),
                src: VReg(1),
                imm: 2,
            },
            MInst::ShrImm {
                dst: VReg(3),
                src: VReg(0),
                imm: 4,
            },
            MInst::AndImm {
                dst: VReg(4),
                src: VReg(3),
                imm: 0xf,
            },
            MInst::ShlImm {
                dst: VReg(5),
                src: VReg(4),
                imm: 8,
            },
            MInst::Or {
                dst: VReg(6),
                lhs: VReg(2),
                rhs: VReg(5),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: VReg(6),
                size: OpSize::S16,
            },
            MInst::Return,
        ],
        7,
    );

    optimize(&mut func);

    let insts = &func.blocks[0].insts;
    assert!(
        insts.iter().any(|inst| matches!(
            inst,
            MInst::Pdep {
                dst: VReg(6),
                src: VReg(0),
                ..
            }
        )),
        "{insts:#?}"
    );
}

#[test]
fn folds_exact_byte_enable_spread_to_pdep() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 8,
                size: OpSize::S8,
            },
            MInst::ShlImm {
                dst: VReg(1),
                src: VReg(0),
                imm: 28,
            },
            MInst::Or {
                dst: VReg(2),
                lhs: VReg(0),
                rhs: VReg(1),
            },
            MInst::LoadImm {
                dst: VReg(3),
                value: 0x0000_000f_0000_000f,
            },
            MInst::And {
                dst: VReg(4),
                lhs: VReg(2),
                rhs: VReg(3),
            },
            MInst::ShlImm {
                dst: VReg(5),
                src: VReg(4),
                imm: 14,
            },
            MInst::Or {
                dst: VReg(6),
                lhs: VReg(4),
                rhs: VReg(5),
            },
            MInst::LoadImm {
                dst: VReg(7),
                value: 0x0003_0003_0003_0003,
            },
            MInst::And {
                dst: VReg(8),
                lhs: VReg(6),
                rhs: VReg(7),
            },
            MInst::ShlImm {
                dst: VReg(9),
                src: VReg(8),
                imm: 7,
            },
            MInst::Or {
                dst: VReg(10),
                lhs: VReg(8),
                rhs: VReg(9),
            },
            MInst::LoadImm {
                dst: VReg(11),
                value: 0x0101_0101_0101_0101,
            },
            MInst::And {
                dst: VReg(12),
                lhs: VReg(10),
                rhs: VReg(11),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: VReg(12),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        13,
    );

    fold_byte_enable_spread_to_pdep(&mut func);

    assert!(matches!(
        func.blocks[0].insts[12],
        MInst::Pdep {
            dst: VReg(12),
            src: VReg(0),
            mask: VReg(11),
        }
    ));
}

#[test]
fn folds_chunk_extract_chain_to_pext() {
    if !crate::native::features::X86Features::detect().bmi2() {
        return;
    }

    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 8,
                size: OpSize::S64,
            },
            MInst::ShrImm {
                dst: VReg(1),
                src: VReg(0),
                imm: 2,
            },
            MInst::AndImm {
                dst: VReg(2),
                src: VReg(1),
                imm: 0xf,
            },
            MInst::ShrImm {
                dst: VReg(3),
                src: VReg(0),
                imm: 8,
            },
            MInst::AndImm {
                dst: VReg(4),
                src: VReg(3),
                imm: 0xf,
            },
            MInst::ShlImm {
                dst: VReg(5),
                src: VReg(4),
                imm: 4,
            },
            MInst::Or {
                dst: VReg(6),
                lhs: VReg(2),
                rhs: VReg(5),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: VReg(6),
                size: OpSize::S8,
            },
            MInst::Return,
        ],
        7,
    );

    optimize(&mut func);

    let insts = &func.blocks[0].insts;
    assert!(
        insts.iter().any(|inst| matches!(
            inst,
            MInst::Pext {
                dst: VReg(6),
                src: VReg(0),
                ..
            }
        )),
        "{insts:#?}"
    );
}

#[test]
fn folds_dynamic_bit_toggle_insert_to_xor() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 8,
                size: OpSize::S64,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 1,
            },
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S8,
            },
            MInst::Shl {
                dst: VReg(3),
                lhs: VReg(1),
                rhs: VReg(2),
            },
            MInst::BitNot {
                dst: VReg(4),
                src: VReg(3),
            },
            MInst::And {
                dst: VReg(5),
                lhs: VReg(0),
                rhs: VReg(4),
            },
            MInst::Shr {
                dst: VReg(6),
                lhs: VReg(0),
                rhs: VReg(2),
            },
            MInst::And {
                dst: VReg(7),
                lhs: VReg(6),
                rhs: VReg(1),
            },
            MInst::Xor {
                dst: VReg(8),
                lhs: VReg(7),
                rhs: VReg(1),
            },
            MInst::Shl {
                dst: VReg(9),
                lhs: VReg(8),
                rhs: VReg(2),
            },
            MInst::Or {
                dst: VReg(10),
                lhs: VReg(5),
                rhs: VReg(9),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: VReg(10),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        11,
    );

    optimize(&mut func);

    let insts = &func.blocks[0].insts;
    assert!(
        insts.iter().any(|inst| matches!(
            inst,
            MInst::Xor {
                dst: VReg(10),
                lhs: VReg(0),
                rhs: VReg(3),
            } | MInst::Xor {
                dst: VReg(10),
                lhs: VReg(3),
                rhs: VReg(0),
            }
        )),
        "{insts:#?}"
    );
}

#[test]
fn does_not_fold_add_tree_with_duplicate_bit() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 8,
                size: OpSize::S64,
            },
            MInst::ShrImm {
                dst: VReg(1),
                src: VReg(0),
                imm: 0,
            },
            MInst::AndImm {
                dst: VReg(2),
                src: VReg(1),
                imm: 1,
            },
            MInst::ShrImm {
                dst: VReg(3),
                src: VReg(0),
                imm: 0,
            },
            MInst::AndImm {
                dst: VReg(4),
                src: VReg(3),
                imm: 1,
            },
            MInst::ShrImm {
                dst: VReg(5),
                src: VReg(0),
                imm: 2,
            },
            MInst::AndImm {
                dst: VReg(6),
                src: VReg(5),
                imm: 1,
            },
            MInst::Add {
                dst: VReg(7),
                lhs: VReg(2),
                rhs: VReg(4),
            },
            MInst::Add {
                dst: VReg(8),
                lhs: VReg(7),
                rhs: VReg(6),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: VReg(8),
                size: OpSize::S8,
            },
            MInst::Return,
        ],
        9,
    );

    optimize(&mut func);

    let insts = &func.blocks[0].insts;
    assert!(!insts.iter().any(|inst| matches!(
        inst,
        MInst::Popcnt {
            dst: VReg(8),
            src: _
        }
    )));
}
