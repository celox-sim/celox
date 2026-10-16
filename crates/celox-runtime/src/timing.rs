//! Process waits, counted signals and nonblocking assignments of process
//! kernels.
//!
//! Most watched signals change at most once between two points at which the
//! runtime checks them, so a wait compares values: it remembers the value
//! of each of its signals when it began and tests every later value against
//! the edge it waits for (IEEE 1800-2023 9.4.2, Table 9-2). A *counted*
//! signal may change several times without a check in between; the code
//! that writes it counts each occurrence, and a wait on it compares
//! counters instead.

use celox_design::CounterKind;
use fxhash::{FxHashMap, FxHashSet};
use num_bigint::BigUint;

use crate::{SignalRef, SimulatorErrorCode, backend::SimBackend};

/// Why a simulation stopped executing before completing the work it was
/// given: the design finished the simulation, or an error occurred.
#[derive(Debug, Clone, PartialEq)]
pub enum Halt {
    /// `$finish`: the simulation ends where it is (IEEE 1800-2023 20.2).
    Finish,
    Error(SimulatorErrorCode),
}

impl From<SimulatorErrorCode> for Halt {
    fn from(error: SimulatorErrorCode) -> Self {
        Self::Error(error)
    }
}

/// A four-state value: payload and unknown-bit mask.
pub type Value = (BigUint, BigUint);

pub(crate) fn read<B: SimBackend>(backend: &B, signal: SignalRef) -> Value {
    if signal.is_4state {
        backend.get_four_state(signal)
    } else {
        (backend.get(signal), BigUint::default())
    }
}

/// Whether the change from `old` to `new` is an occurrence of `kind`.
pub fn occurs(kind: CounterKind, old: &Value, new: &Value) -> bool {
    if kind == CounterKind::Changes {
        return old != new;
    }
    let bit = |value: &Value| (value.0.bit(0), value.1.bit(0));
    edge(kind, bit(old), bit(new))
}

/// Whether a least significant bit changing from `old` to `new`, each a
/// value and an unknown flag, is an edge of `kind`.
#[inline]
fn edge(
    kind: CounterKind,
    (old_value, old_unknown): (bool, bool),
    (new_value, new_unknown): (bool, bool),
) -> bool {
    if (old_value, old_unknown) == (new_value, new_unknown) {
        return false;
    }
    let (from, to) = if kind == CounterKind::Rises {
        (false, true)
    } else {
        (true, false)
    };
    // Table 9-2: a positive edge leaves 0 or reaches 1, a negative edge
    // leaves 1 or reaches 0.
    (!old_unknown && old_value == from) || (!new_unknown && new_value == to)
}

/// Occurrence counters of a counted signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CountedRefs {
    pub signal: SignalRef,
    pub changes: SignalRef,
    pub rises: SignalRef,
    pub falls: SignalRef,
}

impl CountedRefs {
    pub fn counter(&self, kind: CounterKind) -> SignalRef {
        match kind {
            CounterKind::Changes => self.changes,
            CounterKind::Rises => self.rises,
            CounterKind::Falls => self.falls,
        }
    }

    /// Count the occurrences of a change from `old` to `new`.
    pub fn bump<B: SimBackend>(&self, backend: &mut B, old: &Value, new: &Value) {
        for kind in CounterKind::ALL {
            if occurs(kind, old, new) {
                let counter = self.counter(kind);
                let value: u32 = backend.get_as(counter);
                backend.set(counter, value.wrapping_add(1));
            }
        }
    }
}

/// Host writes the runtime must observe one at a time: writes of counted
/// signals bump their counters, and writes of signals a wait watches are
/// logged for the waits to examine. Empty unless a timed simulation with
/// such signals is attached, so ordinary writes pay one emptiness check.
#[derive(Clone, Debug, Default)]
pub struct HostWatch {
    counted: FxHashMap<usize, CountedRefs>,
    waited: FxHashSet<usize>,
    log: Vec<(SignalRef, Value)>,
}

