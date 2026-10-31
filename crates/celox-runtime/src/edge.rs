//! Explicit edges of event signals.
//!
//! An edge moves an event's signal to a new level and runs every domain
//! sensitive to that transition: the event's own domain when the transition
//! is its active edge, clocks computed combinationally from the signal that
//! see their active edge (judged from the state before the edge), and the
//! cascade rounds after the commit. The signal stays at its new level.
//!
//! An event without derived domains costs one load and one store of its
//! level on top of its domain's kernel; its signal settles first only when
//! combinational logic reads it. An edge that runs no domain leaves
//! combinational logic stale only when combinational logic reads the signal,
//! so the inactive edge of a clock that only clocks flip-flops costs no
//! settle on the next active edge.

use celox_design::{DomainKind, EventTopology, GateOp, edges::LEVEL_UNKNOWN};

use crate::{
    AbsoluteAddr, SignalRef,
    backend::{EventHandle, SimBackend},
    derived_tick::{
        DerivedTickPlan, DerivedTickPlans, DerivedTickScratch, EdgeRun, EventDomain, TickExecutor,
    },
    simulation::{edge_level, event_level},
    timing::Halt,
};

/// A transition of an event signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Edge {
    /// To 1: `posedge`.
    Rising,
    /// To 0: `negedge`.
    Falling,
}

impl Edge {
    /// The level the signal has after the edge.
    pub fn level(self) -> u8 {
        match self {
            Edge::Rising => 1,
            Edge::Falling => 0,
        }
    }

    /// The other transition.
    pub fn opposite(self) -> Edge {
        match self {
            Edge::Rising => Edge::Falling,
            Edge::Falling => Edge::Rising,
        }
    }
}

/// What an edge needs to know about one event.
#[derive(Clone, Copy, Debug)]
pub struct EventEdge {
    /// The event's canonical signal.
    pub signal: SignalRef,
    pub kind: Option<DomainKind>,
    /// Combinational logic reads the signal.
    pub comb_read: bool,
    /// A run of the event's own domain can observe the signal's level:
    /// combinational logic or one of the domain's flip-flops reads it.
    pub level_read: bool,
    /// The edge opposite to [`Self::active_edge`] can run a domain: the
    /// event's own, or that of a clock computed combinationally from the
    /// signal that this edge can move to its active edge. When it cannot,
    /// nothing commits on that edge, so no domain clocked from state runs
    /// either, and a host only needs to store the level.
    pub inactive_runs: bool,
}

impl EventEdge {
    /// The edge that is the event's own active edge.
    pub fn active_edge(&self) -> Edge {
        match self.kind {
            Some(DomainKind::ClockNegedge | DomainKind::ResetAsyncLow) => Edge::Falling,
            _ => Edge::Rising,
        }
    }

    /// Whether `edge` can run the event's own domain at all, whatever the
    /// level before it: always, except for the inactive edge of an event of
    /// known polarity.
    pub fn can_fire(&self, edge: Edge) -> bool {
        self.fires(None, edge)
    }

    /// Whether `edge` from `old` (`None` when unknown) runs the event's own
    /// domain: its active transition, or any change for a domain of unknown
    /// kind. An unknown level counts as either level, as in SystemVerilog.
    pub fn fires(&self, old: Option<bool>, edge: Edge) -> bool {
        match self.kind {
            Some(
                DomainKind::ClockPosedge
                | DomainKind::ClockNegedge
                | DomainKind::ResetAsyncHigh
                | DomainKind::ResetAsyncLow,
            ) => edge == self.active_edge(),
            _ => old != Some(edge == Edge::Rising),
        }
    }
}

/// Per event ID, what an edge needs to know about it.
pub type EventEdges = std::sync::Arc<[EventEdge]>;

