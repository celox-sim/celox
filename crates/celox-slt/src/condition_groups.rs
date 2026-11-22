//! Emit a decision shared by scheduled writes, then repeat on each arm.
//! Callers supply the legal write order and store semantics for their region.
use crate::{HashMap, HashSet, NodeId, SLTNode, SLTNodeArena, SLTToSIRLowerer};
use celox_design::{BinaryOp, UnaryOp};
use celox_sir::{BlockId, RegisterId, SIRBuilder, SIRTerminator};
use std::{
    fmt::{Debug, Display},
    hash::Hash,
};

#[derive(Clone, Copy)]
pub(crate) struct Write {
    pub index: usize,
    pub value: NodeId,
}

fn arms<A: Clone + Eq + Hash>(
    arena: &SLTNodeArena<A>,
    node: NodeId,
) -> Option<(NodeId, NodeId, NodeId)> {
    match *arena.get(node) {
        SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } => Some((cond, then_expr, else_expr)),
        _ => None,
    }
}

/// Do not unfold a shared mux once per path to it. Even a small DAG can
/// contain an exponential number of such paths. Only mux arms are unfolded.
fn shared_muxes<A: Clone + Eq + Hash>(
    arena: &SLTNodeArena<A>,
    writes: &[Write],
) -> HashSet<NodeId> {
    let mut pending: Vec<_> = writes.iter().map(|w| w.value).collect();
    let mut seen = HashSet::default();
    let mut shared = HashSet::default();
    while let Some(node) = pending.pop() {
        if let Some((_, yes, no)) = arms(arena, node) {
            if seen.insert(node) {
                pending.extend([yes, no]);
            } else {
                shared.insert(node);
            }
        }
    }
    shared
}

fn collect_arm<A: Clone + Eq + Hash>(
    root: NodeId,
    arena: &SLTNodeArena<A>,
    nodes: &mut HashSet<NodeId>,
) -> bool {
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if !nodes.insert(node) {
            continue;
        }
        match arena.get(node) {
            SLTNode::Input { index, .. } => pending.extend(index.iter().map(|i| i.node)),
            SLTNode::Constant(..) => {}
            SLTNode::Binary(lhs, _, rhs) => pending.extend([*lhs, *rhs]),
            SLTNode::Unary(_, inner)
            | SLTNode::Slice { expr: inner, .. }
            | SLTNode::Capture { expr: inner, .. } => pending.push(*inner),
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => pending.extend([*cond, *then_expr, *else_expr]),
            SLTNode::Concat(parts) => pending.extend(parts.iter().map(|(n, _)| *n)),
            SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => return false,
        }
    }
    true
}

fn scheduled_node_cost<A: Clone + Eq + Hash>(node: NodeId, arena: &SLTNodeArena<A>) -> u128 {
    match arena.get(node) {
        SLTNode::Input { .. } | SLTNode::Constant(..) => 0,
        _ => crate::get_width(node, arena).div_ceil(64).max(1) as u128,
    }
}

fn scheduled_condition_probability<A: Clone + Eq + Hash>(
    mut condition: NodeId,
    arena: &SLTNodeArena<A>,
) -> (u128, u128) {
    let mut inverted = false;
    loop {
        match arena.get(condition) {
            SLTNode::Unary(UnaryOp::Ident | UnaryOp::ToTwoState, inner) => condition = *inner,
            SLTNode::Unary(UnaryOp::LogicNot, inner) => {
                inverted = !inverted;
                condition = *inner;
            }
            _ => break,
        }
    }
    let constant = |node| matches!(arena.get(node), SLTNode::Constant(..));
    let equality = matches!(
        arena.get(condition),
        SLTNode::Binary(lhs, BinaryOp::Eq | BinaryOp::EqWildcard, rhs)
            if constant(*lhs) || constant(*rhs)
    );
    let inequality = matches!(
        arena.get(condition),
        SLTNode::Binary(lhs, BinaryOp::Ne | BinaryOp::NeWildcard, rhs)
            if constant(*lhs) || constant(*rhs)
    );
    let true_weight = if equality {
        1
    } else if inequality {
        4
    } else {
        return (1, 2);
    };
    if inverted {
        (5 - true_weight, 5)
    } else {
        (true_weight, 5)
    }
}

