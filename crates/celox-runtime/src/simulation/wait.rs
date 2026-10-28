//! Waiting processes: registering waits and testing them against writes.

use std::collections::BTreeSet;

use celox_design::CounterKind;

use super::SimulationState;
use crate::{
    SignalRef, SimulatorErrorCode, TermRefs,
    backend::SimBackend,
    timing::{
        GateCounterRefs, GateRefs, GatedWriteRefs, Halt, ProcessRefs, ProcessState, StateView,
        Value, copy_planes, encode_planes, occurs, planes, planes_equal, planes_occur, read,
        write_gated,
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
        let frame = &self.model.processes[process].frame;
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
                    if frame.as_ref().is_some_and(|frame| {
                        frame.locals.contains(&term.signal) || frame.private.contains(&term.signal)
                    }) {
                        *baseline = Baseline::Value(read(backend, term.signal));
                        continue;
                    }
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
                let kernel = self
                    .process_state
                    .activations
                    .get(process)
                    .map_or(process, |a| a.kernel);
                let gates = gates.get(kernel).map_or(&[][..], |sites| &sites[site]);
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
    ) -> Result<(), Halt> {
        if self.model.counted.is_empty()
            && self.model.probes_by_offset.is_empty()
            && self.process_state.waiters.is_empty()
        {
            write(backend);
            return Ok(());
        }
        let counted = self.model.counted_by_offset.get(&signal.offset).copied();
        let probes = self.model.probes_by_offset.get(&signal.offset);
        if counted.is_none() && probes.is_none() {
            if !self.model.gated_writes.is_empty()
                && let Some(gated) = self.model.gated_writes.get(&signal.offset)
            {
                write_gated(backend, signal, gated, write);
            } else {
                write(backend);
            }
        } else {
            let old = read(backend, signal);
            write(backend);
            let new = read(backend, signal);
            if let Some(counted) = counted {
                self.model.counted[counted].refs.bump(backend, &old, &new);
            }
            for term in self
                .model
                .gated_writes
                .get(&signal.offset)
                .into_iter()
                .flatten()
            {
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
            self.observe(backend, Some(signal))?;
        }
        Ok(())
    }

    /// Observe writes before or after combinational settling. Ungated
    /// designs monomorphize away every gate lookup.
    pub(super) fn observe(&mut self, backend: &mut B, only: Option<SignalRef>) -> Result<(), Halt> {
        self.observe_waits(backend, only, false, false)
    }
    pub(super) fn observe_settled(&mut self, backend: &mut B) -> Result<(), Halt> {
        self.observe_waits(backend, None, false, true)
    }
    pub(super) fn observe_driven(&mut self, backend: &mut B) -> Result<(), Halt> {
        if self.process_state.waiters.none_driven() {
            return Ok(());
        }
        self.observe_waits(backend, None, true, true)
    }
    pub(super) fn observe_driven_unsettled(&mut self, backend: &mut B) -> Result<(), Halt> {
        if self.process_state.waiters.none_driven() {
            return Ok(());
        }
        self.observe_waits(backend, None, true, false)
    }
    #[inline]
    fn observe_waits(
        &mut self,
        backend: &mut B,
        only: Option<SignalRef>,
        driven_only: bool,
        settled: bool,
    ) -> Result<(), Halt> {
        if self.model.wait_gates.is_empty() {
            self.check_waits::<false>(backend, only, driven_only, settled)
        } else {
            self.check_waits::<true>(backend, only, driven_only, settled)
        }
    }

    fn check_waits<const GATED: bool>(
        &mut self,
        backend: &mut B,
        only: Option<SignalRef>,
        driven_only: bool,
        settled: bool,
    ) -> Result<(), Halt> {
        let processes = &self.model.processes;
        let ProcessState {
            waiters,
            ready,
            reactive,
            activations,
            cells,
            ..
        } = &mut self.process_state;
        let all_gates = &self.model.wait_gates;
        let mut error = None;
        waiters.retain(driven_only, |process, waiter| {
            if error.is_some() {
                return true;
            }
            let terms = &processes[process].wait_sites[waiter.site];
            let mut wake = false;
            for (index, term) in terms.iter().enumerate() {
                let gate = if GATED {
                    let kernel = activations.get(process).map_or(process, |a| a.kernel);
                    all_gates
                        .get(kernel)
                        .and_then(|sites| sites.get(waiter.site))
                        .and_then(|gates| gates.get(index))
                        .copied()
                        .flatten()
                } else {
                    None
                };
                if let Some(gate) = gate {
                    if let Some(counters) = gate.counters {
                        if only.is_some_and(|signal| signal != term.signal)
                            || (driven_only && !term.driven)
                        {
                            continue;
                        }
                        let state = StateView::new(&*backend);
                        wake |= Self::gated_occurred(
                            &state,
                            backend,
                            term,
                            gate.signal,
                            counters,
                            &mut waiter.baselines[index],
                        );
                        continue;
                    }
                    if !settled && !gate.at_write {
                        continue;
                    }
                }
                let gate_holds = match gate {
                    None => true,
                    Some(gate) => {
                        let probe = processes[process].frame.as_ref().and_then(|frame| {
                            frame
                                .event_probes
                                .iter()
                                .find(|probe| probe.result == gate.signal)
                        });
                        let value = if let Some(probe) = probe {
                            match super::activation::event_probe(
                                backend,
                                &processes[process],
                                &activations[process],
                                cells,
                                probe,
                            ) {
                                Ok(value) => value,
                                Err(failure) => {
                                    error = Some(failure);
                                    return true;
                                }
                            }
                        } else {
                            super::activation::activation_value(
                                activations,
                                cells,
                                &processes[process],
                                process,
                                gate.signal,
                            )
                            .cloned()
                            .unwrap_or_else(|| read(backend, gate.signal))
                        };
                        value.0.bit(0) && !value.1.bit(0)
                    }
                };
                if let Some(probe) = processes[process].frame.as_ref().and_then(|frame| {
                    frame
                        .event_probes
                        .iter()
                        .find(|probe| probe.result == term.signal)
                }) {
                    if only.is_some_and(|signal| !probe.inputs.contains(&signal)) {
                        continue;
                    }
                    match super::activation::event_probe(
                        backend,
                        &processes[process],
                        &activations[process],
                        cells,
                        probe,
                    ) {
                        Ok(value) => {
                            let Baseline::Value(last) = &mut waiter.baselines[index] else {
                                error = Some(SimulatorErrorCode::InternalError.into());
                                return true;
                            };
                            wake |= occurs(term.kind, last, &value) & gate_holds;
                            *last = value;
                        }
                        Err(failure) => {
                            error = Some(failure);
                            return true;
                        }
                    }
                    continue;
                }
                let state = StateView::new(&*backend);
                if only.is_some_and(|signal| signal != term.signal) || (driven_only && !term.driven)
                {
                    continue;
                }
                let baseline = &mut waiter.baselines[index];
                if let Some(value) = super::activation::activation_value(
                    activations,
                    cells,
                    &processes[process],
                    process,
                    term.signal,
                ) {
                    if let Baseline::Value(last) = baseline {
                        wake |= occurs(term.kind, last, value) & gate_holds;
                        *last = value.clone();
                        continue;
                    }
                }
                let occurred = Self::occurred(&state, backend, term, baseline);
                wake |= occurred && gate_holds;
                if occurred
                    && !gate_holds
                    && let Some(counter) = term.counter
                {
                    if let Baseline::RawCounter { seen, .. } | Baseline::Counter { seen, .. } =
                        baseline
                    {
                        *seen = backend.get_as(counter);
                    }
                }
            }
            if wake {
                wake_in_region(processes, ready, reactive, process);
            }
            !wake
        });
        error.map_or(Ok(()), Err)
    }

    fn gated_occurred(
        state: &StateView<'_>,
        backend: &B,
        term: &TermRefs,
        gate: SignalRef,
        counters: GateCounterRefs,
        baseline: &mut Baseline,
    ) -> bool {
        let now = state.planes(term.signal).expect("a plain signal");
        let all = state.counter(counters.all);
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
        state.counter(counters.gated) != *gated
            || (uncounted && Self::gate_holds(state, backend, gate))
    }

    /// Whether `term` occurred since `baseline`, which becomes the current
    /// state: the body of [`Self::check_waits`] for one term.
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
        state.gate_holds(gate).unwrap_or_else(|| {
            let (value, unknown) = read(backend, gate);
            value.bit(0) && !unknown.bit(0)
        })
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

    /// The host wrote gated signals between steps: with `raw`, in place,
    /// otherwise through tracked writes, which counted their occurrences.
    /// A raw change of a gated term's signal since its baseline counts as
    /// one write, judging the gate as the writes left it. Either way the
    /// baseline (the signal's planes and the counter of all occurrences)
    /// becomes the current state, so the check that follows judges only
    /// later changes, and checkpoints and state files keep it.
    pub fn note_gated_host_writes(&mut self, backend: &mut B, raw: bool) {
        if self.model.wait_gates.is_empty() {
            return;
        }
        let processes = &self.model.processes;
        let gates = &self.model.wait_gates;
        for (process, waiter) in self.process_state.waiters.iter_mut() {
            let kernel = self
                .process_state
                .activations
                .get(process)
                .map_or(process, |a| a.kernel);
            let Some(gates) = gates.get(kernel).and_then(|sites| sites.get(waiter.site)) else {
                continue;
            };
            for (index, gate) in gates.iter().enumerate() {
                let Some(GateRefs {
                    signal: gate,
                    counters: Some(counters),
                    ..
                }) = *gate
                else {
                    continue;
                };
                let term = processes[process].wait_sites[waiter.site][index];
                let Baseline::Gated { all, raw: last, .. } = &mut waiter.baselines[index] else {
                    continue;
                };
                let Some(now) = planes(backend, term.signal).map(<[u8]>::to_vec) else {
                    continue;
                };
                if raw && planes_occur(term.kind, last, &now, term.signal.width) {
                    let refs = GatedWriteRefs {
                        signal: term.signal,
                        kind: term.kind,
                        gate,
                        counters,
                    };
                    refs.count(backend);
                }
                copy_planes(last, &now, term.signal.width);
                *all = backend.get_as(counters.all);
            }
        }
    }

    /// Show the waits one value the host wrote to `signal`.
    pub(super) fn observe_host_write(&mut self, signal: SignalRef, value: &Value) {
        let processes = &self.model.processes;
        let gates = &self.model.wait_gates;
        let ProcessState {
            waiters,
            ready,
            reactive,
            activations,
            cells,
            ..
        } = &mut self.process_state;
        waiters.retain(false, |process, waiter| {
            let gates = gates
                .get(activations.get(process).map_or(process, |a| a.kernel))
                .map_or(&[][..], |sites| &sites[waiter.site]);
            let mut wake = false;
            for (index, (term, baseline)) in processes[process].wait_sites[waiter.site]
                .iter()
                .zip(&mut waiter.baselines)
                .enumerate()
            {
                if term.signal != signal {
                    continue;
                }
                let gate_holds = match gates.get(index).copied().flatten() {
                    None => true,
                    Some(gate) if gate.at_write && gate.counters.is_none() => {
                        // No module counter can capture an activation-local
                        // gate. Replay each host value with that activation's
                        // unchanged gate before HDL execution resumes.
                        let Some(value) = super::activation::activation_value(
                            activations,
                            cells,
                            &processes[process],
                            process,
                            gate.signal,
                        ) else {
                            continue;
                        };
                        value.0.bit(0) && !value.1.bit(0)
                    }
                    Some(_) => continue,
                };
                match baseline {
                    // Counters are bumped by the write itself.
                    Baseline::Counter { .. }
                    | Baseline::RawCounter { .. }
                    | Baseline::Gated { .. } => {}
                    Baseline::Raw(last) => {
                        let now = encode_planes(value, signal.width, last.len());
                        wake |= Self::advance(term.kind, last, &now, signal.width) & gate_holds;
                    }
                    Baseline::Value(last) => {
                        wake |= occurs(term.kind, last, value) & gate_holds;
                        *last = value.clone();
                    }
                }
            }
            if wake {
                wake_in_region(processes, ready, reactive, process);
            }
            !wake
        });
    }
}

/// Make the woken `process` ready: in the Reactive region if it is a program
/// process (IEEE 1800-2023 24.3.1), otherwise in the Active region.
#[inline]
pub(super) fn wake_in_region(
    processes: &[ProcessRefs],
    ready: &mut BTreeSet<usize>,
    reactive: &mut BTreeSet<usize>,
    process: usize,
) {
    if processes[process].program.is_some() {
        reactive.insert(process);
    } else {
        ready.insert(process);
    }
}

/// What one term of a waiting process compares against.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Waiter {
    pub site: usize,
    pub baselines: Vec<Baseline>,
}

/// The processes suspended at a wait, by process index. A slot keeps its
/// storage while its process runs, so suspending again reuses it.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
