use super::*;
use num_bigint::BigUint;
use num_traits::{ToPrimitive, Zero};

fn fixture(
    lanes: usize,
    element_width: usize,
    word_width: usize,
) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let r = RegisterId;
    let key_width = lanes.trailing_zeros() as usize;
    let bit = |width| RegisterType::Bit {
        width,
        signed: false,
    };
    let index_type = RegisterType::Bit {
        width: 32,
        signed: true,
    };
    let value_type = RegisterType::Logic { width: word_width };
    let register_map = (0..31)
        .map(|id| {
            let ty = match id {
                0 | 9 | 16 => bit(key_width),
                3 | 5 | 7 | 12 | 27 => bit(key_width + 1),
                4 | 6 | 13 | 29 => index_type.clone(),
                8 | 10 => bit(64),
                19 => RegisterType::Logic { width: 64 },
                15 => bit(32),
                17 => RegisterType::Logic { width: 1 },
                18 | 28 => bit(1),
                11 | 20 | 21 => bit(word_width),
                _ => value_type.clone(),
            };
            (r(id), ty)
        })
        .collect();
    let entry = BasicBlock {
        id: BlockId(0),
        params: vec![r(0), r(1), r(2)],
        instructions: vec![
            SIRInstruction::Imm(r(3), SIRValue::new(lanes as u64)),
            SIRInstruction::Imm(r(4), SIRValue::new(0u8)),
            SIRInstruction::Imm(r(5), SIRValue::new(1u8)),
            SIRInstruction::Imm(r(6), SIRValue::new(1u8)),
            SIRInstruction::Imm(r(7), SIRValue::new(0u8)),
            SIRInstruction::Imm(r(8), SIRValue::new(0u8)),
            SIRInstruction::Imm(r(9), SIRValue::new((lanes - 1) as u64)),
            SIRInstruction::Imm(r(10), SIRValue::new(element_width as u64)),
        ],
        terminator: SIRTerminator::Jump(BlockId(1), vec![r(3), r(4), r(2)]),
    };
    let scan = BasicBlock {
        id: BlockId(1),
        params: vec![r(12), r(13), r(14)],
        instructions: vec![
            SIRInstruction::Imm(r(11), SIRValue::new((1u64 << element_width) - 1)),
            SIRInstruction::Binary(r(15), r(13), BinaryOp::Shr, r(8)),
            SIRInstruction::Binary(r(16), r(15), BinaryOp::And, r(9)),
            SIRInstruction::Binary(r(17), r(0), BinaryOp::Eq, r(16)),
            SIRInstruction::Unary(r(18), UnaryOp::ToTwoState, r(17)),
            SIRInstruction::Binary(r(19), r(13), BinaryOp::Mul, r(10)),
            SIRInstruction::Binary(r(20), r(11), BinaryOp::Shl, r(19)),
            SIRInstruction::Unary(r(21), UnaryOp::BitNot, r(20)),
            SIRInstruction::Binary(r(22), r(14), BinaryOp::And, r(21)),
            SIRInstruction::Binary(r(23), r(1), BinaryOp::Shl, r(19)),
            SIRInstruction::Binary(r(24), r(23), BinaryOp::And, r(20)),
            SIRInstruction::Binary(r(25), r(22), BinaryOp::Or, r(24)),
            SIRInstruction::Mux(r(26), r(18), r(25), r(14)),
            SIRInstruction::Binary(r(27), r(12), BinaryOp::Sub, r(5)),
            SIRInstruction::Binary(r(28), r(27), BinaryOp::Ne, r(7)),
            SIRInstruction::Binary(r(29), r(13), BinaryOp::Add, r(6)),
        ],
        terminator: SIRTerminator::Branch {
            cond: r(28),
            true_block: (BlockId(1), vec![r(27), r(29), r(26)]),
            false_block: (BlockId(2), vec![r(26)]),
        },
    };
    let exit = BasicBlock {
        id: BlockId(2),
        params: vec![r(30)],
        instructions: vec![SIRInstruction::RuntimeEvent {
            site_id: 0,
            args: vec![r(30)],
        }],
        terminator: SIRTerminator::Return,
    };
    ExecutionUnit {
        entry_block_id: BlockId(0),
        register_map,
        blocks: [entry, scan, exit].into_iter().map(|b| (b.id, b)).collect(),
    }
}

