//! A design's scheduling facts resolved against one backend.

use celox_design::{DomainKind, schedule::ScheduleModel};
use fxhash::{FxHashMap, FxHashSet};

use crate::{
    AbsoluteAddr, EventDomain, EventInfo, HostWatch, SignalRef,
    backend::{EventHandle, SimBackend},
    timing::{CountedDomainRefs, CountedRefs, NbaSiteRefs, ProcessRefs, TermRefs},
};

/// The facts a timed simulation keeps but never changes: event signals,
/// domain kinds and kernels by event ID, counted signals and process sites,
/// all resolved from a [`ScheduleModel`] against a backend.
pub struct ResolvedSchedule<B: SimBackend> {
    /// Event signals in evaluation order and their event IDs, `usize::MAX`
    /// for a signal without one.
    pub(crate) topo_signals: Vec<(SignalRef, usize)>,
    pub(crate) domain_kinds: Vec<Option<DomainKind>>,
    pub(crate) event_info: Vec<EventInfo<B>>,
    pub(crate) signal_to_id: FxHashMap<SignalRef, usize>,
    pub(crate) signal_index_by_id: Vec<usize>,
    pub(crate) processes: Vec<ProcessRefs>,
    /// Counted signals and the domains their counters trigger.
    pub(crate) counted: Vec<CountedDomainRefs>,
    pub(crate) counted_by_offset: FxHashMap<usize, usize>,
    /// Whether any process has a nonblocking site with a pending flag.
    pub(crate) has_pending_nba_sites: bool,
    /// Per event ID, the event IDs of clocks computed combinationally from
    /// the event's signal. Empty when the design has none.
    pub(crate) combinational_derived: Vec<Box<[usize]>>,
    /// The design has process kernels or counted signals. Without either, a
    /// round is exactly the batch step and skips every process check.
    pub(crate) timed: bool,
    /// Canonical event IDs whose evaluation commits their blocking results.
    pub(crate) active_commits: bit_set::BitSet,
    /// Signals the current time is stored into before each round.
    pub(crate) time_slots: Vec<SignalRef>,
}

