//! Sampled values as keys, and the matchers that find the bins of one.
//!
//! The key of a value of a coverpoint is its bits with the sign bit of a
//! signed type inverted, so that keys order as the values do whatever the
//! signedness. A coverpoint of at most 64 bits has `u64` keys and never
//! allocates when it is sampled; a wider one has [`BigUint`] keys.

use std::collections::{BTreeMap, BTreeSet};

use num_bigint::{BigInt, BigUint};
use serde::{Deserialize, Serialize};

/// A key of a coverpoint value.
pub(crate) trait Key: Clone + Ord + std::fmt::Debug {
    /// The key of bits `bits`, which fit the key type.
    fn from_bits(bits: &BigUint) -> Self;
    /// The key divided by `per`, which is not zero.
    fn quotient(&self, per: &Self) -> u64;
    /// The bits of the key that `care` has set.
    fn masked(&self, care: &Self) -> Self;
}

impl Key for u64 {
    fn from_bits(bits: &BigUint) -> Self {
        bits.iter_u64_digits().next().unwrap_or(0)
    }

    fn quotient(&self, per: &Self) -> u64 {
        self / per
    }

    fn masked(&self, care: &Self) -> Self {
        self & care
    }
}

impl Key for BigUint {
    fn from_bits(bits: &BigUint) -> Self {
        bits.clone()
    }

    fn quotient(&self, per: &Self) -> u64 {
        u64::try_from(self / per).unwrap_or(u64::MAX)
    }

    fn masked(&self, care: &Self) -> Self {
        self & care
    }
}

/// The low `width` bits set.
pub(crate) fn ones(width: u32) -> BigUint {
    (BigUint::from(1u8) << width) - 1u8
}

/// The low `width` bits of a word set, for a width of at most 64.
pub(crate) fn low_ones(width: u32) -> u64 {
    if width >= 64 {
        u64::MAX
    } else {
        (1 << width) - 1
    }
}

/// What a key of a type of `width` bits differs from the bits of its value
/// by: the sign bit of a signed type.
pub(crate) fn bias(width: u32, signed: bool) -> BigUint {
    if signed && width != 0 {
        BigUint::from(1u8) << (width - 1)
    } else {
        BigUint::ZERO
    }
}

/// The value that the low `width` bits of `bits` are in a type of that
/// width and signedness.
pub(crate) fn numeric(bits: &BigUint, width: u32, signed: bool) -> BigInt {
    let bits = bits & ones(width);
    if signed && width != 0 && bits.bit(u64::from(width) - 1) {
        BigInt::from(bits) - (BigInt::from(1) << width)
    } else {
        BigInt::from(bits)
    }
}

/// The value of key `key` of a type of `width` bits.
pub(crate) fn value_of_key(key: &BigUint, width: u32, signed: bool) -> BigInt {
    BigInt::from(key.clone()) - BigInt::from(bias(width, signed))
}

/// A key as a bin identity holds it: one word where it fits one. The order
/// is that of the keys.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) enum KeyValue {
    Narrow(u64),
    Wide(BigUint),
}

impl KeyValue {
    pub(crate) fn new(key: &BigUint) -> Self {
        match u64::try_from(key) {
            Ok(word) => Self::Narrow(word),
            Err(_) => Self::Wide(key.clone()),
        }
    }

    pub(crate) fn key(&self) -> BigUint {
        match self {
            Self::Narrow(word) => BigUint::from(*word),
            Self::Wide(key) => key.clone(),
        }
    }
}

/// The values `low..=high`, as keys, of bin `bin`.
pub(crate) struct Interval {
    pub low: BigUint,
    pub high: BigUint,
    pub bin: u32,
}

/// A value with x or z bits, as the bits of its two planes, of bin `bin`.
pub(crate) struct Exact {
    pub value: BigUint,
    pub mask: BigUint,
    pub bin: u32,
}

/// The values whose bits are those of `value` where `care` has a bit set,
/// of bin `bin`: a wildcard value (19.5.4) of more ranges of values than a
/// bin lists.
pub(crate) struct Wild {
    pub value: BigUint,
    pub care: BigUint,
    pub bin: u32,
}

/// The equal partition of all the keys of a coverpoint into its automatic
/// bins (19.5.3): `bins` bins from bin index `first`, each of `per` keys
/// but the last, which takes the rest.
pub(crate) struct Partition {
    pub per: BigUint,
    pub bins: u32,
    pub first: u32,
}

/// The values of the bins of a coverpoint.
#[derive(Default)]
pub(crate) struct Values {
    pub intervals: Vec<Interval>,
    pub exact: Vec<Exact>,
    pub wild: Vec<Wild>,
    pub partition: Option<Partition>,
}

