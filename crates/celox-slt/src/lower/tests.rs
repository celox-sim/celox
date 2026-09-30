mod concat;
mod cost;
mod count_idioms;
mod loops;
mod mux;
mod or_scan;
mod slices;
mod types;

use super::*;
use crate::SLTNodeArena;
use celox_design::BitAccess;
use celox_sir::{BlockId, ExecutionUnit};

fn input(arena: &mut SLTNodeArena<u32>, variable: u32, width: usize) -> NodeId {
    arena
        .alloc(SLTNode::Input {
            variable,
            signed: false,
            index: vec![],
            access: BitAccess::new(0, width - 1),
        })
        .unwrap()
}

fn input_bit(arena: &mut SLTNodeArena<u32>, variable: u32, bit: usize) -> NodeId {
    arena
        .alloc(SLTNode::Input {
            variable,
            signed: false,
            index: vec![],
            access: BitAccess::new(bit, bit),
        })
        .unwrap()
}

fn constant(arena: &mut SLTNodeArena<u32>, value: u64, width: usize) -> NodeId {
    arena
        .alloc(SLTNode::Constant(value.into(), 0u8.into(), width, false))
        .unwrap()
}

fn operation_chain(
    arena: &mut SLTNodeArena<u32>,
    mut value: NodeId,
    op: BinaryOp,
    operations: usize,
    constant_base: u64,
    width: usize,
) -> NodeId {
    for index in 0..operations {
        let rhs = constant(arena, constant_base + index as u64, width);
        value = arena.alloc(SLTNode::Binary(value, op, rhs)).unwrap();
    }
    value
}

fn finish_lowering(mut builder: SIRBuilder<u32>) -> ExecutionUnit<u32> {
    builder.seal_block(SIRTerminator::Return);
    let (blocks, register_map, _) = builder.drain();
    let eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    };
    eu.verify_result()
        .unwrap_or_else(|error| panic!("{error}\n{eu}"));
    eu
}

fn instruction_count(
    eu: &ExecutionUnit<u32>,
    predicate: impl Fn(&SIRInstruction<u32>) -> bool,
) -> usize {
    eu.blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| predicate(instruction))
        .count()
}

