//! Worklist state shared by the production greedy register allocator.
//!
//! This follows LLVM RAGreedy's `ExtraRegInfo` contract: allocation stage is
//! attached to a live interval, while a monotonically increasing cascade
//! prevents cyclic eviction. Split and spill policy lives outside this file;
//! neither the queue nor the matrix is allowed to turn a child into a final
//! memory value.

use crate::backend::native::mir::VReg;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum LiveRangeStage {
    #[default]
    New,
    Assign,
    Split,
    Split2,
    Spill,
    Done,
}

impl LiveRangeStage {
    #[cfg(test)]
    pub(super) fn is_primary(self) -> bool {
        matches!(self, Self::Assign | Self::Done)
    }

    pub(super) fn may_split(self) -> bool {
        self < Self::Spill
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct LiveRangeInfo {
    stage: LiveRangeStage,
    cascade: u32,
}

/// Per-VReg allocation state. VReg identities are monotonic, so rows are never
/// recycled during one allocation session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GreedyLiveRanges {
    rows: Vec<LiveRangeInfo>,
    next_cascade: u32,
}

impl GreedyLiveRanges {
    pub(super) fn new(value_count: u32) -> Self {
        Self {
            rows: vec![LiveRangeInfo::default(); value_count as usize],
            next_cascade: 1,
        }
    }

    pub(super) fn grow(&mut self, value_count: u32) {
        self.rows
            .resize(value_count as usize, LiveRangeInfo::default());
    }

    fn row(&self, value: VReg) -> &LiveRangeInfo {
        &self.rows[value.0 as usize]
    }

    fn row_mut(&mut self, value: VReg) -> &mut LiveRangeInfo {
        &mut self.rows[value.0 as usize]
    }

    pub(super) fn stage(&self, value: VReg) -> LiveRangeStage {
        self.row(value).stage
    }

    /// Enqueueing is the only `New -> Assign` transition. Split children are
    /// born `New`, then compete in the same primary queue as original ranges.
    pub(super) fn on_enqueue(&mut self, value: VReg) -> LiveRangeStage {
        let row = self.row_mut(value);
        if row.stage == LiveRangeStage::New {
            row.stage = LiveRangeStage::Assign;
        }
        row.stage
    }

    pub(super) fn defer_for_split(&mut self, value: VReg) -> bool {
        let row = self.row_mut(value);
        if row.stage != LiveRangeStage::Assign {
            return false;
        }
        row.stage = LiveRangeStage::Split;
        true
    }

    pub(super) fn require_split_progress(&mut self, value: VReg) {
        let row = self.row_mut(value);
        if row.stage < LiveRangeStage::Split2 {
            row.stage = LiveRangeStage::Split2;
        }
    }

    #[cfg(test)]
    pub(super) fn require_spill(&mut self, value: VReg) {
        let row = self.row_mut(value);
        if row.stage < LiveRangeStage::Spill {
            row.stage = LiveRangeStage::Spill;
        }
    }

    pub(super) fn mark_done(&mut self, value: VReg) {
        self.row_mut(value).stage = LiveRangeStage::Done;
    }

    /// A newly created interval has no inherited allocation stage or cascade.
    pub(super) fn reset_new(&mut self, value: VReg) {
        *self.row_mut(value) = LiveRangeInfo::default();
    }

    pub(super) fn cascade(&self, value: VReg) -> u32 {
        self.row(value).cascade
    }

    /// An interval without a cascade behaves as if it owned the next cascade
    /// for eligibility checks. This lets a fresh interval evict an older
    /// cascade without consuming a number until an eviction is committed.
    pub(super) fn effective_cascade(&self, value: VReg) -> u32 {
        match self.cascade(value) {
            0 => self.next_cascade,
            cascade => cascade,
        }
    }

    pub(super) fn may_evict(&self, candidate: VReg, victim: VReg) -> bool {
        let candidate = self.effective_cascade(candidate);
        let victim = self.cascade(victim);
        victim == 0 || candidate > victim
    }

    /// Commit one eviction cascade and return the number that every victim
    /// must inherit. Existing cascade numbers are never decreased.
    pub(super) fn begin_eviction(&mut self, candidate: VReg) -> Option<u32> {
        let existing = self.cascade(candidate);
        if existing != 0 {
            return Some(existing);
        }
        let cascade = self.next_cascade;
        self.next_cascade = self.next_cascade.checked_add(1)?;
        self.row_mut(candidate).cascade = cascade;
        Some(cascade)
    }

    pub(super) fn inherit_eviction(&mut self, victim: VReg, cascade: u32) -> bool {
        let row = self.row_mut(victim);
        if row.cascade >= cascade {
            return false;
        }
        row.cascade = cascade;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enqueue_promotes_only_new_intervals() {
        let mut ranges = GreedyLiveRanges::new(2);
        assert_eq!(ranges.on_enqueue(VReg(0)), LiveRangeStage::Assign);
        assert!(ranges.defer_for_split(VReg(0)));
        assert_eq!(ranges.on_enqueue(VReg(0)), LiveRangeStage::Split);
        assert_eq!(ranges.on_enqueue(VReg(1)), LiveRangeStage::Assign);
    }

    #[test]
    fn split_children_reenter_the_primary_stage() {
        let mut ranges = GreedyLiveRanges::new(2);
        ranges.mark_done(VReg(0));
        ranges.reset_new(VReg(1));
        assert_eq!(ranges.stage(VReg(1)), LiveRangeStage::New);
        assert_eq!(ranges.on_enqueue(VReg(1)), LiveRangeStage::Assign);
    }

    #[test]
    fn repeated_split_and_spill_transitions_are_monotonic() {
        let mut ranges = GreedyLiveRanges::new(1);
        ranges.on_enqueue(VReg(0));
        ranges.require_split_progress(VReg(0));
        assert_eq!(ranges.stage(VReg(0)), LiveRangeStage::Split2);
        ranges.require_spill(VReg(0));
        assert_eq!(ranges.stage(VReg(0)), LiveRangeStage::Spill);
        ranges.require_split_progress(VReg(0));
        assert_eq!(ranges.stage(VReg(0)), LiveRangeStage::Spill);
    }

    #[test]
    fn one_eviction_cascade_cannot_immediately_reverse() {
        let mut ranges = GreedyLiveRanges::new(2);
        assert!(ranges.may_evict(VReg(0), VReg(1)));
        let cascade = ranges.begin_eviction(VReg(0)).unwrap();
        assert!(ranges.inherit_eviction(VReg(1), cascade));
        assert!(!ranges.may_evict(VReg(1), VReg(0)));
    }

    #[test]
    fn a_newer_cascade_can_displace_an_older_one() {
        let mut ranges = GreedyLiveRanges::new(3);
        let first = ranges.begin_eviction(VReg(0)).unwrap();
        assert!(ranges.inherit_eviction(VReg(1), first));
        let second = ranges.begin_eviction(VReg(2)).unwrap();
        assert!(second > first);
        assert!(ranges.may_evict(VReg(2), VReg(0)));
    }

    #[test]
    fn spill_products_are_done_but_still_enter_the_assignment_queue() {
        let mut ranges = GreedyLiveRanges::new(1);
        ranges.mark_done(VReg(0));
        assert_eq!(ranges.on_enqueue(VReg(0)), LiveRangeStage::Done);
        assert!(ranges.stage(VReg(0)).is_primary());
        assert!(!ranges.stage(VReg(0)).may_split());
    }
}