impl HostWatch {
    pub fn new(counted: impl IntoIterator<Item = CountedRefs>, waited: FxHashSet<usize>) -> Self {
        Self {
            counted: counted
                .into_iter()
                .map(|refs| (refs.signal.offset, refs))
                .collect(),
            waited,
            log: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.counted.is_empty() && self.waited.is_empty()
    }

    /// Run `write`, which writes `signal` in `backend`, and observe it.
    pub fn write<B: SimBackend>(
        &mut self,
        backend: &mut B,
        signal: SignalRef,
        write: impl FnOnce(&mut B),
    ) {
        let counted = self.counted.get(&signal.offset).copied();
        let waited = self.waited.contains(&signal.offset);
        if counted.is_none() && !waited {
            write(backend);
            return;
        }
        let old = read(backend, signal);
        write(backend);
        let new = read(backend, signal);
        if let Some(counted) = counted {
            counted.bump(backend, &old, &new);
        }
        if waited && old != new {
            self.log.push((signal, new));
        }
    }

    /// Take the logged values of waited signals, in write order.
    pub fn take_log(&mut self) -> Vec<(SignalRef, Value)> {
        std::mem::take(&mut self.log)
    }

    pub fn has_log(&self) -> bool {
        !self.log.is_empty()
    }
}

/// One term of a wait site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TermRefs {
    pub signal: SignalRef,
    pub kind: CounterKind,
    /// The counter of `kind`, if the signal is counted.
    pub counter: Option<SignalRef>,
    /// Design code may write the signal. Otherwise only the host and the
    /// scheduler do, and the runtime checks the term only where they write.
    pub driven: bool,
}

/// A nonblocking assignment site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NbaSiteRefs {
    pub target: SignalRef,
    pub lsb: usize,
    pub width: usize,
    pub value: SignalRef,
    pub offset: Option<SignalRef>,
    pub pending: Option<SignalRef>,
}

/// Control slots and sites of one process kernel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessRefs {
    pub status: SignalRef,
    pub delay: SignalRef,
    /// Wait sites by local index: their terms.
    pub wait_sites: Vec<Vec<TermRefs>>,
    /// Nonblocking sites by local index.
    pub nba_sites: Vec<NbaSiteRefs>,
}

impl ProcessRefs {
    /// A process without wait or nonblocking sites.
    pub fn new(status: SignalRef, delay: SignalRef) -> Self {
        Self {
            status,
            delay,
            wait_sites: Vec::new(),
            nba_sites: Vec::new(),
        }
    }
}

/// A counted signal and the canonical event domains its counters trigger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CountedDomainRefs {
    pub refs: CountedRefs,
    /// Event ids and the counter that triggers each.
    pub domains: Vec<(usize, CounterKind)>,
}

/// What one term of a waiting process compares against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Baseline {
    /// The counter's value when the wait began.
    Counter(u32),
    /// The signal's value at the last check.
    Value(Value),
    /// The signal's state at the last check, as the bytes of its planes
    /// (see [`planes`]) with the bits above its width cleared. Checks
    /// compare it with the state in place.
    Raw(Vec<u8>),
}

/// The state of a signal without unpacked dimensions: the bytes of its
/// value plane followed, for a four-state signal in a four-state layout, by
/// those of its mask plane. `None` for an unpacked array.
pub fn planes<B: SimBackend>(backend: &B, signal: SignalRef) -> Option<&[u8]> {
    StateView::new(backend).planes(signal)
}

/// The state image of a backend, read without going through the backend
/// for every signal.
pub struct StateView<'a> {
    memory: &'a [u8],
    four_state: bool,
}

impl<'a> StateView<'a> {
    pub fn new<B: SimBackend>(backend: &'a B) -> Self {
        let (ptr, size) = backend.memory_as_ptr();
        Self {
            // Safety: the backend exposes `size` readable bytes at `ptr` for
            // as long as it is borrowed.
            memory: unsafe { std::slice::from_raw_parts(ptr, size) },
            four_state: backend.layout().four_state,
        }
    }

