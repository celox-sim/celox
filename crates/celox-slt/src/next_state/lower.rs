//! Lower the sampled roots and indexed writes of one state transition together.
use super::NextStateForm;
use crate::{HashMap, HashSet, SLTNode, SLTNodeArena, SLTToSIRLowerer};
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
        &HashSet::default(),
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
    lower_edge(
        dag,
        form,
        Publication::Deferred,
        false,
        address,
        element_widths,
        captured,
    )
}

fn lower_edge<A: Copy + Eq + Hash + Debug + Display, B: Copy + Eq + Hash + Debug + Display>(
    dag: &SLTNodeArena<A>,
    form: &NextStateForm<A>,
    publication: Publication,
    four_state: bool,
    address: impl Fn(A, u32) -> B,
    element_widths: &HashMap<A, usize>,
    captured: &HashSet<A>,
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
                let SLTNode::Mux {
                    cond,
                    then_expr,
                    else_expr,
                } = *mapped.get(*node)
                else {
                    return None;
                };
                let holds = |node| {
                    matches!(mapped.get(node), SLTNode::Input {variable, access, index, ..}
                if *variable == address(root.target, STABLE_REGION) && *access == root.access && index.is_empty())
                };
                let (polarity, data) = if holds(else_expr) {
                    (true, then_expr)
                } else if holds(then_expr) {
                    (false, else_expr)
                } else {
                    return None;
                };
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
        let taken = b.new_block();
        let done = b.new_block();
        let hold = if publication == Publication::Deferred {
            b.new_block()
        } else {
            done
        };
        let (yes, no) = if polarity {
            (taken, hold)
        } else {
            (hold, taken)
        };
        b.seal_block(SIRTerminator::Branch {
            cond,
            true_block: (yes, Vec::new()),
            false_block: (no, Vec::new()),
        });
        if hold != done {
            b.switch_to_block(hold);
            for root in &form.roots {
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
        b.switch_to_block(taken);
        mapped_nodes = active;
        join = Some(done);
    }
    // Materialize shared full-width roots before any root that selects
    // part of them. Keep publication in source order.
    let mut ordered = mapped_nodes.clone();
    ordered.sort_unstable_by_key(|node| node.0);
    ordered.dedup();
    for node in ordered {
        lowerer.lower(&mut b, node, &mapped, &mut lowered);
    }
    let registers: Vec<RegisterId> = mapped_nodes.iter().map(|id| lowered[id]).collect();
    let (root_registers, port_registers) = registers.split_at(form.roots.len());
    for (root, &value) in form.roots.iter().zip(root_registers) {
        let width = root.access.msb - root.access.lsb + 1;
        if b.register(&value).width() != width {
            return Err(format!("root of {width} bits has a value of another width"));
        }
        let capture = captured.contains(&root.target);
        let store = SIRInstruction::Store(
            address(
                root.target,
                if capture {
                    STABLE_REGION
                } else {
                    publication.region()
                },
            ),
            SIROffset::Static(root.access.lsb),
            width,
            value,
            Vec::new(),
            Vec::new(),
        );
        if capture {
            samples.push(SampledStore { store, guard: None });
        } else {
            b.emit(store);
        }
    }
    for (port, registers) in form.ports.iter().zip(port_registers.chunks(3)) {
        let &[index, value, guard] = registers else {
            unreachable!("a port computes three nodes");
        };
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
