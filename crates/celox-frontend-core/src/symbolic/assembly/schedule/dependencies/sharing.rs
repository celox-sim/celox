//! Placement of calculations shared by observations and clocked updates.
use super::*;
use celox_design::UnaryOp;
use celox_slt::SLTNode;

impl StateDependencies {
    /// Schedule backwards from observations and clock samples. A calculation
    /// with consumers in different schedules belongs to the shared settle.
    /// Its inputs then serve just that settle, avoiding extra stores deeper
    /// in the shared cone. Wiring forwards its consumers to its producer.
    pub(in super::super) fn shared_calculations(
        &self,
        arena: &SLTNodeArena<AbsoluteAddr>,
        event_nets: HashSet<AbsoluteAddr>,
        full_settle_samples: Option<&EventEvaluations>,
    ) -> HashSet<AbsoluteAddr> {
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum Consumer {
            Observation,
            Clock(AbsoluteAddr),
            Shared,
        }
        let mut owners = HashMap::default();
        let demand = |owners: &mut HashMap<_, _>, net, consumer| {
            owners
                .entry(net)
                .and_modify(|old| {
                    if *old != consumer {
                        *old = Consumer::Shared;
                    }
                })
                .or_insert(consumer);
        };
        // A full settle runs even while every data arm of a clocked mux is
        // inactive. Its selector is always sampled. Data and RAM port payloads
        // can share an already observed result without making their producer
        // run on every settle just because two clocks might sample it.
        let mut conditional_reads = HashSet::default();
        if let Some(events) = full_settle_samples {
            for (&event, batches) in events {
                let mut roots = HashSet::default();
                for batch in batches {
                    let EdgeEvaluation::Graph(form) = batch else {
                        continue;
                    };
                    roots.extend(form.roots.iter().map(|r| eager_sample_root(arena, r.value)));
                    roots.extend(form.ports.iter().map(|p| p.guard));
                }
                let mut inputs = HashSet::default();
                for root in roots {
                    crate::flattening::collect_inputs(root, arena, &mut inputs);
                }
                for input in inputs {
                    demand(&mut owners, input.id, Consumer::Clock(event));
                }
            }
        }
        for p in &self.producers {
            let consumer = match p.update {
                Update::Sample(_) if full_settle_samples.is_some() => {
                    conditional_reads.extend(p.inputs.iter().map(|a| a.absolute_addr()));
                    continue;
                }
                Update::Sample(event) | Update::Procedure(Some(event)) => Consumer::Clock(event),
                Update::Procedure(None) => Consumer::Observation,
                _ => continue,
            };
            for addr in &p.inputs {
                if addr.region == STABLE_REGION {
                    demand(&mut owners, addr.absolute_addr(), consumer);
                }
            }
        }
        for net in event_nets {
            demand(&mut owners, net, Consumer::Observation);
        }

        // Visit a definition after all its users. Multiple writers and cycles
        // keep their existing schedules.
        let mut definitions = HashMap::default();
        let mut users = HashMap::<_, usize>::default();
        for (&addr, writers) in &self.writers {
            let [id] = writers.as_slice() else { continue };
            let writer = &self.producers[*id];
            if let Update::Expression { root, .. } = writer.update {
                definitions.insert(addr.absolute_addr(), (root, writer));
                for input in &writer.inputs {
                    *users.entry(input.absolute_addr()).or_default() += 1;
                }
            }
        }
        let mut ready: Vec<_> = definitions
            .keys()
            .copied()
            .filter(|net| !users.contains_key(net))
            .collect();
        let mut shared = HashSet::default();
        while let Some(net) = ready.pop() {
            let (root, writer) = definitions[&net];
            let mut owner = owners.get(&net).copied();
            let mut sampled_lazily = conditional_reads.contains(&net);
            if (owner == Some(Consumer::Shared)
                || (owner == Some(Consumer::Observation) && sampled_lazily))
                && is_calculation(arena, root)
            {
                shared.insert(net);
                owner = Some(Consumer::Observation);
                sampled_lazily = false;
            }
            for addr in &writer.inputs {
                let input = addr.absolute_addr();
                if sampled_lazily {
                    conditional_reads.insert(input);
                }
                if let Some(owner) = owner {
                    demand(&mut owners, input, owner);
                }
                let remaining = users.get_mut(&input).expect("indexed user");
                *remaining -= 1;
                if *remaining == 0 && definitions.contains_key(&input) {
                    ready.push(input);
                }
            }
        }
        shared
    }
}

// A scalar root can have a conditional data arm, possibly under an identity
// cast. RAM write ports represent their predicate separately.
fn eager_sample_root(arena: &SLTNodeArena<AbsoluteAddr>, mut node: NodeId) -> NodeId {
    loop {
        match arena.get(node) {
            SLTNode::Unary(UnaryOp::Ident | UnaryOp::ToTwoState, arg) => node = *arg,
            SLTNode::Mux { cond, .. } => return *cond,
            _ => return node,
        }
    }
}

// Copies carry demand to the value being copied. A slice or a type wrapper
// can also wrap an entire expression from the same continuous assignment.
fn is_calculation(arena: &SLTNodeArena<AbsoluteAddr>, mut node: NodeId) -> bool {
    loop {
        match arena.get(node) {
            SLTNode::Input { index, .. } => return !index.is_empty(),
            SLTNode::Constant(..) => return false,
            SLTNode::Slice { expr, .. }
            | SLTNode::Unary(UnaryOp::Ident | UnaryOp::ToTwoState, expr) => node = *expr,
            _ => return true,
        }
    }
}