pub(crate) fn profitable<A: Clone + Eq + Hash>(
    arena: &SLTNodeArena<A>,
    condition: NodeId,
    values: impl ExactSizeIterator<Item = NodeId>,
) -> bool {
    const CONTROL_COST: u128 = 2;
    const MISPREDICT_COST: u128 = 16;

    let mut true_nodes = HashSet::default();
    let mut false_nodes = HashSet::default();
    let count = values.len();
    for value in values {
        let Some((_, yes, no)) = arms(arena, value) else {
            return false;
        };
        if !collect_arm(yes, arena, &mut true_nodes) || !collect_arm(no, arena, &mut false_nodes) {
            return false;
        }
    }
    let true_owned = true_nodes
        .difference(&false_nodes)
        .map(|node| scheduled_node_cost(*node, arena))
        .sum::<u128>();
    let false_owned = false_nodes
        .difference(&true_nodes)
        .map(|node| scheduled_node_cost(*node, arena))
        .sum::<u128>();
    let (true_weight, total_weight) = scheduled_condition_probability(condition, arena);
    let false_weight = total_weight - true_weight;
    let removed_mux_cost = total_weight.saturating_mul(count as u128);
    let saved = false_weight
        .saturating_mul(true_owned)
        .saturating_add(true_weight.saturating_mul(false_owned))
        .saturating_add(removed_mux_cost);
    let introduced = total_weight.saturating_mul(CONTROL_COST).saturating_add(
        true_weight
            .min(false_weight)
            .saturating_mul(MISPREDICT_COST),
    );
    saved > introduced
}

/// Choose whether the selected values may all be evaluated before publication.
pub(crate) enum Merge {
    Separate,
    Branch,
    BranchWithIndependentWrites,
}

enum Task {
    Group {
        writes: Vec<Write>,
        decision: Option<(NodeId, Merge)>,
        return_values: bool,
    },
    Arm {
        block: BlockId,
        writes: Vec<Write>,
        join: BlockId,
        condition: NodeId,
        taken: bool,
        return_values: bool,
    },
    EndArm {
        join: BlockId,
        checkpoint: usize,
        condition: NodeId,
        outputs: Vec<(usize, usize)>,
    },
    Join {
        block: BlockId,
        outputs: Vec<(Write, RegisterId)>,
        return_values: bool,
    },
}