/// The most pairs of overlapping bins that a matcher lists.
const MAX_OVERLAPS: usize = 64;

/// The pairs of bins that share a value.
#[derive(Default)]
pub(crate) struct Overlaps {
    pub pairs: BTreeSet<(u32, u32)>,
    /// Whether there are more pairs than these.
    pub more: bool,
}

impl Overlaps {
    /// Note that each two of `bins`, which are in order, share a value.
    fn add(&mut self, bins: &[u32]) {
        for (index, &first) in bins.iter().enumerate() {
            for &second in &bins[index + 1..] {
                if self.pairs.len() < MAX_OVERLAPS {
                    self.pairs.insert((first, second));
                } else if !self.pairs.contains(&(first, second)) {
                    self.more = true;
                    return;
                }
            }
        }
    }
}

/// The matcher of a coverpoint with what making it found.
pub(crate) struct Matched {
    pub matcher: Matcher,
    /// By bin, whether the bin holds a value: a value of an ignore or
    /// illegal bin is one of no other bin (19.5.5, 19.5.6).
    pub holds: Vec<bool>,
    pub overlaps: Overlaps,
}

/// The matcher of a coverpoint, by the type of its keys.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Matcher {
    Narrow(MatcherOf<u64>),
    Wide(MatcherOf<BigUint>),
}

impl Matcher {
    /// The matcher of a coverpoint of `width` bits and signedness `signed`
    /// with `bins` bins of the values `values`, where `removes` holds of the
    /// ignore and illegal bins, and with `overlaps` the pairs of bins that
    /// share a value. The values of a wildcard value that is kept as a
    /// pattern are not known here: they are removed from no bin, no bin
    /// removes them, and they overlap nothing.
    pub(crate) fn build(
        width: u32,
        signed: bool,
        bins: usize,
        values: Values,
        removes: &dyn Fn(u32) -> bool,
        overlaps: bool,
    ) -> Matched {
        let Values {
            intervals,
            mut exact,
            wild,
            partition,
        } = values;
        let mut holds = vec![false; bins];
        let mut found = Overlaps::default();
        let segments = segments(
            intervals,
            removes,
            &mut holds,
            overlaps.then_some(&mut found),
        );
        // A value that a bin lists twice is one value of the bin, as a
        // 2-state value is in one segment: a sample hits a bin once.
        exact.sort_unstable_by(|first, second| {
            (&first.value, &first.mask, first.bin).cmp(&(&second.value, &second.mask, second.bin))
        });
        exact.dedup_by(|second, first| {
            first.bin == second.bin && first.value == second.value && first.mask == second.mask
        });
        let mut kept = Vec::with_capacity(exact.len());
        let mut group: Vec<u32> = Vec::new();
        let mut at = 0;
        while at < exact.len() {
            let same = exact[at..]
                .iter()
                .take_while(|other| other.value == exact[at].value && other.mask == exact[at].mask)
                .count();
            group.clear();
            group.extend(exact[at..at + same].iter().map(|exact| exact.bin));
            if group.iter().any(|&bin| removes(bin)) {
                group.retain(|&bin| removes(bin));
            }
            if overlaps {
                found.add(&group);
            }
            for &bin in &group {
                holds[bin as usize] = true;
                kept.push(Exact {
                    value: exact[at].value.clone(),
                    mask: exact[at].mask.clone(),
                    bin,
                });
            }
            at += same;
        }
        for wild in &wild {
            holds[wild.bin as usize] = true;
        }
        if let Some(partition) = &partition {
            automatic_holds(width, partition, &segments, &mut holds);
        }
        let mut wild: Vec<(Wild, bool)> = wild
            .into_iter()
            .map(|wild| {
                // The pattern of the key: the sign bit of a key is
                // inverted.
                let removes = removes(wild.bin);
                let value = (&wild.value ^ bias(width, signed)) & &wild.care;
                (Wild { value, ..wild }, removes)
            })
            .collect();
        // The patterns of a bin follow each other, which is how a sample
        // that several of them match is found to hit the bin once, and a
        // pattern that a bin lists twice is one pattern of the bin.
        wild.sort_unstable_by(|(first, _), (second, _)| {
            (first.bin, &first.value, &first.care).cmp(&(second.bin, &second.value, &second.care))
        });
        wild.dedup_by(|(second, _), (first, _)| {
            first.bin == second.bin && first.value == second.value && first.care == second.care
        });
        let matcher = if width <= 64 {
            Self::Narrow(MatcherOf::new(segments, kept, wild, partition))
        } else {
            Self::Wide(MatcherOf::new(segments, kept, wild, partition))
        };
        Matched {
            matcher,
            holds,
            overlaps: found,
        }
    }
}

