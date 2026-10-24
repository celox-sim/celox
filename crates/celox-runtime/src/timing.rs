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

use celox_design::{CounterKind, NbaPosition};
use num_bigint::BigUint;

pub use crate::simulation::{Baseline, HostWatch, NbaUpdate, Waiter, Waiters};
use crate::{SignalRef, SimulatorErrorCode, backend::SimBackend};

/// Why a simulation stopped executing before completing the work it was
/// given: the design finished the simulation, or an error occurred.
#[derive(Debug, Clone, PartialEq)]
pub enum Halt {
    /// `$finish`: the simulation ends where it is (IEEE 1800-2023 20.2).
    Finish,
    /// `$stop`: the simulation is suspended where it is (IEEE 1800-2023
    /// 20.2), and continues there with the next step.
    Stop,
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

/// A bit range whose changes bump an occurrence counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbeRefs {
    pub lsb: usize,
    pub msb: usize,
    pub counter: SignalRef,
}

impl ProbeRefs {
    /// Whether the probed range differs between `old` and `new`.
    pub fn changed(&self, old: &Value, new: &Value) -> bool {
        let width = self.msb - self.lsb + 1;
        let mask = (BigUint::from(1u8) << width) - 1u8;
        let bits = |plane: &BigUint| (plane >> self.lsb) & &mask;
        bits(&old.0) != bits(&new.0) || bits(&old.1) != bits(&new.1)
    }

    pub fn bump<B: SimBackend>(&self, backend: &mut B) {
        let value: u32 = backend.get_as(self.counter);
        backend.set(self.counter, value.wrapping_add(1));
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
    pub position: Option<NbaPosition<SignalRef>>,
    pub pending: Option<SignalRef>,
}

/// Control slots and sites of one process kernel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessRefs {
    pub is_initializer: bool,
    /// The status slot naming the program instance of a program kernel
    /// ([`celox_design::ProcessSlots::program`]), which runs in the reactive
    /// region set.
    pub program: Option<SignalRef>,
    pub resume: SignalRef,
    /// A converted edge procedure starts by registering its event control
    /// before scheduled time-zero inputs are applied.
    pub starts_with_event_wait: bool,
    pub status: SignalRef,
    pub delay: SignalRef,
    /// Wait sites by local index: their terms.
    pub wait_sites: Vec<Vec<TermRefs>>,
    /// Nonblocking sites by local index.
    pub nba_sites: Vec<NbaSiteRefs>,
    pub host_requests: Vec<celox_design::HostRequestSite<SignalRef>>,
}

impl ProcessRefs {
    /// A process without wait or nonblocking sites.
    pub fn new(status: SignalRef, delay: SignalRef) -> Self {
        Self {
            is_initializer: false,
            program: None,
            resume: status,
            starts_with_event_wait: false,
            status,
            delay,
            wait_sites: Vec::new(),
            nba_sites: Vec::new(),
            host_requests: Vec::new(),
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
    /// Program processes ready to run in the Reactive region of the current
    /// time (IEEE 1800-2023 4.4.2.6).
    pub reactive: std::collections::BTreeSet<usize>,
    /// Program processes that resume in the Re-Inactive region (`#0`,
    /// 4.4.2.7).
    pub re_inactive: std::collections::BTreeSet<usize>,
    /// Nonblocking assignments of program processes for the Re-NBA region
    /// (4.4.2.8), in issue order.
    pub re_nba_queue: Vec<NbaUpdate>,
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
