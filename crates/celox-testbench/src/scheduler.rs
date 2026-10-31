//! Persistent state of the generated Veryl testbench scheduler.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestbenchScheduler<A> {
    pub entry: Option<usize>,
    /// Continuation, exit, simulation time, elapsed ticks, tick limit,
    /// remaining slots before a runtime safe point, current process, edge-path
    /// permission, finish state, settle-path permission, event reporting time, and settle continuation.
    /// Indices are the `TB_*` constants below.
    pub control: [A; 12],
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
pub const TB_NATIVE_EDGES: usize = 7;
pub const TB_STOPPED: usize = 8;
pub const TB_NATIVE_SETTLE: usize = 9;
pub const TB_FINISHED: u64 = 8;
pub const TB_REPORT_TIME: usize = 10;
pub const TB_SETTLE_CONT: usize = 11;
