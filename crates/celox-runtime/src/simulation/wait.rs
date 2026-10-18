//! Waiting processes: registering waits and testing them against writes.

use celox_design::CounterKind;

use super::SimulationState;
use crate::{
    SignalRef, SimulatorErrorCode,
    backend::SimBackend,
    timing::{
        Halt, ProcessState, StateView, Value, copy_planes, encode_planes, occurs, planes,
        planes_equal, planes_occur, read,
    },
};

impl<B: SimBackend> SimulationState<B> {
    pub(super) fn site_error(process: usize, site: usize) -> Halt {
        Halt::Error(SimulatorErrorCode::Runtime {
            message: format!("process {process} reported unknown site {site}"),
            signals: Vec::new(),
        })
    }

    /// Process `process` waits at its wait site `site`: remember what each
    /// term compares against from now on.
    pub(super) fn register_waiter(
        &mut self,
        backend: &B,
        process: usize,
        site: usize,
    ) -> Result<(), Halt> {
        let terms = self.model.processes[process]
            .wait_sites
            .get(site)
            .ok_or_else(|| Self::site_error(process, site))?;
        let driven = terms.iter().any(|term| term.driven);
        self.process_state
            .waiters
            .register(process, driven, |waiter| {
                // Re-arming at the same site reuses the baselines' storage.
                let reuse = waiter.site == site && waiter.baselines.len() == terms.len();
                waiter.site = site;
                if !reuse {
                    waiter.baselines.clear();
                    waiter
                        .baselines
                        .extend(terms.iter().map(|_| Baseline::Value(Value::default())));
                }
                for (term, baseline) in terms.iter().zip(&mut waiter.baselines) {
                    match term.counter {
                        Some(counter) => {
                            *baseline = Baseline::Counter {
                                seen: backend.get_as(counter),
                                value: read(backend, term.signal),
                            }
                        }
                        None => match planes(backend, term.signal) {
                            Some(image) => {
                                if let Baseline::Raw(raw) = baseline {
                                    copy_planes(raw, image, term.signal.width);
                                } else {
                                    let mut raw = Vec::with_capacity(image.len());
                                    copy_planes(&mut raw, image, term.signal.width);
                                    *baseline = Baseline::Raw(raw);
                                }
                            }
                            None => *baseline = Baseline::Value(read(backend, term.signal)),
                        },
                    }
                }
            });
        Ok(())
    }

    /// Write `signal` on behalf of the runtime, counting a counted signal's
    /// occurrences and showing the value to the waits on it.
    #[inline]
    pub(super) fn write_signal(
        &mut self,
        backend: &mut B,
        signal: SignalRef,
        write: impl FnOnce(&mut B),
    ) {
        if self.model.counted.is_empty() && self.process_state.waiters.is_empty() {
            write(backend);
            return;
        }
        match self.model.counted_by_offset.get(&signal.offset).copied() {
            Some(counted) => {
                let old = read(backend, signal);
                write(backend);
                let new = read(backend, signal);
                self.model.counted[counted].refs.bump(backend, &old, &new);
            }
            None => write(backend),
        }
        if !self.process_state.waiters.is_empty() {
            self.observe(backend, Some(signal));
        }
    }

    /// Test the waits against the current state, or only their terms on
    /// signal `only` after a write of it. Woken processes become ready.
    #[inline]
    pub(super) fn observe(&mut self, backend: &B, only: Option<SignalRef>) {
        self.check_waits(backend, only, false);
    }

    /// Like [`Self::observe`], at a point where only design code wrote
    /// state: terms on signals that only the host and the scheduler write
    /// cannot have changed.
    #[inline]
    pub(super) fn observe_driven(&mut self, backend: &B) {
        if !self.process_state.waiters.none_driven() {
            self.check_waits(backend, None, true);
        }
    }

    fn check_waits(&mut self, backend: &B, only: Option<SignalRef>, driven_only: bool) {
        let processes = &self.model.processes;
        let state = StateView::new(backend);
        let ProcessState { waiters, ready, .. } = &mut self.process_state;
        waiters.retain(driven_only, |process, waiter| {
            let terms = &processes[process].wait_sites[waiter.site];
            let mut wake = false;
            for (index, term) in terms.iter().enumerate() {
                if only.is_some_and(|signal| signal != term.signal) || (driven_only && !term.driven)
                {
                    continue;
                }
                let baseline = &mut waiter.baselines[index];
                wake |= match baseline {
                    Baseline::Counter { seen, value } => {
                        let counted = term
                            .counter
                            .is_some_and(|counter| backend.get_as::<u32>(counter) != *seen);
                        let now = read(backend, term.signal);
                        let changed = occurs(term.kind, value, &now);
                        *value = now;
                        counted || changed
                    }
                    Baseline::Raw(last) => {
                        let now = state.planes(term.signal).expect("a plain signal");
                        Self::advance(term.kind, last, now, term.signal.width)
                    }
                    Baseline::Value(last) => match state.planes(term.signal) {
                        // A baseline loaded from a state file.
                        Some(now) => {
                            let mut raw = encode_planes(last, term.signal.width, now.len());
                            let hit = Self::advance(term.kind, &mut raw, now, term.signal.width);
                            *baseline = Baseline::Raw(raw);
                            hit
                        }
                        None => {
                            let now = read(backend, term.signal);
                            let hit = occurs(term.kind, last, &now);
                            *last = now;
                            hit
                        }
                    },
                };
            }
            if wake {
                ready.insert(process);
            }
            !wake
        });
    }

