//! The next-state DAG of each clock domain (`celox_slt::next_state`).
//!
//! The next-state forms that a frontend lowered (`SimModule::next_state`)
//! are relocated into the design's arena, one form per canonical clock. The
//! form of a domain then becomes a DAG of its own. Its nodes are interned
//! canonically (`celox_slt::canon::intern`). Each read of a combinational
//! net is replaced by the net's drivers, so the DAG reads only registers,
//! inputs, and the nets it cannot see through:
//! - nets a process drives;
//! - nets on a combinational loop;
//! - nets whose drivers have a loop fold;
//! - nets read at a run-time index;
//! - nets read as signed values.

mod samples;

use super::dependencies::SamplePlacement;
use super::*;
use crate::symbolic::artifact::EdgeEvaluation;
use celox_slt::SLTNode;
use celox_slt::canon::{intern, reachable, same_op_bit_runs};
use celox_slt::next_state::{NextStateForm, Publication, ValueRoot, lower_transition};

/// Lower a sampled clock domain in the address space of the relocated design.
pub(super) fn lower_clock(
    arena: &SLTNodeArena<AbsoluteAddr>,
    form: &NextStateForm<AbsoluteAddr>,
    elements: &HashMap<AbsoluteAddr, usize>,
) -> Result<ExecutionUnit<RegionedAbsoluteAddr>, String> {
    lower_transition(
        arena,
        form,
        Publication::Direct,
        false,
        |addr, region| RegionedAbsoluteAddr::from_absolute_addr(region, addr),
        elements,
    )
}

pub(super) struct EventSchedule {
    pub(super) graph_clocks: HashSet<AbsoluteAddr>,
    pub(super) write_domains: HashMap<AbsoluteAddr, Vec<AbsoluteAddr>>,
    pub(super) shared_calculations: HashSet<AbsoluteAddr>,
}

impl StateGraph {
    /// Resolve samples against the whole design before choosing executable
    /// kernels. Pure events can publish directly; every event also has a
    /// staged evaluation for simultaneous edges.
    pub(super) fn lower_events(
        &mut self,
        design: &Design,
        units: &mut Units,
        initial: &[crate::symbolic::artifact::InstanceValue],
        split_commits: &SplitCommits,
        shapes: &HashMap<AbsoluteAddr, (usize, bool)>,
        parallel: &ParallelScheduleOptions,
        four_state: bool,
        source_actions: &HashMap<AbsoluteAddr, Vec<EventAction>>,
        measure_graphs: bool,
    ) -> Result<EventSchedule, ParserError> {
        let active_writes: HashMap<_, HashSet<_>> = split_commits
            .iter()
            .map(|(&event, groups)| {
                let writes = groups
                    .iter()
                    .flatten()
                    .flat_map(|s| s.active.blocks.values())
                    .flat_map(|b| &b.instructions)
                    .flat_map(crate::symbolic::written_objects)
                    .filter(|a| a.region == STABLE_REGION)
                    .map(|a| a.absolute_addr())
                    .collect();
                (event, writes)
            })
            .collect();
        let graph_clocks = if !four_state && !parallel.enabled() {
            self.evaluations
                .iter()
                .filter_map(|(&event, plan)| {
                    (!source_actions.contains_key(&event)
                        && !plan.is_empty()
                        && active_writes.get(&event).is_none_or(HashSet::is_empty)
                        && plan
                            .iter()
                            .all(|batch| matches!(batch, EdgeEvaluation::Graph(_))))
                    .then_some(event)
                })
                .collect::<HashSet<_>>()
        } else {
            Default::default()
        };
        let dependencies = StateDependencies::new(self, units);
        let placement = if !four_state
            && !parallel.enabled()
            && source_actions.is_empty()
            && !self.evaluations.is_empty()
        {
            let clocks: HashSet<_> = self.evaluations.keys().copied().collect();
            // Estimate demand before inlining moves any of these producers.
            dependencies.sample_placement(
                &self.global_arena,
                event_nets(&clocks, &design.clock_domains),
                &self.evaluations,
            )
        } else {
            Default::default()
        };
        self.connect_samples(
            &dependencies,
            &placement,
            design,
            initial,
            shapes,
            &active_writes,
            parallel,
            four_state,
        )?;
        for (&event, plan) in &self.evaluations {
            if graph_clocks.contains(&event) || measure_graphs {
                let form = self.transitions.entry(event).or_default();
                for batch in plan {
                    if let EdgeEvaluation::Graph(graph) = batch {
                        form.roots.extend_from_slice(&graph.roots);
                        form.ports.extend_from_slice(&graph.ports);
                    }
                }
            }
        }
        lower_event_evaluations(
            std::mem::take(&mut self.evaluations),
            &self.global_arena,
            &design.unpacked_element_widths,
            &mut units.updates,
            &graph_clocks,
            four_state,
        )?;
        if measure_graphs {
            // A frontend may also provide non-executable forms to compare its
            // procedural or parallel lowering with a symbolic implementation.
            for (&instance, module) in &design.instance_modules {
                let module = &design.modules[module];
                for (trigger, form) in &module.next_state.forms {
                    if module.next_state.evaluations.contains_key(trigger) {
                        continue;
                    }
                    let locate = |id: &SourceVarId| design.bindings.locate(instance, *id);
                    let clock = locate(&trigger.clock);
                    let clock = design.clock_domains.get(&clock).copied().unwrap_or(clock);
                    let graph = form
                        .relocate(&module.arena, &mut self.global_arena, &locate)
                        .map_err(|e| {
                            ParserError::illegal_context(
                                "diagnostic next-state form",
                                e.to_string(),
                                None,
                            )
                        })?;
                    let domain = self.transitions.entry(clock).or_default();
                    domain.roots.extend(graph.roots);
                    domain.ports.extend(graph.ports);
                }
            }
        }
        Ok(EventSchedule {
            graph_clocks,
            write_domains: dependencies.write_domains(),
            shared_calculations: placement.shared,
        })
    }

