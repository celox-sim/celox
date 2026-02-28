use crate::ir::BitAccess;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeStore<T> {
    /// key: lsb (absolute position)
    /// value: (expression, width, origin LSB when this data was originally placed)
    pub ranges: BTreeMap<usize, (T, usize, usize)>,
}

impl<T: Clone + PartialEq + Eq> RangeStore<T> {
    pub fn new(initial: T, width: usize) -> Self {
        let mut ranges = BTreeMap::new();
        if width > 0 {
            // In initial state, absolute position 0 and origin 0 match
            ranges.insert(0, (initial, width, 0));
        }
        Self { ranges }
    }

    /// Split the range at the specified bit position.
    /// Even if split, origin_lsb (the 3rd element) is maintained.
    pub fn split_at(&mut self, bit: usize) {
        if bit == 0 {
            return;
        }

        let mut split = None;
        if let Some((&lsb, (expr, width, origin))) = self.ranges.range(..bit).next_back() {
            let msb = lsb + width - 1;
            if bit > lsb && bit <= msb {
                // Left width: bit - lsb
                // Right width: msb - bit + 1
                // Both inherit the original origin
                split = Some((lsb, bit, expr.clone(), bit - lsb, msb - bit + 1, *origin));
            }
        }

        if let Some((lsb, bit, expr, left_w, right_w, origin)) = split {
            self.ranges.insert(lsb, (expr.clone(), left_w, origin));
            self.ranges.insert(bit, (expr, right_w, origin));
        }
    }

    /// Update the specified range with a new value.
    /// The origin_lsb of the updated range will match access.lsb of that assignment.
    pub fn update(&mut self, access: BitAccess, value: T) {
        self.split_at(access.lsb);
        self.split_at(access.msb + 1);

        let keys_to_remove: Vec<usize> = self
            .ranges
            .range(access.lsb..=access.msb)
            .map(|(&k, _)| k)
            .collect();
        for k in keys_to_remove {
            self.ranges.remove(&k);
        }

        // When inserting a new range, record access.lsb as the origin
        let width = access.msb - access.lsb + 1;
        self.ranges.insert(access.lsb, (value, width, access.lsb));
    }

    /// Returns parts overlapping with the requested range.
    /// relative_access will be the relative position from the origin of that expression.
    pub fn get_parts(&self, access: BitAccess) -> Vec<(T, BitAccess)> {
        let mut parts = Vec::new();
        for (&range_lsb, (expr, range_width, origin)) in self.ranges.range(..=access.msb) {
            let range_msb = range_lsb + range_width - 1;

            let overlap_lsb = range_lsb.max(access.lsb);
            let overlap_msb = range_msb.min(access.msb);

            if overlap_lsb <= overlap_msb {
                // By subtracting origin from absolute position (overlap),
                // calculate the correct relative index for the original data.
                let relative_access = BitAccess::new(overlap_lsb - origin, overlap_msb - origin);
                parts.push((expr.clone(), relative_access));
            }
        }
        parts
    }
}