/// Resolve the edge information of every event of `backend`.
pub fn build_event_edges<B: SimBackend>(
    backend: &B,
    events: &EventTopology<AbsoluteAddr>,
    kinds: &fxhash::FxHashMap<AbsoluteAddr, DomainKind>,
) -> EventEdges {
    backend
        .id_to_addr_slice()
        .iter()
        .map(|address| {
            let canonical = events.canonical(*address);
            let kind = kinds.get(&canonical).copied();
            let comb_read = events.combinationally_read.contains(&canonical);
            let mut info = EventEdge {
                signal: backend.resolve_signal(&canonical),
                kind,
                comb_read,
                level_read: comb_read || !events.level_unread_by_domain.contains(&canonical),
                inactive_runs: false,
            };
            let inactive = info.active_edge().opposite();
            info.inactive_runs = info.can_fire(inactive)
                || events
                    .combinational_derived
                    .get(&canonical)
                    .into_iter()
                    .flatten()
                    // A clock without a domain of its own runs nothing.
                    .filter(|clock| backend.resolve_event_opt(&clock.clock).is_some())
                    .any(|clock| {
                        let moves = clock
                            .gate
                            .as_deref()
                            .and_then(dependence)
                            .unwrap_or(Dependence::Mixed);
                        moves.can_reach(inactive, kinds.get(&clock.clock).copied())
                    });
            info
        })
        .collect()
}

/// How a gate program's value depends on its source's level, bitwise:
/// whether raising the source can only raise each bit, only lower it, or
/// either. The operations it treats as monotone also are over the order
/// 0 < x < 1 of four-state values, so it holds for four-state designs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dependence {
    Independent,
    Positive,
    Negative,
    Mixed,
}

impl Dependence {
    fn invert(self) -> Self {
        match self {
            Dependence::Positive => Dependence::Negative,
            Dependence::Negative => Dependence::Positive,
            other => other,
        }
    }

    /// A monotone function of both operands, such as `&` or `|`.
    fn join(self, other: Self) -> Self {
        match (self, other) {
            (Dependence::Independent, other) | (other, Dependence::Independent) => other,
            (lhs, rhs) if lhs == rhs => lhs,
            _ => Dependence::Mixed,
        }
    }

    /// A function of the operands that is not monotone, such as `^`.
    fn scramble(self, other: Self) -> Self {
        if self == Dependence::Independent && other == Dependence::Independent {
            Dependence::Independent
        } else {
            Dependence::Mixed
        }
    }

    /// Whether `edge` of the source can be an edge of a clock of `kind`
    /// computed with this dependence.
    fn can_reach(self, edge: Edge, kind: Option<DomainKind>) -> bool {
        let moves = match self {
            Dependence::Independent => return false,
            Dependence::Mixed => return true,
            Dependence::Positive => edge,
            Dependence::Negative => edge.opposite(),
        };
        match kind {
            Some(DomainKind::ClockPosedge | DomainKind::ResetAsyncHigh) => moves == Edge::Rising,
            Some(DomainKind::ClockNegedge | DomainKind::ResetAsyncLow) => moves == Edge::Falling,
            _ => true,
        }
    }
}

/// The [`Dependence`] of a gate program's value on its source, or `None`
/// for a malformed program.
fn dependence<A>(ops: &[GateOp<A>]) -> Option<Dependence> {
    let mut stack: Vec<Dependence> = Vec::new();
    for op in ops {
        let value = match op {
            GateOp::Source => Dependence::Positive,
            GateOp::Load { .. } | GateOp::Const(_) => Dependence::Independent,
            GateOp::Not { .. } | GateOp::LogicNot => stack.pop()?.invert(),
            GateOp::ReduceAnd { .. } | GateOp::ReduceOr | GateOp::Slice { .. } => stack.pop()?,
            GateOp::ReduceXor => stack.pop()?.scramble(Dependence::Independent),
            GateOp::And | GateOp::Or | GateOp::LogicAnd | GateOp::LogicOr => {
                let rhs = stack.pop()?;
                stack.pop()?.join(rhs)
            }
            GateOp::Xor | GateOp::Eq | GateOp::Ne => {
                let rhs = stack.pop()?;
                stack.pop()?.scramble(rhs)
            }
            GateOp::Mux => {
                let otherwise = stack.pop()?;
                let then = stack.pop()?;
                match stack.pop()? {
                    // A fixed condition selects one of two monotone values.
                    Dependence::Independent => then.join(otherwise),
                    _ => Dependence::Mixed,
                }
            }
        };
        stack.push(value);
    }
    match stack.as_slice() {
        [value] => Some(*value),
        _ => None,
    }
}

