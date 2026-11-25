//! The activation counts of a compiled scheduler
//! (`SimulatorBuilder::activation_profile`): how often each part of a time
//! slot ran, what started it, and how often it passed an edge or a change
//! on.

/// What part of the compiled scheduler an [`ActivationSite`] counts.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ActivationKind {
    /// The time slots the unit runs.
    Slot,
    /// A host periodic clock (`add_clock`): the slots it is due in and the
    /// edges it gives.
    HostClock,
    /// A process kernel the unit runs: its runs and the edges its writes
    /// give.
    Kernel,
    /// The judgment of which clock domains a wave of a slot triggers:
    /// how often it is made, and how often it finds none.
    DomainJudge,
    /// The sequential units of one event's domain, by what runs: `eval_apply`,
    /// `eval_only`, `apply` or `fused` (with the settle before it).
    Domain,
    /// A request for the shared settle, by the point of the slot that
    /// requested it: what woke the settle.
    SettleCall,
    /// The shared settle: entered, run because its inputs changed, or
    /// skipped because they did not.
    Settle,
    /// A group of the event-driven settle, which runs only when an event it
    /// depends on occurred.
    SettleGroup,
    /// The stateful part of a deferred settle.
    EagerSettle,
    /// The observers of combinational processes (`$display` and deferred
    /// assertions in `always_comb`, `$monitor`) a settle checks, and a host
    /// slot observer (VPI callbacks, waveform dumps) the slots notify.
    Observer,
}

/// One counter of a compiled scheduler built with
/// `SimulatorBuilder::activation_profile`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ActivationSite {
    pub kind: ActivationKind,
    /// What the counter belongs to: an event signal, a kernel, a call site.
    pub subject: String,
    /// What it counts: `runs`, `edges`, `quiet`, `dirty`, ...
    pub measure: String,
}

/// One counter of [`ActivationProfile`] and its count.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ActivationCount {
    #[serde(flatten)]
    pub site: ActivationSite,
    pub count: u64,
}

/// The activations a compiled scheduler counted since the simulation
/// started (`SimulatorBuilder::activation_profile`).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct ActivationProfile {
    pub counters: Vec<ActivationCount>,
}

impl ActivationProfile {
    #[cfg(feature = "host-runtime")]
    pub(crate) fn new(sites: &[ActivationSite], counts: &[u64]) -> Self {
        Self {
            counters: sites
                .iter()
                .zip(counts)
                .map(|(site, &count)| ActivationCount {
                    site: site.clone(),
                    count,
                })
                .collect(),
        }
    }

    /// The count of `kind`'s `measure` on `subject`, if counted.
    pub fn count(&self, kind: ActivationKind, measure: &str, subject: &str) -> Option<u64> {
        self.counters
            .iter()
            .find(|counter| {
                counter.site.kind == kind
                    && counter.site.measure == measure
                    && counter.site.subject == subject
            })
            .map(|counter| counter.count)
    }
}
