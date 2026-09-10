//! Write activity shared by generated code and waveform observers.
//!
//! An object is assigned by its stable home, so aliases share notifications
//! and partial/dynamic stores also notify observers of the complete object.
use serde::{Deserialize, Serialize};

pub const TRACE_GROUP_BYTES: usize = 64;
const GROUPS_PER_SUMMARY: usize = 64;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TraceLayout {
    pub flags_offset: usize,
    pub group_count: usize,
    pub summary_offset: usize,
    pub summary_count: usize,
    /// Disjoint physical homes, including both planes and element padding.
    homes: Vec<(usize, usize)>,
}

impl TraceLayout {
    pub(crate) fn new(offset: usize, stable_size: usize, mut homes: Vec<(usize, usize)>) -> Self {
        homes.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::new();
        for (start, end) in homes {
            if let Some(last) = merged.last_mut()
                && last.0 == start
            {
                last.1 = last.1.max(end);
            } else {
                merged.push((start, end));
            }
        }
        let group_count = stable_size.div_ceil(TRACE_GROUP_BYTES);
        let summary_count = group_count.div_ceil(GROUPS_PER_SUMMARY);
        Self {
            flags_offset: offset,
            group_count,
            summary_offset: offset + group_count,
            summary_count,
            homes: merged,
        }
    }

    pub fn end_offset(&self) -> usize {
        self.summary_offset + self.summary_count
    }

    pub fn validate(
        &self,
        stable_size: usize,
        metadata_start: usize,
        scratch_start: usize,
    ) -> bool {
        self.group_count == stable_size.div_ceil(TRACE_GROUP_BYTES)
            && self.summary_count == self.group_count.div_ceil(GROUPS_PER_SUMMARY)
            && self.flags_offset >= metadata_start
            && self.flags_offset.checked_add(self.group_count) == Some(self.summary_offset)
            && self
                .summary_offset
                .checked_add(self.summary_count)
                .is_some_and(|end| end <= scratch_start)
            && self
                .homes
                .iter()
                .all(|&(start, end)| start <= end && end <= stable_size)
            && self.homes.windows(2).all(|homes| homes[0].1 <= homes[1].0)
    }

    /// Two byte stores suffice: no read/modify/write in generated hot paths.
    pub fn notification_offsets(&self, stable_home: usize) -> [usize; 2] {
        let group = stable_home / TRACE_GROUP_BYTES;
        [
            self.flags_offset + group,
            self.summary_offset + group / GROUPS_PER_SUMMARY,
        ]
    }

    /// # Safety
    /// `memory` must exclusively reference the complete writable state image.
    pub unsafe fn mark_home(&self, memory: *mut u8, home: usize) {
        for offset in self.notification_offsets(home) {
            unsafe {
                *memory.add(offset) = 1;
            }
        }
    }

    /// Mark host writes, including an element view into a larger array.
    /// # Safety
    /// `memory` must exclusively reference the complete writable state image.
    pub unsafe fn mark_range(&self, memory: *mut u8, offset: usize, len: usize) {
        if len == 0 {
            return;
        }
        let first = self
            .homes
            .partition_point(|&(start, _)| start <= offset)
            .saturating_sub(1);
        for &(start, end) in &self.homes[first..] {
            if start >= offset.saturating_add(len) {
                break;
            }
            if end > offset {
                unsafe {
                    self.mark_home(memory, start);
                }
            }
        }
    }

    /// Consume only nonempty groups. Clock trigger clearing never touches this
    /// region. The caller owns the reusable result allocation.
    pub fn take(&self, memory: &mut [u8], groups: &mut Vec<usize>) {
        groups.clear();
        let (prefix, summaries) = memory.split_at_mut(self.summary_offset);
        let flags = &mut prefix[self.flags_offset..][..self.group_count];
        take_nonzero(&mut summaries[..self.summary_count], |summary| {
            let start = summary * GROUPS_PER_SUMMARY;
            let flags = &mut flags[start..];
            let len = flags.len().min(GROUPS_PER_SUMMARY);
            take_nonzero(&mut flags[..len], |index| groups.push(start + index));
        });
    }
}

