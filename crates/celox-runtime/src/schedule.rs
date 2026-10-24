//! A design's scheduling facts resolved against one backend.

use celox_design::{DomainKind, schedule::ScheduleModel};
use fxhash::{FxHashMap, FxHashSet};

use crate::{
    AbsoluteAddr, EventDomain, EventInfo, HostWatch, SignalRef,
    backend::{EventHandle, SimBackend},
    timing::{CountedDomainRefs, CountedRefs, NbaSiteRefs, ProbeRefs, ProcessRefs, TermRefs},
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
    /// The kernels of program instances, in declaration order.
    pub(crate) program_processes: Vec<usize>,
    /// Counted signals and the domains their counters trigger.
    pub(crate) counted: Vec<CountedDomainRefs>,
    pub(crate) counted_by_offset: FxHashMap<usize, usize>,
    /// Each counter that triggers a domain: the counter, the event id, and
    /// where its last seen value is kept ([`ProcessState::domain_counters`]).
    pub(crate) counted_triggers: Vec<CountedTrigger>,
    /// Occurrence probes by the offset of their signal.
    pub(crate) probes_by_offset: FxHashMap<usize, Vec<ProbeRefs>>,
    /// Whether any process has a nonblocking site with a pending flag.
    pub(crate) has_pending_nba_sites: bool,
    /// Per event ID, the event IDs of clocks computed combinationally from
    /// the event's signal. Empty when the design has none.
    pub(crate) combinational_derived: Vec<Box<[usize]>>,
    /// The design has process kernels. Without them, a round is the batch
    /// step and skips every process check.
    pub(crate) timed: bool,
    /// Canonical event IDs whose evaluation commits their blocking results.
    pub(crate) active_commits: bit_set::BitSet,
    /// Signals the current time is stored into before each round.
    pub(crate) time_slots: Vec<SignalRef>,
    /// The compiled scheduler, when one was generated.
    pub(crate) scheduler: Option<SchedulerRefs>,
    /// Per process kernel, why the compiled scheduler does not run it, or
    /// `None` when it does. Empty when no scheduler was considered.
    pub(crate) kernel_fallback: Vec<Option<String>>,
}

/// A design's compiled scheduler resolved against a backend: its process
/// unit and the slots it exchanges a run with.
pub(crate) struct SchedulerRefs {
    pub(crate) unit: usize,
    pub(crate) limit: SignalRef,
    pub(crate) now: SignalRef,
    pub(crate) exit: SignalRef,
    pub(crate) exit_arg: SignalRef,
    pub(crate) finish: SignalRef,
    pub(crate) count: SignalRef,
    pub(crate) current: SignalRef,
    pub(crate) levels: SignalRef,
    pub(crate) ready: SignalRef,
    /// The wake and waiting slots of each kernel the scheduler runs that
    /// only delays, by process index.
    pub(crate) wake: Vec<Option<(SignalRef, SignalRef)>>,
    /// The waiting and baseline slots of each kernel the scheduler runs that
    /// waits for an edge, by process index.
    pub(crate) waits: Vec<Option<(SignalRef, SignalRef)>>,
    /// The number of such kernels.
    pub(crate) wait_kernels: usize,
    /// The bit of each kernel the scheduler runs in its ready mask: its
    /// place among them in kernel order.
    pub(crate) ordinals: Vec<Option<u32>>,
    /// The event ids of the bits of `levels`.
    pub(crate) events: Vec<usize>,
    /// The event id and signal of each host clock the scheduler may run,
    /// in the order of its clock slots.
    pub(crate) clocks: Vec<(usize, SignalRef)>,
    pub(crate) clock_next: SignalRef,
    pub(crate) clock_period: SignalRef,
    pub(crate) clock_value: SignalRef,
    pub(crate) clock_armed: SignalRef,
}

impl SchedulerRefs {
    /// Element `bit` of the per-clock slot `slots`.
    pub(crate) fn clock_element(slots: SignalRef, bit: usize) -> SignalRef {
        let width = celox_design::SCHEDULER_TIME_WIDTH;
        SignalRef {
            offset: slots.offset + bit * width / 8,
            width,
            is_4state: false,
            array_layout: None,
        }
    }