/// What an edge did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeOutcome {
    /// The signal already was at the edge's level: nothing changed.
    Unchanged,
    /// The signal moved but no domain ran, and combinational logic reads
    /// it: combinational logic is stale.
    Moved,
    /// The signal moved but no domain ran, and combinational logic does not
    /// read it: combinational logic is exactly as settled as before.
    Stored,
    /// Domains ran and combinational logic is settled.
    Settled,
}

/// The outcome of a move that ran no domain.
fn moved_only(comb_read: bool) -> EdgeOutcome {
    if comb_read {
        EdgeOutcome::Moved
    } else {
        EdgeOutcome::Stored
    }
}

/// The edge level of `signal` (see [`celox_design::edges`]): 0, 1, or, in a
/// four-state design, x or z.
fn level<B: SimBackend>(backend: &B, signal: SignalRef) -> u8 {
    if signal.is_4state && backend.layout().four_state {
        edge_level::<true, _>(backend, signal)
    } else {
        u8::from(event_level(backend, signal) != 0)
    }
}

/// The known value of an edge level, or `None` for x and z.
fn known(level: u8) -> Option<bool> {
    (level & LEVEL_UNKNOWN == 0).then_some(level != 0)
}

/// Writes an event signal's new level: the host's own write of the signal,
/// which a host observing its writes (a timed simulation's waits and
/// counters) routes through its watch.
pub trait SignalWrite<B> {
    fn write(&mut self, backend: &mut B, signal: SignalRef, level: u8);
}

impl<B, F: FnMut(&mut B, SignalRef, u8)> SignalWrite<B> for F {
    fn write(&mut self, backend: &mut B, signal: SignalRef, level: u8) {
        self(backend, signal, level);
    }
}

/// A plain write of the level, for hosts that observe nothing.
pub fn set_level<B: SimBackend>(backend: &mut B, signal: SignalRef, level: u8) {
    backend.set(signal, level);
}

/// Whether `info`'s signal is at the known level `edge` leaves it at.
#[inline]
pub fn at_level<B: SimBackend>(backend: &B, info: &EventEdge, edge: Edge) -> bool {
    level(backend, info.signal) == edge.level()
}

/// Move `info`'s signal to the level of `edge` through `write`. Returns the
/// level it had (`None` while unknown), or `None` when it already was at
/// that level.
pub fn move_signal<B: SimBackend>(
    backend: &mut B,
    info: &EventEdge,
    edge: Edge,
    write: impl SignalWrite<B>,
) -> Option<Option<bool>> {
    move_level(backend, info, edge, write).map(known)
}

/// [`move_signal`], returning the edge level the signal had: x or z leave
/// for 1 or 0 like 0 or 1 do (IEEE 1800-2023 Table 9-2).
fn move_level<B: SimBackend>(
    backend: &mut B,
    info: &EventEdge,
    edge: Edge,
    mut write: impl SignalWrite<B>,
) -> Option<u8> {
    let old = level(backend, info.signal);
    let new = edge.level();
    if old == new {
        return None;
    }
    write.write(backend, info.signal, new);
    Some(old)
}