fn branch_count(eu: &ExecutionUnit<u32>) -> usize {
    eu.blocks
        .values()
        .filter(|block| matches!(block.terminator, SIRTerminator::Branch { .. }))
        .count()
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TestSIRValue {
    payload: BigUint,
    mask: BigUint,
}

fn width_mask(width: usize) -> BigUint {
    (BigUint::from(1u8) << width) - BigUint::from(1u8)
}

/// Execute the small, value-only SIR subset emitted by ForFoldGroup.
/// Keeping this interpreter local to the lowering tests lets the tests pin
/// exact iteration and four-state merge semantics without adding a second
/// production execution path.
fn execute_fold_group_sir(eu: &ExecutionUnit<u32>) -> crate::HashMap<RegisterId, TestSIRValue> {
    execute_fold_group_sir_with_memory(eu, &crate::HashMap::default())
}

fn execute_fold_group_sir_with_memory(
    eu: &ExecutionUnit<u32>,
    memory: &crate::HashMap<u32, TestSIRValue>,
) -> crate::HashMap<RegisterId, TestSIRValue> {
    let mut values = crate::HashMap::default();
    let mut current = eu.entry_block_id;

    for _ in 0..100 {
        let block = &eu.blocks[&current];
        for instruction in &block.instructions {
            match instruction {
                SIRInstruction::Imm(dst, value) => {
                    values.insert(
                        *dst,
                        TestSIRValue {
                            payload: value.payload.clone(),
                            mask: value.mask.clone(),
                        },
                    );
                }
                SIRInstruction::Binary(dst, lhs, op, rhs) => {
                    let lhs_reg = *lhs;
                    let rhs_reg = *rhs;
                    let lhs = &values[&lhs_reg];
                    let rhs = &values[&rhs_reg];
                    let width = eu.register_map[dst].width();
                    let modulus = BigUint::from(1u8) << width;
                    let (payload, mask) = match op {
                        BinaryOp::LogicAnd | BinaryOp::LogicOr => {
                            let truth = |reg: RegisterId, value: &TestSIRValue| {
                                let known = width_mask(eu.register_map[&reg].width()) ^ &value.mask;
                                if (&value.payload & known) != BigUint::from(0u8) {
                                    Some(true)
                                } else if value.mask.is_zero() {
                                    Some(false)
                                } else {
                                    None
                                }
                            };
                            let lhs_truth = truth(lhs_reg, lhs);
                            let rhs_truth = truth(rhs_reg, rhs);
                            let known = match op {
                                BinaryOp::LogicAnd => {
                                    if lhs_truth == Some(false) || rhs_truth == Some(false) {
                                        Some(false)
                                    } else if lhs_truth == Some(true) && rhs_truth == Some(true) {
                                        Some(true)
                                    } else {
                                        None
                                    }
                                }
                                BinaryOp::LogicOr => {
                                    if lhs_truth == Some(true) || rhs_truth == Some(true) {
                                        Some(true)
                                    } else if lhs_truth == Some(false) && rhs_truth == Some(false) {
                                        Some(false)
                                    } else {
                                        None
                                    }
                                }
                                _ => unreachable!(),
                            };
                            match known {
                                Some(value) => (BigUint::from(value), BigUint::from(0u8)),
                                None => (BigUint::from(0u8), BigUint::from(1u8)),
                            }
                        }
                        _ => {
                            assert_eq!(lhs.mask, BigUint::from(0u8));
                            assert_eq!(rhs.mask, BigUint::from(0u8));
                            let payload = match op {
                                BinaryOp::Add => (&lhs.payload + &rhs.payload) % &modulus,
                                BinaryOp::Mul => (&lhs.payload * &rhs.payload) % &modulus,
                                BinaryOp::Sub => {
                                    (&lhs.payload + &modulus - &rhs.payload) % &modulus
                                }
                                BinaryOp::And => &lhs.payload & &rhs.payload,
                                BinaryOp::Or => &lhs.payload | &rhs.payload,
                                BinaryOp::Shl => {
                                    let shift =
                                        rhs.payload.to_u64_digits().first().copied().unwrap_or(0);
                                    if shift > usize::MAX as u64 {
                                        BigUint::from(0u8)
                                    } else {
                                        (&lhs.payload << shift as usize) % &modulus
                                    }
                                }
                                BinaryOp::Shr => {
                                    let shift =
                                        rhs.payload.to_u64_digits().first().copied().unwrap_or(0);
                                    if shift > usize::MAX as u64 {
                                        BigUint::from(0u8)
                                    } else {
                                        &lhs.payload >> shift as usize
                                    }
                                }
                                BinaryOp::Eq | BinaryOp::EqWildcard => {
                                    BigUint::from(lhs.payload == rhs.payload)
                                }
                                BinaryOp::Ne => BigUint::from(lhs.payload != rhs.payload),
                                BinaryOp::GtU => BigUint::from(lhs.payload > rhs.payload),
                                BinaryOp::GeU => BigUint::from(lhs.payload >= rhs.payload),
                                other => {
                                    panic!("unexpected grouped-fold binary op {other:?}")
                                }
                            };
                            (payload, BigUint::from(0u8))
                        }
                    };
                    values.insert(*dst, TestSIRValue { payload, mask });
                }
                SIRInstruction::Unary(dst, op, src) => {
                    let width = eu.register_map[dst].width();
                    let value = &values[src];
                    let (payload, mask) = match op {
                        UnaryOp::Ident => (value.payload.clone(), value.mask.clone()),
                        UnaryOp::ToTwoState => {
                            let known = width_mask(width) ^ &value.mask;
                            (&value.payload & known, BigUint::from(0u8))
                        }
                        UnaryOp::BitNot => {
                            (&width_mask(width) ^ &value.payload, value.mask.clone())
                        }
                        UnaryOp::LogicNot => (
                            BigUint::from(value.payload == BigUint::from(0u8)),
                            value.mask.clone(),
                        ),
                        UnaryOp::Or => (
                            BigUint::from(value.payload != BigUint::from(0u8)),
                            value.mask.clone(),
                        ),
                        UnaryOp::PopCount => (
                            BigUint::from(
                                value
                                    .payload
                                    .to_u64_digits()
                                    .iter()
                                    .map(|word| word.count_ones() as u64)
                                    .sum::<u64>(),
                            ),
                            value.mask.clone(),
                        ),
                        other => panic!("unexpected grouped-fold unary op {other:?}"),
                    };
                    values.insert(*dst, TestSIRValue { payload, mask });
                }
                SIRInstruction::Load(dst, address, offset, width) => {
                    let offset = match offset {
                        SIROffset::Static(offset)
                        | SIROffset::PackedElements {
                            bit_offset: offset, ..
                        } => *offset,
                        SIROffset::Dynamic(offset) => values[offset]
                            .payload
                            .to_u64_digits()
                            .first()
                            .copied()
                            .unwrap_or(0)
                            as usize,
                        SIROffset::Element {
                            index,
                            element_width,
                            bit_offset,
                            dynamic_bit_offset,
                        } => {
                            let element = values[index]
                                .payload
                                .to_u64_digits()
                                .first()
                                .copied()
                                .unwrap_or(0) as usize;
                            let dynamic_bit_offset = dynamic_bit_offset
                                .map(|register| {
                                    values[&register]
                                        .payload
                                        .to_u64_digits()
                                        .first()
                                        .copied()
                                        .unwrap_or(0) as usize
                                })
                                .unwrap_or(0);
                            element * element_width + bit_offset + dynamic_bit_offset
                        }
                    };
                    let source = memory
                        .get(address)
                        .unwrap_or_else(|| panic!("missing test memory value at {address}"));
                    let mask = width_mask(*width);
                    values.insert(
                        *dst,
                        TestSIRValue {
                            payload: (&source.payload >> offset) & &mask,
                            mask: (&source.mask >> offset) & mask,
                        },
                    );
                }
                SIRInstruction::Concat(dst, args) => {
                    let mut payload = BigUint::from(0u8);
                    let mut mask = BigUint::from(0u8);
                    for arg in args {
                        let width = eu.register_map[arg].width();
                        payload = (payload << width) | &values[arg].payload;
                        mask = (mask << width) | &values[arg].mask;
                    }
                    values.insert(*dst, TestSIRValue { payload, mask });
                }
                SIRInstruction::Slice(dst, src, bit_offset, width) => {
                    let mask = width_mask(*width);
                    values.insert(
                        *dst,
                        TestSIRValue {
                            payload: (&values[src].payload >> *bit_offset) & &mask,
                            mask: (&values[src].mask >> *bit_offset) & mask,
                        },
                    );
                }
                SIRInstruction::Mux(dst, cond, then_value, else_value) => {
                    let cond = &values[cond];
                    let selected = if cond.payload == BigUint::from(0u8) {
                        &values[else_value]
                    } else {
                        &values[then_value]
                    };
                    let width = eu.register_map[dst].width();
                    values.insert(
                        *dst,
                        TestSIRValue {
                            payload: &selected.payload & width_mask(width),
                            mask: if cond.mask == BigUint::from(0u8) {
                                &selected.mask & width_mask(width)
                            } else {
                                width_mask(width)
                            },
                        },
                    );
                }
                other => panic!("unexpected grouped-fold instruction {other:?}"),
            }
        }

        let (next, args) = match &block.terminator {
            SIRTerminator::Jump(target, args) => (*target, args),
            SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } => {
                if values[cond].payload == BigUint::from(0u8) {
                    (false_block.0, &false_block.1)
                } else {
                    (true_block.0, &true_block.1)
                }
            }
            SIRTerminator::Switch { .. } => {
                panic!("unexpected Switch in grouped-fold lowering test")
            }
            SIRTerminator::Return => return values,
            SIRTerminator::Error(code) => panic!("unexpected Error({code})"),
        };
        let arguments = args
            .iter()
            .map(|argument| values[argument].clone())
            .collect::<Vec<_>>();
        for (&parameter, argument) in eu.blocks[&next].params.iter().zip(arguments) {
            values.insert(parameter, argument);
        }
        current = next;
    }
    panic!("grouped fold did not terminate at its exact trip count")
}