    /// Select the writes and event signals needed by this edge. The runtime
    /// settles observable outputs after publication, so the tick only needs
    /// their drivers when a sampled value depends on them.
    pub(super) fn tick_programs(
        &self,
        clocks: &HashSet<AbsoluteAddr>,
        event_nets: &HashSet<AbsoluteAddr>,
        shared: &HashSet<AbsoluteAddr>,
        elements: &HashMap<AbsoluteAddr, usize>,
    ) -> EventUnits {
        if clocks.is_empty() {
            return Default::default();
        }
        let mut event_roots = Vec::new();
        let mut driven = HashMap::<AbsoluteAddr, Vec<BitAccess>>::default();
        for path in &self.comb_blocks {
            let LogicPathTarget::Var(target) = &path.target else {
                return Default::default();
            };
            if !path.previous_sources.is_empty()
                || !path.order_before.is_empty()
                || !path.comb_capture_enable_sites.is_empty()
                || !path.pre_lower_nodes.is_empty()
                || !path.local_inputs.is_empty()
                || elements.contains_key(&target.id)
                || self.global_arena.node_width(path.expr)
                    != Some(target.access.msb - target.access.lsb + 1)
            {
                return Default::default();
            }
            driven.entry(target.id).or_default().push(target.access);
            if event_nets.contains(&target.id) {
                event_roots.push(ValueRoot {
                    enable: None,
                    target: target.id,
                    access: target.access,
                    value: path.expr,
                });
            }
        }
        for ranges in driven.values_mut() {
            ranges.sort_unstable_by_key(|r| r.lsb);
            if ranges.windows(2).any(|w| w[0].msb >= w[1].lsb) {
                return Default::default();
            }
        }
        let mut drivers = Drivers::new(&self.global_arena, &self.comb_blocks);
        if drivers.nets.len() != driven.len() {
            return Default::default();
        }
        // A tick that needs a shared result runs after the shared settle.
        // The driven-leaf check below selects this path for that event.
        drivers.nets.retain(|net, _| !shared.contains(net));
        let mut ticks = EventUnits::default();
        for clock in clocks {
            let Some(edge) = self.transitions.get(clock) else {
                continue;
            };
            if edge.roots.iter().any(|r| driven.contains_key(&r.target))
                || edge.ports.iter().any(|p| driven.contains_key(&p.array))
            {
                continue;
            }
            let mut form = edge.clone();
            form.roots.extend_from_slice(&event_roots);
            let Ok(dag) = domain_dag(&drivers, &form) else {
                continue;
            };
            // A read still referring to a driven wire may need the current
            // settle to finish first (e.g. a signed read or partial driver).
            if dag.arena.iter().any(
                |node| matches!(node,SLTNode::Input{variable,..} if driven.contains_key(variable)),
            ) {
                continue;
            }
            if let Ok(unit) = lower_clock(&dag.arena, &dag.form, elements) {
                ticks.insert(*clock, vec![unit]);
            }
        }
        ticks
    }
}