/// A run of keys that the same bins hold.
struct Run {
    low: BigUint,
    high: BigUint,
    bins: Vec<u32>,
    /// Whether an ignore or illegal bin holds the keys: `bins` are those
    /// then.
    removed: bool,
}

/// The disjoint runs of keys that the same bins hold, in key order. In
/// `holds` the bins that hold a key are noted, and in `overlaps` the pairs
/// of bins that share one.
fn segments(
    intervals: Vec<Interval>,
    removes: &dyn Fn(u32) -> bool,
    holds: &mut [bool],
    mut overlaps: Option<&mut Overlaps>,
) -> Vec<Run> {
    // Where a bin starts or stops holding keys: stops sort first.
    let mut events: Vec<(BigUint, bool, u32)> = Vec::with_capacity(intervals.len() * 2);
    for interval in intervals {
        if interval.low > interval.high {
            continue;
        }
        events.push((interval.high + 1u8, false, interval.bin));
        events.push((interval.low, true, interval.bin));
    }
    events.sort_unstable();
    let mut segments: Vec<Run> = Vec::new();
    // The bins that hold the keys from `from` on, with how many of their
    // ranges do, and how many of them are ignore and illegal bins.
    let mut active: BTreeMap<u32, u32> = BTreeMap::new();
    let mut removing = 0usize;
    let mut from = BigUint::ZERO;
    for (at, start, bin) in events {
        if at != from {
            if !active.is_empty() {
                let removed = removing != 0;
                let bins: Vec<u32> = active
                    .keys()
                    .copied()
                    .filter(|&bin| !removed || removes(bin))
                    .collect();
                // Two runs that the same bins hold and that touch are one.
                match segments.last_mut() {
                    Some(last) if last.bins == bins && &last.high + 1u8 == from => {
                        last.high = &at - 1u8;
                    }
                    _ => {
                        for &bin in &bins {
                            holds[bin as usize] = true;
                        }
                        if let Some(overlaps) = overlaps.as_deref_mut() {
                            overlaps.add(&bins);
                        }
                        segments.push(Run {
                            low: from,
                            high: &at - 1u8,
                            bins,
                            removed,
                        });
                    }
                }
            }
            from = at;
        }
        if start {
            let count = active.entry(bin).or_default();
            if *count == 0 && removes(bin) {
                removing += 1;
            }
            *count += 1;
        } else if let Some(count) = active.get_mut(&bin) {
            *count -= 1;
            if *count == 0 {
                active.remove(&bin);
                if removes(bin) {
                    removing -= 1;
                }
            }
        }
    }
    segments
}

/// Note in `holds` the automatic bins of `partition` that hold a key of a
/// coverpoint of `width` bits once the keys of the removed runs of
/// `segments` are none of theirs.
fn automatic_holds(width: u32, partition: &Partition, segments: &[Run], holds: &mut [bool]) {
    let last = partition.bins - 1;
    let top = ones(width);
    let bin_of = |key: &BigUint| {
        u32::try_from(key / &partition.per)
            .unwrap_or(u32::MAX)
            .min(last)
    };
    let low_of = |bin: u32| &partition.per * bin;
    let high_of = |bin: u32| {
        if bin == last {
            top.clone()
        } else {
            &partition.per * (bin + 1) - 1u8
        }
    };
    for bin in 0..partition.bins {
        holds[(partition.first + bin) as usize] = true;
    }
    // The keys removed from the bin being looked at: runs are in key
    // order, so those of a bin follow each other.
    let mut current: Option<(u32, BigUint)> = None;
    let finish = |current: Option<(u32, BigUint)>, holds: &mut [bool]| {
        if let Some((bin, removed)) = current
            && removed == high_of(bin) - low_of(bin) + 1u8
        {
            holds[(partition.first + bin) as usize] = false;
        }
    };
    for run in segments.iter().filter(|run| run.removed) {
        for bin in bin_of(&run.low)..=bin_of(&run.high) {
            let low = low_of(bin).max(run.low.clone());
            let high = high_of(bin).min(run.high.clone());
            let count = high - low + 1u8;
            match &mut current {
                Some((at, removed)) if *at == bin => *removed += count,
                _ => {
                    finish(current.take(), holds);
                    current = Some((bin, count));
                }
            }
        }
    }
    finish(current, holds);
}

/// The keys `low..=high`, which the bins `hits[first..first + count]` hold.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Segment<K> {
    low: K,
    high: K,
    first: u32,
    count: u32,
    /// Whether the keys are those of an ignore or illegal bin, and so of no
    /// other bin.
    removed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Pattern<K> {
    value: K,
    mask: K,
    bin: u32,
}

