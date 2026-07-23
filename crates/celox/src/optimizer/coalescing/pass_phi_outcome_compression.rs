//! Compress edge-known boolean phi groups into one control-outcome selector.
//!
//! A recovered priority region often carries one boolean per visited decision
//! through a merge so a later region can test the same outcome.  Those values
//! are mutually correlated: the incoming CFG edge already determines every
//! boolean.  Keeping them as independent phis creates parallel-copy pressure.
//! This pass carries one edge tag instead and reconstructs each predicate next
//! to the blocks which actually use it.

use super::pass_manager::ExecutionUnitPass;
use super::sir_analysis::{collect_uses, predicate_facts};
use crate::ir::cfg::SirCfg;
use crate::ir::*;
use crate::optimizer::PassOptions;
use crate::{HashMap, HashSet};

pub(super) struct PhiOutcomeCompressionPass;

#[derive(Clone, Copy)]
enum EdgeKind {
    Jump,
    True,
    False,
}

#[derive(Clone)]
struct IncomingEdge {
    predecessor: BlockId,
    kind: EdgeKind,
    arguments: Vec<RegisterId>,
    facts: Vec<(RegisterId, bool)>,
}

#[derive(Clone)]
struct Candidate {
    index: usize,
    parameter: RegisterId,
    true_edges: Vec<usize>,
    use_blocks: Vec<BlockId>,
}

struct CompressionPlan {
    merge: BlockId,
    incoming: Vec<IncomingEdge>,
    candidates: Vec<Candidate>,
}

impl ExecutionUnitPass for PhiOutcomeCompressionPass {
    fn name(&self) -> &'static str {
        "phi_outcome_compression"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _: &PassOptions) {
        for plan in find_compressions(eu) {
            apply_compression(eu, plan.merge, &plan.incoming, &plan.candidates);
        }
        debug_assert_eq!(eu.verify_result(), Ok(()));
    }
}