/// The next-state DAG of one clock domain.
pub(super) struct DomainDag {
    pub arena: SLTNodeArena<AbsoluteAddr>,
    pub form: NextStateForm<AbsoluteAddr>,
    /// The net reads replaced by their drivers.
    pub substituted: usize,
    /// The nets read as leaves although combinational logic drives them.
    pub opaque_nets: usize,
}

/// The combinational drivers of each net that a DAG may see through.
struct Drivers<'a> {
    arena: &'a SLTNodeArena<AbsoluteAddr>,
    /// Each net's driven ranges and their expressions.
    nets: HashMap<AbsoluteAddr, Vec<(BitAccess, NodeId)>>,
}

impl<'a> Drivers<'a> {
    /// The drivers of the nets that `comb_blocks` drive by expression,
    /// without the nets that are on a loop, have a process writer or an
    /// expression with a node that has no canonical form.
    fn new(arena: &'a SLTNodeArena<AbsoluteAddr>, comb_blocks: &[LogicPath<AbsoluteAddr>]) -> Self {
        let mut nets: HashMap<AbsoluteAddr, Vec<(BitAccess, NodeId)>> = HashMap::default();
        let mut opaque: HashSet<AbsoluteAddr> = HashSet::default();
        let mut reads: HashMap<AbsoluteAddr, HashSet<AbsoluteAddr>> = HashMap::default();
        // The nodes with a node below them that has no canonical form. Node
        // ids are topological: operands come first.
        let mut folded = vec![false; arena.len()];
        for (index, node) in arena.iter().enumerate() {
            folded[index] = match node {
                SLTNode::ForFold { .. }
                | SLTNode::ForFoldGroup { .. }
                | SLTNode::Capture { .. } => true,
                SLTNode::Input { index, .. } => index.iter().any(|entry| folded[entry.node.0]),
                SLTNode::Binary(lhs, _, rhs) => folded[lhs.0] || folded[rhs.0],
                SLTNode::Unary(_, operand) | SLTNode::Slice { expr: operand, .. } => {
                    folded[operand.0]
                }
                SLTNode::Mux {
                    cond,
                    then_expr,
                    else_expr,
                } => folded[cond.0] || folded[then_expr.0] || folded[else_expr.0],
                SLTNode::Concat(parts) => parts.iter().any(|(part, _)| folded[part.0]),
                SLTNode::Constant(..) => false,
            };
        }
        for path in comb_blocks {
            match &path.target {
                LogicPathTarget::Var(atom) => {
                    nets.entry(atom.id)
                        .or_default()
                        .push((atom.access, path.expr));
                    reads
                        .entry(atom.id)
                        .or_default()
                        .extend(path.sources.iter().map(|source| source.id));
                    if folded[path.expr.0]
                        || !path.previous_sources.is_empty()
                        || !path.local_inputs.is_empty()
                        || !path.pre_lower_nodes.is_empty()
                        || !path.order_before.is_empty()
                        || !path.comb_capture_enable_sites.is_empty()
                    {
                        opaque.insert(atom.id);
                    }
                }
                LogicPathTarget::Process { unit, .. } => {
                    for block in unit.blocks.values() {
                        for instruction in &block.instructions {
                            opaque.extend(crate::symbolic::written_objects(instruction).copied());
                        }
                    }
                }
                LogicPathTarget::CombCaptureEvent { .. } => {}
            }
        }
        // A net that reads itself, through other nets or not, is evaluated
        // to a fixed point: the DAG reads its value.
        opaque.extend(cyclic(&reads));
        nets.retain(|net, _| !opaque.contains(net));
        Self { arena, nets }
    }
}

