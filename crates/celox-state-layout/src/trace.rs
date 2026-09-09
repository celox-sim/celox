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
    /// Nonzero after a mutable raw view escapes. Such views can change without
    /// any generated notification, so observers must keep scanning all values.
    pub untracked_offset: usize,
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
            untracked_offset: offset + group_count + summary_count,
            homes: merged,
        }
    }

    pub fn end_offset(&self) -> usize {
        self.untracked_offset + 1
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
            && self.summary_offset.checked_add(self.summary_count) == Some(self.untracked_offset)
            && self.untracked_offset < scratch_start
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
    pub fn take(&self, memory: &mut [u8], groups: &mut Vec<usize>) -> bool {
        groups.clear();
        let tracked = memory[self.untracked_offset] == 0;
        for summary in 0..self.summary_count {
            if memory[self.summary_offset + summary] == 0 {
                continue;
            }
            memory[self.summary_offset + summary] = 0;
            let start = summary * GROUPS_PER_SUMMARY;
            let end = (start + GROUPS_PER_SUMMARY).min(self.group_count);
            for group in start..end {
                let flag = &mut memory[self.flags_offset + group];
                if *flag != 0 {
                    *flag = 0;
                    groups.push(group);
                }
            }
        }
        tracked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(trace.take(&mut memory, &mut groups));
        assert_eq!(groups, [0, 8, 64]);
        assert!(trace.take(&mut memory, &mut groups));
        assert!(groups.is_empty());
        memory[trace.untracked_offset] = 1;
        assert!(!trace.take(&mut memory, &mut groups));
        assert!(!trace.take(&mut memory, &mut groups));
    }
}
