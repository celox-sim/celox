//! Lower the sampled roots and indexed writes of one state transition together.
use super::{NextStateForm, ValueRoot};
use crate::condition_groups::{self, Merge, Write};
use crate::{HashMap, HashSet, NodeId, SLTNode, SLTNodeArena, SLTToSIRLowerer};
use celox_design::{SPARSE_WORKING_REGION, STABLE_REGION, WORKING_REGION};
use celox_sir::{
    BlockId, ExecutionUnit, RegisterId, SIRBuilder, SIRInstruction, SIROffset, SIRTerminator,
};
use std::{
    fmt::{Debug, Display},
    hash::Hash,
};

/// Where sampled writes go when lowering an edge.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Publication {
    /// Publish after sampling every input of the transition.
    Direct,
    /// Stage writes for an NBA phase shared with other transitions.
    Deferred,
}

impl Publication {
    fn region(self) -> u32 {
        match self {
            Self::Direct => STABLE_REGION,
            Self::Deferred => WORKING_REGION,
        }
    }
}

/// Sample every right-hand side, index and guard before publishing any write.
/// A split evaluation records indexed writes in sparse working storage for the
/// NBA phase. A direct evaluation can write the sampled values immediately.
pub fn lower_transition<
    A: Copy + Eq + Hash + Debug + Display,
    B: Copy + Eq + Hash + Debug + Display,
>(
    dag: &SLTNodeArena<A>,
    form: &NextStateForm<A>,
    publication: Publication,
    four_state: bool,
    address: impl Fn(A, u32) -> B,
    element_widths: &HashMap<A, usize>,
) -> Result<ExecutionUnit<B>, String> {
    Ok(lower_edge(
        dag,
        form,
        publication,
        four_state,
        address,
        element_widths,
        (&HashSet::default(), &HashSet::default()),
    )?
    .0)
}

/// A sampled store and its optional write enable, in the evaluation's register
/// namespace. A combined clock function can emit it at the original NBA site.
pub struct SampledStore<B> {
    pub store: SIRInstruction<B>,
    pub guard: Option<RegisterId>,
}

/// Sample a two-state graph, keeping selected destinations in SSA registers
/// until publication. Other destinations use ordinary deferred storage.
pub fn sample_transition<
    A: Copy + Eq + Hash + Debug + Display,
    B: Copy + Eq + Hash + Debug + Display,
>(
    dag: &SLTNodeArena<A>,
    form: &NextStateForm<A>,
    address: impl Fn(A, u32) -> B,
    element_widths: &HashMap<A, usize>,
    captured: &HashSet<A>,
) -> Result<(ExecutionUnit<B>, Vec<SampledStore<B>>), String> {
    sample_enabled_transition(
        dag,
        form,
        address,
        element_widths,
        captured,
        &HashSet::default(),
    )
}

/// A captured scalar in `unchanged` still holds its sampled old value at
/// publication. Its explicit feedback arm can leave that destination alone.
pub fn sample_enabled_transition<
    A: Copy + Eq + Hash + Debug + Display,
    B: Copy + Eq + Hash + Debug + Display,
>(
    dag: &SLTNodeArena<A>,
    form: &NextStateForm<A>,
    address: impl Fn(A, u32) -> B,
    element_widths: &HashMap<A, usize>,
    captured: &HashSet<A>,
    unchanged: &HashSet<A>,
) -> Result<(ExecutionUnit<B>, Vec<SampledStore<B>>), String> {
    lower_edge(
        dag,
        form,
        Publication::Deferred,
        false,
        address,
        element_widths,
        (captured, unchanged),
    )
}