/// Clear and visit nonzero bytes in address order. Notifications stay byte
/// stores, while the consumer skips eight empty bytes with one word test.
fn take_nonzero(bytes: &mut [u8], mut visit: impl FnMut(usize)) {
    let (words, tail) = bytes.as_chunks_mut::<8>();
    for (index, bytes) in words.iter_mut().enumerate() {
        // A safe unaligned load; little endian keeps bit order in address order
        // even on big-endian hosts.
        let word = u64::from_le_bytes(*bytes);
        if word == 0 {
            continue;
        }
        bytes.fill(0);
        // Set exactly the high bit of each nonzero byte, including flags other
        // than 1. Adding 0x7f to the low seven bits cannot carry between bytes.
        const LOW_BITS: u64 = 0x7f7f_7f7f_7f7f_7f7f;
        let mut active = (((word & LOW_BITS) + LOW_BITS) | word) & !LOW_BITS;
        while active != 0 {
            visit(index * 8 + active.trailing_zeros() as usize / 8);
            active &= active - 1;
        }
    }
    let base = words.len() * 8;
    for (index, byte) in tail.iter_mut().enumerate() {
        if *byte != 0 {
            *byte = 0;
            visit(base + index);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonzero_bytes_preserve_address_order() {
        for alignment in 0..8 {
            // Exercise every possible byte value, including adjacent zero and
            // nonzero bytes, word boundaries, and short tails.
            for len in 0..=512 {
                let mut bytes = vec![0xa5; alignment + len + 8];
                for (index, byte) in bytes[alignment..][..len].iter_mut().enumerate() {
                    *byte = if index % 2 == 0 { (index / 2) as u8 } else { 0 };
                }
                let expected: Vec<_> = bytes[alignment..][..len]
                    .iter()
                    .enumerate()
                    .filter_map(|(index, &byte)| (byte != 0).then_some(index))
                    .collect();
                let mut actual = vec![];
                take_nonzero(&mut bytes[alignment..][..len], |index| actual.push(index));
                assert_eq!(actual, expected);
                assert!(bytes[..alignment].iter().all(|&byte| byte == 0xa5));
                assert!(bytes[alignment..][..len].iter().all(|&byte| byte == 0));
                assert!(bytes[alignment + len..].iter().all(|&byte| byte == 0xa5));
            }
        }
    }

    #[test]
    fn activity_boundaries_match_scalar_collection() {
        for group_count in [
            0, 1, 7, 8, 9, 63, 64, 65, 127, 128, 447, 448, 449, 511, 512, 513, 575, 576, 577, 1024,
        ] {
            for alignment in 0..8 {
                // Include partial stable groups and unaligned metadata.
                let stable_size = (group_count * TRACE_GROUP_BYTES).saturating_sub(3);
                let trace = TraceLayout::new(stable_size + alignment, stable_size, vec![]);
                assert_eq!(trace.group_count, group_count);
                for pattern in 0..5 {
                    let mut memory = vec![0xa5; trace.end_offset() + 8];
                    memory[trace.flags_offset..trace.end_offset()].fill(0);
                    for group in 0..group_count {
                        let flag = match pattern {
                            0 => 0,
                            1 => 1,
                            2 => u8::from(group % 2 == 0),
                            3 => {
                                u8::from(group == 0 || group + 1 == group_count || group % 64 == 0)
                            }
                            _ => [0, 2, 0, 0x80, 0, 0xff, 1][group % 7],
                        };
                        memory[trace.flags_offset + group] = flag;
                        if flag != 0 {
                            memory[trace.summary_offset + group / GROUPS_PER_SUMMARY] = flag;
                        }
                    }
                    // Both an empty marked summary and an unmarked summary with
                    // stale flags must retain the scalar collector's behavior.
                    if trace.summary_count > 1 && matches!(pattern, 0 | 4) {
                        memory[trace.summary_offset] = 0xff;
                        memory[trace.summary_offset + 1] = 0;
                    }
                    let mut expected_memory = memory.clone();
                    let mut expected_groups = vec![];
                    for summary in 0..trace.summary_count {
                        if expected_memory[trace.summary_offset + summary] == 0 {
                            continue;
                        }
                        expected_memory[trace.summary_offset + summary] = 0;
                        for group in 0..group_count {
                            if group / GROUPS_PER_SUMMARY == summary
                                && expected_memory[trace.flags_offset + group] != 0
                            {
                                expected_memory[trace.flags_offset + group] = 0;
                                expected_groups.push(group);
                            }
                        }
                    }
                    let mut groups = vec![usize::MAX];
                    trace.take(&mut memory, &mut groups);
                    assert_eq!(groups, expected_groups);
                    // This also checks untouched state/clock bytes and the byte
                    // immediately following the last (possibly partial) group.
                    assert_eq!(memory, expected_memory);
                    groups.push(usize::MAX);
                    trace.take(&mut memory, &mut groups);
                    assert!(groups.is_empty());
                    assert_eq!(memory, expected_memory);
                }
            }
        }
    }

    #[test]
    fn aliases_partial_writes_and_separate_consumers() {
        let trace = TraceLayout::new(
            8192,
            8192,
            vec![(0, 512), (0, 64), (512, 520), (4096, 4104)],
        );
        let mut memory = vec![0; trace.end_offset()];
        // A partial write near the end of an array notifies its stable home.
        unsafe {
            trace.mark_range(memory.as_mut_ptr(), 500, 2);
            trace.mark_range(memory.as_mut_ptr(), 510, 4);
            trace.mark_home(memory.as_mut_ptr(), 4096);
        }
        let mut groups = vec![];
        trace.take(&mut memory, &mut groups);
        assert_eq!(groups, [0, 8, 64]);
        trace.take(&mut memory, &mut groups);
        assert!(groups.is_empty());
    }
}
