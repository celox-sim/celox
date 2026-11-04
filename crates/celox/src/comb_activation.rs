//! Activation of combinational observers: a process is observed again when
//! the values of its sensitivity, or the occurrence counters of those
//! values, differ from their last snapshot.
//!
//! The reads are resolved once. A range of a signal of at most 64 bits is
//! read as a word; only wider signals go through `BigUint`.

use num_bigint::BigUint;

use crate::backend::{SimBackend, get_byte_size};
use celox_design::RuntimeCombObserver;
use celox_runtime::{AbsoluteAddr, SignalRef};

#[derive(Clone, Debug)]
enum Read {
    /// Bits `lsb..lsb + width` of a signal of at most 64 bits, with its
    /// mask plane when the signal is held in four-state form.
    Word {
        value: SignalRef,
        mask: Option<SignalRef>,
        lsb: usize,
        bits: u64,
    },
    Wide {
        signal: SignalRef,
        lsb: usize,
        msb: usize,
    },
    Counter(SignalRef),
}

/// The resolved reads of every observer, in schema order.
#[derive(Clone, Debug, Default)]
pub(crate) struct ObserverReads {
    observers: Vec<Vec<Read>>,
    /// Each observer's activation group, numbered densely from 0.
    groups: Vec<usize>,
    group_count: usize,
}

/// The values one observer's reads returned.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ObserverSnapshot {
    words: Vec<u64>,
    wide: Vec<(BigUint, BigUint)>,
}

impl ObserverReads {
    /// Transfer snapshots only at generated-scheduler entry/exit. Inside a
    /// run, generated loads compare and replace these fixed-size words.
    pub(crate) fn pack(&self, snapshots: &[ObserverSnapshot], packed: &mut Vec<u64>) {
        packed.clear();
        for (index, reads) in self.observers.iter().enumerate() {
            let empty = ObserverSnapshot::default();
            let snapshot = snapshots.get(index).unwrap_or(&empty);
            let (mut word, mut wide) = (0, 0);
            for read in reads {
                match read {
                    Read::Word { mask, .. } => {
                        for _ in 0..1 + usize::from(mask.is_some()) {
                            packed.push(snapshot.words.get(word).copied().unwrap_or(0));
                            word += 1;
                        }
                    }
                    Read::Counter(_) => {
                        packed.push(snapshot.words.get(word).copied().unwrap_or(0));
                        word += 1;
                    }
                    Read::Wide { lsb, msb, .. } => {
                        let words = msb.checked_sub(*lsb).map_or(0, |n| (n + 1).div_ceil(64));
                        let zero = (BigUint::default(), BigUint::default());
                        let (value, mask) = snapshot.wide.get(wide).unwrap_or(&zero);
                        for plane in [value, mask] {
                            let at = packed.len();
                            packed.extend(plane.iter_u64_digits().take(words));
                            packed.resize(at + words, 0);
                        }
                        wide += 1;
                    }
                }
            }
        }
    }

    pub(crate) fn unpack(&self, packed: &[u64], snapshots: &mut Vec<ObserverSnapshot>) {
        snapshots.resize_with(self.observers.len(), Default::default);
        let mut at = 0;
        for (reads, snapshot) in self.observers.iter().zip(snapshots) {
            snapshot.words.clear();
            snapshot.wide.clear();
            for read in reads {
                match read {
                    Read::Word { mask, .. } => {
                        let n = 1 + usize::from(mask.is_some());
                        snapshot.words.extend_from_slice(&packed[at..at + n]);
                        at += n;
                    }
                    Read::Counter(_) => {
                        snapshot.words.push(packed[at]);
                        at += 1;
                    }
                    Read::Wide { lsb, msb, .. } => {
                        let words = msb.checked_sub(*lsb).map_or(0, |n| (n + 1).div_ceil(64));
                        let mut plane = || {
                            let bytes: Vec<_> = packed[at..at + words]
                                .iter()
                                .flat_map(|word| word.to_le_bytes())
                                .collect();
                            at += words;
                            BigUint::from_bytes_le(&bytes)
                        };
                        snapshot.wide.push((plane(), plane()));
                    }
                }
            }
        }
        debug_assert_eq!(at, packed.len());
    }