fn lower_edge<A: Copy + Eq + Hash + Debug + Display, B: Copy + Eq + Hash + Debug + Display>(
    dag: &SLTNodeArena<A>,
    form: &NextStateForm<A>,
    publication: Publication,
    four_state: bool,
    address: impl Fn(A, u32) -> B,
    element_widths: &HashMap<A, usize>,
    (captured, unchanged): (&HashSet<A>, &HashSet<A>),
) -> Result<(ExecutionUnit<B>, Vec<SampledStore<B>>), String> {
    let mut samples = Vec::new();
    let mut mapped = SLTNodeArena::<B>::new();
    let mut cache = HashMap::default();
    let stable = |id: &A| address(*id, STABLE_REGION);
    let mut mapped_nodes = form
        .nodes()
        .map(|node| {
            dag.get(node)
                .map_addr(node, dag, &mut mapped, &mut cache, &stable)
                .map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if !four_state {
        crate::decode::reduce_two_state_decodes(&mut mapped, &mut mapped_nodes)
            .map_err(|error| error.to_string())?;
    }
    let element_widths = element_widths
        .iter()
        .map(|(id, w)| (stable(id), *w))
        .collect();
    let mut lowerer =
        SLTToSIRLowerer::new(four_state).with_unpacked_input_types(&mapped, &element_widths);
    let mut b = SIRBuilder::<B>::new();
    let mut lowered = HashMap::default();
    // A common enable is one decision for the entire bank. Sample active
    // data only on the enabled edge. A split evaluation must still leave
    // the old values in its working slots for the later commit.
    let enabled = (!four_state && form.ports.is_empty() && captured.is_empty())
        .then(|| {
            let mut enable = None;
            let mut active = Vec::with_capacity(mapped_nodes.len());
            for (root, node) in form.roots.iter().zip(&mapped_nodes) {
                let (cond, polarity, data) =
                    feedback_arm(&mapped, *node, stable(&root.target), root.access)?;
                if enable
                    .replace((cond, polarity))
                    .is_some_and(|old| old != (cond, polarity))
                {
                    return None;
                }
                active.push(data);
            }
            Some((enable?, active))
        })
        .flatten();
    let mut join = None;
    if let Some(((condition, polarity), active)) = enabled {
        let cond = lowerer.lower(&mut b, condition, &mapped, &mut lowered);
        let done = open_enabled_bank(
            &mut b,
            cond,
            polarity,
            form.roots.iter(),
            &address,
            publication,
        );
        mapped_nodes = active;
        join = Some(done);
    }
    // Deferred stores target a separate region. Consume each value as soon
    // as it is ready instead of keeping the whole clock's sampled state live.
    // Direct publication still needs every old-state read before any write.
    // A fold can run effects or exit with an error. Preserve the original
    // sampling order and finish evaluation before staging those transitions.
    let stream = publication == Publication::Deferred
        && !mapped
            .iter()
            .any(|node| matches!(node, SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. }));
    if !stream {
        let mut ordered = mapped_nodes.clone();
        ordered.sort_unstable_by_key(|node| node.0);
        ordered.dedup();
        for node in ordered {
            lowerer.lower(&mut b, node, &mapped, &mut lowered);
        }
    }
    let (root_nodes, port_nodes) = mapped_nodes.split_at(form.roots.len());
    // Deferred destinations cannot change any graph input. Keep a common
    // selector as control flow, even when the clock also owns unrelated
    // registers or RAM ports. Captured SSA values must dominate publication.
    let target = |i: usize| form.roots[i].target;
    let selector = |w: &Write| match *mapped.get(w.value) {
        SLTNode::Mux { cond, .. } => Some(cond),
        _ => None,
    };
    let group = stream && !four_state;
    let mut order: Vec<_> = root_nodes
        .iter()
        .enumerate()
        .map(|(index, &value)| Write { index, value })
        .collect();
    if group {
        group_staged_roots(
            &mut order,
            |i| address(target(i), WORKING_REGION),
            |i| captured.contains(&target(i)),
            selector,
        );
    }
    let mut enables = HashMap::default();
    for bank in order.chunk_by(|a, b| {
        group
            && !captured.contains(&target(a.index))
            && !captured.contains(&target(b.index))
            && selector(a).is_some_and(|cond| selector(b) == Some(cond))
    }) {
        if let [write] = bank {
            let i = write.index;
            let root = &form.roots[i];
            let capture = captured.contains(&root.target);
            let feedback = (capture && group && unchanged.contains(&root.target))
                .then(|| feedback_arm(&mapped, root_nodes[i], stable(&root.target), root.access))
                .flatten();
            let mut guard = None;
            let node = if let Some((cond, polarity, data)) = feedback {
                guard = Some(*enables.entry((cond, polarity)).or_insert_with(|| {
                    let value = lowerer.lower(&mut b, cond, &mapped, &mut lowered);
                    let truth = b.normalize_branch_condition(value);
                    if polarity {
                        return truth;
                    }
                    let inverse = b.alloc_bit(1, false);
                    b.emit(SIRInstruction::Unary(
                        inverse,
                        celox_sir::UnaryOp::LogicNot,
                        truth,
                    ));
                    inverse
                }));
                data
            } else {
                root_nodes[i]
            };
            let value = lowerer.lower(&mut b, node, &mapped, &mut lowered);
            let value = if join.is_some() || feedback.is_some() {
                lowerer.cast_reg_width(&mut b, value, crate::get_width(root.value, dag))
            } else {
                value
            };
            let region = if capture {
                STABLE_REGION
            } else {
                publication.region()
            };
            let store = root_store(&b, root, address(root.target, region), value)?;
            if capture {
                samples.push(SampledStore { store, guard });
            } else {
                b.emit(store);
            }
            continue;
        }
        let condition = selector(&bank[0]).expect("a bank of muxes");
        condition_groups::emit(
            bank.to_vec(),
            condition,
            &mapped,
            &mut lowerer,
            &mut b,
            &mut lowered,
            |writes, selector| {
                group_staged_roots(
                    writes,
                    |i| address(target(i), WORKING_REGION),
                    |_| false,
                    selector,
                );
            },
            |cond, group| {
                let feedback = |w: &Write| {
                    let root = &form.roots[w.index];
                    feedback_arm(&mapped, w.value, stable(&root.target), root.access).is_some()
                };
                if group.iter().all(feedback) {
                    Merge::Branch
                } else if !condition_groups::profitable(
                    &mapped,
                    cond,
                    group.iter().map(|w| w.value),
                ) {
                    Merge::Separate
                } else if group.iter().any(feedback) {
                    Merge::Branch
                } else {
                    Merge::BranchWithIndependentWrites
                }
            },
            |w, prepared, lowerer, b, cache, local| {
                let root = &form.roots[w.index];
                let working = address(root.target, WORKING_REGION);
                if matches!(mapped.get(w.value),SLTNode::Input {variable,access,index,..}
                    if *variable == stable(&root.target) && *access == root.access && index.is_empty())
                {
                    b.emit(SIRInstruction::Commit(
                        stable(&root.target),
                        working,
                        SIROffset::Static(root.access.lsb),
                        root.access.msb - root.access.lsb + 1,
                        Vec::new(),
                    ));
                } else {
                    let value = prepared.unwrap_or_else(|| {
                        let value = lowerer.lower(b, w.value, &mapped, cache);
                        local.extend(lowerer.take_scheduled_region_insertions());
                        value
                    });
                    let value = lowerer.cast_reg_width(b, value, crate::get_width(root.value, dag));
                    b.emit(root_store(b, root, working, value)?);
                }
                Ok::<_, String>(())
            },
        )?;
    }
    for (port, nodes) in form.ports.iter().zip(port_nodes.as_chunks::<3>().0) {
        let [index, value, guard] =
            nodes.map(|node| lowerer.lower(&mut b, node, &mapped, &mut lowered));
        let capture = captured.contains(&port.array);
        let region = if !capture && publication == Publication::Deferred {
            SPARSE_WORKING_REGION
        } else {
            STABLE_REGION
        };
        let store = SIRInstruction::Store(
            address(port.array, region),
            SIROffset::Element {
                index,
                element_width: port.element_width,
                bit_offset: port.access.lsb,
                dynamic_bit_offset: None,
            },
            port.access.msb - port.access.lsb + 1,
            value,
            Vec::new(),
            Vec::new(),
        );
        if capture {
            samples.push(SampledStore {
                store,
                guard: Some(b.normalize_branch_condition(guard)),
            });
        } else {
            let write = b.new_block();
            let next = b.new_block();
            b.seal_block(SIRTerminator::Branch {
                cond: guard,
                true_block: (write, Vec::new()),
                false_block: (next, Vec::new()),
            });
            b.switch_to_block(write);
            b.emit(store);
            b.seal_block(SIRTerminator::Jump(next, Vec::new()));
            b.switch_to_block(next);
        }
    }
    if let Some(done) = join {
        b.seal_block(SIRTerminator::Jump(done, Vec::new()));
        b.switch_to_block(done);
    }
    b.seal_block(SIRTerminator::Return);
    let (blocks, register_map, _) = b.drain();
    Ok((
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        },
        samples,
    ))
}