    /// See [`planes`].
    pub fn planes(&self, signal: SignalRef) -> Option<&'a [u8]> {
        if signal.array_layout.is_some() {
            return None;
        }
        let bytes = signal.width.div_ceil(8);
        let len = if self.four_state && signal.is_4state {
            2 * bytes
        } else {
            bytes
        };
        Some(&self.memory[signal.offset..signal.offset + len])
    }
}

#[inline]
fn top_mask(width: usize) -> u8 {
    0xff >> ((8 - width % 8) % 8)
}

/// Whether two plane images of a `width`-bit signal hold the same state.
#[inline]
pub fn planes_equal(left: &[u8], right: &[u8], width: usize) -> bool {
    let bytes = width.div_ceil(8).max(1);
    let top = top_mask(width);
    if left.len() != right.len() {
        return false;
    }
    // Images are a few bytes: compare them in place.
    let mut position = 0;
    for (left, right) in left.iter().zip(right) {
        let mask = if position == bytes - 1 { top } else { 0xff };
        if (left ^ right) & mask != 0 {
            return false;
        }
        position = if position == bytes - 1 {
            0
        } else {
            position + 1
        };
    }
    true
}

/// Copy a plane image, clearing the bits above `width`.
#[inline]
pub fn copy_planes(target: &mut Vec<u8>, source: &[u8], width: usize) {
    let bytes = width.div_ceil(8).max(1);
    let top = top_mask(width);
    if target.len() != source.len() {
        target.clear();
        target.resize(source.len(), 0);
    }
    let mut position = 0;
    for (target, source) in target.iter_mut().zip(source) {
        *target = if position == bytes - 1 {
            source & top
        } else {
            *source
        };
        position = if position == bytes - 1 {
            0
        } else {
            position + 1
        };
    }
}

/// The plane image of `value` for a `width`-bit signal whose image is
/// `len` bytes long.
pub fn encode_planes(value: &Value, width: usize, len: usize) -> Vec<u8> {
    let bytes = width.div_ceil(8).max(1);
    let plane = |number: &BigUint| {
        let mut data = number.to_bytes_le();
        data.resize(bytes, 0);
        data
    };
    let mut image = plane(&value.0);
    if len > bytes {
        image.extend(plane(&value.1));
    }
    let mut masked = Vec::with_capacity(len);
    copy_planes(&mut masked, &image, width);
    masked
}

/// Whether the change from plane image `old` to `new` of a `width`-bit
/// signal is an occurrence of `kind` (Table 9-2 on the least significant
/// bit).
#[inline]
pub fn planes_occur(kind: CounterKind, old: &[u8], new: &[u8], width: usize) -> bool {
    if kind == CounterKind::Changes {
        return !planes_equal(old, new, width);
    }
    let bytes = width.div_ceil(8).max(1);
    let lsb = |image: &[u8]| {
        (
            image[0] & 1 != 0,
            image.get(bytes).is_some_and(|mask| mask & 1 != 0),
        )
    };
    edge(kind, lsb(old), lsb(new))
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

/// A nonblocking assignment waiting for the NBA region.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NbaUpdate {
    pub target: SignalRef,
    /// Bit position of the assigned range in the target.
    pub lsb: usize,
    pub width: usize,
    pub value: Value,
}

impl NbaUpdate {
    /// Write the update into the target's value.
    pub fn merge_into(&self, target: &Value) -> Value {
        let width = self.target.width;
        let range = ((BigUint::from(1u8) << self.width) - 1u8) << self.lsb;
        let all = (BigUint::from(1u8) << width) - 1u8;
        let keep = &all ^ (&range & &all);
        let place =
            |old: &BigUint, new: &BigUint| ((old & &keep) | ((new << self.lsb) & &range)) & &all;
        (
            place(&target.0, &self.value.0),
            place(&target.1, &self.value.1),
        )
    }
}

