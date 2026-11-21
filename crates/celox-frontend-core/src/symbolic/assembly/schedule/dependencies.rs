//! Producer/operand edges in the elaborated design. Stable, staged and
//! sparse memory objects have different vertices. An opaque kernel has an
//! edge to each input and from each output it declares; its internal order
//! stays in the kernel.

use super::*;
use crate::symbolic::artifact::EdgeEvaluation;
use crate::symbolic::{read_objects, written_objects};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Update {
    Continuous,
    Sample(AbsoluteAddr),
    Publication(AbsoluteAddr),
    Procedure(Option<AbsoluteAddr>),
}

struct Producer {
    update: Update,
    inputs: Vec<RegionedAbsoluteAddr>,
}

#[derive(Default)]
pub(super) struct StateDependencies {
    producers: Vec<Producer>,
    writers: HashMap<RegionedAbsoluteAddr, Vec<usize>>,
}

fn stable(addr: AbsoluteAddr) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, addr)
}

impl StateDependencies {
    fn insert(
        &mut self,
        update: Update,
        inputs: impl IntoIterator<Item = RegionedAbsoluteAddr>,
        outputs: impl IntoIterator<Item = RegionedAbsoluteAddr>,
    ) {
        let id = self.producers.len();
        self.producers.push(Producer {
            update,
            inputs: inputs.into_iter().collect(),
        });
        for output in outputs {
            self.writers.entry(output).or_default().push(id);
        }
    }

    fn kernel<A>(
        &mut self,
        update: Update,
        unit: &ExecutionUnit<A>,
        locate: impl Fn(&A) -> RegionedAbsoluteAddr,
    ) where
        A: Clone + Eq + std::hash::Hash,
    {
        let mut inputs = HashSet::default();
        let mut outputs = HashSet::default();
        for inst in unit.blocks.values().flat_map(|b| &b.instructions) {
            inputs.extend(read_objects(inst).map(&locate));
            outputs.extend(written_objects(inst).map(&locate));
        }
        self.insert(update, inputs, outputs);
    }

    pub(super) fn new(graph: &StateGraph, units: &Units) -> Self {
        let mut deps = Self::default();
        let inputs = |roots: &[NodeId]| {
            let mut reads = HashSet::default();
            for root in roots {
                crate::flattening::collect_inputs(*root, &graph.global_arena, &mut reads);
            }
            reads
                .into_iter()
                .map(|a| stable(a.id))
                .collect::<HashSet<_>>()
        };
        for path in &graph.comb_blocks {
            match &path.target {
                LogicPathTarget::Var(target) => {
                    let mut reads = inputs(&[path.expr]);
                    reads.extend(
                        path.sources
                            .iter()
                            .chain(&path.previous_sources)
                            .map(|s| stable(s.id)),
                    );
                    deps.insert(
                        if path.transparent_copy {
                            Update::Continuous
                        } else {
                            Update::Procedure(None)
                        },
                        reads,
                        [stable(target.id)],
                    );
                }
                LogicPathTarget::Process { unit, .. } => {
                    deps.kernel(Update::Procedure(None), unit, |a| stable(*a))
                }
                LogicPathTarget::CombCaptureEvent {
                    guard,
                    args,
                    loop_runner,
                    ..
                } => {
                    let roots = guard
                        .iter()
                        .chain(args)
                        .chain(loop_runner)
                        .copied()
                        .collect::<Vec<_>>();
                    deps.insert(Update::Procedure(None), inputs(&roots), []);
                }
            }
        }
        for (&event, plan) in &graph.evaluations {
            for batch in plan {
                match batch {
                    EdgeEvaluation::Graph(form) => {
                        for r in &form.roots {
                            deps.insert(
                                Update::Sample(event),
                                inputs(&[r.value]),
                                [RegionedAbsoluteAddr::from_absolute_addr(
                                    WORKING_REGION,
                                    r.target,
                                )],
                            );
                        }
                        for p in &form.ports {
                            deps.insert(
                                Update::Sample(event),
                                inputs(&[p.guard, p.index, p.value]),
                                [RegionedAbsoluteAddr::from_absolute_addr(
                                    celox_design::SPARSE_WORKING_REGION,
                                    p.array,
                                )],
                            );
                        }
                    }
                    EdgeEvaluation::Procedure(unit) => {
                        deps.kernel(Update::Procedure(Some(event)), unit, |a| *a)
                    }
                }
            }
        }
        for (&event, publications) in &units.apply_ffs {
            for unit in publications {
                // Simple commits are transfers from one staged slot. Keep
                // control and data dependencies together for an arbitrary kernel.
                let is_transfer = |inst: &SIRInstruction<RegionedAbsoluteAddr>| {
                    matches!(inst,
                    SIRInstruction::Commit(src, dst, _, _, triggers)
                    if src.absolute_addr() == dst.absolute_addr()
                    && matches!(src.region, WORKING_REGION | celox_design::SPARSE_WORKING_REGION)
                    && dst.region == STABLE_REGION && triggers.is_empty())
                };
                if unit
                    .blocks
                    .values()
                    .flat_map(|b| &b.instructions)
                    .all(is_transfer)
                {
                    for inst in unit.blocks.values().flat_map(|b| &b.instructions) {
                        deps.insert(
                            Update::Publication(event),
                            read_objects(inst).copied(),
                            written_objects(inst).copied(),
                        );
                    }
                } else {
                    deps.kernel(Update::Procedure(Some(event)), unit, |a| *a);
                }
            }
        }
        // Some source adapters supply just the already combined event.
        for (&event, direct) in &units.eval_apply_ffs {
            if !graph.evaluations.contains_key(&event) {
                for unit in direct {
                    deps.kernel(Update::Procedure(Some(event)), unit, |a| *a);
                }
            }
        }
        for unit in units
            .processes
            .iter()
            .map(|p| &p.kernel)
            .chain(units.functions.iter().map(|f| &f.unit))
        {
            deps.kernel(Update::Procedure(None), unit, |a| *a);
        }
        for observer in &graph.comb_observers {
            let roots = observer
                .guard
                .iter()
                .chain(&observer.args)
                .chain(&observer.loop_runner)
                .chain(observer.local_inputs.iter().map(|(_, node)| node))
                .copied()
                .collect::<Vec<_>>();
            let mut reads = inputs(&roots);
            reads.extend(
                observer
                    .sensitivity
                    .iter()
                    .chain(&observer.observed_inputs)
                    .chain(&observer.position_inputs)
                    .map(|s| stable(s.id)),
            );
            reads.extend(
                observer
                    .sensitivity_counters
                    .iter()
                    .map(|(_, id)| stable(*id)),
            );
            deps.insert(Update::Procedure(None), reads, []);
        }
        for site in &units.timing.wait_sites {
            deps.insert(
                Update::Procedure(None),
                site.reads().map(stable),
                [stable(units.processes[site.process].slots.status)],
            );
        }
        for site in &units.timing.nba_sites {
            let reads = std::iter::once(site.value)
                .chain(site.pending)
                .chain(site.position.into_iter().flat_map(|p| [p.offset, p.width]));
            deps.insert(
                Update::Procedure(None),
                reads.map(stable),
                [stable(site.target)],
            );
        }
        for site in &units.timing.host_request_sites {
            deps.insert(
                Update::Procedure(None),
                site.args.iter().map(|a| stable(a.id)),
                site.results.iter().map(|a| stable(a.id)),
            );
        }
        for &(source, copy) in units
            .timing
            .sample_shadows
            .iter()
            .chain(&units.timing.initial_copies)
        {
            deps.insert(Update::Procedure(None), [stable(source)], [stable(copy)]);
        }
        deps
    }