/// Enter the active branch and return its join. A split NBA commit needs
/// valid working values even on the hold edge. A direct write can skip it.
fn open_enabled_bank<'a, A: Copy + 'a, B>(
    b: &mut SIRBuilder<B>,
    cond: RegisterId,
    active_high: bool,
    roots: impl Iterator<Item = &'a ValueRoot<A>>,
    address: &impl Fn(A, u32) -> B,
    publication: Publication,
) -> BlockId {
    let active = b.new_block();
    let done = b.new_block();
    let hold = if publication == Publication::Deferred {
        b.new_block()
    } else {
        done
    };
    let (yes, no) = if active_high {
        (active, hold)
    } else {
        (hold, active)
    };
    b.seal_block(SIRTerminator::Branch {
        cond,
        true_block: (yes, Vec::new()),
        false_block: (no, Vec::new()),
    });
    if hold != done {
        b.switch_to_block(hold);
        for root in roots {
            b.emit(SIRInstruction::Commit(
                address(root.target, STABLE_REGION),
                address(root.target, WORKING_REGION),
                SIROffset::Static(root.access.lsb),
                root.access.msb - root.access.lsb + 1,
                Vec::new(),
            ));
        }
        b.seal_block(SIRTerminator::Jump(done, Vec::new()));
    }
    b.switch_to_block(active);
    done
}

