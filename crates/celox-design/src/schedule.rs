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
    pub waits: Vec<Vec<TermModel<A>>>,
    pub nbas: Vec<NbaSite<A>>,
    pub host_requests: Vec<crate::HostRequestSite<A>>,
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
            .map(|slots| ProcessModel {
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
            timed: !processes.is_empty(),
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
            scheduler: schema.scheduler.clone(),
        }
    }

    /// The effective kind of the canonical event domain `address`.
    pub fn kind(&self, address: &A) -> Option<DomainKind> {
        self.kinds.get(address).copied()
    }
}