    fn writers(&self, addr: RegionedAbsoluteAddr) -> impl Iterator<Item = &Producer> {
        self.writers
            .get(&addr)
            .into_iter()
            .flatten()
            .map(|&id| &self.producers[id])
    }

    /// Walk upstream from every sampling operation and edge procedure. A
    /// stable register or a procedural writer ends combinational expansion.
    pub(super) fn sampled_wires(&self) -> HashSet<AbsoluteAddr> {
        let mut work = self
            .producers
            .iter()
            .filter(|p| matches!(p.update, Update::Sample(_) | Update::Procedure(Some(_))))
            .flat_map(|p| p.inputs.iter().copied())
            .collect::<Vec<_>>();
        let mut seen = HashSet::default();
        while let Some(addr) = work.pop() {
            if addr.region == STABLE_REGION && seen.insert(addr.absolute_addr()) {
                for writer in self
                    .writers(addr)
                    .filter(|p| p.update == Update::Continuous)
                {
                    work.extend_from_slice(&writer.inputs);
                }
            }
        }
        seen
    }

    /// The register receives only publishes of graph samples. A procedure
    /// writing either its stable value or the staging slot invalidates this proof.
    pub(super) fn sampled_registers(&self) -> HashSet<AbsoluteAddr> {
        self.writers
            .keys()
            .filter(|a| {
                a.region == WORKING_REGION
                    && self
                        .writers(**a)
                        .all(|w| matches!(w.update, Update::Sample(_)))
                    && self
                        .writers(stable(a.absolute_addr()))
                        .all(|w| matches!(w.update, Update::Publication(_)))
            })
            .map(|a| a.absolute_addr())
            .collect()
    }

    /// Recreating a calculation in an event would duplicate it if settle
    /// or a process also needs its stored result. Wiring has no such cost.
    pub(super) fn settled_reads(&self) -> HashSet<AbsoluteAddr> {
        self.producers
            .iter()
            .filter(|p| matches!(p.update, Update::Continuous | Update::Procedure(None)))
            .flat_map(|p| &p.inputs)
            .filter(|a| a.region == STABLE_REGION)
            .map(|a| a.absolute_addr())
            .collect()
    }

    pub(super) fn continuous_inputs(
        &self,
        net: AbsoluteAddr,
    ) -> Option<impl Iterator<Item = AbsoluteAddr> + '_> {
        let mut writers = self.writers(stable(net));
        let producer = writers.next()?;
        (producer.update == Update::Continuous && writers.next().is_none())
            .then(|| producer.inputs.iter().map(|a| a.absolute_addr()))
    }

    /// Event ownership for the settle scheduler, including mixed procedural
    /// domains and reset publications.
    pub(super) fn write_domains(&self) -> HashMap<AbsoluteAddr, Vec<AbsoluteAddr>> {
        let mut domains = HashMap::default();
        for addr in self.writers.keys().filter(|a| a.region == STABLE_REGION) {
            let mut events = self
                .writers(*addr)
                .filter_map(|w| match w.update {
                    Update::Publication(event) | Update::Procedure(Some(event)) => Some(event),
                    _ => None,
                })
                .collect::<Vec<_>>();
            events.sort_unstable();
            events.dedup();
            if !events.is_empty() {
                domains.insert(addr.absolute_addr(), events);
            }
        }
        domains
    }
}