/// A root mux whose inactive arm reads exactly the destination it writes.
fn feedback_arm<A: Clone + Eq + Hash>(
    arena: &SLTNodeArena<A>,
    node: NodeId,
    target: A,
    range: celox_design::BitAccess,
) -> Option<(NodeId, bool, NodeId)> {
    let SLTNode::Mux {
        cond,
        then_expr,
        else_expr,
    } = *arena.get(node)
    else {
        return None;
    };
    let holds = |node| {
        matches!(arena.get(node), SLTNode::Input {variable, access, index, ..}
        if *variable == target && *access == range && index.is_empty())
    };
    if holds(else_expr) {
        Some((cond, true, then_expr))
    } else if holds(then_expr) {
        Some((cond, false, else_expr))
    } else {
        None
    }
}

/// Reads in a pure staged transition see only stable state. Gather independent
/// writes at the first use of their selector. Keep captured values in place,
/// and preserve source order for multiple writes to the same staging slot.
fn group_staged_roots<B: Copy + Eq + Hash>(
    order: &mut [Write],
    target: impl Fn(usize) -> B,
    barrier: impl Fn(usize) -> bool,
    selector: impl Fn(&Write) -> Option<NodeId>,
) {
    let mut seen = HashSet::default();
    let repeated: HashSet<_> = order
        .iter()
        .map(|w| target(w.index))
        .filter(|&addr| !seen.insert(addr))
        .collect();
    for run in order.split_mut(|item| barrier(item.index) || repeated.contains(&target(item.index)))
    {
        let mut first = HashMap::default();
        for i in run.iter() {
            if let Some(cond) = selector(i) {
                first.entry(cond).or_insert(i.index);
            }
        }
        run.sort_by_cached_key(|i| selector(i).map_or_else(|| i.index, |cond| first[&cond]));
    }
}

