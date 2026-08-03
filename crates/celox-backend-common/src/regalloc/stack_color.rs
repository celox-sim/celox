//! Opcode-free stack-slot coloring over exact sparse live intervals.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::fmt;

use super::LiveInterval;

/// Failure while assigning target-owned spill homes to reusable frame slots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StackColorError<V> {
    DuplicateValue(V),
    EmptyInterval(V),
    SlotCountOverflow,
}

impl<V: fmt::Debug> fmt::Display for StackColorError<V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateValue(value) => {
                write!(formatter, "stack interval for {value:?} was colored twice")
            }
            Self::EmptyInterval(value) => {
                write!(
                    formatter,
                    "stack interval for {value:?} has no live segments"
                )
            }
            Self::SlotCountOverflow => formatter.write_str("stack-slot count exceeds u32"),
        }
    }
}

impl<V: fmt::Debug> std::error::Error for StackColorError<V> {}

/// Deterministic stack-slot assignment for target-owned spill homes.
///
/// Backends retain responsibility for choosing spill values and translating a
/// slot number into their frame layout. This helper conservatively projects
/// each exact sparse interval to a linear block-order envelope, then reuses
/// slots with a sweep. Envelope overlap may miss a legal reuse across a sparse
/// gap, but cannot make interfering homes alias. The algorithm is independent
/// of target MIR and opcode semantics and runs in O(n log n).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackSlotColoring<V> {
    assignments: BTreeMap<V, u32>,
    slot_count: u32,
}

impl<V> StackSlotColoring<V>
where
    V: Ord,
{
    pub fn get(&self, value: &V) -> Option<u32> {
        self.assignments.get(value).copied()
    }

    pub fn slot_count(&self) -> u32 {
        self.slot_count
    }
}

/// Color a complete target spill batch using conservative linear envelopes.
pub fn color_stack_slots<'a, V, I>(intervals: I) -> Result<StackSlotColoring<V>, StackColorError<V>>
where
    V: 'a + Copy + Ord,
    I: IntoIterator<Item = &'a LiveInterval<V>>,
{
    let mut seen = BTreeSet::new();
    let mut ordered = Vec::new();
    for interval in intervals {
        if !seen.insert(interval.value) {
            return Err(StackColorError::DuplicateValue(interval.value));
        }
        let Some(first) = interval.segments.first() else {
            return Err(StackColorError::EmptyInterval(interval.value));
        };
        let last = interval
            .segments
            .last()
            .expect("a nonempty interval has a last segment");
        ordered.push((
            (first.block, first.start),
            (last.block, last.end),
            interval.value,
        ));
    }
    ordered.sort_unstable();

    let mut active = BinaryHeap::<Reverse<((usize, u64), u32, V)>>::new();
    let mut available = BinaryHeap::<Reverse<u32>>::new();
    let mut next_slot = 0_u32;
    let mut assignments = BTreeMap::new();
    for (start, end, value) in ordered {
        while active
            .peek()
            .is_some_and(|Reverse((active_end, _, _))| *active_end <= start)
        {
            let Reverse((_, slot, _)) = active.pop().expect("peeked active interval exists");
            available.push(Reverse(slot));
        }
        let slot = if let Some(Reverse(slot)) = available.pop() {
            slot
        } else {
            let slot = next_slot;
            next_slot = next_slot
                .checked_add(1)
                .ok_or(StackColorError::SlotCountOverflow)?;
            slot
        };
        assignments.insert(value, slot);
        active.push(Reverse((end, slot, value)));
    }
    Ok(StackSlotColoring {
        assignments,
        slot_count: next_slot,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regalloc::LiveSegment;

    fn interval(value: u32, segments: &[(usize, u64, u64)]) -> LiveInterval<u32> {
        LiveInterval {
            value,
            segments: segments
                .iter()
                .map(|&(block, start, end)| LiveSegment { block, start, end })
                .collect(),
        }
    }

    #[test]
    fn reuses_a_slot_for_disjoint_sparse_intervals() {
        let intervals = [
            interval(0, &[(0, 0, 4)]),
            interval(1, &[(0, 4, 8)]),
            interval(2, &[(1, 0, 8)]),
        ];
        let coloring = color_stack_slots(&intervals).unwrap();

        assert_eq!(coloring.get(&0), Some(0));
        assert_eq!(coloring.get(&1), Some(0));
        assert_eq!(coloring.get(&2), Some(0));
        assert_eq!(coloring.slot_count(), 1);
    }

    #[test]
    fn separates_any_pair_with_an_overlapping_segment() {
        let intervals = [
            interval(0, &[(0, 0, 2), (2, 0, 8)]),
            interval(1, &[(1, 0, 2), (2, 7, 9)]),
        ];
        let coloring = color_stack_slots(&intervals).unwrap();

        assert_eq!(coloring.get(&0), Some(0));
        assert_eq!(coloring.get(&1), Some(1));
    }

    #[test]
    fn rejects_duplicate_values() {
        let value = interval(7, &[(0, 0, 1)]);

        assert_eq!(
            color_stack_slots([&value, &value]),
            Err(StackColorError::DuplicateValue(7))
        );
    }
}
