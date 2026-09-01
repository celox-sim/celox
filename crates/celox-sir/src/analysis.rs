//! Read-only, target-independent queries over SIR.
//!
//! This module deliberately reports SIR facts only. Backend capability and
//! profitability decisions belong to each target's instruction selector.

use num_traits::Zero;

use crate::{
    BasicBlock, BlockId, ExecutionUnit, HashMap, HashSet, RegisterId, SIRInstruction,
    SIRTerminator, SIRValue,
};

/// One use of an SSA register in an instruction or block terminator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UseSite {
    pub block: BlockId,
    pub inst_idx: Option<usize>,
}

/// An exact, two-state constant which fits in one machine-independent u64.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExactU64Constant {
    pub value: u64,
}

/// Return blocks in deterministic reverse postorder.
///
/// Reachable blocks are ordered from the entry. Any unreachable blocks are
/// then visited from sorted block IDs so malformed or intermediate units are
/// still handled deterministically by read-only analyses.
pub fn reverse_postorder<A>(unit: &ExecutionUnit<A>) -> Vec<BlockId> {
    fn successors(terminator: &SIRTerminator) -> Vec<BlockId> {
        match terminator {
            SIRTerminator::Jump(target, _) => vec![*target],
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } => vec![true_block.0, false_block.0],
            SIRTerminator::Switch { cases, default, .. } => cases
                .iter()
                .map(|case| case.target)
                .chain(std::iter::once(*default))
                .collect(),
            SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
        }
    }

    fn visit_from<A>(
        unit: &ExecutionUnit<A>,
        start: BlockId,
        visited: &mut HashSet<BlockId>,
        postorder: &mut Vec<BlockId>,
    ) {
        let mut stack = vec![(start, false)];
        while let Some((block_id, expanded)) = stack.pop() {
            if !unit.blocks.contains_key(&block_id) {
                continue;
            }
            if expanded {
                postorder.push(block_id);
                continue;
            }
            if !visited.insert(block_id) {
                continue;
            }
            stack.push((block_id, true));
            let mut successors = successors(&unit.blocks[&block_id].terminator);
            successors.reverse();
            for successor in successors {
                if !visited.contains(&successor) {
                    stack.push((successor, false));
                }
            }
        }
    }

    let mut visited = HashSet::default();
    let mut postorder = Vec::new();
    visit_from(unit, unit.entry_block_id, &mut visited, &mut postorder);

    let mut block_ids = unit.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_unstable();
    for block_id in block_ids {
        if !visited.contains(&block_id) {
            visit_from(unit, block_id, &mut visited, &mut postorder);
        }
    }

    postorder.reverse();
    postorder
}

/// Visit every register used by one SIR instruction.
pub fn visit_instruction_uses<A>(
    instruction: &SIRInstruction<A>,
    mut visit: impl FnMut(RegisterId),
) {
    match instruction {
        SIRInstruction::Imm(..) => {}
        SIRInstruction::Binary(_, lhs, _, rhs) => {
            visit(*lhs);
            visit(*rhs);
        }
        SIRInstruction::Unary(_, _, source) | SIRInstruction::Slice(_, source, _, _) => {
            visit(*source);
        }
        SIRInstruction::Load(_, _, offset, _) => {
            for register in offset.dynamic_registers().into_iter().flatten() {
                visit(register);
            }
        }
        SIRInstruction::Store(_, offset, _, source, _, _) => {
            for register in offset.dynamic_registers().into_iter().flatten() {
                visit(register);
            }
            visit(*source);
        }
        SIRInstruction::Commit(_, _, offset, _, _) => {
            for register in offset.dynamic_registers().into_iter().flatten() {
                visit(register);
            }
        }
        SIRInstruction::Concat(_, arguments)
        | SIRInstruction::RuntimeEvent {
            args: arguments, ..
        }
        | SIRInstruction::CombCaptureEvent {
            args: arguments, ..
        } => {
            for &argument in arguments {
                visit(argument);
            }
        }
        SIRInstruction::Mux(_, condition, true_value, false_value) => {
            visit(*condition);
            visit(*true_value);
            visit(*false_value);
        }
        SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => {
            visit(*old);
            visit(*new);
        }
    }
}

