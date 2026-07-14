//! Sparse conditional constant propagation and CFG cleanup for SIR.
//!
//! SIR is produced after a scheduler has already materialized a large amount
//! of mux-shaped dataflow.  A scheduler is allowed to do that, but the
//! resulting CFG still contains ordinary control-flow facts.  This pass uses
//! those facts in the usual compiler way: it propagates constants through
//! executable edges, folds only proven constant branches/muxes, removes
//! unreachable blocks, and then removes the now-dead pure definitions.
//!
//! The analysis is sparse in the SCCP sense.  It visits a block when the block
//! becomes executable or when a block argument/value lattice changes; it does
//! not enumerate paths.  Thus loops and joins are handled by a finite lattice
//! (`Unknown`, one exact constant, `Overdefined`) rather than by path cloning.

use super::pass_manager::ExecutionUnitPass;
use super::shared::{collect_all_used_registers, def_reg};
use crate::ir::*;
use crate::optimizer::PassOptions;
use crate::{HashMap, HashSet};
use num_bigint::BigUint;
use num_traits::{One, Zero};
use std::collections::VecDeque;

pub(super) struct ControlFlowSimplifyPass;

#[derive(Clone, Debug, PartialEq, Eq)]
enum LatticeValue {
    Unknown,
    Constant(SIRValue),
    Overdefined,
}

#[derive(Clone)]
struct Edge {
    target: BlockId,
    arguments: Vec<RegisterId>,
}

struct Analysis {
    executable: HashSet<BlockId>,
    values: HashMap<RegisterId, LatticeValue>,
}

impl ExecutionUnitPass for ControlFlowSimplifyPass {
    fn name(&self) -> &'static str {
        "control_flow_simplify"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, options: &PassOptions) {
        if eu.blocks.is_empty() || eu.verify_result().is_err() {
            return;
        }

        let mut changed = false;
        loop {
            let analysis = analyze(eu);

            // Rewrite only from the final SCCP lattice.  In particular, an
            // overdefined condition never gets treated as a boolean just
            // because one predecessor happened to carry a constant value.
            let sccp_changed = apply_sccp_rewrites(eu, &analysis);

            // Constant propagation handles values known independently of
            // control flow.  The second proof is deliberately different: a
            // block reached only through a dominating branch edge has a known
            // predicate even when the predicate itself is dynamic.  This is
            // the CFG fact needed to discard an arm that was materialized
            // before the branch.  It uses dominance, not a single-predecessor
            // or same-block heuristic.
            let dominated_mux_changed = simplify_dominated_muxes(eu, options.four_state);
            if !sccp_changed && !dominated_mux_changed {
                break;
            }
            changed = true;
        }

        if !changed {
            return;
        }

        // Mux replacement disconnects whole load/expression DAGs.  The
        // existing mark/sweep is linear in def-use edges and treats loads as
        // pure SIR values, which is exactly what is needed here.
        super::pass_vectorize_concat::remove_dead_definitions(eu);
        trim_dead_register_types(eu);

        debug_assert!(eu.verify_result().is_ok());

        // Four-state mode is intentionally not used as a blanket early exit:
        // exact, mask-free branch constants are valid in four-state SIR too.
        // Muxes with an unknown condition never enter `exact_truth`, so their
        // X/Z merge semantics remain untouched.
    }
}

fn apply_sccp_rewrites(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, analysis: &Analysis) -> bool {
    let mut changed = false;
    let mut executable = analysis.executable.iter().copied().collect::<Vec<_>>();
    executable.sort_unstable_by_key(|id| id.0);
    for block_id in executable {
        let Some(block) = eu.blocks.get_mut(&block_id) else {
            continue;
        };

        for instruction in &mut block.instructions {
            let replacement = match instruction {
                SIRInstruction::Mux(dst, condition, then_value, else_value) => {
                    if then_value == else_value {
                        Some((*dst, *then_value))
                    } else {
                        exact_truth(analysis.values.get(condition))
                            .map(|truth| (*dst, if truth { *then_value } else { *else_value }))
                    }
                }
                _ => None,
            };
            if let Some((dst, selected)) = replacement {
                *instruction = SIRInstruction::Unary(dst, UnaryOp::Ident, selected);
                changed = true;
            }
        }

        let replacement = match &block.terminator {
            SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } => exact_truth(analysis.values.get(cond)).map(|truth| {
                if truth {
                    SIRTerminator::Jump(true_block.0, true_block.1.clone())
                } else {
                    SIRTerminator::Jump(false_block.0, false_block.1.clone())
                }
            }),
            _ => None,
        };
        if let Some(terminator) = replacement {
            block.terminator = terminator;
            changed = true;
        }
    }

    // The executable-block set was computed from the same monotone edge
    // analysis, so these blocks cannot be reached after the folded edges are
    // installed.  Removing them before DCE is important: otherwise a later
    // native lowering still sees their definitions in the EU.
    let unreachable = eu
        .blocks
        .keys()
        .copied()
        .filter(|id| !analysis.executable.contains(id))
        .collect::<Vec<_>>();
    if !unreachable.is_empty() {
        changed = true;
        for block_id in unreachable {
            eu.blocks.remove(&block_id);
        }
    }
    changed
}