fn find_compressions(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> Vec<CompressionPlan> {
    let Ok(cfg) = SirCfg::analyze(eu) else {
        return Vec::new();
    };
    let facts = predicate_facts(eu, &cfg);
    let constants = exact_boolean_constants(eu);
    let uses = collect_uses(eu);
    let mut merge_ids = cfg.block_ids.clone();
    merge_ids.sort_unstable();
    let mut plans = Vec::new();

    for merge_id in merge_ids {
        let merge_index = cfg.block_index(merge_id).unwrap();
        if cfg.sccs[cfg.scc_for_block[merge_index]].cyclic {
            continue;
        }
        let params = eu.blocks[&merge_id].params.clone();
        if params.len() < 2 {
            continue;
        }
        let incoming = incoming_edges(eu, &cfg, &facts, merge_id);
        if incoming.len() < 2
            || incoming
                .iter()
                .any(|edge| edge.arguments.len() != params.len())
        {
            continue;
        }

        let mut candidates = Vec::new();
        for (index, &parameter) in params.iter().enumerate() {
            if !matches!(
                eu.register_map.get(&parameter),
                Some(RegisterType::Bit {
                    width: 1,
                    signed: false
                })
            ) {
                continue;
            }
            let mut true_edges = Vec::new();
            let mut exact = true;
            for (edge_index, edge) in incoming.iter().enumerate() {
                let argument = edge.arguments[index];
                let value = edge
                    .facts
                    .iter()
                    .rev()
                    .find_map(|&(condition, value)| (condition == argument).then_some(value))
                    .or_else(|| constants.get(&argument).copied());
                match value {
                    Some(true) => true_edges.push(edge_index),
                    Some(false) => {}
                    None => {
                        exact = false;
                        break;
                    }
                }
            }
            if !exact || true_edges.is_empty() || true_edges.len() == incoming.len() {
                continue;
            }
            let mut parameter_uses = uses
                .get(&parameter)
                .into_iter()
                .flatten()
                .map(|site| site.block())
                .collect::<Vec<_>>();
            parameter_uses.sort_unstable();
            parameter_uses.dedup();
            if parameter_uses.is_empty() {
                continue;
            }
            candidates.push(Candidate {
                index,
                parameter,
                true_edges,
                use_blocks: parameter_uses,
            });
        }
        if candidates.len() < 2 {
            continue;
        }

        let edge_operands_removed = incoming.len() * (candidates.len() - 1);
        let reconstruction_cost = candidates
            .iter()
            .map(|candidate| {
                let comparisons = if candidate.true_edges.len() == 1
                    || incoming.len() - candidate.true_edges.len() == 1
                {
                    2
                } else {
                    candidate.true_edges.len() * 3 - 1
                };
                comparisons * candidate.use_blocks.len()
            })
            .sum::<usize>();
        if reconstruction_cost >= edge_operands_removed {
            continue;
        }

        plans.push(CompressionPlan {
            merge: merge_id,
            incoming,
            candidates,
        });
    }
    plans
}

fn apply_compression(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    merge: BlockId,
    incoming: &[IncomingEdge],
    candidates: &[Candidate],
) {
    let mut next_register = eu
        .register_map
        .keys()
        .map(|register| register.0)
        .max()
        .unwrap_or(0);
    let selector_width = usize::BITS as usize - (incoming.len() - 1).leading_zeros() as usize;
    let selector_type = RegisterType::Bit {
        width: selector_width.max(1),
        signed: false,
    };
    let selector = fresh_register(eu, &mut next_register, selector_type.clone());

    let mut tags = Vec::with_capacity(incoming.len());
    for (tag, edge) in incoming.iter().enumerate() {
        let tag_register = fresh_register(eu, &mut next_register, selector_type.clone());
        eu.blocks
            .get_mut(&edge.predecessor)
            .unwrap()
            .instructions
            .push(SIRInstruction::Imm(tag_register, SIRValue::new(tag)));
        tags.push(tag_register);
    }

    for candidate in candidates {
        let false_count = incoming.len() - candidate.true_edges.len();
        for &use_block in &candidate.use_blocks {
            let mut prefix = Vec::new();
            let replacement = if candidate.true_edges.len() == 1 || false_count == 1 {
                let (tag, operation) = if candidate.true_edges.len() == 1 {
                    (candidate.true_edges[0], BinaryOp::Eq)
                } else {
                    let true_edges = candidate.true_edges.iter().copied().collect::<HashSet<_>>();
                    let false_edge = (0..incoming.len())
                        .find(|edge| !true_edges.contains(edge))
                        .unwrap();
                    (false_edge, BinaryOp::Ne)
                };
                let constant = fresh_register(eu, &mut next_register, selector_type.clone());
                let result = fresh_register(eu, &mut next_register, bit());
                prefix.push(SIRInstruction::Imm(constant, SIRValue::new(tag)));
                prefix.push(SIRInstruction::Binary(
                    result, selector, operation, constant,
                ));
                result
            } else {
                let mut result = None;
                for &tag in &candidate.true_edges {
                    let constant = fresh_register(eu, &mut next_register, selector_type.clone());
                    let equal = fresh_register(eu, &mut next_register, bit());
                    prefix.push(SIRInstruction::Imm(constant, SIRValue::new(tag)));
                    prefix.push(SIRInstruction::Binary(
                        equal,
                        selector,
                        BinaryOp::Eq,
                        constant,
                    ));
                    result = Some(if let Some(previous) = result {
                        let combined = fresh_register(eu, &mut next_register, bit());
                        prefix.push(SIRInstruction::Binary(
                            combined,
                            previous,
                            BinaryOp::Or,
                            equal,
                        ));
                        combined
                    } else {
                        equal
                    });
                }
                result.unwrap()
            };
            let block = eu.blocks.get_mut(&use_block).unwrap();
            for instruction in &mut block.instructions {
                replace_instruction_use(instruction, candidate.parameter, replacement);
            }
            replace_terminator_use(&mut block.terminator, candidate.parameter, replacement);
            prefix.append(&mut block.instructions);
            block.instructions = prefix;
        }
    }

    let mut removed_indices = candidates
        .iter()
        .map(|candidate| candidate.index)
        .collect::<Vec<_>>();
    removed_indices.sort_unstable();
    for edge in incoming.iter().rev() {
        let terminator = &mut eu.blocks.get_mut(&edge.predecessor).unwrap().terminator;
        let arguments = edge_arguments_mut(terminator, edge.kind);
        for &index in removed_indices.iter().rev() {
            arguments.remove(index);
        }
        arguments.push(
            tags[incoming
                .iter()
                .position(|candidate| {
                    candidate.predecessor == edge.predecessor
                        && matches!(
                            (candidate.kind, edge.kind),
                            (EdgeKind::Jump, EdgeKind::Jump)
                                | (EdgeKind::True, EdgeKind::True)
                                | (EdgeKind::False, EdgeKind::False)
                        )
                })
                .unwrap()],
        );
    }
    let merge_block = eu.blocks.get_mut(&merge).unwrap();
    for &index in removed_indices.iter().rev() {
        merge_block.params.remove(index);
    }
    merge_block.params.push(selector);
}

fn incoming_edges(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    facts: &[Vec<(RegisterId, bool)>],
    merge: BlockId,
) -> Vec<IncomingEdge> {
    let merge_index = cfg.block_index(merge).unwrap();
    let mut result = Vec::new();
    for &predecessor_index in &cfg.predecessors[merge_index] {
        let predecessor = cfg.block_ids[predecessor_index];
        match &eu.blocks[&predecessor].terminator {
            SIRTerminator::Jump(target, arguments) if *target == merge => {
                result.push(IncomingEdge {
                    predecessor,
                    kind: EdgeKind::Jump,
                    arguments: arguments.clone(),
                    facts: facts[predecessor_index].clone(),
                });
            }
            SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } => {
                if true_block.0 == merge {
                    let mut edge_facts = facts[predecessor_index].clone();
                    edge_facts.push((*cond, true));
                    result.push(IncomingEdge {
                        predecessor,
                        kind: EdgeKind::True,
                        arguments: true_block.1.clone(),
                        facts: edge_facts,
                    });
                }
                if false_block.0 == merge {
                    let mut edge_facts = facts[predecessor_index].clone();
                    edge_facts.push((*cond, false));
                    result.push(IncomingEdge {
                        predecessor,
                        kind: EdgeKind::False,
                        arguments: false_block.1.clone(),
                        facts: edge_facts,
                    });
                }
            }
            _ => {}
        }
    }
    result.sort_unstable_by_key(|edge| {
        let kind = match edge.kind {
            EdgeKind::Jump => 0,
            EdgeKind::True => 1,
            EdgeKind::False => 2,
        };
        (edge.predecessor.0, kind)
    });
    result
}

