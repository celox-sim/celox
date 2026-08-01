//! Replace a wide group of boolean block parameters with its existing packed
//! representation.
//!
//! Native control recovery can expose this shape:
//!
//! ```text
//! pred A: packed_a = Concat(a31..a0); jump merge(a0..a31)
//! pred B: packed_b = Concat(b31..b0); jump merge(b0..b31)
//! merge(p0..p31): ... Concat(p31..p0)
//! ```
//!
//! Carrying 32 independent phis across the intervening CFG creates 32 long
//! live ranges even though every predecessor and the only consumer already
//! agree on one packed value.  This pass carries that value as one block
//! parameter and turns the final Concat into an identity.

use super::sir_analysis::{UseSite, collect_uses};
use crate::HashMap;
use crate::ir::*;

#[derive(Clone, Copy)]
enum EdgeKind {
    Jump,
    True,
    False,
}

struct IncomingPacked {
    predecessor: BlockId,
    edge: EdgeKind,
    packed: RegisterId,
}

struct Plan {
    merge: BlockId,
    parameter_indices: Vec<usize>,
    sink_block: BlockId,
    sink_instruction: usize,
    sink_destination: RegisterId,
    incoming: Vec<IncomingPacked>,
}

pub(super) fn pack_concat_phis(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) -> usize {
    let mut changed = 0;
    while let Some(plan) = find_plan(eu) {
        apply_plan(eu, plan);
        changed += 1;
    }
    debug_assert_eq!(eu.verify_result(), Ok(()));
    changed
}

fn find_plan(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> Option<Plan> {
    let uses = collect_uses(eu);
    let parameter_origins = eu
        .blocks
        .iter()
        .flat_map(|(&block, data)| {
            data.params
                .iter()
                .copied()
                .enumerate()
                .map(move |(index, parameter)| (parameter, (block, index)))
        })
        .collect::<HashMap<_, _>>();
    let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_unstable();

    for sink_block in block_ids {
        let block = &eu.blocks[&sink_block];
        for (sink_instruction, instruction) in block.instructions.iter().enumerate() {
            let SIRInstruction::Concat(sink_destination, operands) = instruction else {
                continue;
            };
            if !(8..=64).contains(&operands.len()) {
                continue;
            }
            let Some(&(merge, _)) = operands
                .first()
                .and_then(|operand| parameter_origins.get(operand))
            else {
                continue;
            };
            let mut parameter_indices = Vec::with_capacity(operands.len());
            let mut valid = true;
            for &operand in operands {
                let Some(&(owner, index)) = parameter_origins.get(&operand) else {
                    valid = false;
                    break;
                };
                if owner != merge
                    || !matches!(
                        eu.register_map.get(&operand),
                        Some(RegisterType::Bit {
                            width: 1,
                            signed: false
                        }) | Some(RegisterType::Logic { width: 1 })
                    )
                    || uses.get(&operand).is_none_or(|sites| {
                        sites.as_slice()
                            != [UseSite::Instruction {
                                block: sink_block,
                                index: sink_instruction,
                            }]
                    })
                {
                    valid = false;
                    break;
                }
                parameter_indices.push(index);
            }
            if !valid {
                continue;
            }
            parameter_indices.sort_unstable();
            parameter_indices.dedup();
            if parameter_indices.len() != operands.len() {
                continue;
            }
            let sink_type = eu.register_map.get(sink_destination)?;
            if sink_type.width() != operands.len() {
                continue;
            }

            let incoming_edges = collect_incoming_edges(eu, merge);
            if incoming_edges.len() < 2 {
                continue;
            }
            let mut incoming = Vec::with_capacity(incoming_edges.len());
            for (predecessor, edge, arguments) in incoming_edges {
                if arguments.len() != eu.blocks[&merge].params.len() {
                    valid = false;
                    break;
                }
                let concat_arguments = operands
                    .iter()
                    .map(|operand| {
                        let (_, index) = parameter_origins[operand];
                        arguments[index]
                    })
                    .collect::<Vec<_>>();
                let packed = eu.blocks[&predecessor]
                    .instructions
                    .iter()
                    .find_map(|instruction| match instruction {
                        SIRInstruction::Concat(destination, existing)
                            if *existing == concat_arguments
                                && eu
                                    .register_map
                                    .get(destination)
                                    .is_some_and(|ty| ty.width() == sink_type.width()) =>
                        {
                            Some(*destination)
                        }
                        _ => None,
                    });
                let Some(packed) = packed else {
                    valid = false;
                    break;
                };
                incoming.push(IncomingPacked {
                    predecessor,
                    edge,
                    packed,
                });
            }
            if valid {
                return Some(Plan {
                    merge,
                    parameter_indices,
                    sink_block,
                    sink_instruction,
                    sink_destination: *sink_destination,
                    incoming,
                });
            }
        }
    }
    None
}

fn collect_incoming_edges(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    target: BlockId,
) -> Vec<(BlockId, EdgeKind, Vec<RegisterId>)> {
    let mut result = Vec::new();
    let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_unstable();
    for predecessor in block_ids {
        match &eu.blocks[&predecessor].terminator {
            SIRTerminator::Jump(block, arguments) if *block == target => {
                result.push((predecessor, EdgeKind::Jump, arguments.clone()));
            }
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } => {
                if true_block.0 == target {
                    result.push((predecessor, EdgeKind::True, true_block.1.clone()));
                }
                if false_block.0 == target {
                    result.push((predecessor, EdgeKind::False, false_block.1.clone()));
                }
            }
            SIRTerminator::Switch { cases, default, .. }
                if *default == target || cases.iter().any(|case| case.target == target) =>
            {
                return Vec::new();
            }
            _ => {}
        }
    }
    result
}

