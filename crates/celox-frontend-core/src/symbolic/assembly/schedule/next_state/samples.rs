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
        let mut settled_reads = dependencies.settled_read_counts();
        drivers.nets.retain(|target, ranges| {
            let [(range, expr)] = ranges.as_slice() else {
                return false;
            };
            !blocked.contains(target)
                && sampled.contains(target)
                && range.lsb == 0
                && shapes.get(target).map(|s| s.0) == Some(range.msb + 1)
                && arena.node_width(*expr) == Some(range.msb + 1)
                && dependencies.expression_inputs(*target).is_some()
        });
        available.extend(drivers.nets.keys().copied());
        available.retain(|net| !blocked.contains(net));
        drivers.nets.retain(|target, _| {
            dependencies
                .expression_inputs(*target)
                .expect("indexed expression output")
                .all(|input| available.contains(&input))
        });

        let moved = feedback_producers(&drivers, dependencies, &mut settled_reads);
        drivers.nets.retain(|target, ranges| {
            let (_, expr) = ranges[0];
            if moved.contains(target) {
                return true;
            }
            if settled_reads.contains_key(target)
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

/// Follow a register's state transitions back through their exclusive inputs.
/// An operand can move with the last settle reader of its stored value.
fn feedback_producers(
    drivers: &Drivers<'_>,
    dependencies: &StateDependencies,
    settled_reads: &mut HashMap<AbsoluteAddr, usize>,
) -> HashSet<AbsoluteAddr> {
    let mut startup = None;
    let mut starts_at_zero = |root: NodeId| {
        startup.get_or_insert_with(|| initial_values(drivers.arena))[root.0] == Some(0)
    };
    let mut pending: Vec<_> = drivers
        .nets
        .iter()
        .filter(|(net, ranges)| {
            dependencies.is_feedback(**net)
                && !settled_reads.contains_key(net)
                && starts_at_zero(ranges[0].1)
        })
        .map(|(net, _)| *net)
        .collect();
    let mut moved = HashSet::default();
    while let Some(net) = pending.pop() {
        moved.insert(net);
        for input in dependencies
            .expression_inputs(net)
            .expect("indexed expression output")
        {
            let count = settled_reads
                .get_mut(&input)
                .expect("settle reader counted");
            *count -= 1;
            if *count == 0
                && let Some(ranges) = drivers.nets.get(&input)
                && starts_at_zero(ranges[0].1)
            {
                pending.push(input);
            }
        }
    }
    moved
}

/// The value of a pure expression before the first settle, assuming its
/// stable inputs still have their default zero. A missing value also rules
/// out substitution when the expression can introduce unknown bits. Both
/// arms of a mux must qualify, even if only one is selected at startup.
fn initial_values(arena: &SLTNodeArena<AbsoluteAddr>) -> Vec<Option<u64>> {
    let mut values: Vec<Option<u64>> = Vec::with_capacity(arena.len());
    for node in arena.iter() {
        let evaluate = || match node {
            SLTNode::Input { index, .. } if index.is_empty() => Some(0),
            SLTNode::Constant(value, mask, width, _)
                if mask.bits() == 0 && value.bits() <= *width as u64 =>
            {
                u64::try_from(value).ok()
            }
            SLTNode::Unary(UnaryOp::Ident | UnaryOp::ToTwoState, arg) => values[arg.0],
            SLTNode::Unary(UnaryOp::Or, arg) => Some(u64::from(values[arg.0]? != 0)),
            SLTNode::Unary(UnaryOp::LogicNot, arg) => Some(u64::from(values[arg.0]? == 0)),
            SLTNode::Unary(UnaryOp::BitNot, arg) => {
                let width = arena.node_width(*arg)?;
                let value = values[arg.0]?;
                (1..=64)
                    .contains(&width)
                    .then(|| !value & (u64::MAX >> (64 - width)))
            }
            SLTNode::Binary(lhs, op, rhs) => {
                let (left, right) = (values[lhs.0]?, values[rhs.0]?);
                match op {
                    BinaryOp::Eq | BinaryOp::EqCase | BinaryOp::Ne | BinaryOp::NeCase
                        if arena.node_width(*lhs) == arena.node_width(*rhs) =>
                    {
                        Some(u64::from(
                            (left == right) == matches!(op, BinaryOp::Eq | BinaryOp::EqCase),
                        ))
                    }
                    BinaryOp::And if left == 0 || right == 0 => Some(0),
                    BinaryOp::LogicAnd => Some(u64::from(left != 0 && right != 0)),
                    BinaryOp::LogicOr => Some(u64::from(left != 0 || right != 0)),
                    BinaryOp::Or
                    | BinaryOp::Xor
                    | BinaryOp::Add
                    | BinaryOp::Sub
                    | BinaryOp::Mul
                        if left == 0 && right == 0 =>
                    {
                        Some(0)
                    }
                    _ => None,
                }
            }
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                let (a, b) = (values[then_expr.0]?, values[else_expr.0]?);
                let selected = if values[cond.0]? != 0 { a } else { b };
                (selected == 0 || arena.node_width(*then_expr) == arena.node_width(*else_expr))
                    .then_some(selected)
            }
            SLTNode::Slice { expr, access } => {
                let value = values[expr.0]?;
                let width = access.msb - access.lsb + 1;
                Some(if access.lsb >= 64 {
                    0
                } else {
                    (value >> access.lsb) & (u64::MAX >> (64 - width.min(64)))
                })
            }
            SLTNode::Concat(parts) if parts.iter().all(|(part, _)| values[part.0] == Some(0)) => {
                Some(0)
            }
            _ => None,
        };
        values.push(evaluate());
    }
    values
}