/// Move `event`'s signal through `edge` and run the domains sensitive to it.
/// `info` and `plan` are the event's entries of its host's tables; `dirty`
/// says whether combinational logic has pending changes; `write` moves the
/// signal.
#[allow(clippy::too_many_arguments)]
pub fn run_edge<X, E>(
    executor: &mut X,
    plan: Option<&DerivedTickPlan<E>>,
    info: EventEdge,
    scratch: &mut DerivedTickScratch,
    event: E,
    edge: Edge,
    dirty: bool,
    write: impl SignalWrite<X::Backend>,
) -> Result<EdgeOutcome, Halt>
where
    X: TickExecutor,
    X::Backend: SimBackend<Event = E>,
    E: EventHandle,
{
    let Some(from) = move_level(executor.backend_mut(), &info, edge, write) else {
        return Ok(EdgeOutcome::Unchanged);
    };
    let new = edge.level();
    let fires = info.fires(known(from), edge);
    let comb_read =
        info.comb_read && (dirty || !executor.backend_mut().eval_clock_aliases(event)?);
    let settle = dirty || comb_read;
    if let Some(plan) = plan {
        // A gated clock is judged from the lever's real level before the
        // edge, x and z included.
        let transition = [(from, new)];
        let transitions: &[(u8, u8)] = if plan.lever_signals().next().is_some() {
            &transition
        } else {
            &[]
        };
        let settled = plan.run(
            executor,
            scratch,
            settle,
            EdgeRun {
                transitions,
                persist: true,
                fire_sources: fires,
            },
        )?;
        return Ok(if settled {
            EdgeOutcome::Settled
        } else {
            moved_only(comb_read)
        });
    }
    if !fires {
        return Ok(moved_only(comb_read));
    }
    if settle {
        executor.eval_comb_apply_ff_at(event)?;
    } else {
        executor.eval_apply_ff_at(event)?;
    }
    executor.eval_comb()?;
    Ok(EdgeOutcome::Settled)
}

/// Move several event signals at the same instant and run the domains
/// sensitive to their edges: every domain evaluates against the state
/// before the edge, then all of them commit. `write` moves the signals.
pub fn run_edges<X, E>(
    executor: &mut X,
    plans: &DerivedTickPlans<E>,
    edges: &[EventEdge],
    scratch: &mut DerivedTickScratch,
    moves: &[(E, Edge)],
    dirty: bool,
    write: impl SignalWrite<X::Backend>,
) -> Result<EdgeOutcome, Halt>
where
    X: TickExecutor,
    X::Backend: SimBackend<Event = E>,
    E: EventHandle,
{
    run_edges_impl(executor, plans, edges, scratch, moves, dirty, write, None)
}

/// Bounded reuse of coincident-edge plans for a simulator's fixed topology.
/// Signal levels and gate predicates are still evaluated on every call.
/// Changing either topology automatically discards the cached plans.
pub struct CoincidentEdgeCache<E> {
    topology: Option<(DerivedTickPlans<E>, EventEdges)>,
    entries: Vec<(Vec<(usize, bool)>, DerivedTickPlan<E>)>,
    replace: usize,
}

impl<E> Default for CoincidentEdgeCache<E> {
    fn default() -> Self {
        Self {
            topology: None,
            entries: Vec::new(),
            replace: 0,
        }
    }
}

impl<E: EventHandle> CoincidentEdgeCache<E> {
    /// Run explicit edges while retaining up to eight recurring combinations.
    #[allow(clippy::too_many_arguments)]
    pub fn run<X>(
        &mut self,
        executor: &mut X,
        plans: &DerivedTickPlans<E>,
        edges: &EventEdges,
        scratch: &mut DerivedTickScratch,
        moves: &[(E, Edge)],
        dirty: bool,
        write: impl SignalWrite<X::Backend>,
    ) -> Result<EdgeOutcome, Halt>
    where
        X: TickExecutor,
        X::Backend: SimBackend<Event = E>,
    {
        if self.topology.as_ref().is_none_or(|(old_plans, old_edges)| {
            !std::sync::Arc::ptr_eq(old_plans, plans) || !std::sync::Arc::ptr_eq(old_edges, edges)
        }) {
            self.entries.clear();
            self.replace = 0;
            self.topology = Some((std::sync::Arc::clone(plans), std::sync::Arc::clone(edges)));
        }
        run_edges_impl(
            executor,
            plans,
            edges,
            scratch,
            moves,
            dirty,
            write,
            Some(self),
        )
    }