/// The nets on a cycle of `reads` (net -> the nets its drivers read).
fn cyclic(reads: &HashMap<AbsoluteAddr, HashSet<AbsoluteAddr>>) -> HashSet<AbsoluteAddr> {
    // Tarjan's algorithm, iteratively.
    let mut index: HashMap<AbsoluteAddr, (usize, usize)> = HashMap::default();
    let mut on_stack: HashSet<AbsoluteAddr> = HashSet::default();
    let mut stack: Vec<AbsoluteAddr> = Vec::new();
    let mut cyclic = HashSet::default();
    let mut next = 0;
    let mut roots: Vec<AbsoluteAddr> = reads.keys().copied().collect();
    roots.sort();
    let successors = |net: &AbsoluteAddr| -> Vec<AbsoluteAddr> {
        let mut successors: Vec<_> = reads
            .get(net)
            .into_iter()
            .flatten()
            .copied()
            .filter(|successor| reads.contains_key(successor))
            .collect();
        successors.sort();
        successors
    };
    for root in roots {
        if index.contains_key(&root) {
            continue;
        }
        let mut work: Vec<(AbsoluteAddr, Vec<AbsoluteAddr>)> = Vec::new();
        index.insert(root, (next, next));
        next += 1;
        stack.push(root);
        on_stack.insert(root);
        work.push((root, successors(&root)));
        while let Some((net, pending)) = work.last_mut() {
            let net = *net;
            if let Some(successor) = pending.pop() {
                if successor == net {
                    cyclic.insert(net);
                }
                match index.get(&successor).copied() {
                    None => {
                        index.insert(successor, (next, next));
                        next += 1;
                        stack.push(successor);
                        on_stack.insert(successor);
                        let successors = successors(&successor);
                        work.push((successor, successors));
                    }
                    Some((successor_index, _)) if on_stack.contains(&successor) => {
                        let entry = index.get_mut(&net).expect("visited");
                        entry.1 = entry.1.min(successor_index);
                    }
                    Some(_) => {}
                }
                continue;
            }
            work.pop();
            let (net_index, low) = index[&net];
            if let Some((parent, _)) = work.last() {
                let parent_entry = index.get_mut(parent).expect("visited");
                parent_entry.1 = parent_entry.1.min(low);
            }
            if low == net_index {
                let mut component = Vec::new();
                while let Some(member) = stack.pop() {
                    on_stack.remove(&member);
                    component.push(member);
                    if member == net {
                        break;
                    }
                }
                if component.len() > 1 {
                    cyclic.extend(component);
                }
            }
        }
    }
    cyclic
}

/// Build the DAG of `form`, whose nodes are in `drivers.arena`.
fn domain_dag(
    drivers: &Drivers<'_>,
    form: &NextStateForm<AbsoluteAddr>,
) -> Result<DomainDag, String> {
    let (arena, copies, substituted, opaque_nets) = copy_roots(drivers, form.nodes())?;
    let form = form.map(|var| *var, |node| Ok::<_, String>(copies[&node]))?;
    Ok(DomainDag {
        arena,
        form,
        substituted,
        opaque_nets,
    })
}

fn copy_roots(
    drivers: &Drivers<'_>,
    roots: impl IntoIterator<Item = NodeId>,
) -> Result<
    (
        SLTNodeArena<AbsoluteAddr>,
        HashMap<NodeId, NodeId>,
        usize,
        usize,
    ),
    String,
