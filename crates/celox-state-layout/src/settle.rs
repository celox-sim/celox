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
    /// One byte per partition, then the untracked byte and the wait flags
    /// (`celox_design::settle_untracked_flag`, `settle_wait_flag`).
    pub flags_offset: usize,
    pub partitions: usize,
    /// Sorted by `start`, disjoint.
    pub objects: Vec<SettleObject>,
    /// Debug cross-check: the backends run each settle again with every
    /// partition flagged and panic where the results differ.
    #[serde(default)]
    pub check: bool,
    /// Flag bytes after the untracked byte: one per process, set when a
    /// signal its edge wait watches changes, cleared when the wait is
    /// checked.
    #[serde(default)]
    pub wait_flags: usize,
    /// Whether the flags end with run counters
    /// (`celox_design::settle_profile_offset`).
    #[serde(default)]
    pub profile: bool,
}

impl SettleLayout {
    pub fn new(flags_offset: usize, partitions: usize, mut objects: Vec<SettleObject>) -> Self {
        objects.sort_unstable_by_key(|object| object.start);
        Self {
            flags_offset,
            partitions,
            objects,
            check: false,
            wait_flags: 0,
            profile: false,
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

    /// The bytes of the flags: the untracked byte, the wait flags and the
    /// run counters included.
    pub fn flags(&self) -> std::ops::Range<usize> {
        let end = if self.profile {
            celox_design::settle_profile_offset(self.partitions, self.wait_flags)
                + celox_design::settle_profile_bytes(self.partitions)
        } else {
            celox_design::settle_flag_count(self.partitions, self.wait_flags)
        };
        self.flags_offset..self.flags_offset + end
    }

    /// The run counters of the settle when it counts its work, read from
    /// the state image `memory` ([`SettleProfile`]).
    pub fn profile(&self, memory: &[u8]) -> Option<SettleProfile> {
        if !self.profile {
            return None;
        }
        let start = self.flags_offset
            + celox_design::settle_profile_offset(self.partitions, self.wait_flags);
        let word = |index: usize| {
            let at = start + index * 8;
            Some(u64::from_le_bytes(memory.get(at..at + 8)?.try_into().ok()?))
        };
        Some(SettleProfile {
            runs: (0..self.partitions).map(word).collect::<Option<_>>()?,
            changed: (self.partitions..2 * self.partitions)
                .map(word)
                .collect::<Option<_>>()?,
            settles: word(2 * self.partitions)?,
        })
    }

    /// Run every partition on every settle from now on: a raw view of the
    /// state can change it without reporting.
    ///
    /// # Safety
    /// `memory` must point to the writable state image this layout describes.
    pub unsafe fn mark_untracked(&self, memory: *mut u8) {
        // SAFETY: the untracked byte follows the flags (caller contract).
        unsafe {
            *memory.add(self.flags_offset + celox_design::settle_untracked_flag(self.partitions)) =
                1
        };
    }
}

/// What a partitioned settle did, counted since the simulation started
/// ([`SettleLayout::profile`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettleProfile {
    /// The runs of each partition.
    pub runs: Vec<u64>,
    /// The runs of each partition in which something it compares after it
    /// ran changed. A partition comparing nothing (what it writes is read
    /// only outside the settle) counts every run.
    pub changed: Vec<u64>,
    /// The settles.
    pub settles: u64,
}

impl SettleProfile {
    /// The partitions' runs in all.
    pub fn total_runs(&self) -> u64 {
        self.runs.iter().sum()
    }

    /// The runs that changed nothing the partition compares: wasted, as
    /// far as the settle can tell.
    pub fn unchanged_runs(&self) -> u64 {
        self.runs
            .iter()
            .zip(&self.changed)
            .map(|(runs, changed)| runs - changed)
            .sum()
    }
}