fn exact_boolean_constants(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> HashMap<RegisterId, bool> {
    let mut result = HashMap::default();
    for block in eu.blocks.values() {
        for instruction in &block.instructions {
            if let SIRInstruction::Imm(dst, value) = instruction
                && value.mask.to_u64_digits().is_empty()
            {
                result.insert(*dst, !value.payload.to_u64_digits().is_empty());
            }
        }
    }
    result
}

fn replace_instruction_use(
    instruction: &mut SIRInstruction<RegionedAbsoluteAddr>,
    old: RegisterId,
    new: RegisterId,
) {
    match instruction {
        SIRInstruction::Imm(..) => {}
        SIRInstruction::Binary(_, lhs, _, rhs) => {
            replace(lhs, old, new);
            replace(rhs, old, new);
        }
        SIRInstruction::Unary(_, _, source) | SIRInstruction::Slice(_, source, _, _) => {
            replace(source, old, new);
        }
        SIRInstruction::Load(_, _, offset, _) | SIRInstruction::Commit(_, _, offset, _, _) => {
            replace_offset_use(offset, old, new);
        }
        SIRInstruction::Store(_, offset, _, source, _, _) => {
            replace_offset_use(offset, old, new);
            replace(source, old, new);
        }
        SIRInstruction::Concat(_, arguments)
        | SIRInstruction::RuntimeEvent {
            args: arguments, ..
        }
        | SIRInstruction::CombCaptureEvent {
            args: arguments, ..
        } => {
            for argument in arguments {
                replace(argument, old, new);
            }
        }
        SIRInstruction::Mux(_, condition, true_value, false_value) => {
            replace(condition, old, new);
            replace(true_value, old, new);
            replace(false_value, old, new);
        }
        SIRInstruction::CombCaptureEnableIfChanged {
            old: lhs, new: rhs, ..
        } => {
            replace(lhs, old, new);
            replace(rhs, old, new);
        }
    }
}

fn replace_offset_use(offset: &mut SIROffset, old: RegisterId, new: RegisterId) {
    match offset {
        SIROffset::Static(_) => {}
        SIROffset::Dynamic(register) => replace(register, old, new),
        SIROffset::Element {
            index,
            dynamic_bit_offset,
            ..
        } => {
            replace(index, old, new);
            if let Some(offset) = dynamic_bit_offset {
                replace(offset, old, new);
            }
        }
    }
}

fn replace_terminator_use(terminator: &mut SIRTerminator, old: RegisterId, new: RegisterId) {
    match terminator {
        SIRTerminator::Jump(_, arguments) => {
            for argument in arguments {
                replace(argument, old, new);
            }
        }
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            replace(cond, old, new);
            for argument in true_block.1.iter_mut().chain(&mut false_block.1) {
                replace(argument, old, new);
            }
        }
        SIRTerminator::Switch { selector, .. } => replace(selector, old, new),
        SIRTerminator::Return | SIRTerminator::Error(_) => {}
    }
}

fn edge_arguments_mut(terminator: &mut SIRTerminator, kind: EdgeKind) -> &mut Vec<RegisterId> {
    match (terminator, kind) {
        (SIRTerminator::Jump(_, arguments), EdgeKind::Jump) => arguments,
        (SIRTerminator::Branch { true_block, .. }, EdgeKind::True) => &mut true_block.1,
        (SIRTerminator::Branch { false_block, .. }, EdgeKind::False) => &mut false_block.1,
        _ => unreachable!(),
    }
}

