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

use celox_design::{DomainKind, EventTopology};

use crate::{
    AbsoluteAddr, SignalRef,
    backend::{EventHandle, SimBackend},
    derived_tick::{
        DerivedTickPlan, DerivedTickPlans, DerivedTickScratch, EdgeRun, EventDomain, TickExecutor,
    },
    simulation::event_level,
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
    fn level(self) -> u8 {
        match self {
            Edge::Rising => 1,
            Edge::Falling => 0,
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
}

impl EventEdge {
    /// The edge that is the event's own active edge.
    pub fn active_edge(&self) -> Edge {
        match self.kind {
            Some(DomainKind::ClockNegedge | DomainKind::ResetAsyncLow) => Edge::Falling,
            _ => Edge::Rising,
        }
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
            EventEdge {
                signal: backend.resolve_signal(&canonical),
                kind: kinds.get(&canonical).copied(),
                comb_read: events.combinationally_read.contains(&canonical),
            }
        })
        .collect()
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

/// The level of `signal`, or `None` while it is unknown.
fn level<B: SimBackend>(backend: &B, signal: SignalRef) -> Option<bool> {
    if signal.is_4state && backend.layout().four_state {
        let (value, mask) = backend.get_four_state(signal);
        if mask.bit(0) {
            return None;
        }
        return Some(value.bit(0));
    }
    Some(event_level(backend, signal) != 0)
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

/// Move `info`'s signal to the level of `edge` through `write`. Returns the
/// level it had (`None` while unknown), or `None` when it already was at
/// that level.
pub fn move_signal<B: SimBackend>(
    backend: &mut B,
    info: &EventEdge,
    edge: Edge,
    mut write: impl SignalWrite<B>,
) -> Option<Option<bool>> {
    let old = level(backend, info.signal);
    let new = edge.level();
    if old == Some(new != 0) {
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
    let Some(old) = move_signal(executor.backend_mut(), &info, edge, write) else {
        return Ok(EdgeOutcome::Unchanged);
    };
    let new = edge.level();
    let fires = info.fires(old, edge);
    let settle = dirty || info.comb_read;
    if let Some(plan) = plan {
        let from = old.map_or(new ^ 1, u8::from);
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
            moved_only(info.comb_read)
        });
    }
    if !fires {
        return Ok(moved_only(info.comb_read));
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
    mut write: impl SignalWrite<X::Backend>,
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
    let mut sources: Vec<EventDomain<E>> = Vec::with_capacity(moves.len());
    let mut moved_plans = Vec::new();
    let mut transitions: Vec<(SignalRef, (u8, u8))> = Vec::new();
    let mut comb_read = false;
    let mut any_moved = false;
    for &(event, edge) in moves {
        let id = event.id();
        let info = &edges[id];
        let old = level(executor.backend(), info.signal);
        let new = edge.level();
        if old == Some(new != 0) || transitions.iter().any(|(signal, _)| *signal == info.signal) {
            continue;
        }
        any_moved = true;
        write.write(executor.backend_mut(), info.signal, new);
        transitions.push((info.signal, (old.map_or(new ^ 1, u8::from), new)));
        comb_read |= info.comb_read;
        if info.fires(old, edge) {
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
    if !any_moved {
        return Ok(EdgeOutcome::Unchanged);
    }
    let plan = DerivedTickPlan::merge(sources, moved_plans);
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
