//! The scheduling facts of a design, derived once from its elaborated
//! design and runtime schema.

use fxhash::FxHashMap as HashMap;
use serde::{Deserialize, Serialize};

use crate::{
    CounterKind, CounterSlots, DomainKind, ElaboratedDesign, EventTopology, NbaSite, ProcessSlots,
    RuntimeEventKind, RuntimeSchema,
};

/// What a timed scheduler needs to know about a design, keyed by address
/// and independent of any backend. Every input is already persisted with
/// a compiled design, so the model is derived again rather than stored.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(bound(
    serialize = "A: Serialize + Eq + std::hash::Hash + Ord",
    deserialize = "A: Deserialize<'de> + Eq + std::hash::Hash + Ord"
))]
pub struct ScheduleModel<A> {
    /// The event-domain topology.
    pub events: EventTopology<A>,
    /// Effective kinds of the canonical event domains
    /// ([`ElaboratedDesign::event_kinds`]).
    pub kinds: HashMap<A, DomainKind>,
    /// Counted signals with all their counters, and the canonical domains
    /// each counter triggers, in [`RuntimeSchema::watches`] order.
    pub counted: Vec<CountedModel<A>>,
    /// Bit ranges whose changes bump an occurrence counter
    /// ([`RuntimeSchema::occurrence_probes`]).
    pub probes: Vec<(crate::VarAtomBase<A>, A)>,
    /// Process kernels in kernel order.
    pub processes: Vec<ProcessModel<A>>,
    /// Signals a wait watches, which the host's writes must be logged for.
    pub waited: Vec<A>,
    /// The design has process kernels. Without them, a round of the
    /// scheduler is the batch step, which also checks counted signals.
    pub timed: bool,
    /// Some nonblocking site has a pending flag.
    pub has_pending_nba_sites: bool,
    /// The design can execute `$finish`.
    pub finish_sites: bool,
    /// Canonical event domains whose evaluation commits their blocking
    /// results ([`RuntimeSchema::active_commits`]).
    pub active_commits: Vec<A>,
    /// Objects the scheduler stores the current time into
    /// ([`RuntimeSchema::time_slots`]).
    pub time_slots: Vec<A>,
    /// Sampled variables and their copies
    /// ([`RuntimeSchema::sample_shadows`]).
    pub sample_shadows: Vec<(A, A)>,
    /// The compiled scheduler, if one was generated
    /// ([`RuntimeSchema::scheduler`]).
    pub scheduler: Option<crate::SchedulerSchema<A>>,
}

/// A counted signal and the canonical domains its counters trigger.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CountedModel<A> {
    pub slots: CounterSlots<A>,
    pub domains: Vec<(A, CounterKind)>,
}

/// One process kernel: its control slots, its wait sites by local index
/// and its nonblocking sites by local index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessModel<A> {
    pub slots: ProcessSlots<A>,
    pub frame: Option<crate::ProcessFrame<A>>,
    pub waits: Vec<Vec<TermModel<A>>>,
    pub nbas: Vec<NbaSite<A>>,
    pub host_requests: Vec<crate::HostRequestSite<A>>,
}

impl<A: Copy + Eq> ProcessModel<A> {
    /// The common event-expression list when every site waits for the same
    /// expressions. Different scalar sites use `wait_terms` instead.
    pub fn common_wait_terms(&self) -> Option<&[TermModel<A>]> {
        let first = self.waits.first()?;
        (!first.is_empty() && self.waits.iter().all(|site| site == first))
            .then_some(first.as_slice())
    }

    /// For a process whose every wait site has one term, the distinct
    /// edges it waits for, in the order of the first site that waits for
    /// each, and the edge of each site as an index into them.
    pub fn wait_terms(&self) -> Option<(Vec<(A, CounterKind)>, Vec<usize>)> {
        let mut terms: Vec<(A, CounterKind)> = Vec::new();
        let site_terms = self
            .waits
            .iter()
            .map(|site| {
                let [term] = site.as_slice() else {
                    return None;
                };
                let edge = (term.signal, term.kind);
                Some(
                    terms
                        .iter()
                        .position(|&known| known == edge)
                        .unwrap_or_else(|| {
                            terms.push(edge);
                            terms.len() - 1
                        }),
                )
            })
            .collect::<Option<_>>()?;
        Some((terms, site_terms))
    }
}

/// One term of a wait site.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermModel<A> {
    pub signal: A,
    pub kind: CounterKind,
    /// Design code may write the signal. Otherwise only the host and the
    /// scheduler do.
    pub driven: bool,
    /// The term's `iff` condition ([`crate::WaitSite::gates`]).
    pub gate: Option<crate::WaitGate<A>>,
}

