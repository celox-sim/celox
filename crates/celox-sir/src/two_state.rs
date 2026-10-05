//! Canonicalization of SIR for two-state simulation.
//!
//! Frontends lower designs without knowing the simulation state mode, so they
//! emit four-state immediates such as an X literal or the unknown result of an
//! out-of-range read. Two-state backends evaluate only value bits, which would
//! expose the arbitrary value-bit half of those encodings. A two-state
//! simulation instead converts every unknown bit to 0, as a four-state to
//! two-state conversion does (IEEE 1800-2023 6.3.2.2).
//!
//! After this conversion no register can hold an unknown bit, so the X/Z test
//! `index === ToTwoState(index)` that guards dynamic accesses is always true
//! and is folded to a constant.

use num_traits::Zero;

use crate::{
    BinaryOp, ExecutionUnit, HashMap, HashSet, RegisterId, SIRInstruction, SIRTerminator, SIRValue,
    SirProgram, UnaryOp, analysis,
};

/// Canonicalize every execution unit of `program` for two-state simulation.
pub fn canonicalize_program<EventAddr, StateAddr>(program: &mut SirProgram<EventAddr, StateAddr>) {
    let SirProgram {
        eval_comb,
        eval_apply_ffs,
        eval_comb_apply_ffs,
        eval_only_ffs,
        apply_ffs,
    } = program;
    let units = eval_comb.iter_mut().chain(
        [
            eval_apply_ffs,
            eval_comb_apply_ffs,
            eval_only_ffs,
            apply_ffs,
        ]
        .into_iter()
        .flat_map(|units| units.values_mut().flatten()),
    );
    for unit in units {
        canonicalize_unit(unit);
    }
}

/// Canonicalize one execution unit for two-state simulation.
pub fn canonicalize_unit<A>(unit: &mut ExecutionUnit<A>) {
    let patterns = wildcard_pattern_registers(unit);
    let mut two_state_of = HashMap::default();
    for block in unit.blocks.values() {
        for instruction in &block.instructions {
            if let SIRInstruction::Unary(destination, UnaryOp::ToTwoState, source) = instruction {
                two_state_of.insert(*destination, *source);
            }
        }
    }
    for block in unit.blocks.values_mut() {
        for instruction in &mut block.instructions {
            match instruction {
                // Wildcard patterns use their mask as "don't care" bits in
                // both state modes.
                SIRInstruction::Imm(destination, value)
                    if !value.mask.is_zero() && !patterns.contains(destination) =>
                {
                    let payload = &value.payload ^ (&value.payload & &value.mask);
                    *value = SIRValue::new(payload);
                }
                SIRInstruction::Binary(destination, lhs, BinaryOp::EqCase, rhs)
                    if two_state_of.get(rhs) == Some(lhs) || two_state_of.get(lhs) == Some(rhs) =>
                {
                    *instruction = SIRInstruction::Imm(*destination, SIRValue::new(1u8));
                }
                _ => {}
            }
        }
    }
}