impl<B: SimBackend> ResolvedSchedule<B> {
    pub fn resolve(backend: &B, model: &ScheduleModel<AbsoluteAddr>) -> Self {
        let num_events = backend.num_events();
        let events = &model.events;
        let topo_signals: Vec<(SignalRef, usize)> = events
            .ordered_events
            .iter()
            .map(|addr| {
                let signal = backend.resolve_signal(addr);
                let id = backend
                    .resolve_event_opt(addr)
                    .map(|ev| ev.id())
                    .unwrap_or(usize::MAX);
                (signal, id)
            })
            .collect();

        let mut domain_kinds = vec![None; num_events];
        for (_, id) in topo_signals.iter().copied() {
            if id != usize::MAX {
                let addr = backend.id_to_addr_slice()[id];
                domain_kinds[id] = model.kind(&addr);
            }
        }

        let mut event_info = vec![
            EventInfo {
                canonical_id: usize::MAX,
                is_cascaded: false,
                eval_ff_event: None,
                eval_only_event: None,
                apply_event: None,
            };
            num_events
        ];
        for (id, info) in event_info.iter_mut().enumerate() {
            let addr = backend.id_to_addr_slice()[id];
            let canonical = events.canonical(addr);
            let is_cascaded = events.cascaded_events.contains(&canonical);

            if let Some(domain) = EventDomain::resolve(backend, &canonical) {
                *info = EventInfo {
                    canonical_id: domain.eval_apply.id(),
                    is_cascaded,
                    eval_ff_event: Some(domain.eval_apply),
                    eval_only_event: domain.eval_only,
                    apply_event: domain.apply,
                };
            }
        }

        let mut signal_to_id = FxHashMap::default();
        let mut signal_index_by_id = vec![usize::MAX; num_events];
        for (index, (signal, id)) in topo_signals.iter().copied().enumerate() {
            if id == usize::MAX {
                continue;
            }
            signal_to_id.insert(signal, id);
            signal_index_by_id[id] = index;
        }

        let resolve = |addr: &AbsoluteAddr| backend.resolve_signal(addr);
        let counted: Vec<CountedDomainRefs> = model
            .counted
            .iter()
            .map(|counted| CountedDomainRefs {
                refs: counted_refs(backend, counted),
                domains: counted
                    .domains
                    .iter()
                    .filter_map(|(event, kind)| {
                        Some((backend.resolve_event_opt(event)?.id(), *kind))
                    })
                    .collect(),
            })
            .collect();
        let counter_of = |signal: SignalRef, kind| {
            counted
                .iter()
                .find(|counted| counted.refs.signal == signal)
                .map(|counted| counted.refs.counter(kind))
        };
        let processes: Vec<ProcessRefs> = model
            .processes
            .iter()
            .map(|process| ProcessRefs {
                status: resolve(&process.slots.status),
                delay: resolve(&process.slots.delay),
                wait_sites: process
                    .waits
                    .iter()
                    .map(|terms| {
                        terms
                            .iter()
                            .map(|term| {
                                let signal = resolve(&term.signal);
                                TermRefs {
                                    signal,
                                    kind: term.kind,
                                    counter: counter_of(signal, term.kind),
                                    driven: term.driven,
                                }
                            })
                            .collect()
                    })
                    .collect(),
                nba_sites: process
                    .nbas
                    .iter()
                    .map(|site| NbaSiteRefs {
                        target: resolve(&site.target),
                        lsb: site.lsb,
                        width: site.width,
                        value: resolve(&site.value),
                        offset: site.offset.as_ref().map(resolve),
                        pending: site.pending.as_ref().map(resolve),
                    })
                    .collect(),
            })
            .collect();

        let combinational_derived = if events.combinational_derived.is_empty() {
            Vec::new()
        } else {
            backend
                .id_to_addr_slice()
                .iter()
                .map(|address| {
                    events
                        .combinational_derived
                        .get(&events.canonical(*address))
                        .into_iter()
                        .flatten()
                        .filter_map(|derived| backend.resolve_event_opt(&derived.clock))
                        .map(|event| event.id())
                        .collect()
                })
                .collect()
        };

        let mut active_commits = bit_set::BitSet::new();
        for address in &model.active_commits {
            if let Some(event) = backend.resolve_event_opt(address) {
                active_commits.insert(event.id());
            }
        }

        let counted_by_offset = counted
            .iter()
            .enumerate()
            .map(|(index, counted)| (counted.refs.signal.offset, index))
            .collect();
        Self {
            topo_signals,
            domain_kinds,
            event_info,
            signal_to_id,
            signal_index_by_id,
            processes,
            counted,
            counted_by_offset,
            has_pending_nba_sites: model.has_pending_nba_sites,
            combinational_derived,
            timed: model.timed,
            active_commits,
            time_slots: model.time_slots.iter().map(resolve).collect(),
        }
    }
}

fn counted_refs<B: SimBackend>(
    backend: &B,
    counted: &celox_design::schedule::CountedModel<AbsoluteAddr>,
) -> CountedRefs {
    let resolve = |addr: &AbsoluteAddr| backend.resolve_signal(addr);
    CountedRefs {
        signal: resolve(&counted.slots.signal),
        changes: resolve(&counted.slots.changes),
        rises: resolve(&counted.slots.rises),
        falls: resolve(&counted.slots.falls),
    }
}

/// Host writes a timed simulation of a design observes one at a time: those
/// of counted signals and of signals a wait watches.
pub fn host_watch<B: SimBackend>(backend: &B, model: &ScheduleModel<AbsoluteAddr>) -> HostWatch {
    HostWatch::new(
        model
            .counted
            .iter()
            .map(|counted| counted_refs(backend, counted)),
        model
            .waited
            .iter()
            .map(|signal| backend.resolve_signal(signal).offset)
            .collect::<FxHashSet<usize>>(),
    )
}