impl<A: Copy + Eq + std::hash::Hash> ScheduleModel<A> {
    pub fn derive(design: &ElaboratedDesign<A>, schema: &RuntimeSchema<A>) -> Self {
        let counted: Vec<CountedModel<A>> = schema
            .watches
            .iter()
            .filter_map(|watch| {
                Some(CountedModel {
                    slots: CounterSlots {
                        signal: watch.signal,
                        changes: watch.changes?,
                        rises: watch.rises?,
                        falls: watch.falls?,
                    },
                    domains: watch.domains.clone(),
                })
            })
            .collect();
        // Signals design code writes; the rest only the host and the
        // scheduler write.
        let driven: fxhash::FxHashSet<A> = schema.rtl_writes.iter().map(|atom| atom.id).collect();
        let mut processes: Vec<ProcessModel<A>> = schema
            .processes
            .iter()
            .enumerate()
            .map(|(index, slots)| ProcessModel {
                frame: schema.process_frames.get(index).cloned().flatten(),
                slots: *slots,
                waits: Vec::new(),
                nbas: Vec::new(),
                host_requests: Vec::new(),
            })
            .collect();
        for site in &schema.wait_sites {
            let terms = site
                .terms
                .iter()
                .enumerate()
                .map(|(index, &(signal, kind))| {
                    let gate = site.gate(index);
                    TermModel {
                        signal,
                        kind,
                        // A gate without counters is judged at checks
                        // after settling, which only check driven terms.
                        driven: driven.contains(&signal)
                            || gate.is_some_and(|gate| gate.counters.is_none()),
                        gate,
                    }
                })
                .collect();
            processes[site.process].waits.push(terms);
        }
        for site in &schema.nba_sites {
            processes[site.process].nbas.push(site.clone());
        }
        for site in &schema.host_request_sites {
            processes[site.process].host_requests.push(site.clone());
        }
        let waited = schema
            .wait_sites
            .iter()
            .flat_map(|site| site.terms.iter().map(|&(signal, _)| signal))
            .collect();
        let has_pending_nba_sites = processes
            .iter()
            .any(|process| process.nbas.iter().any(|site| site.pending.is_some()));
        Self {
            events: design.events.clone(),
            kinds: design.event_kinds(),
            timed: processes
                .iter()
                .any(|process| process.slots.kind != crate::ProcessKind::Callable),
            counted,
            probes: schema.occurrence_probes.clone(),
            processes,
            waited,
            has_pending_nba_sites,
            finish_sites: schema
                .runtime_event_sites
                .iter()
                .any(|site| matches!(site.kind, RuntimeEventKind::Finish)),
            active_commits: schema.active_commits.clone(),
            time_slots: schema.time_slots.clone(),
            sample_shadows: schema.sample_shadows.clone(),
            scheduler: schema.scheduler.clone(),
        }
    }

    /// The effective kind of the canonical event domain `address`.
    pub fn kind(&self, address: &A) -> Option<DomainKind> {
        self.kinds.get(address).copied()
    }
}

/// Raw word layout shared by generated waits and their runtime handover.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompiledWaitLayout {
    pub terms: Vec<CompiledWaitTerm>,
    pub bits: usize,
    pub scalar_byte: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompiledWaitTerm {
    pub width: usize,
    pub value_offset: usize,
    pub mask_offset: Option<usize>,
    pub plane_bits: usize,
}

impl CompiledWaitLayout {
    /// A single scalar retains the existing byte encoding. Other lists hold
    /// raw value/mask planes in complete words, so generated comparisons can
    /// distinguish X and Z without constructing a runtime BigUint per slot.
    pub fn new(terms: &[(usize, bool)]) -> Option<Self> {
        if terms.is_empty() || terms.iter().any(|&(width, _)| width == 0) {
            return None;
        }
        if let &[(1, mask)] = terms {
            return Some(Self {
                terms: vec![CompiledWaitTerm {
                    width: 1,
                    value_offset: 0,
                    mask_offset: mask.then_some(1),
                    plane_bits: 1,
                }],
                bits: 8,
                scalar_byte: true,
            });
        }
        let mut offset = 0usize;
        let terms = terms
            .iter()
            .map(|&(width, mask)| {
                let plane_bits = width.div_ceil(64).checked_mul(64)?;
                let value_offset = offset;
                offset = offset.checked_add(plane_bits)?;
                let mask_offset = if mask {
                    let at = offset;
                    offset = offset.checked_add(plane_bits)?;
                    Some(at)
                } else {
                    None
                };
                Some(CompiledWaitTerm {
                    width,
                    value_offset,
                    mask_offset,
                    plane_bits,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            terms,
            bits: offset,
            scalar_byte: false,
        })
    }
}