fn fresh_register(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    counter: &mut usize,
    register_type: RegisterType,
) -> RegisterId {
    *counter += 1;
    let register = RegisterId(*counter);
    eu.register_map.insert(register, register_type);
    register
}

fn bit() -> RegisterType {
    RegisterType::Bit {
        width: 1,
        signed: false,
    }
}

fn replace(register: &mut RegisterId, old: RegisterId, new: RegisterId) {
    if *register == old {
        *register = new;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn jump_to_merge(arguments: Vec<RegisterId>) -> SIRTerminator {
        SIRTerminator::Jump(BlockId(4), arguments)
    }

    #[test]
    fn compresses_priority_history_to_one_edge_selector() {
        let bit = RegisterType::Bit {
            width: 1,
            signed: false,
        };
        let mut register_map = HashMap::default();
        for register in 0..=6 {
            register_map.insert(RegisterId(register), bit.clone());
        }
        let mut blocks = HashMap::default();
        block(
            &mut blocks,
            0,
            vec![RegisterId(0), RegisterId(1), RegisterId(2)],
            vec![SIRInstruction::Imm(RegisterId(3), SIRValue::new(0u8))],
            SIRTerminator::Branch {
                cond: RegisterId(0),
                true_block: (BlockId(1), Vec::new()),
                false_block: (BlockId(2), Vec::new()),
            },
        );
        block(
            &mut blocks,
            1,
            Vec::new(),
            Vec::new(),
            jump_to_merge(vec![RegisterId(0), RegisterId(3), RegisterId(3)]),
        );
        block(
            &mut blocks,
            2,
            Vec::new(),
            Vec::new(),
            SIRTerminator::Branch {
                cond: RegisterId(1),
                true_block: (BlockId(3), Vec::new()),
                false_block: (BlockId(7), Vec::new()),
            },
        );
        block(
            &mut blocks,
            3,
            Vec::new(),
            Vec::new(),
            jump_to_merge(vec![RegisterId(0), RegisterId(1), RegisterId(3)]),
        );
        block(
            &mut blocks,
            7,
            Vec::new(),
            Vec::new(),
            SIRTerminator::Branch {
                cond: RegisterId(2),
                true_block: (BlockId(8), Vec::new()),
                false_block: (BlockId(9), Vec::new()),
            },
        );
        for id in [8, 9] {
            block(
                &mut blocks,
                id,
                Vec::new(),
                Vec::new(),
                jump_to_merge(vec![RegisterId(0), RegisterId(1), RegisterId(2)]),
            );
        }
        block(
            &mut blocks,
            4,
            vec![RegisterId(4), RegisterId(5), RegisterId(6)],
            Vec::new(),
            SIRTerminator::Branch {
                cond: RegisterId(4),
                true_block: (BlockId(10), Vec::new()),
                false_block: (BlockId(5), Vec::new()),
            },
        );
        block(
            &mut blocks,
            5,
            Vec::new(),
            Vec::new(),
            SIRTerminator::Branch {
                cond: RegisterId(5),
                true_block: (BlockId(10), Vec::new()),
                false_block: (BlockId(6), Vec::new()),
            },
        );
        block(
            &mut blocks,
            6,
            Vec::new(),
            Vec::new(),
            SIRTerminator::Branch {
                cond: RegisterId(6),
                true_block: (BlockId(11), Vec::new()),
                false_block: (BlockId(10), Vec::new()),
            },
        );
        block(
            &mut blocks,
            11,
            Vec::new(),
            Vec::new(),
            SIRTerminator::Jump(BlockId(10), Vec::new()),
        );
        block(
            &mut blocks,
            10,
            Vec::new(),
            Vec::new(),
            SIRTerminator::Return,
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };
        eu.verify_result().unwrap();

        PhiOutcomeCompressionPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        assert_eq!(eu.blocks[&BlockId(4)].params.len(), 1);
        assert_eq!(
            eu.register_map[&eu.blocks[&BlockId(4)].params[0]].width(),
            2
        );
        for predecessor in [1, 3, 8, 9].map(BlockId) {
            let SIRTerminator::Jump(_, arguments) = &eu.blocks[&predecessor].terminator else {
                unreachable!()
            };
            assert_eq!(arguments.len(), 1);
        }
        for (block_id, old_parameter) in [(4, 4), (5, 5), (6, 6)] {
            let SIRTerminator::Branch { cond, .. } = &eu.blocks[&BlockId(block_id)].terminator
            else {
                unreachable!()
            };
            assert_ne!(*cond, RegisterId(old_parameter));
            assert!(
                eu.blocks[&BlockId(block_id)]
                    .instructions
                    .iter()
                    .any(|instruction| matches!(
                        instruction,
                        SIRInstruction::Binary(_, _, BinaryOp::Eq | BinaryOp::Ne, _)
                    ))
            );
        }
    }
}
