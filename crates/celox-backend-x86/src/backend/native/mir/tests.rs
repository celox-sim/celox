use super::*;

#[test]
fn uses_keeps_all_machine_operands_inline() {
    let values = (0..MAX_USES as u32).map(VReg).collect::<Vec<_>>();
    let uses = Uses::from_slice(&values);
    assert_eq!(uses.as_slice(), values);
    assert_eq!(uses.into_iter().next_back(), Some(VReg(4)));
}

#[test]
fn uses_does_not_inflate_the_previous_inline_representation() {
    #[allow(dead_code)]
    struct PreviousUses {
        buf: [VReg; 16],
        len: u8,
    }

    assert!(
        std::mem::size_of::<Uses>() <= std::mem::size_of::<PreviousUses>(),
        "Uses grew from {} to {} bytes",
        std::mem::size_of::<PreviousUses>(),
        std::mem::size_of::<Uses>(),
    );
}

#[test]
fn memory_alias_range_must_be_nonempty() {
    assert_eq!(MemoryAliasRange::new(8, 0), None);
    assert_eq!(MemoryAliasRange::new(8, 1).unwrap().end(), 9);
}

#[test]
fn try_alloc_reports_exhaustion_without_changing_state() {
    let mut allocator = VRegAllocator::new();
    allocator.set_next_for_test(u32::MAX);

    assert_eq!(allocator.try_alloc(), Err(VRegAllocError));
    assert_eq!(allocator.count(), u32::MAX);
}

struct UseCase {
    name: &'static str,
    inst: MInst,
    expected: Vec<VReg>,
}

fn vreg(index: u32) -> VReg {
    VReg(index)
}