fn simplify_dominated_muxes(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    four_state: bool,
) -> bool {
    if !eu.blocks.values().any(|block| {
        block
            .instructions
            .iter()
            .any(|inst| matches!(inst, SIRInstruction::Mux(..)))
    }) {
        return false;
    }
    let predecessors = super::pass_guarded_region_sinking::predecessor_map(eu);
    let dominators = super::pass_guarded_region_sinking::Dominators::compute(eu, &predecessors);
    let parameter_facts = collect_edge_parameter_facts(eu);
    let mut branch_facts = HashMap::<RegisterId, Vec<(BlockId, BlockId, bool)>>::default();
    for block in eu.blocks.values() {
        if let SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } = &block.terminator
        {
            branch_facts.entry(*cond).or_default().extend([
                (block.id, true_block.0, true),
                (block.id, false_block.0, false),
            ]);
        }
    }

    let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_unstable_by_key(|id| id.0);
    let mut changed = false;
    for block_id in block_ids {
        let Some(block) = eu.blocks.get_mut(&block_id) else {
            continue;
        };
        let mut aliases = HashMap::<RegisterId, (RegisterId, bool)>::default();
        for instruction in &mut block.instructions {
            if let SIRInstruction::Mux(dst, condition, then_value, else_value) = instruction {
                let (root, inverted) = resolve_condition_alias(*condition, &aliases);
                let mut proven = parameter_facts
                    .get(&(block_id, root))
                    .copied()
                    .map(|truth| truth ^ inverted);
                if let Some(facts) = branch_facts.get(&root) {
                    for &(source, successor, truth) in facts {
                        if source != block_id && dominators.dominates(successor, block_id) {
                            let truth = truth ^ inverted;
                            if proven.is_some_and(|previous| previous != truth) {
                                proven = None;
                                break;
                            }
                            proven = Some(truth);
                        }
                    }
                }
                if let Some(truth) = proven {
                    *instruction = SIRInstruction::Unary(
                        *dst,
                        UnaryOp::Ident,
                        if truth { *then_value } else { *else_value },
                    );
                    changed = true;
                }
            }

            match instruction {
                SIRInstruction::Unary(dst, UnaryOp::Ident | UnaryOp::ToTwoState, source) => {
                    if let Some(&(root, inverted)) = aliases.get(source) {
                        aliases.insert(*dst, (root, inverted));
                    } else {
                        aliases.insert(*dst, (*source, false));
                    }
                }
                SIRInstruction::Unary(dst, UnaryOp::LogicNot, source) if !four_state => {
                    if let Some(&(root, inverted)) = aliases.get(source) {
                        aliases.insert(*dst, (root, !inverted));
                    } else {
                        aliases.insert(*dst, (*source, true));
                    }
                }
                _ => {}
            }
        }
    }
    changed
}

