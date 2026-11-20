use super::*;
use crate::ir::{BasicBlock, BlockId, SIRTerminator};

const SOURCE: RegisterId = RegisterId(0);

/// A unit whose one block XORs single-bit slices of `SOURCE` (a
/// `source_width`-bit input) at `bits`, left to right, and reports the
/// parity through a runtime event.
fn parity_unit(source_width: usize, bits: &[usize]) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let bit = RegisterType::Bit {
        width: 1,
        signed: false,
    };
    let mut register_map = HashMap::default();
    register_map.insert(
        SOURCE,
        RegisterType::Bit {
            width: source_width,
            signed: false,
        },
    );
    let mut instructions = Vec::new();
    let mut next = 1;
    let mut fresh = |register_map: &mut HashMap<RegisterId, RegisterType>| {
        let register = RegisterId(next);
        next += 1;
        register_map.insert(register, bit.clone());
        register
    };
    let mut parity = None;
    for &position in bits {
        let slice = fresh(&mut register_map);
        instructions.push(SIRInstruction::Slice(slice, SOURCE, position, 1));
        parity = Some(match parity {
            None => slice,
            Some(previous) => {
                let xor = fresh(&mut register_map);
                instructions.push(SIRInstruction::Binary(xor, previous, BinaryOp::Xor, slice));
                xor
            }
        });
    }
    instructions.push(SIRInstruction::RuntimeEvent {
        site_id: 0,
        args: vec![parity.unwrap()],
    });
    let mut blocks = HashMap::default();
    blocks.insert(
        BlockId(0),
        BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions,
            terminator: SIRTerminator::Return,
        },
    );
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    }
}

/// The value the unit reports for `source`, evaluating only the
/// instructions these units contain.
fn reported(eu: &ExecutionUnit<RegionedAbsoluteAddr>, source: &BigUint) -> BigUint {
    let width_mask = |register: &RegisterId| {
        (BigUint::from(1u8) << eu.register_map[register].width()) - BigUint::from(1u8)
    };
    let mut values = HashMap::default();
    values.insert(SOURCE, source.clone());
    for instruction in &eu.blocks[&BlockId(0)].instructions {
        match instruction {
            SIRInstruction::Imm(dst, value) => {
                values.insert(*dst, value.payload.clone() & width_mask(dst));
            }
            SIRInstruction::Slice(dst, src, offset, width) => {
                let mask = (BigUint::from(1u8) << *width) - BigUint::from(1u8);
                values.insert(*dst, (&values[src] >> *offset) & mask);
            }
            SIRInstruction::Binary(dst, lhs, op, rhs) => {
                let value = match op {
                    BinaryOp::And => &values[lhs] & &values[rhs],
                    BinaryOp::Xor => &values[lhs] ^ &values[rhs],
                    other => panic!("unexpected binary {other:?}"),
                };
                values.insert(*dst, value & width_mask(dst));
            }
            SIRInstruction::Unary(dst, UnaryOp::Xor, src) => {
                let ones = values[src].count_ones();
                values.insert(*dst, BigUint::from(ones % 2));
            }
            SIRInstruction::Unary(dst, UnaryOp::Ident, src) => {
                values.insert(*dst, values[src].clone());
            }
            SIRInstruction::RuntimeEvent { args, .. } => return values[&args[0]].clone(),
            other => panic!("unexpected instruction {other:?}"),
        }
    }
    unreachable!("the unit reports its parity")
}

/// Fold the unit and check that it reports the same parity as before for
/// several sources, including ones with every bit set.
fn check_folding(source_width: usize, bits: &[usize]) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let original = parity_unit(source_width, bits);
    let mut folded = original.clone();
    XorChainFoldingPass.run(&mut folded, &PassOptions::default());
    let all = (BigUint::from(1u8) << source_width) - BigUint::from(1u8);
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut sources = vec![BigUint::ZERO, all.clone()];
    for _ in 0..64 {
        let mut value = BigUint::ZERO;
        for _ in 0..source_width.div_ceil(64) {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            value = (value << 64) | BigUint::from(seed);
        }
        sources.push(value & &all);
    }
    for source in &sources {
        assert_eq!(
            reported(&folded, source),
            reported(&original, source),
            "parity of bits {bits:?} of {source:#x}"
        );
    }
    folded
}

fn folds(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> bool {
    eu.blocks[&BlockId(0)]
        .instructions
        .iter()
        .any(|instruction| matches!(instruction, SIRInstruction::Unary(_, UnaryOp::Xor, _)))
}

#[test]
fn folds_a_parity_of_a_narrow_source() {
    assert!(folds(&check_folding(32, &[0, 3, 7, 31])));
}

/// An ECC check over a 71-bit word takes the check bits above bit 63:
/// the mask must keep them (VeeR-EL2 `rvecc_decode` of its icache words).
#[test]
fn folds_a_parity_that_takes_bits_above_the_first_word() {
    assert!(folds(&check_folding(71, &[70, 57, 58, 59, 60, 61, 62, 63])));
    assert!(folds(&check_folding(142, &[141, 128, 129, 130, 64, 70, 3])));
}

#[test]
fn a_bit_taken_twice_cancels_out_of_the_parity() {
    assert!(folds(&check_folding(16, &[3, 5, 3, 7])));
    check_folding(16, &[3, 5, 5, 3]);
}