/// Visit every register used by one SIR block terminator.
pub fn visit_terminator_uses(terminator: &SIRTerminator, mut visit: impl FnMut(RegisterId)) {
    match terminator {
        SIRTerminator::Jump(_, arguments) => {
            for &argument in arguments {
                visit(argument);
            }
        }
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            visit(*cond);
            for &argument in &true_block.1 {
                visit(argument);
            }
            for &argument in &false_block.1 {
                visit(argument);
            }
        }
        SIRTerminator::Switch { selector, .. } => visit(*selector),
        SIRTerminator::Return | SIRTerminator::Error(_) => {}
    }
}

/// Return the register defined by an instruction, if it produces a value.
pub fn instruction_definition<A>(instruction: &SIRInstruction<A>) -> Option<RegisterId> {
    instruction.defined_register()
}

/// Map each instruction-defined register in a block to its instruction index.
pub fn block_instruction_definitions<A>(block: &BasicBlock<A>) -> HashMap<RegisterId, usize> {
    block
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(index, instruction)| {
            instruction_definition(instruction).map(|register| (register, index))
        })
        .collect()
}

/// Collect all instruction and terminator use sites in an execution unit.
pub fn collect_use_sites<A>(unit: &ExecutionUnit<A>) -> HashMap<RegisterId, Vec<UseSite>> {
    let mut uses = HashMap::<RegisterId, Vec<UseSite>>::default();
    for block_id in reverse_postorder(unit) {
        let block = &unit.blocks[&block_id];
        for (inst_idx, instruction) in block.instructions.iter().enumerate() {
            visit_instruction_uses(instruction, |register| {
                uses.entry(register).or_default().push(UseSite {
                    block: block_id,
                    inst_idx: Some(inst_idx),
                });
            });
        }
        visit_terminator_uses(&block.terminator, |register| {
            uses.entry(register).or_default().push(UseSite {
                block: block_id,
                inst_idx: None,
            });
        });
    }
    uses
}

/// Interpret a two-state SIR constant as u64 without truncation.
pub fn exact_u64_constant(value: &SIRValue) -> Option<ExactU64Constant> {
    if !value.mask.is_zero() {
        return None;
    }
    let digits = value.payload.to_u64_digits();
    let value = match digits.as_slice() {
        [] => 0,
        [value] => *value,
        _ => return None,
    };
    Some(ExactU64Constant { value })
}

/// Collect exact u64 immediates whose register has one unambiguous definition.
pub fn collect_unique_exact_u64_constants<A>(
    unit: &ExecutionUnit<A>,
) -> HashMap<RegisterId, ExactU64Constant> {
    let mut constants = HashMap::default();
    let mut ambiguous = HashSet::default();
    for block_id in reverse_postorder(unit) {
        for instruction in &unit.blocks[&block_id].instructions {
            let SIRInstruction::Imm(destination, value) = instruction else {
                continue;
            };
            let Some(value) = exact_u64_constant(value) else {
                continue;
            };
            if constants.insert(*destination, value).is_some() {
                ambiguous.insert(*destination);
            }
        }
    }
    for register in ambiguous {
        constants.remove(&register);
    }
    constants
}

#[cfg(test)]
mod tests {
    use num_bigint::BigUint;

    use super::*;
    use crate::{BinaryOp, RegisterType, SIROffset, SIRSwitchCase};

    fn block(
        id: usize,
        instructions: Vec<SIRInstruction<()>>,
        terminator: SIRTerminator,
    ) -> BasicBlock<()> {
        BasicBlock {
            id: BlockId(id),
            params: Vec::new(),
            instructions,
            terminator,
        }
    }