fn apply_plan(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, plan: Plan) {
    let packed_parameter = RegisterId(
        eu.register_map
            .keys()
            .map(|register| register.0)
            .max()
            .unwrap_or(0)
            + 1,
    );
    let packed_type = eu.register_map[&plan.sink_destination].clone();
    eu.register_map.insert(packed_parameter, packed_type);

    let merge = eu.blocks.get_mut(&plan.merge).unwrap();
    for &index in plan.parameter_indices.iter().rev() {
        merge.params.remove(index);
    }
    merge.params.push(packed_parameter);

    for incoming in plan.incoming {
        let arguments = edge_arguments_mut(
            &mut eu.blocks.get_mut(&incoming.predecessor).unwrap().terminator,
            incoming.edge,
        );
        for &index in plan.parameter_indices.iter().rev() {
            arguments.remove(index);
        }
        arguments.push(incoming.packed);
    }

    eu.blocks.get_mut(&plan.sink_block).unwrap().instructions[plan.sink_instruction] =
        SIRInstruction::Unary(plan.sink_destination, UnaryOp::Ident, packed_parameter);
}

fn edge_arguments_mut(terminator: &mut SIRTerminator, edge: EdgeKind) -> &mut Vec<RegisterId> {
    match (terminator, edge) {
        (SIRTerminator::Jump(_, arguments), EdgeKind::Jump) => arguments,
        (SIRTerminator::Branch { true_block, .. }, EdgeKind::True) => &mut true_block.1,
        (SIRTerminator::Branch { false_block, .. }, EdgeKind::False) => &mut false_block.1,
        _ => unreachable!("planned incoming edge changed before phi packing"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bit() -> RegisterType {
        RegisterType::Bit {
            width: 1,
            signed: false,
        }
    }

    fn block(
        blocks: &mut HashMap<BlockId, BasicBlock<RegionedAbsoluteAddr>>,
        id: usize,
        params: Vec<RegisterId>,
        instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
        terminator: SIRTerminator,
    ) {
        blocks.insert(
            BlockId(id),
            BasicBlock {
                id: BlockId(id),
                params,
                instructions,
                terminator,
            },
        );
    }

    #[test]
    fn carries_an_existing_concat_instead_of_eight_boolean_phis() {
        let mut register_map = HashMap::default();
        for register in (0..16).chain(18..26) {
            register_map.insert(RegisterId(register), bit());
        }
        register_map.insert(RegisterId(27), bit());
        for register in [16, 17] {
            register_map.insert(
                RegisterId(register),
                RegisterType::Bit {
                    width: 8,
                    signed: false,
                },
            );
        }
        register_map.insert(RegisterId(26), RegisterType::Logic { width: 8 });
        let mut blocks = HashMap::default();
        block(
            &mut blocks,
            0,
            Vec::new(),
            (0..16)
                .map(|register| {
                    SIRInstruction::Imm(RegisterId(register), SIRValue::new(register as u8 & 1))
                })
                .chain(std::iter::once(SIRInstruction::Imm(
                    RegisterId(27),
                    SIRValue::new(1u8),
                )))
                .collect(),
            SIRTerminator::Branch {
                cond: RegisterId(27),
                true_block: (BlockId(1), Vec::new()),
                false_block: (BlockId(2), Vec::new()),
            },
        );
        block(
            &mut blocks,
            1,
            Vec::new(),
            vec![SIRInstruction::Concat(
                RegisterId(16),
                (0..8).rev().map(RegisterId).collect(),
            )],
            SIRTerminator::Jump(BlockId(3), (0..8).map(RegisterId).collect()),
        );
        block(
            &mut blocks,
            2,
            Vec::new(),
            vec![SIRInstruction::Concat(
                RegisterId(17),
                (8..16).rev().map(RegisterId).collect(),
            )],
            SIRTerminator::Jump(BlockId(3), (8..16).map(RegisterId).collect()),
        );
        block(
            &mut blocks,
            3,
            (18..26).map(RegisterId).collect(),
            vec![SIRInstruction::Concat(
                RegisterId(26),
                (18..26).rev().map(RegisterId).collect(),
            )],
            SIRTerminator::Return,
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };
        eu.verify_result().unwrap();
        pack_concat_phis(&mut eu);

        assert_eq!(eu.blocks[&BlockId(3)].params.len(), 1);
        assert_eq!(
            eu.blocks[&BlockId(1)].terminator,
            SIRTerminator::Jump(BlockId(3), vec![RegisterId(16)])
        );
        assert_eq!(
            eu.blocks[&BlockId(2)].terminator,
            SIRTerminator::Jump(BlockId(3), vec![RegisterId(17)])
        );
        assert!(matches!(
            eu.blocks[&BlockId(3)].instructions[0],
            SIRInstruction::Unary(RegisterId(26), UnaryOp::Ident, _)
        ));
    }
}