    fn plan(
        &mut self,
        changed: &[(E, bool)],
        make: impl FnOnce() -> DerivedTickPlan<E>,
    ) -> &DerivedTickPlan<E> {
        let index = if let Some(index) = self.entries.iter().position(|(known, _)| {
            known.len() == changed.len()
                && known
                    .iter()
                    .zip(changed)
                    .all(|(&(id, fires), &(event, now))| id == event.id() && fires == now)
        }) {
            index
        } else {
            let key = changed
                .iter()
                .map(|&(event, fires)| (event.id(), fires))
                .collect();
            let entry = (key, make());
            if self.entries.len() < 8 {
                let index = self.entries.len();
                self.entries.push(entry);
                index
            } else {
                let index = self.replace;
                self.replace = (index + 1) % self.entries.len();
                self.entries[index] = entry;
                index
            }
        };
        &self.entries[index].1
    }
}

#[allow(clippy::too_many_arguments)]
fn run_edges_impl<X, E>(
    executor: &mut X,
    plans: &DerivedTickPlans<E>,
    edges: &[EventEdge],
    scratch: &mut DerivedTickScratch,
    moves: &[(E, Edge)],
    dirty: bool,
    mut write: impl SignalWrite<X::Backend>,
    cache: Option<&mut CoincidentEdgeCache<E>>,
) -> Result<EdgeOutcome, Halt>
where
    X: TickExecutor,
    X::Backend: SimBackend<Event = E>,
    E: EventHandle,
{
    if let [(event, edge)] = moves {
        let plan = plans.get(event.id()).and_then(Option::as_ref);
        return run_edge(
            executor,
            plan,
            edges[event.id()],
            scratch,
            *event,
            *edge,
            dirty,
            write,
        );
    }
    let cached = cache.is_some();
    let mut sources: Vec<EventDomain<E>> = Vec::with_capacity(if cached { 0 } else { moves.len() });
    let mut moved_plans = Vec::new();
    let mut changed = Vec::with_capacity(if cached { moves.len() } else { 0 });
    let mut transitions: Vec<(SignalRef, (u8, u8))> = Vec::new();
    let mut comb_read = false;
    let mut any_moved = false;
    for &(event, edge) in moves {
        let id = event.id();
        let info = &edges[id];
        let old = level(executor.backend(), info.signal);
        let new = edge.level();
        if old == new || transitions.iter().any(|(signal, _)| *signal == info.signal) {
            continue;
        }
        any_moved = true;
        write.write(executor.backend_mut(), info.signal, new);
        transitions.push((info.signal, (old, new)));
        comb_read |=
            info.comb_read && (dirty || !executor.backend_mut().eval_clock_aliases(event)?);
        let fires = info.fires(known(old), edge);
        if cached {
            changed.push((event, fires));
        } else {
            if fires {
                let address = executor.backend().id_to_addr_slice()[id];
                sources.push(EventDomain {
                    eval_apply: event,
                    eval_only: executor.backend().resolve_eval_only_event(&address),
                    apply: executor.backend().resolve_apply_event(&address),
                });
            }
            if let Some(Some(plan)) = plans.get(id) {
                moved_plans.push(plan);
            }
        }
    }
    if !any_moved {
        return Ok(EdgeOutcome::Unchanged);
    }
    let uncached;
    let make = || {
        // Resolving phase handles and collecting constituent plans belongs to
        // a cache miss. Hits still inspect and drive the actual signal levels
        // above, and evaluate gate predicates when the retained plan runs.
        for &(event, fires) in &changed {
            let id = event.id();
            if fires {
                let address = executor.backend().id_to_addr_slice()[id];
                sources.push(EventDomain {
                    eval_apply: event,
                    eval_only: executor.backend().resolve_eval_only_event(&address),
                    apply: executor.backend().resolve_apply_event(&address),
                });
            }
            if let Some(Some(plan)) = plans.get(id) {
                moved_plans.push(plan);
            }
        }
        DerivedTickPlan::merge(sources, moved_plans)
    };
    let plan = if let Some(cache) = cache {
        cache.plan(&changed, make)
    } else {
        uncached = make();
        &uncached
    };
    let lever_transitions: Vec<(u8, u8)> = plan
        .lever_signals()
        .map(|signal| {
            transitions
                .iter()
                .find(|(moved, _)| *moved == signal)
                .map_or((0, 0), |(_, transition)| *transition)
        })
        .collect();
    let settled = plan.run(
        executor,
        scratch,
        dirty || comb_read,
        EdgeRun {
            transitions: &lever_transitions,
            persist: true,
            fire_sources: true,
        },
    )?;
    Ok(if settled {
        EdgeOutcome::Settled
    } else {
        moved_only(comb_read)
    })
}