/// The keys that are `value` in the bits of `care`, of bin `bin`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct WildPattern<K> {
    value: K,
    care: K,
    bin: u32,
    /// Whether the bin is an ignore or illegal bin.
    removes: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Auto<K> {
    per: K,
    bins: u32,
    first: u32,
}

/// The bins of the values of a coverpoint with keys of type `K`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MatcherOf<K> {
    /// Sorted and disjoint.
    segments: Vec<Segment<K>>,
    /// The bins of the segments: those of one segment are in ascending
    /// order, each once.
    hits: Vec<u32>,
    /// The values with x or z bits, which only an equal value matches
    /// (19.5.4). No two are equal in one bin, and where an ignore or
    /// illegal bin has a value no other bin has it.
    patterns: Vec<Pattern<K>>,
    /// The wildcard values that are no ranges, in the order of their bins.
    wild: Vec<WildPattern<K>>,
    auto: Option<Auto<K>>,
}

impl<K: Key> MatcherOf<K> {
    fn new(
        segments: Vec<Run>,
        exact: Vec<Exact>,
        wild: Vec<(Wild, bool)>,
        partition: Option<Partition>,
    ) -> Self {
        let mut hits = Vec::new();
        let segments = segments
            .into_iter()
            .map(|run| {
                let first = hits.len() as u32;
                hits.extend_from_slice(&run.bins);
                Segment {
                    low: K::from_bits(&run.low),
                    high: K::from_bits(&run.high),
                    first,
                    count: run.bins.len() as u32,
                    removed: run.removed,
                }
            })
            .collect();
        Self {
            segments,
            hits,
            patterns: exact
                .into_iter()
                .map(|exact| Pattern {
                    value: K::from_bits(&exact.value),
                    mask: K::from_bits(&exact.mask),
                    bin: exact.bin,
                })
                .collect(),
            wild: wild
                .into_iter()
                .map(|(wild, removes)| WildPattern {
                    value: K::from_bits(&wild.value),
                    care: K::from_bits(&wild.care),
                    bin: wild.bin,
                    removes,
                })
                .collect(),
            // One bin takes every key, and the number of keys of a 64-bit
            // type does not fit a key.
            auto: partition.map(|partition| Auto {
                per: if partition.bins == 1 {
                    K::from_bits(&BigUint::from(1u8))
                } else {
                    K::from_bits(&partition.per)
                },
                bins: partition.bins,
                first: partition.first,
            }),
        }
    }

    /// Call `hit` with each bin that holds the value of key `key`, which
    /// has no x or z bit, and with whether the value is one of an ignore or
    /// illegal bin: it is a value of no other bin then, though a wildcard
    /// value may name one that had it. A bin is named once, however many
    /// of its values the value is (19.5).
    pub(crate) fn hits(&self, key: &K, mut hit: impl FnMut(u32, bool)) {
        let at = self.segments.partition_point(|segment| segment.high < *key);
        let segment = self.segments.get(at).filter(|segment| segment.low <= *key);
        let matches = |wild: &WildPattern<K>| key.masked(&wild.care) == wild.value;
        let removed = segment.is_some_and(|segment| segment.removed)
            || self.wild.iter().any(|wild| wild.removes && matches(wild));
        if !removed && let Some(auto) = &self.auto {
            let index = if auto.bins == 1 {
                0
            } else {
                key.quotient(&auto.per).min(u64::from(auto.bins - 1)) as u32
            };
            hit(auto.first + index, removed);
        }
        let listed = segment.map_or(&[][..], |segment| {
            let first = segment.first as usize;
            &self.hits[first..first + segment.count as usize]
        });
        for &bin in listed {
            hit(bin, removed);
        }
        // A bin that a pattern names may be one of the segment, or the bin
        // of the pattern before.
        let mut last = None;
        for wild in &self.wild {
            if last != Some(wild.bin) && matches(wild) {
                last = Some(wild.bin);
                if listed.binary_search(&wild.bin).is_err() {
                    hit(wild.bin, removed);
                }
            }
        }
    }

    /// Call `hit` with each bin that holds the value of bits `value` with
    /// the x and z bits `mask`, of which there are some: once for a bin,
    /// as its patterns differ.
    pub(crate) fn exact_hits(&self, value: &K, mask: &K, mut hit: impl FnMut(u32)) {
        for pattern in &self.patterns {
            if pattern.value == *value && pattern.mask == *mask {
                hit(pattern.bin);
            }
        }
    }
}