    /// Whether the change from the image `last` to `now` is an occurrence of
    /// `kind`; `last` becomes `now`.
    #[inline]
    fn advance(kind: CounterKind, last: &mut Vec<u8>, now: &[u8], width: usize) -> bool {
        if planes_equal(last, now, width) {
            return false;
        }
        let hit = planes_occur(kind, last, now, width);
        copy_planes(last, now, width);
        hit
    }

    /// Show the waits one value the host wrote to `signal`.
    pub(super) fn observe_host_write(&mut self, signal: SignalRef, value: &Value) {
        let processes = &self.model.processes;
        let ProcessState { waiters, ready, .. } = &mut self.process_state;
        waiters.retain(false, |process, waiter| {
            let terms = &processes[process].wait_sites[waiter.site];
            let mut wake = false;
            for (term, baseline) in terms.iter().zip(&mut waiter.baselines) {
                if term.signal != signal {
                    continue;
                }
                match baseline {
                    // Counters are bumped by the write itself.
                    Baseline::Counter { .. } => {}
                    Baseline::Raw(last) => {
                        let now = encode_planes(value, signal.width, last.len());
                        wake |= Self::advance(term.kind, last, &now, signal.width);
                    }
                    Baseline::Value(last) => {
                        wake |= occurs(term.kind, last, value);
                        *last = value.clone();
                    }
                }
            }
            if wake {
                ready.insert(process);
            }
            !wake
        });
    }
}

/// What one term of a waiting process compares against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Baseline {
    /// The counter's value when the wait began, and the signal's value at
    /// the last check. Only kernels and combinational process units count a
    /// signal's writes, so a write the counter misses, such as one of a
    /// continuous assignment to other bits, shows as a change of value.
    Counter { seen: u32, value: Value },
    /// The signal's value at the last check.
    Value(Value),
    /// The signal's state at the last check, as the bytes of its planes
    /// (see [`planes`]) with the bits above its width cleared. Checks
    /// compare it with the state in place.
    Raw(Vec<u8>),
}

/// A process suspended at a wait.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Waiter {
    pub site: usize,
    pub baselines: Vec<Baseline>,
}

/// The processes suspended at a wait, by process index. A slot keeps its
/// storage while its process runs, so suspending again reuses it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Waiters {
    slots: Vec<Waiter>,
    waiting: Vec<bool>,
    /// Whether a slot's site has a term on a driven signal.
    driven: Vec<bool>,
    count: usize,
    driven_count: usize,
}

impl Waiters {
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn len(&self) -> usize {
        self.count
    }

    /// Whether no waiting process has a term on a driven signal.
    pub fn none_driven(&self) -> bool {
        self.driven_count == 0
    }

    fn slot(&mut self, process: usize) -> &mut Waiter {
        if process >= self.slots.len() {
            self.slots.resize_with(process + 1, Waiter::default);
            self.waiting.resize(process + 1, false);
            self.driven.resize(process + 1, false);
        }
        &mut self.slots[process]
    }

    /// Process `process` waits at a site with a term on a driven signal if
    /// `driven`; `fill` sets its site and baselines in the slot's previous
    /// storage.
    pub fn register(&mut self, process: usize, driven: bool, fill: impl FnOnce(&mut Waiter)) {
        fill(self.slot(process));
        self.remove(process);
        self.waiting[process] = true;
        self.driven[process] = driven;
        self.count += 1;
        self.driven_count += usize::from(driven);
    }

    /// Process `process` waits as `waiter`.
    pub fn insert(&mut self, process: usize, driven: bool, waiter: Waiter) {
        self.register(process, driven, |slot| *slot = waiter);
    }

    /// Process `process` no longer waits.
    pub fn remove(&mut self, process: usize) {
        if self.waiting.get(process).copied().unwrap_or(false) {
            self.waiting[process] = false;
            self.count -= 1;
            self.driven_count -= usize::from(self.driven[process]);
        }
    }

    /// The waiting processes in index order.
    pub fn iter(&self) -> impl Iterator<Item = (usize, &Waiter)> {
        self.slots
            .iter()
            .enumerate()
            .filter(|(process, _)| self.waiting[*process])
    }

    /// Keep the waiting processes for which `keep` holds, in index order.
    /// Keep the waiting processes for which `keep` holds, in index order;
    /// with `driven_only`, consider only those with a term on a driven
    /// signal.
    #[inline]
    pub fn retain(&mut self, driven_only: bool, mut keep: impl FnMut(usize, &mut Waiter) -> bool) {
        for process in 0..self.slots.len() {
            if self.waiting[process]
                && (!driven_only || self.driven[process])
                && !keep(process, &mut self.slots[process])
            {
                self.remove(process);
            }
        }
    }

    /// The waiting processes in index order, for updating their baselines.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (usize, &mut Waiter)> {
        let waiting = &self.waiting;
        self.slots
            .iter_mut()
            .enumerate()
            .filter(move |(process, _)| waiting[*process])
    }
}
