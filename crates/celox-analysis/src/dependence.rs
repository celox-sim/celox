//! Ordered memory-dependence construction over abstract objects and byte ranges.
//!
//! Exact ranges are represented by an interval partition whose size is bounded
//! by access endpoints, not by the number of bytes in an object. An exact
//! access touching `k` live segments takes `O((k + 1) log S + D)` time, where
//! `S` is the number of segments and `D` is the number of dependencies emitted.
//! Storage is `O(S + R)`, where `R` is the unresolved readers retained for WAR
//! edges. Unknown-object operations intentionally scan one object; UnknownAll
//! operations scan all currently represented objects.

use std::collections::{BTreeMap, BTreeSet};

use crate::memory::MemoryEffect;

#[derive(Debug, Clone, PartialEq, Eq)]
struct AccessHistory<I> {
    last_writer: Option<I>,
    readers_since_write: Vec<I>,
}

impl<I> Default for AccessHistory<I> {
    fn default() -> Self {
        Self {
            last_writer: None,
            readers_since_write: Vec::new(),
        }
    }
}

impl<I: Copy + Eq> AccessHistory<I> {
    fn record_reader(&mut self, instruction: I) {
        if self.readers_since_write.last() != Some(&instruction) {
            self.readers_since_write.push(instruction);
        }
    }
}

#[derive(Debug, Clone)]
struct MemorySegment<I> {
    end: i64,
    history: AccessHistory<I>,
}

#[derive(Debug)]
struct RangeMemoryHistory<I> {
    segments: BTreeMap<i64, MemorySegment<I>>,
}

impl<I> Default for RangeMemoryHistory<I> {
    fn default() -> Self {
        Self {
            segments: BTreeMap::new(),
        }
    }
}

impl<I: Copy + Ord> RangeMemoryHistory<I> {
    fn split_at(&mut self, point: i64) {
        let Some((&start, segment)) = self.segments.range(..=point).next_back() else {
            return;
        };
        if start == point || point >= segment.end {
            return;
        }
        let tail = segment.clone();
        self.segments
            .get_mut(&start)
            .expect("the selected memory segment exists")
            .end = point;
        self.segments.insert(point, tail);
    }

    fn read(&mut self, offset: i64, end: i64, instruction: I, dependencies: &mut BTreeSet<I>) {
        self.split_at(offset);
        self.split_at(end);

        let existing = self
            .segments
            .range(offset..end)
            .map(|(&start, segment)| (start, segment.end))
            .collect::<Vec<_>>();
        let mut cursor = offset;
        for &(start, segment_end) in &existing {
            if cursor < start {
                self.segments.insert(
                    cursor,
                    MemorySegment {
                        end: start,
                        history: AccessHistory::default(),
                    },
                );
            }
            cursor = cursor.max(segment_end);
        }
        if cursor < end {
            self.segments.insert(
                cursor,
                MemorySegment {
                    end,
                    history: AccessHistory::default(),
                },
            );
        }

        let starts = self
            .segments
            .range(offset..end)
            .map(|(&start, _)| start)
            .collect::<Vec<_>>();
        for start in starts {
            let history = &mut self
                .segments
                .get_mut(&start)
                .expect("the covered memory segment exists")
                .history;
            dependencies.extend(history.last_writer);
            history.record_reader(instruction);
        }
    }

    fn write(&mut self, offset: i64, end: i64, instruction: I, dependencies: &mut BTreeSet<I>) {
        self.split_at(offset);
        self.split_at(end);
        let starts = self
            .segments
            .range(offset..end)
            .map(|(&start, _)| start)
            .collect::<Vec<_>>();
        for start in &starts {
            let history = &self
                .segments
                .get(start)
                .expect("the overlapping memory segment exists")
                .history;
            dependencies.extend(history.last_writer);
            dependencies.extend(history.readers_since_write.iter().copied());
        }
        for start in starts {
            self.segments.remove(&start);
        }
        self.segments.insert(
            offset,
            MemorySegment {
                end,
                history: AccessHistory {
                    last_writer: Some(instruction),
                    readers_since_write: Vec::new(),
                },
            },
        );
        self.coalesce_around(offset);
    }