    #[test]
    fn collects_instruction_and_terminator_uses() {
        let entry = block(
            0,
            vec![
                SIRInstruction::Binary(RegisterId(2), RegisterId(0), BinaryOp::Add, RegisterId(1)),
                SIRInstruction::Store(
                    (),
                    SIROffset::Element {
                        index: RegisterId(3),
                        element_width: 8,
                        bit_offset: 0,
                        dynamic_bit_offset: Some(RegisterId(4)),
                    },
                    8,
                    RegisterId(2),
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            SIRTerminator::Branch {
                cond: RegisterId(5),
                true_block: (BlockId(1), vec![RegisterId(2)]),
                false_block: (BlockId(1), vec![RegisterId(1)]),
            },
        );
        let exit = block(1, Vec::new(), SIRTerminator::Return);
        let unit = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(BlockId(0), entry), (BlockId(1), exit)]
                .into_iter()
                .collect(),
            register_map: (0..=5)
                .map(|register| (RegisterId(register), RegisterType::Logic { width: 8 }))
                .collect(),
        };

        let uses = collect_use_sites(&unit);
        assert_eq!(uses[&RegisterId(0)][0].inst_idx, Some(0));
        assert_eq!(uses[&RegisterId(3)][0].inst_idx, Some(1));
        assert_eq!(uses[&RegisterId(4)][0].inst_idx, Some(1));
        assert_eq!(uses[&RegisterId(5)][0].inst_idx, None);
        assert_eq!(uses[&RegisterId(2)].len(), 2);
    }

    #[test]
    fn exact_constants_reject_masks_width_overflow_and_ambiguous_defs() {
        let unit = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(
                BlockId(0),
                block(
                    0,
                    vec![
                        SIRInstruction::Imm(RegisterId(0), SIRValue::new(7u8)),
                        SIRInstruction::Imm(RegisterId(1), SIRValue::new_four_state(1u8, 1u8)),
                        SIRInstruction::Imm(
                            RegisterId(2),
                            SIRValue::new(BigUint::from(1u8) << 80usize),
                        ),
                        SIRInstruction::Imm(RegisterId(3), SIRValue::new(1u8)),
                        SIRInstruction::Imm(RegisterId(3), SIRValue::new(2u8)),
                    ],
                    SIRTerminator::Return,
                ),
            )]
            .into_iter()
            .collect(),
            register_map: HashMap::default(),
        };

        let constants = collect_unique_exact_u64_constants(&unit);
        assert_eq!(constants[&RegisterId(0)].value, 7);
        assert!(!constants.contains_key(&RegisterId(1)));
        assert!(!constants.contains_key(&RegisterId(2)));
        assert!(!constants.contains_key(&RegisterId(3)));
    }

    #[test]
    fn reverse_postorder_is_deterministic_and_includes_unreachable_blocks() {
        let unit = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [
                (
                    BlockId(0),
                    block(
                        0,
                        Vec::new(),
                        SIRTerminator::Switch {
                            selector: RegisterId(0),
                            cases: vec![SIRSwitchCase {
                                value: BigUint::from(0u8),
                                target: BlockId(2),
                            }],
                            default: BlockId(1),
                        },
                    ),
                ),
                (BlockId(1), block(1, Vec::new(), SIRTerminator::Return)),
                (BlockId(2), block(2, Vec::new(), SIRTerminator::Return)),
                (BlockId(9), block(9, Vec::new(), SIRTerminator::Return)),
            ]
            .into_iter()
            .collect(),
            register_map: HashMap::default(),
        };

        let order = reverse_postorder(&unit);
        assert_eq!(order.len(), 4);
        assert_eq!(order[0], BlockId(9));
        assert_eq!(order[1], BlockId(0));
        assert_eq!(&order[2..], &[BlockId(1), BlockId(2)]);
    }
}