> {
    let source = drivers.arena;
    let mut dag = SLTNodeArena::new();
    let mut copies: HashMap<NodeId, NodeId> = HashMap::default();
    let mut substituted = 0;
    let mut opaque_nets: HashSet<AbsoluteAddr> = HashSet::default();
    // The drivers a read of a net is replaced by: the parts of the read
    // range, most significant first, each a driver's slice or a leaf gap.
    enum Part {
        Driver(NodeId, BitAccess),
        Gap(BitAccess),
    }
    let parts_of = |node: NodeId| -> Option<(AbsoluteAddr, Vec<Part>)> {
        let SLTNode::Input {
            variable,
            signed: false,
            index,
            access,
        } = source.get(node)
        else {
            return None;
        };
        if !index.is_empty() {
            return None;
        }
        let drivers = drivers.nets.get(variable)?;
        let mut parts = Vec::new();
        let mut bit = access.lsb;
        let mut sorted: Vec<_> = drivers
            .iter()
            .filter(|(range, _)| range.lsb <= access.msb && range.msb >= access.lsb)
            .collect();
        sorted.sort_by_key(|(range, _)| range.lsb);
        for (range, expr) in sorted {
            let lsb = range.lsb.max(access.lsb);
            let msb = range.msb.min(access.msb);
            if lsb > bit {
                parts.push(Part::Gap(BitAccess::new(bit, lsb - 1)));
            }
            if lsb < bit {
                // Overlapping drivers: leave the net to its own evaluation.
                return None;
            }
            parts.push(Part::Driver(
                *expr,
                BitAccess::new(lsb - range.lsb, msb - range.lsb),
            ));
            bit = msb + 1;
        }
        if parts.is_empty() {
            return None;
        }
        if bit <= access.msb {
            parts.push(Part::Gap(BitAccess::new(bit, access.msb)));
        }
        parts.reverse();
        Some((*variable, parts))
    };
    let operands = |node: NodeId| -> Vec<NodeId> {
        if let Some((_, parts)) = parts_of(node) {
            return parts
                .iter()
                .filter_map(|part| match part {
                    Part::Driver(expr, _) => Some(*expr),
                    Part::Gap(_) => None,
                })
                .collect();
        }
        match source.get(node) {
            SLTNode::Input { index, .. } => index.iter().map(|entry| entry.node).collect(),
            SLTNode::Binary(lhs, _, rhs) => vec![*lhs, *rhs],
            SLTNode::Unary(_, operand) | SLTNode::Slice { expr: operand, .. } => vec![*operand],
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => vec![*cond, *then_expr, *else_expr],
            SLTNode::Concat(parts) => parts.iter().map(|(part, _)| *part).collect(),
            _ => Vec::new(),
        }
    };
    let intern = |dag: &mut SLTNodeArena<AbsoluteAddr>, node: SLTNode<AbsoluteAddr>| {
        intern(dag, node).map_err(|error| error.to_string())
    };
    for root in roots {
        let mut stack = vec![(root, false)];
        while let Some((node, expanded)) = stack.pop() {
            if copies.contains_key(&node) {
                continue;
            }
            if !expanded {
                stack.push((node, true));
                stack.extend(
                    operands(node)
                        .into_iter()
                        .filter(|operand| !copies.contains_key(operand))
                        .map(|operand| (operand, false)),
                );
                continue;
            }
            let copy = |operand: &NodeId| copies[operand];
            let id = if let Some((variable, parts)) = parts_of(node) {
                substituted += 1;
                let mut pieces = Vec::with_capacity(parts.len());
                for part in parts {
                    let piece = match part {
                        Part::Driver(expr, range) => intern(
                            &mut dag,
                            SLTNode::Slice {
                                expr: copy(&expr),
                                access: range,
                            },
                        )?,
                        Part::Gap(range) => intern(
                            &mut dag,
                            SLTNode::Input {
                                variable,
                                signed: false,
                                index: Vec::new(),
                                access: range,
                            },
                        )?,
                    };
                    let width = dag.node_width(piece).unwrap_or(0);
                    pieces.push((piece, width));
                }
                intern(&mut dag, SLTNode::Concat(pieces))?
            } else {
                let rebuilt = match source.get(node) {
                    SLTNode::Input {
                        variable,
                        signed,
                        index,
                        access,
                    } => {
                        if drivers.nets.contains_key(variable) {
                            opaque_nets.insert(*variable);
                        }
                        SLTNode::Input {
                            variable: *variable,
                            signed: *signed,
                            index: index
                                .iter()
                                .map(|entry| {
                                    let mut entry = entry.clone();
                                    entry.node = copy(&entry.node);
                                    entry
                                })
                                .collect(),
                            access: *access,
                        }
                    }
                    SLTNode::Constant(..) => source.get(node).clone(),
                    SLTNode::Binary(lhs, op, rhs) => SLTNode::Binary(copy(lhs), *op, copy(rhs)),
                    SLTNode::Unary(op, operand) => SLTNode::Unary(*op, copy(operand)),
                    SLTNode::Mux {
                        cond,
                        then_expr,
                        else_expr,
                    } => SLTNode::Mux {
                        cond: copy(cond),
                        then_expr: copy(then_expr),
                        else_expr: copy(else_expr),
                    },
                    SLTNode::Slice { expr, access } => SLTNode::Slice {
                        expr: copy(expr),
                        access: *access,
                    },
                    SLTNode::Concat(parts) => SLTNode::Concat(
                        parts
                            .iter()
                            .map(|(part, width)| (copy(part), *width))
                            .collect(),
                    ),
                    _ => return Err(format!("node n{} has no canonical form", node.0)),
                };
                intern(&mut dag, rebuilt)?
            };
            copies.insert(node, id);
        }
    }
    Ok((dag, copies, substituted, opaque_nets.len()))
}

