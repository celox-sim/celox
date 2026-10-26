//! Dirty flags of a partitioned combinational settle.
//!
//! Generated code sets the flags of the partitions reading the state it
//! writes; host writes go through [`SettleLayout::mark_range`]. Writes no
//! one reports, through a retained raw view of the state, set the untracked
//! byte, after which every partition runs on every settle.
use serde::{Deserialize, Serialize};

/// State written outside generated code and the partitions reading it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettleObject {
    /// Byte range of the object, both planes.
    pub start: usize,
    pub end: usize,
    pub partitions: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettleLayout {
    /// One byte per partition, then the untracked byte.
    pub flags_offset: usize,
    pub partitions: usize,
    /// Sorted by `start`, disjoint.
    pub objects: Vec<SettleObject>,
    /// Debug cross-check: the backends run each settle again with every
    /// partition flagged and panic where the results differ.
    #[serde(default)]
    pub check: bool,
}

impl SettleLayout {
    pub fn new(flags_offset: usize, partitions: usize, mut objects: Vec<SettleObject>) -> Self {
        objects.sort_unstable_by_key(|object| object.start);
        Self {
            flags_offset,
            partitions,
            objects,
            check: false,
        }
    }

    /// Set the flags of the partitions reading state in `offset..offset + len`.
    ///
    /// # Safety
    /// `memory` must point to the writable state image this layout describes.
    pub unsafe fn mark_range(&self, memory: *mut u8, offset: usize, len: usize) {
        let end = offset + len.max(1);
        let first = self.objects.partition_point(|object| object.end <= offset);
        for object in self.objects[first..]
            .iter()
            .take_while(|object| object.start < end)
        {
            for &partition in &object.partitions {
                // SAFETY: flags lie inside the image (caller contract).
                unsafe { *memory.add(self.flags_offset + partition as usize) = 1 };
            }
        }
    }

    /// The bytes of the flags, the untracked byte included.
    pub fn flags(&self) -> std::ops::Range<usize> {
        self.flags_offset..self.flags_offset + self.partitions + 1
    }

    /// Run every partition on every settle from now on: a raw view of the
    /// state can change it without reporting.
    ///
    /// # Safety
    /// `memory` must point to the writable state image this layout describes.
    pub unsafe fn mark_untracked(&self, memory: *mut u8) {
        // SAFETY: the untracked byte follows the flags (caller contract).
        unsafe { *memory.add(self.flags_offset + self.partitions) = 1 };
    }
}