/// Prove a predicate carried through an SSA block argument.  Every incoming
/// edge must pass the branch condition itself, and every such edge must carry
/// the same truth value.  This is intentionally an all-incoming-edge proof;
/// one unproven edge makes the result unknown at the join.
fn collect_edge_parameter_facts(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<(BlockId, RegisterId), bool> {
    let mut incoming =
        HashMap::<BlockId, Vec<(Vec<RegisterId>, Option<(RegisterId, bool)>)>>::default();
    for block in eu.blocks.values() {
        match &block.terminator {
            SIRTerminator::Jump(target, arguments) => {
                incoming
                    .entry(*target)
                    .or_default()
                    .push((arguments.clone(), None));
            }
            SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } => {
                incoming
                    .entry(true_block.0)
                    .or_default()
                    .push((true_block.1.clone(), Some((*cond, true))));
                incoming
                    .entry(false_block.0)
                    .or_default()
                    .push((false_block.1.clone(), Some((*cond, false))));
            }
            SIRTerminator::Return | SIRTerminator::Error(_) => {}
        }
    }

    let mut facts = HashMap::default();
    for (&target, edges) in &incoming {
        let Some(block) = eu.blocks.get(&target) else {
            continue;
        };
        for (index, &parameter) in block.params.iter().enumerate() {
            let mut proven = None;
            let mut valid = true;
            for (arguments, branch) in edges {
                let Some((condition, truth)) = branch else {
                    valid = false;
                    break;
                };
                if arguments.get(index) != Some(condition) {
                    valid = false;
                    break;
                }
                if proven.is_some_and(|previous| previous != *truth) {
                    valid = false;
                    break;
                }
                proven = Some(*truth);
            }
            if valid {
                if let Some(proven) = proven {
                    facts.insert((target, parameter), proven);
                }
            }
        }
    }
    facts
}

fn resolve_condition_alias(
    mut register: RegisterId,
    aliases: &HashMap<RegisterId, (RegisterId, bool)>,
) -> (RegisterId, bool) {
    let mut inverted = false;
    let mut steps = 0usize;
    while let Some(&(next, next_inverted)) = aliases.get(&register) {
        register = next;
        inverted ^= next_inverted;
        steps += 1;
        if steps > aliases.len() {
            break;
        }
    }
    (register, inverted)
}