/// Mutable process scheduling state beyond the delay queue: who waits for
/// what, the inactive and post-NBA queues, and the queued nonblocking
/// assignments. All of it is empty in a design without process kernels.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcessState {
    /// Processes suspended at a wait.
    pub waiters: Waiters,
    /// Processes ready to run in the active region of the current time.
    pub ready: std::collections::BTreeSet<usize>,
    /// Processes that resume in the inactive region (`#0`).
    pub inactive: std::collections::BTreeSet<usize>,
    /// Queued nonblocking assignments, in issue order.
    pub nba_queue: Vec<NbaUpdate>,
    /// Counter values each counted domain last triggered on, by counted
    /// signal index and kind.
    pub domain_counters: Vec<[u32; 3]>,
}

pub(crate) fn kind_index(kind: CounterKind) -> usize {
    match kind {
        CounterKind::Changes => 0,
        CounterKind::Rises => 1,
        CounterKind::Falls => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plane_images_ignore_bits_above_the_width() {
        // A 4-bit four-state signal: value 0b0101 and mask 0b0010, with
        // garbage above bit 3 in both planes.
        let memory = [0b1111_0101, 0b1010_0010];
        let mut image = Vec::new();
        copy_planes(&mut image, &memory, 4);
        assert_eq!(image, [0b0101, 0b0010]);
        assert!(planes_equal(&image, &memory, 4));
        assert_eq!(
            encode_planes(&(BigUint::from(5u8), BigUint::from(2u8)), 4, 2),
            image
        );
        assert!(!planes_equal(&image, &[0b0101, 0b0011], 4));
        // 0 with an unknown bit 1 -> 1: no edge on the least significant bit,
        // but a change.
        assert!(!planes_occur(CounterKind::Rises, &image, &[0b0101, 0], 4));
        assert!(planes_occur(CounterKind::Changes, &image, &[0b0101, 0], 4));
        assert!(planes_occur(
            CounterKind::Falls,
            &image,
            &[0b0100, 0b0010],
            4
        ));
    }

    fn v(value: u8, mask: u8) -> Value {
        (BigUint::from(value), BigUint::from(mask))
    }

    #[test]
    fn edges_follow_table_9_2() {
        // (old, new, posedge, negedge); 2 = x, 3 = z share the mask bit.
        let x = v(0, 1);
        let z = v(1, 1);
        let zero = v(0, 0);
        let one = v(1, 0);
        for (old, new, rise, fall) in [
            (&zero, &one, true, false),
            (&zero, &x, true, false),
            (&zero, &z, true, false),
            (&x, &one, true, false),
            (&z, &one, true, false),
            (&one, &zero, false, true),
            (&one, &x, false, true),
            (&one, &z, false, true),
            (&x, &zero, false, true),
            (&z, &zero, false, true),
            (&x, &z, false, false),
            (&one, &one, false, false),
        ] {
            assert_eq!(
                occurs(CounterKind::Rises, old, new),
                rise,
                "{old:?} -> {new:?}"
            );
            assert_eq!(
                occurs(CounterKind::Falls, old, new),
                fall,
                "{old:?} -> {new:?}"
            );
        }
        assert!(occurs(CounterKind::Changes, &x, &z));
        assert!(!occurs(CounterKind::Changes, &x, &x));
    }

    #[test]
    fn nba_updates_merge_their_range() {
        let target = SignalRef {
            offset: 0,
            width: 8,
            is_4state: true,
            array_layout: None,
        };
        let update = NbaUpdate {
            target,
            lsb: 2,
            width: 3,
            value: v(0b101, 0b010),
        };
        assert_eq!(
            update.merge_into(&v(0xff, 0x00)),
            v(0b1111_0111, 0b0000_1000)
        );
    }
}