fn execute(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    selector: u64,
    payload: &BigUint,
    initial: &BigUint,
) -> Vec<BigUint> {
    let mut values = HashMap::from_iter([
        (RegisterId(0), BigUint::from(selector)),
        (RegisterId(1), payload.clone()),
        (RegisterId(2), initial.clone()),
    ]);
    let mut current = eu.entry_block_id;
    for _ in 0..300 {
        let block = &eu.blocks[&current];
        for inst in &block.instructions {
            let (dst, value) = match inst {
                SIRInstruction::Imm(dst, value) => (*dst, value.payload.clone()),
                SIRInstruction::Binary(dst, lhs, op, rhs) => {
                    let (lhs, rhs) = (&values[lhs], &values[rhs]);
                    let value = match op {
                        BinaryOp::Add => lhs + rhs,
                        BinaryOp::Sub => lhs - rhs,
                        BinaryOp::Mul => lhs * rhs,
                        BinaryOp::And => lhs & rhs,
                        BinaryOp::Or => lhs | rhs,
                        BinaryOp::Shr => lhs >> rhs.to_usize().unwrap(),
                        BinaryOp::Shl => lhs << rhs.to_usize().unwrap(),
                        BinaryOp::Eq => BigUint::from(u8::from(lhs == rhs)),
                        BinaryOp::Ne => BigUint::from(u8::from(lhs != rhs)),
                        _ => panic!("unsupported fixture operation {op:?}"),
                    };
                    (*dst, value)
                }
                SIRInstruction::Unary(dst, op, input) => {
                    let value = match op {
                        UnaryOp::BitNot => {
                            ((BigUint::from(1u8) << eu.register_map[dst].width()) - 1u8)
                                ^ &values[input]
                        }
                        UnaryOp::Ident | UnaryOp::ToTwoState => values[input].clone(),
                        _ => panic!("unsupported fixture operation {op:?}"),
                    };
                    (*dst, value)
                }
                SIRInstruction::Mux(dst, condition, yes, no) => (
                    *dst,
                    values[if values[condition].is_zero() { no } else { yes }].clone(),
                ),
                SIRInstruction::RuntimeEvent { args, .. } => {
                    return args.iter().map(|reg| values[reg].clone()).collect();
                }
                _ => panic!("unsupported fixture instruction {inst:?}"),
            };
            let mask = (BigUint::from(1u8) << eu.register_map[&dst].width()) - 1u8;
            values.insert(dst, value & mask);
        }
        let (target, args) = match &block.terminator {
            SIRTerminator::Jump(target, args) => (*target, args),
            SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } => {
                let target = if values[cond].is_zero() {
                    false_block
                } else {
                    true_block
                };
                (target.0, &target.1)
            }
            _ => panic!("fixture did not publish a result"),
        };
        let args = args
            .iter()
            .map(|reg| values[reg].clone())
            .collect::<Vec<_>>();
        for (&param, value) in eu.blocks[&target].params.iter().zip(args) {
            values.insert(param, value);
        }
        current = target;
    }
    panic!("fixture did not terminate")
}

#[test]
fn updates_every_selector_and_preserves_bits_across_word_boundaries() {
    for (lanes, element_width, word_width) in [(8, 3, 32), (32, 6, 192)] {
        let original = fixture(lanes, element_width, word_width);
        original.verify();
        let mut rewritten = original.clone();
        assert_eq!(run(&mut rewritten), 1);
        rewritten.verify();
        assert!(!rewritten.blocks.contains_key(&BlockId(1)));
        assert_eq!(run(&mut rewritten), 0);
        let word_mask = (BigUint::from(1u8) << word_width) - 1u8;
        let element_mask = (BigUint::from(1u8) << element_width) - 1u8;
        for selector in 0..lanes {
            for initial in [
                BigUint::zero(),
                word_mask.clone(),
                (BigUint::from(1u8) << (word_width - 1)) | BigUint::from(0x12345678u32),
            ] {
                for payload in [
                    BigUint::zero(),
                    element_mask.clone(),
                    BigUint::from(0xabcdefu32),
                ] {
                    let shift = selector * element_width;
                    let mask = &element_mask << shift;
                    let expected =
                        (&initial & (&word_mask ^ &mask)) | ((&payload & &element_mask) << shift);
                    assert_eq!(
                        execute(&original, selector as u64, &payload, &initial),
                        vec![expected.clone()]
                    );
                    assert_eq!(
                        execute(&rewritten, selector as u64, &payload, &initial),
                        vec![expected]
                    );
                }
            }
        }
    }
}