    fn collect_writers(&self, dependencies: &mut BTreeSet<I>) {
        dependencies.extend(
            self.segments
                .values()
                .filter_map(|segment| segment.history.last_writer),
        );
    }

    fn collect_reads_and_writes(&self, dependencies: &mut BTreeSet<I>) {
        for segment in self.segments.values() {
            dependencies.extend(segment.history.last_writer);
            dependencies.extend(segment.history.readers_since_write.iter().copied());
        }
    }

    fn coalesce_around(&mut self, mut start: i64) {
        let Some(mut current) = self.segments.get(&start).cloned() else {
            return;
        };
        let predecessor = self
            .segments
            .range(..start)
            .next_back()
            .map(|(&other_start, other)| (other_start, other.clone()));
        if let Some((other_start, other)) = predecessor
            && other.end == start
            && other.history == current.history
        {
            self.segments.remove(&start);
            self.segments
                .get_mut(&other_start)
                .expect("the predecessor memory segment exists")
                .end = current.end;
            start = other_start;
            current.end = self.segments[&start].end;
        }
        if let Some(successor) = self.segments.get(&current.end).cloned()
            && successor.history == current.history
        {
            self.segments.remove(&current.end);
            self.segments
                .get_mut(&start)
                .expect("the current memory segment exists")
                .end = successor.end;
        }
    }
}

#[derive(Debug)]
struct MemoryObjectHistory<I> {
    exact: RangeMemoryHistory<I>,
    last_unknown_writer: Option<I>,
    unknown_readers_since_write: Vec<I>,
}

impl<I> Default for MemoryObjectHistory<I> {
    fn default() -> Self {
        Self {
            exact: RangeMemoryHistory::default(),
            last_unknown_writer: None,
            unknown_readers_since_write: Vec::new(),
        }
    }
}

impl<I: Copy + Ord> MemoryObjectHistory<I> {
    fn read_exact(
        &mut self,
        offset: i64,
        end: i64,
        instruction: I,
        dependencies: &mut BTreeSet<I>,
    ) {
        dependencies.extend(self.last_unknown_writer);
        self.exact.read(offset, end, instruction, dependencies);
    }

    fn write_exact(
        &mut self,
        offset: i64,
        end: i64,
        instruction: I,
        dependencies: &mut BTreeSet<I>,
    ) {
        dependencies.extend(self.last_unknown_writer);
        dependencies.extend(self.unknown_readers_since_write.iter().copied());
        self.exact.write(offset, end, instruction, dependencies);
    }

    fn read_unknown(&mut self, instruction: I, dependencies: &mut BTreeSet<I>) {
        dependencies.extend(self.last_unknown_writer);
        self.exact.collect_writers(dependencies);
        if self.unknown_readers_since_write.last() != Some(&instruction) {
            self.unknown_readers_since_write.push(instruction);
        }
    }

    fn write_unknown(&mut self, instruction: I, dependencies: &mut BTreeSet<I>) {
        self.collect_reads_and_writes(dependencies);
        self.exact.segments.clear();
        self.last_unknown_writer = Some(instruction);
        self.unknown_readers_since_write.clear();
    }

    fn collect_writers(&self, dependencies: &mut BTreeSet<I>) {
        dependencies.extend(self.last_unknown_writer);
        self.exact.collect_writers(dependencies);
    }

    fn collect_reads_and_writes(&self, dependencies: &mut BTreeSet<I>) {
        dependencies.extend(self.last_unknown_writer);
        dependencies.extend(self.unknown_readers_since_write.iter().copied());
        self.exact.collect_reads_and_writes(dependencies);
    }
}

/// Incrementally records the dependencies required to preserve source-order
/// memory semantics. Returned edges include RAW, WAR, and WAW, but never an
/// edge solely between two reads.
#[derive(Debug)]
pub struct MemoryDependencyTracker<O, I> {
    objects: BTreeMap<O, MemoryObjectHistory<I>>,
    last_global_writer: Option<I>,
    global_readers_since_write: Vec<I>,
}

impl<O, I> Default for MemoryDependencyTracker<O, I> {
    fn default() -> Self {
        Self {
            objects: BTreeMap::new(),
            last_global_writer: None,
            global_readers_since_write: Vec::new(),
        }
    }
}

