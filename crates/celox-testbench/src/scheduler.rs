//! Persistent state of the generated Veryl testbench scheduler.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestbenchScheduler<A> {
    pub entry: Option<usize>,
    /// Continuation, exit, simulation time, elapsed ticks, tick limit,
    /// remaining slots before a runtime safe point, current process, edge-path
    /// permission, finish state, settle-path permission, event reporting time, settle continuation,
    /// passing-assertion recording, runtime events since the last time marker, and runtime events
    /// since the host last drained them.
    /// Indices are the `TB_*` constants below.
    pub control: [A; 15],
    /// State (ready/waiting/done/releasing), remaining cycles, clock, wake time, reset request.
    pub processes: Vec<[A; 5]>,
    /// Next rising edge, next falling edge, falling armed, generator enabled.
    pub clocks: Vec<[A; 4]>,
    pub events: Vec<A>,
    pub periods: Vec<u64>,
    pub declared: usize,
    /// Per-process homes for function temporaries; design variables stay shared.
    pub locals: Vec<Vec<(A, A)>>,
    pub baselines: Vec<(A, A)>,
    /// The runtime event site of time markers: a marker gives the events
    /// recorded before it, since the previous one, the simulation time its
    /// argument holds.
    pub marker_site: Option<u32>,
}

impl<A: Clone> TestbenchScheduler<A> {
    pub fn map<B>(self, mut f: impl FnMut(A) -> B) -> TestbenchScheduler<B> {
        TestbenchScheduler {
            entry: self.entry,
            control: self.control.map(&mut f),
            processes: self.processes.into_iter().map(|v| v.map(&mut f)).collect(),
            clocks: self.clocks.into_iter().map(|v| v.map(&mut f)).collect(),
            events: self.events.into_iter().map(&mut f).collect(),
            periods: self.periods,
            declared: self.declared,
            marker_site: self.marker_site,
            baselines: self
                .baselines
                .into_iter()
                .map(|(a, b)| (f(a), f(b)))
                .collect(),
            locals: self
                .locals
                .into_iter()
                .map(|v| v.into_iter().map(|(a, b)| (f(a), f(b))).collect())
                .collect(),
        }
    }
}

pub const TB_PHASE: usize = 0;
pub const TB_EXIT: usize = 1;
pub const TB_NOW: usize = 2;
pub const TB_TICKS: usize = 3;
pub const TB_LIMIT: usize = 4;
pub const TB_BUDGET: usize = 5;
pub const TB_CURRENT: usize = 6;
pub const TB_HOST: u64 = 1;
pub const TB_EDGES: u64 = 2;
pub const TB_SAFEPOINT: u64 = 3;
pub const TB_DONE: u64 = 4;
pub const TB_LIMITED: u64 = 5;
pub const TB_OVERFLOW: u64 = 6;

pub const TB_RESET: u64 = 7;
/// A time step ended; the host records it in the waveform.
pub const TB_PUBLISH: u64 = 11;
/// `control[TB_NATIVE_EDGES]`: 0 when edges run in the host, 1 when in the
/// unit, [`TB_EDGES_PUBLISHED`] when in the unit with each time step published.
pub const TB_NATIVE_EDGES: usize = 7;
pub const TB_EDGES_PUBLISHED: u64 = 2;
pub const TB_STOPPED: usize = 8;
pub const TB_NATIVE_SETTLE: usize = 9;
pub const TB_FINISHED: u64 = 8;
pub const TB_REPORT_TIME: usize = 10;
pub const TB_SETTLE_CONT: usize = 11;
/// Nonzero when the host records passing assertions: they then run there.
pub const TB_RECORD: usize = 12;
pub const TB_UNMARKED: usize = 13;
pub const TB_PENDING: usize = 14;
/// The host only drains runtime events; the unit resumes where it left.
pub const TB_DRAIN: u64 = 9;
/// A fatal assertion failed. Its report is in the runtime events.
pub const TB_FAILED: u64 = 10;
