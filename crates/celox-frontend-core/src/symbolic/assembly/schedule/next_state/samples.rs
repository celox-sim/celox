//! Connect the inputs of edge evaluations to their relocated producers.
//! At a sampling boundary a settled expression may be expanded if its
//! inputs cannot change in the active region and its initial value agrees
//! with the uninitialized two-state wire.

use super::*;
use crate::symbolic::artifact::{EdgeEvaluation, InstanceValue};
use celox_design::{BinaryOp, DeadStorePolicy, UnaryOp};

impl StateGraph {
    pub(in super::super) fn connect_samples(
        &mut self,
        dependencies: &StateDependencies,
        design: &Design,
        units: &mut Units,
        initial: &[InstanceValue],
        shapes: &HashMap<AbsoluteAddr, (usize, bool)>,
        active_writes: &HashMap<AbsoluteAddr, HashSet<AbsoluteAddr>>,
        parallel: &ParallelScheduleOptions,
        four_state: bool,
    ) -> Result<(), ParserError> {
        if four_state
            || parallel.enabled()
            || parallel.unobserved_nets == DeadStorePolicy::Off
            || self.evaluations.is_empty()
        {
            return Ok(());
        }
        let mut drivers = Drivers::new(&self.global_arena, &self.comb_blocks);
        let mut available = dependencies.sampled_registers();
        let mut blocked: HashSet<_> = active_writes.values().flatten().copied().collect();
        let tops = design.top_instances();
        for (&instance, module) in &design.instance_modules {
            let module = &design.modules[module];
            let locate = |id| design.bindings.locate(instance, id);
            blocked.extend(
                module
                    .initial_memory_values
                    .iter()
                    .chain(&module.declaration_initial_memory_values)
                    .filter(|v| match &v.data {
                        // Four-state declarations initialize a reg to X and a
                        // net to Z. Both initialize two-state storage to zero.
                        InitialStateData::Packed {
                            value,
                            mask,
                            written_mask,
                        } => {
                            let bits = value & written_mask;
                            (&bits & mask) != bits
                        }
                        _ => true,
                    })
                    .chain(&module.folded_initial_memory_values)
                    .map(|v| locate(v.address)),
            );
            for (&var, info) in &module.variables {
                if info.kind.is_port()
                    && (tops.contains(&instance)
                        || parallel.unobserved_nets == DeadStorePolicy::PreserveAllPorts)
                    || !info.metadata.array_dims.is_empty()
                {
                    blocked.insert(locate(var));
                }
            }
        }
        for value in initial {
            if let Some(&instance) = design.expanded.get(&InstancePath(value.path.clone())) {
                blocked.insert(design.bindings.locate(instance, value.variable));
            }
        }
        // A settled internal wire can itself be a leaf. Its current value
        // stays available until the next settle, even when the logic that
        // produced it ultimately depends on a host input or a procedure.
        let arena = &self.global_arena;
        let sampled = dependencies.sampled_wires();
        let settled_reads = dependencies.settled_reads();
        drivers.nets.retain(|target, ranges| {
            let [(range, expr)] = ranges.as_slice() else {
                return false;
            };
            !blocked.contains(target)
                && sampled.contains(target)
                && range.lsb == 0
                && shapes.get(target).map(|s| s.0) == Some(range.msb + 1)
                && arena.node_width(*expr) == Some(range.msb + 1)
                && dependencies.continuous_inputs(*target).is_some()
        });
        available.extend(drivers.nets.keys().copied());
        available.retain(|net| !blocked.contains(net));
        drivers.nets.retain(|target, ranges| {
            let (_, expr) = ranges[0];
            if settled_reads.contains(target)
                && !matches!(
                    arena.get(expr),
                    SLTNode::Input { .. }
                        | SLTNode::Slice { .. }
                        | SLTNode::Unary(UnaryOp::Ident | UnaryOp::ToTwoState, _)
                        | SLTNode::Constant(..)
                )
            {
                return false;
            }
            if !dependencies
                .continuous_inputs(*target)
                .expect("indexed continuous output")
                .all(|input| available.contains(&input))
            {
                return false;
            }
            reachable(arena, [expr])
                .into_iter()
                .all(|n| match arena.get(n) {
                    SLTNode::Input { index, .. } => index.is_empty(),
                    SLTNode::Constant(value, mask, ..) => {
                        *value == Default::default() && *mask == Default::default()
                    }
                    SLTNode::Binary(
                        _,
                        BinaryOp::And
                        | BinaryOp::Or
                        | BinaryOp::Xor
                        | BinaryOp::Add
                        | BinaryOp::Sub
                        | BinaryOp::Mul,
                        _,
                    )
                    | SLTNode::Unary(UnaryOp::Ident | UnaryOp::ToTwoState, _)
                    | SLTNode::Concat(_)
                    | SLTNode::Mux { .. }
                    | SLTNode::Slice { .. } => true,
                    _ => false,
                })
        });
        if drivers.nets.is_empty() {
            return Ok(());
        }

        // A procedure can load an identity wire directly from its producer.
        // Other expressions remain at their sample point in the symbolic plan.
        let mut aliases = HashMap::default();
        for (net, ranges) in &drivers.nets {
            let (range, mut expr) = ranges[0];
            loop {
                match arena.get(expr) {
                    SLTNode::Unary(UnaryOp::Ident | UnaryOp::ToTwoState, child) => expr = *child,
                    SLTNode::Input {
                        variable,
                        access,
                        index,
                        ..
                    } if index.is_empty()
                        && *access == range
                        && shapes.get(variable).map(|s| s.0) == Some(range.msb + 1) =>
                    {
                        aliases.insert(*net, *variable);
                        break;
                    }
                    _ => break,
                }
            }
        }
        let rewrite = |unit: &mut ExecutionUnit<RegionedAbsoluteAddr>| {
            for inst in unit.blocks.values_mut().flat_map(|b| &mut b.instructions) {
                if let SIRInstruction::Load(_, addr, ..) = inst
                    && addr.region == STABLE_REGION
                {
                    let mut id = addr.absolute_addr();
                    while let Some(source) = aliases.get(&id) {
                        id = *source;
                    }
                    *addr = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, id);
                }
            }
        };
        for unit in units.eval_apply_ffs.values_mut().flatten() {
            rewrite(unit);
        }
        let mut roots = HashSet::default();
        for batch in self.evaluations.values_mut().flatten() {
            match batch {
                EdgeEvaluation::Graph(form) => roots.extend(form.nodes()),
                EdgeEvaluation::Procedure(unit) => rewrite(unit),
            }
        }
        let error = |e: String| ParserError::illegal_context("design sample graph", e, None);
        let (linked, copies, substituted, _) =
            copy_roots(&drivers, roots.iter().copied()).map_err(error)?;
        let mut restored = HashMap::default();
        let mut nodes = HashMap::default();
        for root in roots {
            let node = copies[&root];
            let value = linked
                .get(node)
                .map_addr(
                    node,
                    &linked,
                    &mut self.global_arena,
                    &mut restored,
                    &|id| *id,
                )
                .map_err(|e| error(e.to_string()))?;
            nodes.insert(root, value);
        }
        for batch in self.evaluations.values_mut().flatten() {
            if let EdgeEvaluation::Graph(form) = batch {
                *form = form.map(|id| *id, |node| Ok::<_, ParserError>(nodes[&node]))?;
            }
        }
        tracing::debug!(target: "celox::edge_graph", substituted, "connected relocated sample inputs");
        Ok(())
    }
}