impl<O: Copy + Ord, I: Copy + Ord> MemoryDependencyTracker<O, I> {
    /// Add one ordered memory event. Reads are processed before writes, so an
    /// event may represent a read-modify-write instruction. Any self-edge is
    /// removed before returning.
    pub fn add_event<R, W>(
        &mut self,
        instruction: I,
        reads: R,
        writes: W,
        dependencies: &mut BTreeSet<I>,
    ) where
        R: IntoIterator<Item = MemoryEffect<O>>,
        W: IntoIterator<Item = MemoryEffect<O>>,
    {
        for effect in reads {
            self.read(effect, instruction, dependencies);
        }
        for effect in writes {
            self.write(effect, instruction, dependencies);
        }
        dependencies.remove(&instruction);
    }

    fn read(&mut self, effect: MemoryEffect<O>, instruction: I, dependencies: &mut BTreeSet<I>) {
        match effect {
            MemoryEffect::Exact(location) => {
                if location.byte_len == 0 {
                    return;
                }
                dependencies.extend(self.last_global_writer);
                match location.end() {
                    Some(end) => self.objects.entry(location.object).or_default().read_exact(
                        location.offset,
                        end,
                        instruction,
                        dependencies,
                    ),
                    None => self
                        .objects
                        .entry(location.object)
                        .or_default()
                        .read_unknown(instruction, dependencies),
                }
            }
            MemoryEffect::UnknownObject(object) => {
                dependencies.extend(self.last_global_writer);
                self.objects
                    .entry(object)
                    .or_default()
                    .read_unknown(instruction, dependencies);
            }
            MemoryEffect::UnknownAll => {
                dependencies.extend(self.last_global_writer);
                for history in self.objects.values() {
                    history.collect_writers(dependencies);
                }
                if self.global_readers_since_write.last() != Some(&instruction) {
                    self.global_readers_since_write.push(instruction);
                }
            }
        }
    }

    fn write(&mut self, effect: MemoryEffect<O>, instruction: I, dependencies: &mut BTreeSet<I>) {
        match effect {
            MemoryEffect::Exact(location) => {
                if location.byte_len == 0 {
                    return;
                }
                self.collect_global_history(dependencies);
                match location.end() {
                    Some(end) => self
                        .objects
                        .entry(location.object)
                        .or_default()
                        .write_exact(location.offset, end, instruction, dependencies),
                    None => self
                        .objects
                        .entry(location.object)
                        .or_default()
                        .write_unknown(instruction, dependencies),
                }
            }
            MemoryEffect::UnknownObject(object) => {
                self.collect_global_history(dependencies);
                self.objects
                    .entry(object)
                    .or_default()
                    .write_unknown(instruction, dependencies);
            }
            MemoryEffect::UnknownAll => {
                self.collect_global_history(dependencies);
                for history in self.objects.values() {
                    history.collect_reads_and_writes(dependencies);
                }
                self.objects.clear();
                self.last_global_writer = Some(instruction);
                self.global_readers_since_write.clear();
            }
        }
    }