fn root_store<A, B: Copy + Eq + Hash + Debug + Display>(
    b: &SIRBuilder<B>,
    root: &ValueRoot<A>,
    address: B,
    value: RegisterId,
) -> Result<SIRInstruction<B>, String> {
    let width = root.access.msb - root.access.lsb + 1;
    if b.register(&value).width() != width {
        return Err(format!("root of {width} bits has a value of another width"));
    }
    Ok(SIRInstruction::Store(
        address,
        SIROffset::Static(root.access.lsb),
        width,
        value,
        Vec::new(),
        Vec::new(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use celox_design::BitAccess;

    fn input(arena: &mut SLTNodeArena<u32>, variable: u32, width: usize) -> NodeId {
        arena
            .alloc(SLTNode::Input {
                variable,
                access: BitAccess::new(0, width - 1),
                index: Vec::new(),
                signed: false,
            })
            .unwrap()
    }

    #[test]
    fn interleaved_registers_share_a_selector_in_a_mixed_sample() {
        let mut arena = SLTNodeArena::new();
        let cond = input(&mut arena, 0, 1);
        let [a, b, c] = [1, 2, 3].map(|id| input(&mut arena, id, 8));
        let narrow = input(&mut arena, 4, 3);
        let mut mux = |then_expr, else_expr| {
            arena
                .alloc(SLTNode::Mux {
                    cond,
                    then_expr,
                    else_expr,
                })
                .unwrap()
        };
        let root = |target, value| ValueRoot {
            target,
            value,
            access: BitAccess::new(0, 7),
        };
        let form = NextStateForm {
            roots: vec![
                root(10, mux(narrow, b)),
                root(11, c),
                root(12, mux(b, narrow)),
                root(13, a),
            ],
            ports: Vec::new(),
        };
        let address = |id, region| region * 100 + id;
        let (unit, captures) = sample_transition(
            &arena,
            &form,
            address,
            &HashMap::default(),
            &HashSet::from_iter([13]),
        )
        .unwrap();
        unit.verify();
        assert_eq!(captures.len(), 1);
        let writes: Vec<_> = unit
            .blocks
            .values()
            .map(|block| {
                block
                    .instructions
                    .iter()
                    .filter_map(|ins| match ins {
                        SIRInstruction::Store(addr, ..) => Some(*addr),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        let bank = [address(10, WORKING_REGION), address(12, WORKING_REGION)];
        assert_eq!(
            writes
                .iter()
                .filter(|block| block.as_slice() == bank)
                .count(),
            2
        );
    }

    #[test]
    fn split_reset_bank_stages_held_values_under_its_inner_enable() {
        let mut dag = SLTNodeArena::new();
        let reset = input(&mut dag, 0, 1);
        let disable = input(&mut dag, 1, 1);
        let data = input(&mut dag, 2, 3);
        let cleared = input(&mut dag, 3, 8);
        let mut form = NextStateForm::default();
        for target in [4, 5] {
            let old = input(&mut dag, target, 8);
            let next = dag
                .alloc(SLTNode::Mux {
                    cond: disable,
                    then_expr: old,
                    else_expr: data,
                })
                .unwrap();
            let value = dag
                .alloc(SLTNode::Mux {
                    cond: reset,
                    then_expr: cleared,
                    else_expr: next,
                })
                .unwrap();
            form.roots.push(ValueRoot {
                target,
                access: BitAccess::new(0, 7),
                value,
            });
        }
        let address = |id, region| region * 100 + id;
        let unit = lower_transition(
            &dag,
            &form,
            Publication::Deferred,
            false,
            address,
            &HashMap::default(),
        )
        .unwrap();
        unit.verify();
        assert!(unit.blocks.values().all(|b| {
            !b.instructions
                .iter()
                .any(|i| matches!(i, SIRInstruction::Mux(..)))
        }));
        let staged: Vec<_> = unit
            .blocks
            .values()
            .filter_map(|b| {
                let copies: Vec<_> = b
                    .instructions
                    .iter()
                    .filter_map(|i| match i {
                        SIRInstruction::Commit(src, dst, SIROffset::Static(0), 8, _) => {
                            Some((*src, *dst))
                        }
                        _ => None,
                    })
                    .collect();
                (!copies.is_empty()).then_some(copies)
            })
            .collect();
        assert_eq!(
            staged,
            [vec![
                (4, address(4, WORKING_REGION)),
                (5, address(5, WORKING_REGION))
            ]]
        );
    }

    #[test]
    fn shared_enable_widens_the_active_value_before_storing() {
        let mut arena = SLTNodeArena::new();
        let cond = input(&mut arena, 0, 1);
        let data = input(&mut arena, 1, 32);
        let hold = input(&mut arena, 2, 64);
        let mux = arena
            .alloc(SLTNode::Mux {
                cond,
                then_expr: data,
                else_expr: hold,
            })
            .unwrap();
        let form = NextStateForm {
            roots: vec![ValueRoot {
                target: 2,
                access: BitAccess::new(0, 63),
                value: mux,
            }],
            ports: Vec::new(),
        };
        for publication in [Publication::Direct, Publication::Deferred] {
            let unit = lower_transition(
                &arena,
                &form,
                publication,
                false,
                |id, region| region * 100 + id,
                &HashMap::default(),
            )
            .unwrap();
            unit.verify();
        }
    }

    #[test]
    fn overlapping_writes_and_captured_values_keep_their_positions() {
        let mut order = std::array::from_fn::<_, 8, _>(|index| Write {
            index,
            value: NodeId(index),
        });
        let slots = [0, 1, 2, 3, 3, 5, 6, 7];
        let guards = [0, 1, 0, 0, 1, 0, 1, 0];
        group_staged_roots(
            &mut order,
            |i| slots[i],
            |i| i == 6,
            |w| Some(NodeId(guards[w.index])),
        );
        assert_eq!(order.map(|w| w.index), [0, 2, 1, 3, 4, 5, 6, 7]);
    }
}