#[test]
fn keeps_shared_literals_available_after_removing_the_scan() {
    let mut original = fixture(8, 3, 32);
    original.blocks.get_mut(&BlockId(2)).unwrap().instructions[0] = SIRInstruction::RuntimeEvent {
        site_id: 0,
        args: vec![RegisterId(30), RegisterId(11)],
    };
    original.verify();
    let mut rewritten = original.clone();
    assert_eq!(run(&mut rewritten), 1);
    rewritten.verify();
    assert_eq!(
        execute(&original, 3, &BigUint::from(2u8), &BigUint::from(0xffu8)),
        execute(&rewritten, 3, &BigUint::from(2u8), &BigUint::from(0xffu8))
    );
}

#[test]
fn native_four_state_recovery_retains_the_scan() {
    let mut eu = fixture(32, 6, 192);
    let original = eu.clone();
    super::super::pass_guarded_region_sinking::recover_merged_effect_regions(&mut eu, true);
    assert_eq!(eu, original);
}

#[test]
fn rejects_incomplete_domains_effects_and_escaping_loop_values() {
    let mut variants = (0..12).map(|_| fixture(8, 3, 32)).collect::<Vec<_>>();
    variants[0]
        .blocks
        .get_mut(&BlockId(0))
        .unwrap()
        .instructions[0] = SIRInstruction::Imm(RegisterId(3), SIRValue::new(4u8));
    variants[1]
        .blocks
        .get_mut(&BlockId(0))
        .unwrap()
        .instructions[1] = SIRInstruction::Imm(RegisterId(4), SIRValue::new(1u8));
    variants[2]
        .blocks
        .get_mut(&BlockId(0))
        .unwrap()
        .instructions[3] = SIRInstruction::Imm(RegisterId(6), SIRValue::new(2u8));
    variants[3]
        .blocks
        .get_mut(&BlockId(0))
        .unwrap()
        .instructions[6] = SIRInstruction::Imm(RegisterId(9), SIRValue::new(3u8));
    variants[4]
        .blocks
        .get_mut(&BlockId(0))
        .unwrap()
        .instructions[5] = SIRInstruction::Imm(RegisterId(8), SIRValue::new(1u8));
    variants[5]
        .blocks
        .get_mut(&BlockId(1))
        .unwrap()
        .instructions
        .push(SIRInstruction::RuntimeEvent {
            site_id: 1,
            args: vec![],
        });
    variants[6]
        .blocks
        .get_mut(&BlockId(2))
        .unwrap()
        .instructions[0] = SIRInstruction::RuntimeEvent {
        site_id: 0,
        args: vec![RegisterId(30), RegisterId(27)],
    };
    for reg in [0, 9, 16] {
        variants[7].register_map.insert(
            RegisterId(reg),
            RegisterType::Bit {
                width: 4,
                signed: false,
            },
        );
    }
    // A signed one-bit step represents -1, and a narrow signed index
    // becomes negative before completing the unsigned selector domain.
    variants[8].register_map.insert(
        RegisterId(6),
        RegisterType::Bit {
            width: 1,
            signed: true,
        },
    );
    for reg in [4, 6, 13, 15, 29] {
        variants[9].register_map.insert(
            RegisterId(reg),
            RegisterType::Bit {
                width: 3,
                signed: true,
            },
        );
    }
    variants[10]
        .register_map
        .insert(RegisterId(19), RegisterType::Logic { width: 4 });
    variants[11].register_map.insert(
        RegisterId(10),
        RegisterType::Bit {
            width: 64,
            signed: true,
        },
    );
    for mut eu in variants {
        eu.verify();
        let before = eu.clone();
        assert_eq!(run(&mut eu), 0);
        assert_eq!(eu, before);
    }
}