/// One tick of `event`: a clock period of its signal, defined as the
/// SystemVerilog statements
///
/// ```systemverilog
/// s <= A; #H;
/// s <= I; #L;
/// ```
///
/// where `A` and `I` are the event's active and inactive levels: the active
/// edge, then the inactive edge, each running every domain sensitive to it.
/// Combinational logic is settled afterwards. `dirty` says whether it has
/// pending changes; `write` moves the signal.
#[allow(clippy::too_many_arguments)]
pub fn run_tick<X, E>(
    executor: &mut X,
    plan: Option<&DerivedTickPlan<E>>,
    info: EventEdge,
    scratch: &mut DerivedTickScratch,
    event: E,
    dirty: bool,
    mut write: impl FnMut(&mut X::Backend, SignalRef, u8),
) -> Result<(), Halt>
where
    X: TickExecutor,
    X::Backend: SimBackend<Event = E>,
    E: EventHandle,
{
    let active = info.active_edge();
    let mut dirty = dirty;
    for edge in [active, active.opposite()] {
        dirty = match run_edge(
            executor, plan, info, scratch, event, edge, dirty, &mut write,
        )? {
            EdgeOutcome::Unchanged | EdgeOutcome::Stored => dirty,
            EdgeOutcome::Moved => true,
            EdgeOutcome::Settled => false,
        };
    }
    if dirty {
        executor.eval_comb()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    type Op = GateOp<()>;

    const LOAD: Op = GateOp::Load {
        address: (),
        lsb: 0,
        width: 1,
    };

    #[test]
    fn gates_follow_their_source_through_monotone_operations() {
        use Dependence::*;
        let cases: [(&[Op], Dependence); 8] = [
            (&[GateOp::Source, LOAD, GateOp::And], Positive),
            (&[GateOp::Source, GateOp::Not { width: 1 }], Negative),
            (
                &[GateOp::Source, GateOp::LogicNot, LOAD, GateOp::LogicOr],
                Negative,
            ),
            (&[LOAD, GateOp::Const(1), GateOp::Or], Independent),
            (&[GateOp::Source, LOAD, GateOp::Xor], Mixed),
            (
                &[
                    GateOp::Source,
                    GateOp::Source,
                    GateOp::Not { width: 1 },
                    GateOp::Or,
                ],
                Mixed,
            ),
            (
                &[LOAD, GateOp::Source, GateOp::Const(0), GateOp::Mux],
                Positive,
            ),
            (
                &[GateOp::Source, LOAD, GateOp::Const(0), GateOp::Mux],
                Mixed,
            ),
        ];
        for (ops, expected) in cases {
            assert_eq!(dependence(ops), Some(expected), "{ops:?}");
        }
        assert_eq!(dependence::<()>(&[GateOp::And]), None);
        assert_eq!(dependence::<()>(&[GateOp::Source, GateOp::Source]), None);
    }

    #[test]
    fn a_clock_is_reached_by_the_edges_its_dependence_allows() {
        use DomainKind::*;
        let posedge = Some(ClockPosedge);
        let negedge = Some(ClockNegedge);
        // The falling edge of a positive-edge clock's source.
        let edge = Edge::Falling;
        assert!(!Dependence::Positive.can_reach(edge, posedge));
        assert!(Dependence::Positive.can_reach(edge, negedge));
        assert!(Dependence::Negative.can_reach(edge, posedge));
        assert!(!Dependence::Negative.can_reach(edge, Some(ResetAsyncLow)));
        assert!(Dependence::Positive.can_reach(edge, None));
        assert!(!Dependence::Independent.can_reach(edge, None));
        assert!(Dependence::Mixed.can_reach(edge, posedge));
    }
}
