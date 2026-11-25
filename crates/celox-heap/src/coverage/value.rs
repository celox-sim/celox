//! Sampled values as keys, and the matchers that find the bins of one.
//!
//! The key of a value of a coverpoint is its bits with the sign bit of a
//! signed type inverted, so that keys order as the values do whatever the
//! signedness. A coverpoint of at most 64 bits has `u64` keys and never
//! allocates when it is sampled; a wider one has [`BigUint`] keys.

use std::collections::BTreeMap;

use num_bigint::{BigInt, BigUint};
use serde::{Deserialize, Serialize};

/// A key of a coverpoint value.
pub(crate) trait Key: Clone + Ord + std::fmt::Debug {
    /// The key of bits `bits`, which fit the key type.
    fn from_bits(bits: &BigUint) -> Self;
    /// The key divided by `per`, which is not zero.
    fn quotient(&self, per: &Self) -> u64;
}

impl Key for u64 {
    fn from_bits(bits: &BigUint) -> Self {
        bits.iter_u64_digits().next().unwrap_or(0)
    }

    fn quotient(&self, per: &Self) -> u64 {
        self / per
    }
}

impl Key for BigUint {
    fn from_bits(bits: &BigUint) -> Self {
        bits.clone()
    }

    fn quotient(&self, per: &Self) -> u64 {
        u64::try_from(self / per).unwrap_or(u64::MAX)
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

/// A key as a bin identity holds it: one word where it fits one.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

/// The equal partition of all the keys of a coverpoint into its automatic
/// bins (19.5.3): `bins` bins from bin index `first`, each of `per` keys
/// but the last, which takes the rest.
pub(crate) struct Partition {
    pub per: BigUint,
    pub bins: u32,
    pub first: u32,
}

/// The matcher of a coverpoint, by the type of its keys.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Matcher {
    Narrow(MatcherOf<u64>),
    Wide(MatcherOf<BigUint>),
}

impl Matcher {
    /// The matcher of a coverpoint of `width` bits with the value sets
    /// `intervals` and `exact` and the automatic bins `partition`, and with
    /// `overlaps` the pairs of bins that share a value.
    pub(crate) fn new(
        width: u32,
        intervals: Vec<Interval>,
        mut exact: Vec<Exact>,
        partition: Option<Partition>,
        overlaps: bool,
    ) -> (Self, Vec<(u32, u32)>) {
        let (segments, mut overlaps) = segments(intervals, overlaps);
        // A value that a bin lists twice is one value of the bin, as a
        // 2-state value is in one segment: a sample hits a bin once.
        exact.sort_unstable_by(|first, second| {
            (first.bin, &first.value, &first.mask).cmp(&(second.bin, &second.value, &second.mask))
        });
        exact.dedup_by(|second, first| {
            first.bin == second.bin && first.value == second.value && first.mask == second.mask
        });
        if let Some(overlaps) = &mut overlaps {
            for (index, first) in exact.iter().enumerate() {
                for second in &exact[index + 1..] {
                    if first.bin != second.bin
                        && first.value == second.value
                        && first.mask == second.mask
                    {
                        overlaps.push((first.bin.min(second.bin), first.bin.max(second.bin)));
                    }
                }
            }
            overlaps.sort_unstable();
            overlaps.dedup();
        }
        let overlaps = overlaps.unwrap_or_default();
        let matcher = if width <= 64 {
            Self::Narrow(MatcherOf::new(segments, exact, partition))
        } else {
            Self::Wide(MatcherOf::new(segments, exact, partition))
        };
        (matcher, overlaps)
    }
}

/// The disjoint runs of keys that the same bins hold, in key order, and
/// with `overlaps` the pairs of bins that share a key.
fn segments(
    intervals: Vec<Interval>,
    overlaps: bool,
) -> (Vec<(BigUint, BigUint, Vec<u32>)>, Option<Vec<(u32, u32)>>) {
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
    let mut segments = Vec::new();
    let mut overlaps = overlaps.then(Vec::new);
    // The bins that hold the keys from `from` on, with how many of their
    // ranges do.
    let mut active: BTreeMap<u32, u32> = BTreeMap::new();
    let mut from = BigUint::ZERO;
    for (at, start, bin) in events {
        if at != from {
            if !active.is_empty() {
                let bins: Vec<u32> = active.keys().copied().collect();
                if let Some(overlaps) = &mut overlaps {
                    for (index, &first) in bins.iter().enumerate() {
                        overlaps.extend(bins[index + 1..].iter().map(|&second| (first, second)));
                    }
                }
                segments.push((from, &at - 1u8, bins));
            }
            from = at;
        }
        if start {
            *active.entry(bin).or_default() += 1;
        } else if let Some(count) = active.get_mut(&bin) {
            *count -= 1;
            if *count == 0 {
                active.remove(&bin);
            }
        }
    }
    (segments, overlaps)
}

/// The keys `low..=high`, which the bins `hits[first..first + count]` hold.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Segment<K> {
    low: K,
    high: K,
    first: u32,
    count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Pattern<K> {
    value: K,
    mask: K,
    bin: u32,
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
    hits: Vec<u32>,
    /// The values with x or z bits, which only an equal value matches
    /// (19.5.4). No two are equal.
    patterns: Vec<Pattern<K>>,
    auto: Option<Auto<K>>,
}

impl<K: Key> MatcherOf<K> {
    fn new(
        segments: Vec<(BigUint, BigUint, Vec<u32>)>,
        exact: Vec<Exact>,
        partition: Option<Partition>,
    ) -> Self {
        let mut hits = Vec::new();
        let segments = segments
            .into_iter()
            .map(|(low, high, bins)| {
                let first = hits.len() as u32;
                hits.extend_from_slice(&bins);
                Segment {
                    low: K::from_bits(&low),
                    high: K::from_bits(&high),
                    first,
                    count: bins.len() as u32,
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
    /// has no x or z bit.
    pub(crate) fn hits(&self, key: &K, mut hit: impl FnMut(u32)) {
        if let Some(auto) = &self.auto {
            let index = if auto.bins == 1 {
                0
            } else {
                key.quotient(&auto.per).min(u64::from(auto.bins - 1)) as u32
            };
            hit(auto.first + index);
        }
        let at = self.segments.partition_point(|segment| segment.high < *key);
        if let Some(segment) = self.segments.get(at)
            && segment.low <= *key
        {
            let first = segment.first as usize;
            for &bin in &self.hits[first..first + segment.count as usize] {
                hit(bin);
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