fn use_cases() -> Vec<UseCase> {
    let dst = vreg(100);
    let a = vreg(1);
    let b = vreg(2);
    let c = vreg(3);
    let d = vreg(4);
    let e = vreg(5);
    let block_a = BlockId(1);
    let block_b = BlockId(2);

    vec![
        UseCase {
            name: "Mov",
            inst: MInst::Mov { dst, src: a },
            expected: vec![a],
        },
        UseCase {
            name: "LoadImm",
            inst: MInst::LoadImm { dst, value: 42 },
            expected: vec![],
        },
        UseCase {
            name: "Scratch",
            inst: MInst::Scratch { dst },
            expected: vec![],
        },
        UseCase {
            name: "LoadConstantTableAddr",
            inst: MInst::LoadConstantTableAddr {
                dst,
                table: ConstantTableId(0),
            },
            expected: vec![],
        },
        UseCase {
            name: "Load",
            inst: MInst::Load {
                dst,
                base: BaseReg::SimState,
                offset: 8,
                size: OpSize::S64,
            },
            expected: vec![],
        },
        UseCase {
            name: "Store",
            inst: MInst::Store {
                base: BaseReg::SimState,
                offset: 8,
                src: a,
                size: OpSize::S64,
            },
            expected: vec![a],
        },
        UseCase {
            name: "LoadPtr",
            inst: MInst::LoadPtr {
                dst,
                ptr: a,
                offset: 8,
                size: OpSize::S64,
            },
            expected: vec![a],
        },
        UseCase {
            name: "StorePtr",
            inst: MInst::StorePtr {
                ptr: a,
                offset: 8,
                src: b,
                size: OpSize::S64,
            },
            expected: vec![a, b],
        },
        UseCase {
            name: "ReleaseStorePtr",
            inst: MInst::ReleaseStorePtr {
                ptr: a,
                offset: 8,
                src: b,
                size: OpSize::S64,
            },
            expected: vec![a, b],
        },
        UseCase {
            name: "LoadIndexed",
            inst: MInst::LoadIndexed {
                dst,
                base: BaseReg::SimState,
                offset: 8,
                index: a,
                scale: 1,
                size: OpSize::S64,
                alias_range: None,
            },
            expected: vec![a],
        },
        UseCase {
            name: "StoreIndexed",
            inst: MInst::StoreIndexed {
                base: BaseReg::SimState,
                offset: 8,
                index: a,
                src: b,
                size: OpSize::S64,
                alias_range: None,
            },
            expected: vec![a, b],
        },
        UseCase {
            name: "OrStoreIndexed",
            inst: MInst::OrStoreIndexed {
                base: BaseReg::SimState,
                offset: 8,
                index: a,
                src: b,
                size: OpSize::S64,
                alias_range: None,
            },
            expected: vec![a, b],
        },
        UseCase {
            name: "LoadPtrIndexed",
            inst: MInst::LoadPtrIndexed {
                dst,
                ptr: a,
                offset: 8,
                index: b,
                size: OpSize::S64,
            },
            expected: vec![a, b],
        },
        UseCase {
            name: "StorePtrIndexed",
            inst: MInst::StorePtrIndexed {
                ptr: a,
                offset: 8,
                index: b,
                src: c,
                size: OpSize::S64,
            },
            expected: vec![a, b, c],
        },
        UseCase {
            name: "ReleaseStorePtrIndexed",
            inst: MInst::ReleaseStorePtrIndexed {
                ptr: a,
                offset: 8,
                index: b,
                src: c,
                size: OpSize::S64,
            },
            expected: vec![a, b, c],
        },
        UseCase {
            name: "MemCopy",
            inst: MInst::MemCopy {
                src_offset: 0,
                dst_offset: 8,
                byte_len: 16,
            },
            expected: vec![],
        },
        UseCase {
            name: "MemFill",
            inst: MInst::MemFill {
                dst_offset: 0,
                byte_len: 16,
                value: 0x5a,
            },
            expected: vec![],
        },
        UseCase {
            name: "SparseMarkActive",
            inst: MInst::SparseMarkActive {
                active_index: 0,
                active_bits_offset: 8,
                active_capacity: 1,
            },
            expected: vec![],
        },
        UseCase {
            name: "Add",
            inst: MInst::Add {
                dst,
                lhs: a,
                rhs: b,
            },
            expected: vec![a, b],
        },
        UseCase {
            name: "Sub",
            inst: MInst::Sub {
                dst,
                lhs: a,
                rhs: b,
            },
            expected: vec![a, b],
        },
        UseCase {
            name: "Mul",
            inst: MInst::Mul {
                dst,
                lhs: a,
                rhs: b,
            },
            expected: vec![a, b],
        },
        UseCase {
            name: "UMulHi",
            inst: MInst::UMulHi {
                dst,
                lhs: a,
                rhs: b,
            },
            expected: vec![a, b],
        },
        UseCase {
            name: "And",
            inst: MInst::And {
                dst,
                lhs: a,
                rhs: b,
            },
            expected: vec![a, b],
        },
        UseCase {
            name: "Or",
            inst: MInst::Or {
                dst,
                lhs: a,
                rhs: b,
            },
            expected: vec![a, b],
        },
        UseCase {
            name: "Xor",
            inst: MInst::Xor {
                dst,
                lhs: a,
                rhs: b,
            },
            expected: vec![a, b],
        },
        UseCase {
            name: "Shr",
            inst: MInst::Shr {
                dst,
                lhs: a,
                rhs: b,
            },
            expected: vec![a, b],
        },
        UseCase {
            name: "Shl",
            inst: MInst::Shl {
                dst,
                lhs: a,
                rhs: b,
            },
            expected: vec![a, b],
        },
        UseCase {
            name: "Sar",
            inst: MInst::Sar {
                dst,
                lhs: a,
                rhs: b,
            },
            expected: vec![a, b],
        },
        UseCase {
            name: "AndImm",
            inst: MInst::AndImm {
                dst,
                src: a,
                imm: 0xff,
            },
            expected: vec![a],
        },
        UseCase {
            name: "OrImm",
            inst: MInst::OrImm {
                dst,
                src: a,
                imm: 0xff,
            },
            expected: vec![a],
        },
        UseCase {
            name: "ShrImm",
            inst: MInst::ShrImm {
                dst,
                src: a,
                imm: 3,
            },
            expected: vec![a],
        },
        UseCase {
            name: "ShlImm",
            inst: MInst::ShlImm {
                dst,
                src: a,
                imm: 3,
            },
            expected: vec![a],
        },
        UseCase {
            name: "SarImm",
            inst: MInst::SarImm {
                dst,
                src: a,
                imm: 3,
            },
            expected: vec![a],
        },
        UseCase {
            name: "AddImm",
            inst: MInst::AddImm {
                dst,
                src: a,
                imm: 3,
            },
            expected: vec![a],
        },
        UseCase {
            name: "SubImm",
            inst: MInst::SubImm {
                dst,
                src: a,
                imm: 3,
            },
            expected: vec![a],
        },
        UseCase {
            name: "Cmp",
            inst: MInst::Cmp {
                dst,
                lhs: a,
                rhs: b,
                kind: CmpKind::Eq,
            },
            expected: vec![a, b],
        },
        UseCase {
            name: "CmpImm",
            inst: MInst::CmpImm {
                dst,
                lhs: a,
                imm: 3,
                kind: CmpKind::Eq,
            },
            expected: vec![a],
        },
        UseCase {
            name: "UDiv",
            inst: MInst::UDiv {
                dst,
                lhs: a,
                rhs: b,
            },
            expected: vec![a, b],
        },
        UseCase {
            name: "URem",
            inst: MInst::URem {
                dst,
                lhs: a,
                rhs: b,
            },
            expected: vec![a, b],
        },
        UseCase {
            name: "SDiv",
            inst: MInst::SDiv {
                dst,
                lhs: a,
                rhs: b,
            },
            expected: vec![a, b],
        },
        UseCase {
            name: "SRem",
            inst: MInst::SRem {
                dst,
                lhs: a,
                rhs: b,
            },
            expected: vec![a, b],
        },
        UseCase {
            name: "BitNot",
            inst: MInst::BitNot { dst, src: a },
            expected: vec![a],
        },
        UseCase {
            name: "Neg",
            inst: MInst::Neg { dst, src: a },
            expected: vec![a],
        },
        UseCase {
            name: "Popcnt",
            inst: MInst::Popcnt { dst, src: a },
            expected: vec![a],
        },
        UseCase {
            name: "Bsf",
            inst: MInst::Bsf { dst, src: a },
            expected: vec![a],
        },
        UseCase {
            name: "Bsr",
            inst: MInst::Bsr { dst, src: a },
            expected: vec![a],
        },
        UseCase {
            name: "BsrOr",
            inst: MInst::BsrOr {
                dst,
                src: a,
                zero_value: 63,
            },
            expected: vec![a],
        },
        UseCase {
            name: "Pext",
            inst: MInst::Pext {
                dst,
                src: a,
                mask: b,
            },
            expected: vec![a, b],
        },
        UseCase {
            name: "Pdep",
            inst: MInst::Pdep {
                dst,
                src: a,
                mask: b,
            },
            expected: vec![a, b],
        },
        UseCase {
            name: "Select",
            inst: MInst::Select {
                dst,
                cond: a,
                true_val: b,
                false_val: c,
            },
            expected: vec![a, b, c],
        },
        UseCase {
            name: "CmpSelect",
            inst: MInst::CmpSelect {
                dst,
                lhs: a,
                rhs: b,
                kind: CmpKind::Eq,
                true_val: c,
                false_val: d,
            },
            expected: vec![a, b, c, d],
        },
        UseCase {
            name: "CmpImmSelect",
            inst: MInst::CmpImmSelect {
                dst,
                lhs: a,
                imm: 3,
                kind: CmpKind::Eq,
                true_val: b,
                false_val: c,
            },
            expected: vec![a, b, c],
        },
        UseCase {
            name: "GuardedCmpSelect",
            inst: MInst::GuardedCmpSelect {
                dst,
                guard: a,
                lhs: b,
                rhs: c,
                kind: CmpKind::Eq,
                true_val: d,
                false_val: e,
            },
            expected: vec![a, b, c, d, e],
        },
        UseCase {
            name: "Branch",
            inst: MInst::Branch {
                cond: a,
                true_bb: block_a,
                false_bb: block_b,
            },
            expected: vec![a],
        },
        UseCase {
            name: "Jump",
            inst: MInst::Jump { target: block_a },
            expected: vec![],
        },
        UseCase {
            name: "Return",
            inst: MInst::Return,
            expected: vec![],
        },
        UseCase {
            name: "ReturnError",
            inst: MInst::ReturnError { code: 1 },
            expected: vec![],
        },
    ]
}