    /// The clock slot that runs a scheduled `event` of `signal`, if any.
    pub(crate) fn clock_of(&self, event: usize, signal: SignalRef) -> Option<usize> {
        self.clocks
            .iter()
            .position(|&(id, clock)| id == event && clock == signal)
    }
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
                is_initializer: process.slots.is_initializer,
                program: process.slots.program.as_ref().map(resolve),
                resume: resolve(&process.slots.resume),
                starts_with_event_wait: process.slots.requires_timed_simulation,
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
                host_requests: process
                    .host_requests
                    .iter()
                    .map(|site| site.map(|id| resolve(&id)))
                    .collect(),
                nba_sites: process
                    .nbas
                    .iter()
                    .map(|site| NbaSiteRefs {
                        target: resolve(&site.target),
                        lsb: site.lsb,
                        width: site.width,
                        value: resolve(&site.value),
                        position: site
                            .position
                            .map(|position| position.map(|id| resolve(&id))),
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
        let counted_triggers = counted
            .iter()
            .enumerate()
            .flat_map(|(index, counted)| {
                counted
                    .domains
                    .iter()
                    .map(move |&(id, kind)| CountedTrigger {
                        counter: counted.refs.counter(kind),
                        id,
                        seen: (index, crate::timing::kind_index(kind)),
                    })
            })
            .collect();
        let mut probes_by_offset: FxHashMap<usize, Vec<ProbeRefs>> = FxHashMap::default();
        for (source, counter) in &model.probes {
            let probe = ProbeRefs {
                lsb: source.access.lsb,
                msb: source.access.msb,
                counter: resolve(counter),
            };
            let probes = probes_by_offset
                .entry(resolve(&source.id).offset)
                .or_default();
            if !probes.contains(&probe) {
                probes.push(probe);
            }
        }
        Self {
            topo_signals,
            domain_kinds,
            event_info,
            signal_to_id,
            signal_index_by_id,
            program_processes: processes
                .iter()
                .enumerate()
                .filter(|(_, process)| process.program.is_some())
                .map(|(index, _)| index)
                .collect(),
            processes,
            counted,
            counted_by_offset,
            counted_triggers,
            probes_by_offset,
            has_pending_nba_sites: model.has_pending_nba_sites,
            combinational_derived,
            timed: model.timed,
            active_commits,
            time_slots: model.time_slots.iter().map(resolve).collect(),
            scheduler: model.scheduler.as_ref().and_then(|scheduler| {
                let slots = &scheduler.slots;
                Some(SchedulerRefs {
                    unit: scheduler.unit?,
                    limit: resolve(&slots.limit),
                    now: resolve(&slots.now),
                    exit: resolve(&slots.exit),
                    exit_arg: resolve(&slots.exit_arg),
                    finish: resolve(&slots.finish),
                    count: resolve(&slots.count),
                    current: resolve(&slots.current),
                    levels: resolve(&slots.levels),
                    ready: resolve(&slots.ready),
                    wake: (0..scheduler.fallback.len())
                        .map(|index| {
                            (scheduler.fallback[index].is_none()
                                && model.processes[index].waits.is_empty())
                            .then(|| (resolve(&slots.wake[index]), resolve(&slots.waiting[index])))
                        })
                        .collect(),
                    waits: (0..scheduler.fallback.len())
                        .map(|index| {
                            (scheduler.fallback[index].is_none()
                                && !model.processes[index].waits.is_empty())
                            .then(|| {
                                (
                                    resolve(&slots.waiting[index]),
                                    resolve(&slots.baseline[index]),
                                )
                            })
                        })
                        .collect(),
                    ordinals: {
                        let mut next = 0;
                        scheduler
                            .fallback
                            .iter()
                            .map(|fallback| {
                                fallback.is_none().then(|| {
                                    next += 1;
                                    next - 1
                                })
                            })
                            .collect()
                    },
                    wait_kernels: (0..scheduler.fallback.len())
                        .filter(|&index| {
                            scheduler.fallback[index].is_none()
                                && !model.processes[index].waits.is_empty()
                        })
                        .count(),
                    events: scheduler
                        .events
                        .iter()
                        .map(|event| backend.resolve_event_opt(event).map(|event| event.id()))
                        .collect::<Option<_>>()?,
                    clocks: scheduler
                        .clocks
                        .iter()
                        .map(|clock| {
                            let event = backend.resolve_event_opt(clock)?;
                            Some((event.id(), resolve(clock)))
                        })
                        .collect::<Option<_>>()?,
                    clock_next: resolve(&slots.clock_next),
                    clock_period: resolve(&slots.clock_period),
                    clock_value: resolve(&slots.clock_value),
                    clock_armed: resolve(&slots.clock_armed),
                })
            }),
            kernel_fallback: model
                .scheduler
                .as_ref()
                .map(|scheduler| scheduler.fallback.clone())
                .unwrap_or_default(),
        }
    }
}

/// A counter that triggers a domain.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CountedTrigger {
    pub counter: SignalRef,
    pub id: usize,
    /// The index of the counted signal and of the counter kind.
    pub seen: (usize, usize),
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