/// The initial group has already been scheduled under one common condition.
/// Scope cached SSA values to the arm defining them. Stable predicates can
/// also be substituted when encountered again below that arm.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit<A: Copy + Eq + Hash + Debug + Display, E>(
    writes: Vec<Write>,
    condition: NodeId,
    arena: &SLTNodeArena<A>,
    lowerer: &mut SLTToSIRLowerer,
    b: &mut SIRBuilder<A>,
    cache: &mut HashMap<NodeId, RegisterId>,
    reorder: impl Fn(&mut [Write], &dyn Fn(&Write) -> Option<NodeId>),
    mut merge: impl FnMut(NodeId, &[Write]) -> Merge,
    mut store: impl FnMut(
        Write,
        Option<RegisterId>,
        &mut SLTToSIRLowerer,
        &mut SIRBuilder<A>,
        &mut HashMap<NodeId, RegisterId>,
        &mut Vec<NodeId>,
    ) -> Result<(), E>,
) -> Result<(), E> {
    let shared = shared_muxes(arena, &writes);
    let selector = |w: &Write| {
        arms(arena, w.value)
            .map(|(c, ..)| c)
            .filter(|_| !shared.contains(&w.value))
    };
    let mut tasks = vec![Task::Group {
        writes,
        decision: Some((condition, Merge::Branch)),
        return_values: false,
    }];
    // Results are keyed by destination index, independent of arm scheduling order.
    let mut selected = HashMap::default();
    let mut insertions = Vec::new();
    let mut decisions = HashMap::default();
    while let Some(task) = tasks.pop() {
        match task {
            Task::Arm {
                block,
                mut writes,
                join,
                condition,
                taken,
                return_values,
            } => {
                b.switch_to_block(block);
                decisions.insert(condition, taken);
                let outputs = if return_values {
                    writes
                        .iter()
                        .map(|w| (w.index, crate::get_width(w.value, arena)))
                        .collect()
                } else {
                    Vec::new()
                };
                for w in &mut writes {
                    while let Some((cond, yes, no)) = arms(arena, w.value)
                        && let Some(&active) = decisions.get(&cond)
                    {
                        w.value = if active { yes } else { no };
                    }
                }
                tasks.push(Task::EndArm {
                    join,
                    checkpoint: insertions.len(),
                    condition,
                    outputs,
                });
                reorder(&mut writes, &selector);
                for group in writes.chunk_by(|a, b| selector(a) == selector(b)).rev() {
                    let cond = selector(&group[0]).filter(|_| group.len() > 1);
                    let decision = cond.and_then(|c| match merge(c, group) {
                        Merge::Separate => None,
                        kind => Some((c, kind)),
                    });
                    tasks.push(Task::Group {
                        writes: group.to_vec(),
                        decision,
                        return_values,
                    });
                }
            }
            Task::Group {
                writes,
                decision: Some((condition, kind)),
                return_values,
            } => {
                let cond = lowerer.lower(b, condition, arena, cache);
                insertions.extend(lowerer.take_scheduled_region_insertions());
                let yes = b.new_block();
                let no = b.new_block();
                // Every edge produces the same destinations. When later values
                // cannot observe earlier stores, publish once at their common join.
                let select = return_values || matches!(kind, Merge::BranchWithIndependentWrites);
                let outputs: Vec<_> = if select {
                    writes
                        .iter()
                        .map(|&w| (w, b.alloc_logic(crate::get_width(w.value, arena))))
                        .collect()
                } else {
                    Vec::new()
                };
                let join = b.new_block_with(outputs.iter().map(|&(_, param)| param).collect());
                b.seal_block(SIRTerminator::Branch {
                    cond,
                    true_block: (yes, Vec::new()),
                    false_block: (no, Vec::new()),
                });
                tasks.push(Task::Join {
                    block: join,
                    outputs,
                    return_values,
                });
                tasks.push(Task::Arm {
                    block: no,
                    writes: writes.clone(),
                    join,
                    condition,
                    taken: false,
                    return_values: select,
                });
                tasks.push(Task::Arm {
                    block: yes,
                    writes,
                    join,
                    condition,
                    taken: true,
                    return_values: select,
                });
            }
            Task::Group {
                writes,
                decision: None,
                return_values,
            } => {
                for w in writes {
                    if return_values {
                        let value = lowerer.lower(b, w.value, arena, cache);
                        insertions.extend(lowerer.take_scheduled_region_insertions());
                        selected.insert(w.index, value);
                    } else {
                        store(w, None, lowerer, b, cache, &mut insertions)?;
                    }
                }
            }
            Task::EndArm {
                join,
                checkpoint,
                condition,
                outputs,
            } => {
                decisions.remove(&condition);
                for node in insertions.drain(checkpoint..) {
                    cache.remove(&node);
                }
                let args = outputs
                    .into_iter()
                    .map(|(index, width)| {
                        let value = selected
                            .remove(&index)
                            .expect("each arm yields every requested value");
                        lowerer.cast_reg_width(b, value, width)
                    })
                    .collect();
                b.seal_block(SIRTerminator::Jump(join, args));
            }
            Task::Join {
                block,
                outputs,
                return_values,
            } => {
                b.switch_to_block(block);
                for (write, value) in outputs {
                    if return_values {
                        selected.insert(write.index, value);
                    } else {
                        store(write, Some(value), lowerer, b, cache, &mut insertions)?;
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use celox_design::BitAccess;

    #[test]
    fn reconvergent_muxes_remain_shared_values() {
        let mut arena = SLTNodeArena::new();
        let inputs: Vec<_> = (0u32..34)
            .map(|variable| {
                arena
                    .alloc(SLTNode::Input {
                        variable,
                        access: BitAccess::new(0, 0),
                        signed: false,
                        index: Vec::new(),
                    })
                    .unwrap()
            })
            .collect();
        let (mut a, mut b) = (inputs[0], inputs[1]);
        for &cond in &inputs[2..] {
            (a, b) = [(a, b), (b, a)]
                .map(|(then_expr, else_expr)| {
                    arena
                        .alloc(SLTNode::Mux {
                            cond,
                            then_expr,
                            else_expr,
                        })
                        .unwrap()
                })
                .into();
        }
        let writes = [a, b]
            .into_iter()
            .enumerate()
            .map(|(index, value)| Write { index, value })
            .collect();
        let mut leaves = 0;
        emit(
            writes,
            *inputs.last().unwrap(),
            &arena,
            &mut SLTToSIRLowerer::new(false),
            &mut SIRBuilder::new(),
            &mut HashMap::default(),
            |_, _| {},
            |_, _| Merge::Branch,
            |_, _, _, _, _, _| {
                leaves += 1;
                assert!(leaves <= 4);
                Ok::<_, ()>(())
            },
        )
        .unwrap();
        assert_eq!(leaves, 4);
    }
}