/// Registers whose value can flow into an operand of a wildcard comparison.
fn wildcard_pattern_registers<A>(unit: &ExecutionUnit<A>) -> HashSet<RegisterId> {
    let mut definitions = HashMap::default();
    let mut worklist = Vec::new();
    for block in unit.blocks.values() {
        for instruction in &block.instructions {
            if let Some(destination) = instruction.defined_register() {
                definitions.insert(destination, instruction);
            }
            if let SIRInstruction::Binary(
                _,
                lhs,
                BinaryOp::EqWildcard | BinaryOp::NeWildcard,
                rhs,
            ) = instruction
            {
                worklist.extend([*lhs, *rhs]);
            }
        }
    }
    if worklist.is_empty() {
        return HashSet::default();
    }
    // A block parameter receives the argument at its position on every edge.
    let mut parameter_sources: HashMap<RegisterId, Vec<RegisterId>> = HashMap::default();
    for block in unit.blocks.values() {
        let mut edges = Vec::new();
        match &block.terminator {
            SIRTerminator::Jump(target, arguments) => edges.push((*target, arguments)),
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } => {
                edges.push((true_block.0, &true_block.1));
                edges.push((false_block.0, &false_block.1));
            }
            SIRTerminator::Switch { .. } | SIRTerminator::Return | SIRTerminator::Error(_) => {}
        }
        for (target, arguments) in edges {
            let Some(target) = unit.blocks.get(&target) else {
                continue;
            };
            for (&parameter, &argument) in target.params.iter().zip(arguments) {
                parameter_sources
                    .entry(parameter)
                    .or_default()
                    .push(argument);
            }
        }
    }
    let mut reached = HashSet::default();
    while let Some(register) = worklist.pop() {
        if !reached.insert(register) {
            continue;
        }
        if let Some(instruction) = definitions.get(&register) {
            analysis::visit_instruction_uses(*instruction, |source| worklist.push(source));
        }
        if let Some(sources) = parameter_sources.get(&register) {
            worklist.extend(sources.iter().copied());
        }
    }
    reached
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BasicBlock, BlockId, RegisterType};

    fn unit(
        instructions: Vec<SIRInstruction<u32>>,
        registers: &[(usize, RegisterType)],
    ) -> ExecutionUnit<u32> {
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
            register_map: registers
                .iter()
                .map(|(register, ty)| (RegisterId(*register), ty.clone()))
                .collect(),
        }
    }

    fn instructions(unit: &ExecutionUnit<u32>) -> &[SIRInstruction<u32>] {
        &unit.blocks[&BlockId(0)].instructions
    }

    #[test]
    fn unknown_immediate_bits_become_zero() {
        let mut unit = unit(
            vec![SIRInstruction::Imm(
                RegisterId(0),
                // X in bits 0..4, Z in bits 4..8, known 1 in bit 8.
                SIRValue::new_four_state(0x10fu32, 0xffu32),
            )],
            &[(0, RegisterType::Logic { width: 9 })],
        );
        canonicalize_unit(&mut unit);
        assert_eq!(
            instructions(&unit),
            [SIRInstruction::Imm(RegisterId(0), SIRValue::new(0x100u32))]
        );
    }

    #[test]
    fn wildcard_patterns_keep_their_dont_care_mask() {
        let pattern = SIRValue::new_four_state(0x0u32, 0x3u32);
        let mut unit = unit(
            vec![
                SIRInstruction::Imm(RegisterId(0), pattern.clone()),
                SIRInstruction::Concat(RegisterId(1), vec![RegisterId(0)]),
                SIRInstruction::Load(RegisterId(2), 0, crate::SIROffset::Static(0), 4),
                SIRInstruction::Binary(
                    RegisterId(3),
                    RegisterId(2),
                    BinaryOp::EqWildcard,
                    RegisterId(1),
                ),
            ],
            &[
                (0, RegisterType::Logic { width: 4 }),
                (1, RegisterType::Logic { width: 4 }),
                (2, RegisterType::Logic { width: 4 }),
                (
                    3,
                    RegisterType::Bit {
                        width: 1,
                        signed: false,
                    },
                ),
            ],
        );
        canonicalize_unit(&mut unit);
        assert_eq!(
            instructions(&unit)[0],
            SIRInstruction::Imm(RegisterId(0), pattern)
        );
    }

    #[test]
    fn known_index_test_folds_to_true() {
        let mut unit = unit(
            vec![
                SIRInstruction::Load(RegisterId(0), 0, crate::SIROffset::Static(0), 2),
                SIRInstruction::Unary(RegisterId(1), UnaryOp::ToTwoState, RegisterId(0)),
                SIRInstruction::Binary(
                    RegisterId(2),
                    RegisterId(0),
                    BinaryOp::EqCase,
                    RegisterId(1),
                ),
            ],
            &[
                (0, RegisterType::Logic { width: 2 }),
                (
                    1,
                    RegisterType::Bit {
                        width: 2,
                        signed: false,
                    },
                ),
                (
                    2,
                    RegisterType::Bit {
                        width: 1,
                        signed: false,
                    },
                ),
            ],
        );
        canonicalize_unit(&mut unit);
        assert_eq!(
            instructions(&unit)[2],
            SIRInstruction::Imm(RegisterId(2), SIRValue::new(1u8))
        );
    }
}
