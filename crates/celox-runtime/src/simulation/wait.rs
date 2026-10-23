//! Waiting processes: registering waits and testing them against writes.

use celox_design::CounterKind;

use super::SimulationState;
use crate::{
    SignalRef, SimulatorErrorCode, TermRefs,
    backend::SimBackend,
    timing::{
        GateCounterRefs, GateRefs, Halt, ProcessState, StateView, Value, copy_planes,
        encode_planes, occurs, planes, planes_equal, planes_occur, read,
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
        let gates = &self.model.wait_gates;
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
                            let seen = backend.get_as(counter);
                            match planes(backend, term.signal) {
                                Some(image) => {
                                    if let Baseline::RawCounter { seen: last, raw } = baseline {
                                        *last = seen;
                                        copy_planes(raw, image, term.signal.width);
                                    } else {
                                        let mut raw = Vec::with_capacity(image.len());
                                        copy_planes(&mut raw, image, term.signal.width);
                                        *baseline = Baseline::RawCounter { seen, raw };
                                    }
                                }
                                None => {
                                    *baseline = Baseline::Counter {
                                        seen,
                                        value: read(backend, term.signal),
                                    }
                                }
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
                let gates = gates.get(process).map_or(&[][..], |sites| &sites[site]);
                for (index, gate) in gates.iter().enumerate() {
                    if let Some(GateRefs {
                        counters: Some(counters),
                        ..
                    }) = gate
                    {
                        let signal = terms[index].signal;
                        let image = planes(backend, signal).expect("a plain signal");
                        let mut raw = Vec::with_capacity(image.len());
                        copy_planes(&mut raw, image, signal.width);
                        waiter.baselines[index] = Baseline::Gated {
                            gated: backend.get_as(counters.gated),
                            all: backend.get_as(counters.all),
                            raw,
                        };
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
        if self.model.counted.is_empty()
            && self.model.probes_by_offset.is_empty()
            && self.process_state.waiters.is_empty()
        {
            write(backend);
            return;
        }
        let counted = self.model.counted_by_offset.get(&signal.offset).copied();
        let probes = self.model.probes_by_offset.get(&signal.offset);
        let gated = (!self.model.gated_writes.is_empty())
            .then(|| self.model.gated_writes.get(&signal.offset))
            .flatten();
        if counted.is_none() && probes.is_none() && gated.is_none() {
            write(backend);
        } else {
            let old = read(backend, signal);
            write(backend);
            let new = read(backend, signal);
            if let Some(counted) = counted {
                self.model.counted[counted].refs.bump(backend, &old, &new);
            }
            for term in gated.into_iter().flatten() {
                term.bump(backend, &old, &new);
            }
            // One counter can observe several ranges of the signal; a write
            // is one occurrence.
            let mut bumped: Vec<SignalRef> = Vec::new();
            for probe in probes.into_iter().flatten() {
                if !bumped.contains(&probe.counter) && probe.changed(&old, &new) {
                    probe.bump(backend);
                    bumped.push(probe.counter);
                }
            }
        }
        if !self.process_state.waiters.is_empty() {
            self.observe(backend, Some(signal));
        }
    }

    /// Test the waits against the current state, or only their terms on
    /// signal `only` after a write of it, before combinational logic has
    /// settled. Woken processes become ready.
    #[inline]
    pub(super) fn observe(&mut self, backend: &B, only: Option<SignalRef>) {
        self.check_waits(backend, only, false, false);
    }

    /// Like [`Self::observe`] of every term, after combinational logic has
    /// settled.
    #[inline]
    pub(super) fn observe_settled(&mut self, backend: &B) {
        self.check_waits(backend, None, false, true);
    }

    /// Like [`Self::observe_settled`], at a point where only design code
    /// wrote state: terms on signals that only the host and the scheduler
    /// write cannot have changed.
    #[inline]
    pub(super) fn observe_driven(&mut self, backend: &B) {
        if !self.process_state.waiters.none_driven() {
            self.check_waits(backend, None, true, true);
        }
    }

    /// Like [`Self::observe_driven`], before combinational logic has
    /// settled.
    #[inline]
    pub(super) fn observe_driven_unsettled(&mut self, backend: &B) {
        if !self.process_state.waiters.none_driven() {
            self.check_waits(backend, None, true, false);
        }
    }

    /// With `settled`, combinational logic has settled since the last
    /// write. A term with an `iff` gate is judged only then: the gate may
    /// be computed from what was written, so before then its baseline is
    /// left as it is and the occurrence is judged, with the gate, at the
    /// next settled check.
    fn check_waits(
        &mut self,
        backend: &B,
        only: Option<SignalRef>,
        driven_only: bool,
        settled: bool,
    ) {
        let processes = &self.model.processes;
        let state = StateView::new(backend);
        let ProcessState { waiters, ready, .. } = &mut self.process_state;
        if self.model.wait_gates.is_empty() {
            waiters.retain(driven_only, |process, waiter| {
                let terms = &processes[process].wait_sites[waiter.site];
                let mut wake = false;
                for (index, term) in terms.iter().enumerate() {
                    if only.is_some_and(|signal| signal != term.signal)
                        || (driven_only && !term.driven)
                    {
                        continue;
                    }
                    wake |= Self::occurred(&state, backend, term, &mut waiter.baselines[index]);
                }
                if wake {
                    ready.insert(process);
                }
                !wake
            });
            return;
        }
        let gates = &self.model.wait_gates;
        waiters.retain(driven_only, |process, waiter| {
            let terms = &processes[process].wait_sites[waiter.site];
            let gates = &gates[process][waiter.site];
            let mut wake = false;
            for (index, term) in terms.iter().enumerate() {
                if only.is_some_and(|signal| signal != term.signal) || (driven_only && !term.driven)
                {
                    continue;
                }
                let baseline = &mut waiter.baselines[index];
                match gates.get(index).copied().flatten() {
                    None => wake |= Self::occurred(&state, backend, term, baseline),
                    Some(GateRefs {
                        signal: gate,
                        counters: Some(counters),
                    }) => {
                        wake |=
                            Self::gated_occurred(&state, backend, term, gate, counters, baseline);
                    }
                    Some(_) if !settled => {}
                    // The occurrence is consumed even when the gate does not
                    // hold (IEEE 1800-2023 9.4.2.3).
                    Some(gate) => {
                        wake |= Self::occurred(&state, backend, term, baseline)
                            && Self::gate_holds(&state, backend, gate.signal);
                    }
                }
            }
            if wake {
                ready.insert(process);
            }
            !wake
        });
    }

    /// Whether a gated `term` with `counters` occurred since the wait began:
    /// a write counted it while `gate` held, or the signal changed without
    /// any write counting it, as a raw host write does, while `gate` holds.
    fn gated_occurred(
        state: &StateView<'_>,
        backend: &B,
        term: &TermRefs,
        gate: SignalRef,
        counters: GateCounterRefs,
        baseline: &mut Baseline,
    ) -> bool {
        let now = state.planes(term.signal).expect("a plain signal");
        let all: u32 = backend.get_as(counters.all);
        if !matches!(baseline, Baseline::Gated { .. }) {
            // A baseline loaded from a state file.
            let (gated, value) = match baseline {
                Baseline::Counter { seen, value } => (*seen, value.clone()),
                Baseline::Value(value) => (backend.get_as(counters.gated), value.clone()),
                _ => (backend.get_as(counters.gated), read(backend, term.signal)),
            };
            *baseline = Baseline::Gated {
                gated,
                all,
                raw: encode_planes(&value, term.signal.width, now.len()),
            };
        }
        let Baseline::Gated {
            gated,
            all: counted,
            raw,
        } = baseline
        else {
            unreachable!("converted above");
        };
        let changed = Self::advance(term.kind, raw, now, term.signal.width);
        let uncounted = changed && all == *counted;
        *counted = all;
        backend.get_as::<u32>(counters.gated) != *gated
            || (uncounted && Self::gate_holds(state, backend, gate))
    }

    /// Whether `term` occurred since `baseline`, which becomes the current
    /// state.
    #[inline(always)]
    fn occurred(
        state: &StateView<'_>,
        backend: &B,
        term: &TermRefs,
        baseline: &mut Baseline,
    ) -> bool {
        // The common term: a plain signal no counter counts.
        if let Baseline::Raw(last) = baseline {
            let now = state.planes(term.signal).expect("a plain signal");
            return Self::advance(term.kind, last, now, term.signal.width);
        }
        let counted = |seen: u32| {
            term.counter
                .is_some_and(|counter| backend.get_as::<u32>(counter) != seen)
        };
        match baseline {
            Baseline::RawCounter { seen, raw } => {
                let now = state.planes(term.signal).expect("a plain signal");
                let changed = Self::advance(term.kind, raw, now, term.signal.width);
                counted(*seen) || changed
            }
            Baseline::Counter { seen, value } => match state.planes(term.signal) {
                // A baseline loaded from a state file.
                Some(now) => {
                    let seen = *seen;
                    let mut raw = encode_planes(value, term.signal.width, now.len());
                    let changed = Self::advance(term.kind, &mut raw, now, term.signal.width);
                    *baseline = Baseline::RawCounter { seen, raw };
                    counted(seen) || changed
                }
                None => {
                    let now = read(backend, term.signal);
                    let changed = occurs(term.kind, value, &now);
                    *value = now;
                    counted(*seen) || changed
                }
            },
            Baseline::Raw(_) => unreachable!("tested above"),
            // Only gated terms with counters have it.
            Baseline::Gated { .. } => false,
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
        }
    }

    /// Whether the least significant bit of `gate` is a known 1.
    fn gate_holds(state: &StateView<'_>, backend: &B, gate: SignalRef) -> bool {
        match state.planes(gate) {
            Some(image) => {
                let bytes = gate.width.div_ceil(8).max(1);
                image[0] & 1 == 1 && image.get(bytes).is_none_or(|unknown| unknown & 1 == 0)
            }
            None => {
                let (value, unknown) = read(backend, gate);
                value.bit(0) && !unknown.bit(0)
            }
        }
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
        let gates = &self.model.wait_gates;
        let ProcessState { waiters, ready, .. } = &mut self.process_state;
        waiters.retain(false, |process, waiter| {
            let gates = gates
                .get(process)
                .map_or(&[][..], |sites| &sites[waiter.site]);
            let mut wake = false;
            for (index, (term, baseline)) in processes[process].wait_sites[waiter.site]
                .iter()
                .zip(&mut waiter.baselines)
                .enumerate()
            {
                // A gated term is judged at the next settled check.
                if term.signal != signal || gates.get(index).is_some_and(Option::is_some) {
                    continue;
                }
                match baseline {
                    // Counters are bumped by the write itself.
                    Baseline::Counter { .. }
                    | Baseline::RawCounter { .. }
                    | Baseline::Gated { .. } => {}
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
    /// Like `Counter`, with the signal's state at the last check kept as
    /// in [`Baseline::Raw`].
    RawCounter { seen: u32, raw: Vec<u8> },
    /// A term whose `iff` gate a write judges at the write
    /// ([`celox_design::GateCounters`]): the gated counter when the wait
    /// began, the counter of all occurrences and the signal's state as in
    /// [`Baseline::Raw`] at the last check.
    Gated { gated: u32, all: u32, raw: Vec<u8> },
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

    /// The wait of process `process`, if it waits.
    pub fn get(&self, process: usize) -> Option<&Waiter> {
        self.waiting
            .get(process)
            .copied()
            .unwrap_or(false)
            .then(|| &self.slots[process])
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