    pub(crate) fn new<B: SimBackend>(
        backend: &B,
        observers: &[RuntimeCombObserver<AbsoluteAddr>],
    ) -> Self {
        let four_state = backend.layout().four_state;
        let reads = observers
            .iter()
            .map(|observer| {
                observer
                    .sensitivity
                    .iter()
                    .map(|atom| {
                        let signal = backend.resolve_signal(&atom.id);
                        let (lsb, msb) = (atom.access.lsb, atom.access.msb);
                        if signal.width > 64 || signal.array_layout.is_some() || msb < lsb {
                            return Read::Wide { signal, lsb, msb };
                        }
                        let mask = (four_state && signal.is_4state).then(|| SignalRef {
                            offset: signal.offset + get_byte_size(signal.width),
                            width: signal.width,
                            is_4state: false,
                            array_layout: None,
                        });
                        Read::Word {
                            value: signal,
                            mask,
                            lsb,
                            bits: u64::MAX >> (63 - (msb - lsb)),
                        }
                    })
                    .chain(
                        observer
                            .occurrence_counters
                            .iter()
                            .map(|counter| Read::Counter(backend.resolve_signal(counter))),
                    )
                    .collect()
            })
            .collect();
        let mut numbers = crate::HashMap::default();
        let groups = observers
            .iter()
            .map(|observer| {
                let next = numbers.len();
                *numbers.entry(observer.activation_group).or_insert(next)
            })
            .collect();
        Self {
            observers: reads,
            groups,
            group_count: numbers.len(),
        }
    }

    /// Read every observer into `snapshots`, reusing its storage.
    pub(crate) fn snapshot<B: SimBackend>(
        &self,
        backend: &B,
        snapshots: &mut Vec<ObserverSnapshot>,
    ) {
        snapshots.resize_with(self.observers.len(), Default::default);
        for (reads, snapshot) in self.observers.iter().zip(snapshots.iter_mut()) {
            snapshot.words.clear();
            snapshot.wide.clear();
            for read in reads {
                match *read {
                    Read::Word {
                        value,
                        mask,
                        lsb,
                        bits,
                    } => {
                        let word = |signal| (backend.get_as::<u64>(signal) >> lsb) & bits;
                        snapshot.words.push(word(value));
                        if let Some(mask) = mask {
                            snapshot.words.push(word(mask));
                        }
                    }
                    Read::Wide { signal, lsb, msb } => {
                        let (value, mask) = if signal.is_4state {
                            backend.get_four_state(signal)
                        } else {
                            (backend.get(signal), BigUint::default())
                        };
                        snapshot
                            .wide
                            .push((slice(&value, lsb, msb), slice(&mask, lsb, msb)));
                    }
                    Read::Counter(counter) => snapshot
                        .words
                        .push(u64::from(backend.get_as::<u32>(counter))),
                }
            }
        }
    }

    /// A snapshot of every observer.
    pub(crate) fn take<B: SimBackend>(&self, backend: &B) -> Vec<ObserverSnapshot> {
        let mut snapshots = Vec::new();
        self.snapshot(backend, &mut snapshots);
        snapshots
    }
}

fn slice(value: &BigUint, lsb: usize, msb: usize) -> BigUint {
    if msb < lsb {
        return BigUint::default();
    }
    let width = msb - lsb + 1;
    (value >> lsb) & ((BigUint::from(1u8) << width) - 1u8)
}

/// The event sites whose observers are active: an observer whose snapshot
/// differs from the last one, or every observer at the first evaluation,
/// activates the sites of its activation group.
pub(crate) fn active_sites(
    observers: &[RuntimeCombObserver<AbsoluteAddr>],
    site_count: usize,
    reads: &ObserverReads,
    now: &[ObserverSnapshot],
    last: &[ObserverSnapshot],
    initial: bool,
) -> Vec<bool> {
    let mut active_groups = vec![false; reads.group_count];
    for (index, &group) in reads.groups.iter().enumerate() {
        if initial || now.get(index) != last.get(index) {
            active_groups[group] = true;
        }
    }
    let mut active_sites = vec![false; site_count];
    for (observer, &group) in observers.iter().zip(&reads.groups) {
        if active_groups[group] {
            active_sites[observer.site_id as usize] = true;
        }
    }
    active_sites
}