    fn collect_global_history(&self, dependencies: &mut BTreeSet<I>) {
        dependencies.extend(self.last_global_writer);
        dependencies.extend(self.global_readers_since_write.iter().copied());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryLocation;

    fn exact(object: u8, offset: i64, byte_len: usize) -> MemoryEffect<u8> {
        MemoryEffect::Exact(MemoryLocation {
            object,
            offset,
            byte_len,
        })
    }

    fn add(
        tracker: &mut MemoryDependencyTracker<u8, usize>,
        instruction: usize,
        reads: Vec<MemoryEffect<u8>>,
        writes: Vec<MemoryEffect<u8>>,
    ) -> BTreeSet<usize> {
        let mut dependencies = BTreeSet::new();
        tracker.add_event(instruction, reads, writes, &mut dependencies);
        dependencies
    }

    #[test]
    fn emits_raw_war_and_waw_but_no_read_after_read_edge() {
        let mut tracker = MemoryDependencyTracker::default();

        assert!(add(&mut tracker, 0, vec![exact(0, 0, 8)], vec![]).is_empty());
        assert!(add(&mut tracker, 1, vec![exact(0, 4, 4)], vec![]).is_empty());
        assert_eq!(
            add(&mut tracker, 2, vec![], vec![exact(0, 0, 8)]),
            BTreeSet::from([0, 1])
        );
        assert_eq!(
            add(&mut tracker, 3, vec![exact(0, 7, 1)], vec![]),
            BTreeSet::from([2])
        );
        assert_eq!(
            add(&mut tracker, 4, vec![], vec![exact(0, 0, 8)]),
            BTreeSet::from([2, 3])
        );
    }

    #[test]
    fn disjoint_ranges_and_objects_have_independent_histories() {
        let mut tracker = MemoryDependencyTracker::default();
        assert!(add(&mut tracker, 0, vec![], vec![exact(0, 0, 8)]).is_empty());
        assert!(add(&mut tracker, 1, vec![exact(0, 8, 8)], vec![]).is_empty());
        assert!(add(&mut tracker, 2, vec![exact(1, 0, 8)], vec![]).is_empty());
        assert_eq!(
            add(&mut tracker, 3, vec![exact(0, 7, 2)], vec![]),
            BTreeSet::from([0])
        );
    }

    #[test]
    fn unknown_object_aliases_only_its_object() {
        let mut tracker = MemoryDependencyTracker::default();
        assert!(add(&mut tracker, 0, vec![], vec![exact(0, 0, 8)]).is_empty());
        assert!(
            add(
                &mut tracker,
                1,
                vec![MemoryEffect::UnknownObject(1)],
                vec![]
            )
            .is_empty()
        );
        assert_eq!(
            add(
                &mut tracker,
                2,
                vec![MemoryEffect::UnknownObject(0)],
                vec![]
            ),
            BTreeSet::from([0])
        );
        assert_eq!(
            add(&mut tracker, 3, vec![], vec![exact(0, 32, 8)]),
            BTreeSet::from([2])
        );
    }

    #[test]
    fn unknown_all_orders_every_object_and_future_access() {
        let mut tracker = MemoryDependencyTracker::default();
        assert!(add(&mut tracker, 0, vec![], vec![exact(0, 0, 8)]).is_empty());
        assert!(add(&mut tracker, 1, vec![exact(1, 0, 8)], vec![]).is_empty());
        assert_eq!(
            add(&mut tracker, 2, vec![MemoryEffect::UnknownAll], vec![]),
            BTreeSet::from([0])
        );
        assert_eq!(
            add(&mut tracker, 3, vec![], vec![MemoryEffect::UnknownAll]),
            BTreeSet::from([0, 1, 2])
        );
        assert_eq!(
            add(&mut tracker, 4, vec![exact(9, 0, 1)], vec![]),
            BTreeSet::from([3])
        );
    }

    #[test]
    fn read_modify_write_does_not_emit_a_self_edge() {
        let mut tracker = MemoryDependencyTracker::default();
        assert!(add(&mut tracker, 0, vec![exact(0, 0, 8)], vec![exact(0, 0, 8)]).is_empty());
        assert_eq!(
            add(&mut tracker, 1, vec![exact(0, 0, 8)], vec![]),
            BTreeSet::from([0])
        );
    }

    #[test]
    fn storage_scales_with_endpoints_instead_of_range_width() {
        const WRITES: usize = 256;
        const HUGE_RANGE: usize = 4_000_000;
        let mut tracker = MemoryDependencyTracker::default();
        for instruction in 0..WRITES {
            let mut dependencies = BTreeSet::new();
            tracker.add_event(
                instruction,
                [],
                [
                    exact(0, 0, 8),
                    exact(0, 1024 + instruction as i64, 1),
                    exact(0, 1_000_000, HUGE_RANGE),
                ],
                &mut dependencies,
            );
            if instruction != 0 {
                assert!(dependencies.contains(&(instruction - 1)));
            }
        }

        let segments = tracker.objects[&0].exact.segments.len();
        assert_eq!(segments, WRITES + 2);
        assert!(segments < HUGE_RANGE / 1000);
    }

    #[test]
    fn overflowing_exact_range_falls_back_to_unknown_object() {
        let mut tracker = MemoryDependencyTracker::default();
        assert!(add(&mut tracker, 0, vec![], vec![exact(0, 0, 8)]).is_empty());
        assert_eq!(
            add(&mut tracker, 1, vec![exact(0, i64::MAX, 2)], vec![]),
            BTreeSet::from([0])
        );
    }
}