fn analyze(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> Analysis {
    let mut edges = HashMap::<BlockId, Vec<Edge>>::default();
    let mut users = HashMap::<RegisterId, HashSet<BlockId>>::default();

    for (&block_id, block) in &eu.blocks {
        for instruction in &block.instructions {
            for register in instruction_uses(instruction) {
                users.entry(register).or_default().insert(block_id);
            }
        }
        for register in terminator_uses(&block.terminator) {
            users.entry(register).or_default().insert(block_id);
        }

        let outgoing = match &block.terminator {
            SIRTerminator::Jump(target, arguments) => vec![Edge {
                target: *target,
                arguments: arguments.clone(),
            }],
            SIRTerminator::Branch {
                cond: _,
                true_block,
                false_block,
            } => vec![
                Edge {
                    target: true_block.0,
                    arguments: true_block.1.clone(),
                },
                Edge {
                    target: false_block.0,
                    arguments: false_block.1.clone(),
                },
            ],
            SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
        };
        edges.insert(block_id, outgoing);
    }

    let mut values = HashMap::<RegisterId, LatticeValue>::default();
    let mut executable = HashSet::default();
    let mut queued = HashSet::default();
    let mut worklist = VecDeque::new();

    let enqueue = |block: BlockId,
                   executable: &HashSet<BlockId>,
                   queued: &mut HashSet<BlockId>,
                   worklist: &mut VecDeque<BlockId>| {
        if executable.contains(&block) && queued.insert(block) {
            worklist.push_back(block);
        }
    };

    executable.insert(eu.entry_block_id);
    queued.insert(eu.entry_block_id);
    worklist.push_back(eu.entry_block_id);

    while let Some(block_id) = worklist.pop_front() {
        queued.remove(&block_id);
        let Some(block) = eu.blocks.get(&block_id) else {
            continue;
        };

        for instruction in &block.instructions {
            let Some(dst) = def_reg(instruction) else {
                continue;
            };
            let result = evaluate_instruction(instruction, &values, &eu.register_map);
            if merge_value(&mut values, dst, result) {
                if let Some(blocks) = users.get(&dst) {
                    for &user in blocks {
                        enqueue(user, &executable, &mut queued, &mut worklist);
                    }
                }
            }
        }

        let selected_edges = match &block.terminator {
            SIRTerminator::Jump(..) => vec![0usize],
            SIRTerminator::Branch { cond, .. } => match exact_truth(values.get(cond)) {
                Some(true) => vec![0],
                Some(false) => vec![1],
                None => vec![0, 1],
            },
            SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
        };

        if let Some(outgoing) = edges.get(&block_id) {
            for edge_index in selected_edges {
                let Some(edge) = outgoing.get(edge_index) else {
                    continue;
                };
                let target = edge.target;
                if !eu.blocks.contains_key(&target) {
                    continue;
                }
                let new_block = executable.insert(target);
                if new_block {
                    enqueue(target, &executable, &mut queued, &mut worklist);
                }

                if let Some(target_block) = eu.blocks.get(&target) {
                    for (&parameter, &argument) in
                        target_block.params.iter().zip(edge.arguments.iter())
                    {
                        let argument_value = values
                            .get(&argument)
                            .cloned()
                            .unwrap_or(LatticeValue::Unknown);
                        if merge_value(&mut values, parameter, argument_value) {
                            if let Some(blocks) = users.get(&parameter) {
                                for &user in blocks {
                                    enqueue(user, &executable, &mut queued, &mut worklist);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Analysis { executable, values }
}

fn merge_value(
    values: &mut HashMap<RegisterId, LatticeValue>,
    register: RegisterId,
    incoming: LatticeValue,
) -> bool {
    if matches!(incoming, LatticeValue::Unknown) {
        return false;
    }
    let current = values.entry(register).or_insert(LatticeValue::Unknown);
    let next = match (&*current, incoming) {
        (LatticeValue::Unknown, value) => value,
        (_, LatticeValue::Unknown) => return false,
        (LatticeValue::Overdefined, _) => LatticeValue::Overdefined,
        (LatticeValue::Constant(old), LatticeValue::Constant(new)) if old == &new => return false,
        (LatticeValue::Constant(_), LatticeValue::Constant(_))
        | (LatticeValue::Constant(_), LatticeValue::Overdefined) => LatticeValue::Overdefined,
    };
    if *current == next {
        false
    } else {
        *current = next;
        true
    }
}

fn exact_truth(value: Option<&LatticeValue>) -> Option<bool> {
    let LatticeValue::Constant(value) = value? else {
        return None;
    };
    if !value.mask.is_zero() {
        return None;
    }
    Some(!value.payload.is_zero())
}

fn evaluate_instruction(
    instruction: &SIRInstruction<RegionedAbsoluteAddr>,
    values: &HashMap<RegisterId, LatticeValue>,
    types: &HashMap<RegisterId, RegisterType>,
) -> LatticeValue {
    let state = |register: RegisterId| {
        values
            .get(&register)
            .cloned()
            .unwrap_or(LatticeValue::Unknown)
    };
    let unary =
        |source: RegisterId, f: fn(&SIRValue, &RegisterType) -> Option<SIRValue>| match state(
            source,
        ) {
            LatticeValue::Unknown => LatticeValue::Unknown,
            LatticeValue::Overdefined => LatticeValue::Overdefined,
            LatticeValue::Constant(value) => types
                .get(&source)
                .and_then(|ty| f(&value, ty))
                .map(LatticeValue::Constant)
                .unwrap_or(LatticeValue::Overdefined),
        };
    let binary = |lhs: RegisterId,
                  rhs: RegisterId,
                  f: &dyn Fn(&SIRValue, &SIRValue, &RegisterType) -> Option<SIRValue>,
                  dst: RegisterId| {
        match (state(lhs), state(rhs)) {
            (LatticeValue::Unknown, _) | (_, LatticeValue::Unknown) => LatticeValue::Unknown,
            (LatticeValue::Overdefined, _) | (_, LatticeValue::Overdefined) => {
                LatticeValue::Overdefined
            }
            (LatticeValue::Constant(lhs), LatticeValue::Constant(rhs)) => types
                .get(&dst)
                .and_then(|ty| f(&lhs, &rhs, ty))
                .map(LatticeValue::Constant)
                .unwrap_or(LatticeValue::Overdefined),
        }
    };

    match instruction {
        SIRInstruction::Imm(_, value) => LatticeValue::Constant(value.clone()),
        SIRInstruction::Unary(dst, op, source) => match op {
            UnaryOp::Ident => state(*source),
            UnaryOp::ToTwoState => unary(*source, |value, ty| {
                let width_mask = width_mask(ty.width());
                Some(SIRValue::new(
                    (&value.payload & (&width_mask ^ &value.mask)) & width_mask,
                ))
            }),
            UnaryOp::LogicNot => unary(*source, |value, _| {
                if value.mask.is_zero() {
                    Some(SIRValue::new(if value.payload.is_zero() {
                        1u8
                    } else {
                        0u8
                    }))
                } else {
                    None
                }
            }),
            UnaryOp::Or => unary(*source, |value, _| {
                value
                    .mask
                    .is_zero()
                    .then(|| SIRValue::new(if value.payload.is_zero() { 0u8 } else { 1u8 }))
            }),
            UnaryOp::And => unary(*source, |value, ty| {
                value.mask.is_zero().then(|| {
                    SIRValue::new(if value.payload == width_mask(ty.width()) {
                        1u8
                    } else {
                        0u8
                    })
                })
            }),
            UnaryOp::Xor => unary(*source, |value, _| {
                value.mask.is_zero().then(|| {
                    let parity = value
                        .payload
                        .to_u64_digits()
                        .into_iter()
                        .map(|digit| digit.count_ones())
                        .sum::<u32>()
                        & 1;
                    SIRValue::new(parity as u8)
                })
            }),
            UnaryOp::BitNot => unary(*source, |value, ty| {
                value.mask.is_zero().then(|| {
                    SIRValue::new(
                        width_mask(ty.width()) ^ (&value.payload & width_mask(ty.width())),
                    )
                })
            }),
            UnaryOp::Minus => unary(*source, |value, ty| {
                value.mask.is_zero().then(|| {
                    let mask = width_mask(ty.width());
                    SIRValue::new(((&mask + BigUint::one()) - &value.payload) & mask)
                })
            }),
            UnaryOp::PopCount | UnaryOp::CountLeadingZeros | UnaryOp::CountTrailingZeros => {
                let _ = dst;
                LatticeValue::Overdefined
            }
        },
        SIRInstruction::Binary(dst, lhs, op, rhs) => match op {
            BinaryOp::LogicAnd => binary(
                *lhs,
                *rhs,
                &|lhs, rhs, _| {
                    if lhs.mask.is_zero() && rhs.mask.is_zero() {
                        Some(SIRValue::new(
                            if !lhs.payload.is_zero() && !rhs.payload.is_zero() {
                                1u8
                            } else {
                                0u8
                            },
                        ))
                    } else {
                        None
                    }
                },
                *dst,
            ),
            BinaryOp::LogicOr => binary(
                *lhs,
                *rhs,
                &|lhs, rhs, _| {
                    if lhs.mask.is_zero() && rhs.mask.is_zero() {
                        Some(SIRValue::new(
                            if !lhs.payload.is_zero() || !rhs.payload.is_zero() {
                                1u8
                            } else {
                                0u8
                            },
                        ))
                    } else {
                        None
                    }
                },
                *dst,
            ),
            BinaryOp::Eq | BinaryOp::EqWildcard => binary(
                *lhs,
                *rhs,
                &|lhs, rhs, _| {
                    (lhs.mask.is_zero() && rhs.mask.is_zero())
                        .then(|| SIRValue::new((lhs.payload == rhs.payload) as u8))
                },
                *dst,
            ),
            BinaryOp::Ne | BinaryOp::NeWildcard => binary(
                *lhs,
                *rhs,
                &|lhs, rhs, _| {
                    (lhs.mask.is_zero() && rhs.mask.is_zero())
                        .then(|| SIRValue::new((lhs.payload != rhs.payload) as u8))
                },
                *dst,
            ),
            BinaryOp::And | BinaryOp::Or | BinaryOp::Xor => binary(
                *lhs,
                *rhs,
                &|lhs, rhs, ty| {
                    if !lhs.mask.is_zero() || !rhs.mask.is_zero() {
                        return None;
                    }
                    let payload = match op {
                        BinaryOp::And => &lhs.payload & &rhs.payload,
                        BinaryOp::Or => &lhs.payload | &rhs.payload,
                        BinaryOp::Xor => &lhs.payload ^ &rhs.payload,
                        _ => unreachable!(),
                    };
                    Some(SIRValue::new(payload & width_mask(ty.width())))
                },
                *dst,
            ),
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul => binary(
                *lhs,
                *rhs,
                &|lhs, rhs, ty| {
                    if !lhs.mask.is_zero() || !rhs.mask.is_zero() {
                        return None;
                    }
                    let mask = width_mask(ty.width());
                    let payload = match op {
                        BinaryOp::Add => &lhs.payload + &rhs.payload,
                        BinaryOp::Sub => (&lhs.payload + &mask + BigUint::one()) - &rhs.payload,
                        BinaryOp::Mul => &lhs.payload * &rhs.payload,
                        _ => unreachable!(),
                    } & &mask;
                    Some(SIRValue::new(payload))
                },
                *dst,
            ),
            _ => LatticeValue::Overdefined,
        },
        SIRInstruction::Load(..) => LatticeValue::Overdefined,
        SIRInstruction::Store(..)
        | SIRInstruction::Commit(..)
        | SIRInstruction::RuntimeEvent { .. }
        | SIRInstruction::CombCaptureEvent { .. }
        | SIRInstruction::CombCaptureEnableIfChanged { .. } => LatticeValue::Overdefined,
        SIRInstruction::Concat(dst, arguments) => {
            let mut payload = BigUint::zero();
            let mut width = 0usize;
            for &argument in arguments.iter().rev() {
                match state(argument) {
                    LatticeValue::Unknown => return LatticeValue::Unknown,
                    LatticeValue::Overdefined => return LatticeValue::Overdefined,
                    LatticeValue::Constant(value) => {
                        if !value.mask.is_zero() {
                            return LatticeValue::Overdefined;
                        }
                        let Some(argument_width) = types.get(&argument).map(RegisterType::width)
                        else {
                            return LatticeValue::Overdefined;
                        };
                        payload |= (&value.payload & width_mask(argument_width)) << width;
                        width = width.saturating_add(argument_width);
                    }
                }
            }
            let Some(result_width) = types.get(dst).map(RegisterType::width) else {
                return LatticeValue::Overdefined;
            };
            LatticeValue::Constant(SIRValue::new(payload & width_mask(result_width)))
        }
        SIRInstruction::Slice(_dst, source, offset, width) => match state(*source) {
            LatticeValue::Unknown => LatticeValue::Unknown,
            LatticeValue::Overdefined => LatticeValue::Overdefined,
            LatticeValue::Constant(value) => {
                if !value.mask.is_zero() {
                    LatticeValue::Overdefined
                } else {
                    LatticeValue::Constant(SIRValue::new(
                        (&value.payload >> *offset) & width_mask(*width),
                    ))
                }
            }
        },
        SIRInstruction::Mux(_dst, condition, then_value, else_value) => {
            let then_state = state(*then_value);
            let else_state = state(*else_value);
            if *then_value == *else_value {
                return then_state;
            }
            match exact_truth(values.get(condition)) {
                Some(truth) => {
                    if truth {
                        then_state
                    } else {
                        else_state
                    }
                }
                None => {
                    if then_state == else_state {
                        then_state
                    } else if matches!(then_state, LatticeValue::Unknown)
                        || matches!(else_state, LatticeValue::Unknown)
                    {
                        LatticeValue::Unknown
                    } else {
                        LatticeValue::Overdefined
                    }
                }
            }
        }
    }
}

fn width_mask(width: usize) -> BigUint {
    if width == 0 {
        BigUint::zero()
    } else {
        (BigUint::one() << width) - BigUint::one()
    }
}

fn instruction_uses(instruction: &SIRInstruction<RegionedAbsoluteAddr>) -> Vec<RegisterId> {
    match instruction {
        SIRInstruction::Imm(..) => Vec::new(),
        SIRInstruction::Binary(_, lhs, _, rhs) => vec![*lhs, *rhs],
        SIRInstruction::Unary(_, _, source) | SIRInstruction::Slice(_, source, _, _) => {
            vec![*source]
        }
        SIRInstruction::Load(_, _, offset, _) => {
            offset.dynamic_registers().into_iter().flatten().collect()
        }
        SIRInstruction::Store(_, offset, _, source, _, _) => offset
            .dynamic_registers()
            .into_iter()
            .flatten()
            .chain(std::iter::once(*source))
            .collect(),
        SIRInstruction::Commit(_, _, offset, _, _) => {
            offset.dynamic_registers().into_iter().flatten().collect()
        }
        SIRInstruction::Concat(_, arguments)
        | SIRInstruction::RuntimeEvent {
            args: arguments, ..
        }
        | SIRInstruction::CombCaptureEvent {
            args: arguments, ..
        } => arguments.clone(),
        SIRInstruction::Mux(_, condition, then_value, else_value) => {
            vec![*condition, *then_value, *else_value]
        }
        SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => vec![*old, *new],
    }
}

fn terminator_uses(terminator: &SIRTerminator) -> Vec<RegisterId> {
    match terminator {
        SIRTerminator::Jump(_, arguments) => arguments.clone(),
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => std::iter::once(*cond)
            .chain(true_block.1.iter().copied())
            .chain(false_block.1.iter().copied())
            .collect(),
        SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
    }
}

fn trim_dead_register_types(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    let used = collect_all_used_registers(eu);
    let mut live_defs = HashSet::default();
    for block in eu.blocks.values() {
        live_defs.extend(block.params.iter().copied());
        for instruction in &block.instructions {
            if let Some(register) = def_reg(instruction) {
                live_defs.insert(register);
            }
        }
    }
    eu.register_map
        .retain(|register, _| live_defs.contains(register) || used.contains(register));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::InstanceId;
    use veryl_analyzer::ir::VarId;

    fn bit(width: usize) -> RegisterType {
        RegisterType::Bit {
            width,
            signed: false,
        }
    }

    fn address() -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region: 0,
            instance_id: InstanceId(0),
            var_id: VarId::default(),
        }
    }

    fn constant_branch_unit() -> ExecutionUnit<RegionedAbsoluteAddr> {
        let mut register_map = HashMap::default();
        for register in 0..=4 {
            register_map.insert(RegisterId(register), bit(1));
        }
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [
                BasicBlock {
                    id: BlockId(0),
                    params: Vec::new(),
                    instructions: vec![
                        SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                        SIRInstruction::Imm(RegisterId(1), SIRValue::new(1u8)),
                        SIRInstruction::Imm(RegisterId(2), SIRValue::new(0u8)),
                    ],
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), Vec::new()),
                        false_block: (BlockId(2), Vec::new()),
                    },
                },
                BasicBlock {
                    id: BlockId(1),
                    params: Vec::new(),
                    instructions: vec![SIRInstruction::Mux(
                        RegisterId(3),
                        RegisterId(0),
                        RegisterId(1),
                        RegisterId(2),
                    )],
                    terminator: SIRTerminator::Return,
                },
                BasicBlock {
                    id: BlockId(2),
                    params: Vec::new(),
                    instructions: vec![SIRInstruction::Store(
                        address(),
                        SIROffset::Static(0),
                        1,
                        RegisterId(2),
                        Vec::new(),
                        Vec::new(),
                    )],
                    terminator: SIRTerminator::Return,
                },
            ]
            .into_iter()
            .map(|block| (block.id, block))
            .collect(),
            register_map,
        }
    }

    #[test]
    fn folds_constant_branch_and_drops_unreachable_arm() {
        let mut eu = constant_branch_unit();
        eu.verify_result().unwrap();

        ControlFlowSimplifyPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        assert!(!eu.blocks.contains_key(&BlockId(2)));
        assert!(matches!(
            eu.blocks[&BlockId(0)].terminator,
            SIRTerminator::Jump(BlockId(1), _)
        ));
        assert!(
            !eu.blocks[&BlockId(1)]
                .instructions
                .iter()
                .any(|instruction| matches!(instruction, SIRInstruction::Mux(..)))
        );
        assert!(!eu.register_map.contains_key(&RegisterId(2)));
    }

    #[test]
    fn does_not_fold_an_overdefined_join_condition() {
        let mut eu = constant_branch_unit();
        eu.blocks.get_mut(&BlockId(0)).unwrap().instructions[0] =
            SIRInstruction::Load(RegisterId(0), address(), SIROffset::Static(0), 1);
        eu.blocks.get_mut(&BlockId(1)).unwrap().instructions.clear();
        eu.blocks.get_mut(&BlockId(1)).unwrap().terminator =
            SIRTerminator::Jump(BlockId(3), Vec::new());
        eu.blocks.get_mut(&BlockId(2)).unwrap().instructions.clear();
        eu.blocks.get_mut(&BlockId(2)).unwrap().terminator =
            SIRTerminator::Jump(BlockId(3), Vec::new());
        eu.blocks.insert(
            BlockId(3),
            BasicBlock {
                id: BlockId(3),
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(1), RegisterId(2)),
                    SIRInstruction::Store(
                        address(),
                        SIROffset::Static(0),
                        1,
                        RegisterId(3),
                        Vec::new(),
                        Vec::new(),
                    ),
                ],
                terminator: SIRTerminator::Return,
            },
        );
        eu.verify_result().unwrap();

        ControlFlowSimplifyPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        assert!(matches!(
            eu.blocks[&BlockId(0)].terminator,
            SIRTerminator::Branch { .. }
        ));
        assert!(
            eu.blocks[&BlockId(3)]
                .instructions
                .iter()
                .any(|instruction| matches!(instruction, SIRInstruction::Mux(..)))
        );
    }

    #[test]
    fn uses_a_dominating_dynamic_branch_to_remove_a_mux_arm() {
        let mut eu = constant_branch_unit();
        eu.blocks.get_mut(&BlockId(0)).unwrap().instructions[0] =
            SIRInstruction::Load(RegisterId(0), address(), SIROffset::Static(0), 1);
        eu.blocks.get_mut(&BlockId(2)).unwrap().instructions.clear();
        eu.blocks.get_mut(&BlockId(2)).unwrap().terminator = SIRTerminator::Return;
        eu.verify_result().unwrap();

        ControlFlowSimplifyPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        assert!(matches!(
            eu.blocks[&BlockId(0)].terminator,
            SIRTerminator::Branch { .. }
        ));
        assert!(
            !eu.blocks[&BlockId(1)]
                .instructions
                .iter()
                .any(|instruction| matches!(instruction, SIRInstruction::Mux(..)))
        );
    }

    #[test]
    fn reapplies_sccp_after_dominance_mux_simplification() {
        let mut eu = constant_branch_unit();
        eu.blocks.get_mut(&BlockId(0)).unwrap().instructions[0] =
            SIRInstruction::Load(RegisterId(0), address(), SIROffset::Static(0), 1);
        eu.blocks.get_mut(&BlockId(1)).unwrap().terminator = SIRTerminator::Branch {
            cond: RegisterId(3),
            true_block: (BlockId(3), Vec::new()),
            false_block: (BlockId(4), Vec::new()),
        };
        eu.blocks.insert(
            BlockId(3),
            BasicBlock {
                id: BlockId(3),
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Return,
            },
        );
        eu.blocks.insert(
            BlockId(4),
            BasicBlock {
                id: BlockId(4),
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Return,
            },
        );
        eu.verify_result().unwrap();

        ControlFlowSimplifyPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        assert!(matches!(
            eu.blocks[&BlockId(1)].terminator,
            SIRTerminator::Jump(BlockId(3), _)
        ));
        assert!(!eu.blocks.contains_key(&BlockId(4)));
    }

    #[test]
    fn follows_a_branch_predicate_through_a_block_argument() {
        let mut eu = constant_branch_unit();
        eu.register_map.insert(RegisterId(5), bit(1));
        eu.blocks.get_mut(&BlockId(0)).unwrap().instructions[0] =
            SIRInstruction::Load(RegisterId(0), address(), SIROffset::Static(0), 1);
        eu.blocks.get_mut(&BlockId(0)).unwrap().terminator = SIRTerminator::Branch {
            cond: RegisterId(0),
            true_block: (BlockId(1), vec![RegisterId(0)]),
            false_block: (BlockId(2), Vec::new()),
        };
        eu.blocks.get_mut(&BlockId(1)).unwrap().params = vec![RegisterId(4)];
        eu.blocks.get_mut(&BlockId(1)).unwrap().instructions = vec![
            SIRInstruction::Mux(RegisterId(5), RegisterId(4), RegisterId(1), RegisterId(2)),
            SIRInstruction::Store(
                address(),
                SIROffset::Static(0),
                1,
                RegisterId(5),
                Vec::new(),
                Vec::new(),
            ),
        ];
        eu.verify_result().unwrap();

        ControlFlowSimplifyPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        assert!(
            !eu.blocks[&BlockId(1)]
                .instructions
                .iter()
                .any(|instruction| matches!(instruction, SIRInstruction::Mux(..)))
        );
    }
}