/// The DAG of each domain with a form, after the combinational nets the
/// program keeps are known; each reported on the `celox::next_state` target.
pub(super) fn next_state_dags(
    forms: &BTreeMap<AbsoluteAddr, NextStateForm<AbsoluteAddr>>,
    logic: &StateGraph,
) -> BTreeMap<AbsoluteAddr, Result<DomainDag, String>> {
    if forms.is_empty() {
        return BTreeMap::new();
    }
    let drivers = Drivers::new(&logic.global_arena, &logic.comb_blocks);
    forms
        .iter()
        .map(|(domain, form)| {
            let dag = domain_dag(&drivers, form);
            match &dag {
                Ok(dag) => {
                    let nodes: Vec<NodeId> = dag.form.nodes().collect();
                    let unique = reachable(&dag.arena, nodes.iter().copied());
                    let leaves = unique
                        .iter()
                        .filter(|node| matches!(dag.arena.get(**node), SLTNode::Input { .. }))
                        .count();
                    let width = |access: &BitAccess| access.msb - access.lsb + 1;
                    tracing::info!(
                        target: "celox::next_state",
                        "domain={domain:?} roots={} root_bits={} ports={} port_bits={} unique={} leaves={leaves} substituted={} opaque_nets={} same_op_bit_runs={}",
                        dag.form.roots.len(),
                        dag.form.roots.iter().map(|root| width(&root.access)).sum::<usize>(),
                        dag.form.ports.len(),
                        dag.form.ports.iter().map(|port| width(&port.access)).sum::<usize>(),
                        unique.len(),
                        dag.substituted,
                        dag.opaque_nets,
                        same_op_bit_runs(&dag.arena, nodes.iter().copied()),
                    );
                }
                Err(error) => {
                    tracing::info!(target: "celox::next_state", "domain={domain:?} no DAG: {error}");
                }
            }
            (*domain, dag)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use num_bigint::BigUint;

    fn addr(var: usize) -> AbsoluteAddr {
        AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: SourceVarId(var as u32),
        }
    }

    fn input(arena: &mut SLTNodeArena<AbsoluteAddr>, var: usize, lsb: usize, msb: usize) -> NodeId {
        arena
            .alloc(SLTNode::Input {
                variable: addr(var),
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(lsb, msb),
            })
            .unwrap()
    }

    fn net(
        target: usize,
        lsb: usize,
        msb: usize,
        expr: NodeId,
        reads: &[usize],
    ) -> LogicPath<AbsoluteAddr> {
        LogicPath {
            transparent_copy: false,
            target: LogicPathTarget::Var(VarAtomBase::new(addr(target), lsb, msb)),
            sources: reads
                .iter()
                .map(|&read| VarAtomBase::new(addr(read), 0, 0))
                .collect(),
            previous_sources: HashSet::default(),
            address_sources: HashSet::default(),
            local_inputs: Vec::new(),
            order_before: HashSet::default(),
            comb_capture_enable_sites: Vec::new(),
            comb_capture_enable_always: false,
            pre_lower_nodes: Vec::new(),
            expr,
        }
    }

    /// A read of a net is its drivers: the register's next value reads the
    /// registers alone, with each driven range a slice of its driver and the
    /// undriven bits of the net a leaf.
    #[test]
    fn nets_are_replaced_by_their_drivers() {
        let mut arena = SLTNodeArena::new();
        // Net 10[7:0] = r1 & r2 drives bits 3:0 of net 11, whose bits 7:4
        // nothing drives.
        let r1 = input(&mut arena, 1, 0, 7);
        let r2 = input(&mut arena, 2, 0, 7);
        let and = arena
            .alloc(SLTNode::Binary(r1, celox_design::BinaryOp::And, r2))
            .unwrap();
        let net10 = input(&mut arena, 10, 0, 7);
        let low = arena
            .alloc(SLTNode::Slice {
                expr: net10,
                access: BitAccess::new(0, 3),
            })
            .unwrap();
        let paths = vec![net(10, 0, 7, and, &[1, 2]), net(11, 0, 3, low, &[10])];
        let read = input(&mut arena, 11, 0, 7);
        let drivers = Drivers::new(&arena, &paths);
        let form = NextStateForm {
            roots: vec![ValueRoot {
                enable: None,
                target: addr(3),
                access: BitAccess::new(0, 7),
                value: read,
            }],
            ports: Vec::new(),
        };
        let dag = domain_dag(&drivers, &form).unwrap();
        let leaves: Vec<_> = reachable(&dag.arena, dag.form.nodes())
            .into_iter()
            .filter_map(|node| match dag.arena.get(node) {
                SLTNode::Input {
                    variable, access, ..
                } => Some((variable.var_id.0, access.lsb, access.msb)),
                _ => None,
            })
            .collect();
        let mut leaves = leaves;
        leaves.sort();
        assert_eq!(leaves, vec![(1, 0, 7), (2, 0, 7), (11, 4, 7)]);
        assert_eq!(dag.substituted, 2);
    }

    /// Nets that read themselves, directly or through others, are leaves.
    #[test]
    fn nets_on_a_loop_stay_leaves() {
        let mut arena = SLTNodeArena::new();
        let one = arena
            .alloc(SLTNode::Constant(
                BigUint::from(1u8),
                BigUint::default(),
                1,
                false,
            ))
            .unwrap();
        let a = input(&mut arena, 20, 0, 0);
        let b = input(&mut arena, 21, 0, 0);
        let a_next = arena
            .alloc(SLTNode::Binary(b, celox_design::BinaryOp::Xor, one))
            .unwrap();
        let b_next = arena
            .alloc(SLTNode::Binary(a, celox_design::BinaryOp::Xor, one))
            .unwrap();
        let c = input(&mut arena, 22, 0, 0);
        let paths = vec![
            net(20, 0, 0, a_next, &[21]),
            net(21, 0, 0, b_next, &[20]),
            net(23, 0, 0, c, &[22]),
        ];
        let drivers = Drivers::new(&arena, &paths);
        assert!(!drivers.nets.contains_key(&addr(20)));
        assert!(!drivers.nets.contains_key(&addr(21)));
        assert!(drivers.nets.contains_key(&addr(23)));
    }
}