#[test]
fn uses_reports_every_use_operand_for_every_instruction_variant() {
    let cases = use_cases();
    assert_eq!(
        cases.len(),
        57,
        "the MInst variant table must stay exhaustive"
    );

    for case in cases {
        assert_eq!(
            case.inst.uses().into_iter().collect::<Vec<_>>(),
            case.expected,
            "{}",
            case.name
        );
    }
}

#[test]
fn rewrite_use_rewrites_every_operand_reported_by_uses() {
    for case in use_cases() {
        let original_def = case.inst.def();
        for old in case.expected.iter().copied() {
            let replacement = vreg(old.0 + 200);
            let mut rewritten = case.inst.clone();
            rewritten.rewrite_use(old, replacement);

            let expected = case
                .expected
                .iter()
                .copied()
                .map(|used| if used == old { replacement } else { used })
                .collect::<Vec<_>>();
            assert_eq!(
                rewritten.uses().into_iter().collect::<Vec<_>>(),
                expected,
                "{} did not rewrite {old}",
                case.name
            );
            assert_eq!(
                rewritten.def(),
                original_def,
                "{} rewrote its definition while replacing {old}",
                case.name
            );
        }
    }
}

#[test]
fn rewrite_use_rewrites_all_occurrences_of_the_same_vreg() {
    let shared = vreg(50);
    let replacement = vreg(51);

    for case in use_cases() {
        if case.expected.is_empty() {
            continue;
        }
        let original_def = case.inst.def();
        let mut rewritten = case.inst;
        for used in case.expected {
            rewritten.rewrite_use(used, shared);
        }
        rewritten.rewrite_use(shared, replacement);

        assert!(
            rewritten.uses().into_iter().all(|used| used == replacement),
            "{} left an occurrence of the shared use unchanged",
            case.name
        );
        assert_eq!(
            rewritten.def(),
            original_def,
            "{} changed its def",
            case.name
        );
    }
}
