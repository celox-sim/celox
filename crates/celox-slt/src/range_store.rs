use celox_design::BitAccess;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeStoreError {
    message: String,
}

impl RangeStoreError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for RangeStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}

impl std::error::Error for RangeStoreError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(bound(serialize = "T: Serialize", deserialize = "T: Deserialize<'de>"))]
pub struct RangeStore<T> {
    /// key: lsb (absolute position)
    /// value: (expression, width, origin LSB when this data was originally placed)
    pub ranges: BTreeMap<usize, (T, usize, usize)>,
}

pub type AlignedRangePart<'a, T, U> = (BitAccess, (&'a T, BitAccess), (&'a U, BitAccess));

impl<T> RangeStore<T> {
    fn total_width(&self) -> Result<usize, RangeStoreError> {
        let Some((&last_lsb, (_, last_width, _))) = self.ranges.last_key_value() else {
            return Ok(0);
        };
        if *last_width == 0 {
            return Err(RangeStoreError::new(
                "range store contains a zero-width terminal range",
            ));
        }
        last_lsb
            .checked_add(*last_width)
            .ok_or_else(|| RangeStoreError::new("range store total width overflows usize"))
    }

    /// Align two sparse partitions in one ordered pass.
    ///
    /// Each returned absolute range is bounded by the next boundary from
    /// either store. The accompanying accesses are relative to each value's
    /// original placement, so callers can slice the values without searching
    /// either map again.
    pub fn aligned_parts<'a, U>(
        &'a self,
        other: &'a RangeStore<U>,
    ) -> Result<Vec<AlignedRangePart<'a, T, U>>, RangeStoreError> {
        let total_width = self.total_width()?;
        let other_width = other.total_width()?;
        if total_width != other_width {
            return Err(RangeStoreError::new(format!(
                "range store widths differ: {total_width} and {other_width}"
            )));
        }
        if total_width == 0 {
            return Ok(Vec::new());
        }

        let mut left_ranges = self.ranges.iter().peekable();
        let mut right_ranges = other.ranges.iter().peekable();
        let mut left = left_ranges
            .next()
            .ok_or_else(|| RangeStoreError::new("left range store is empty"))?;
        let mut right = right_ranges
            .next()
            .ok_or_else(|| RangeStoreError::new("right range store is empty"))?;
        if *left.0 != 0 || *right.0 != 0 {
            return Err(RangeStoreError::new(
                "range store does not begin at bit zero",
            ));
        }

        let mut parts = Vec::with_capacity(self.ranges.len() + other.ranges.len());
        let mut lsb = 0;
        while lsb < total_width {
            let left_boundary = left_ranges
                .peek()
                .map(|(next_lsb, _)| **next_lsb)
                .unwrap_or(total_width);
            let right_boundary = right_ranges
                .peek()
                .map(|(next_lsb, _)| **next_lsb)
                .unwrap_or(total_width);
            validate_range_boundary(left, left_boundary, total_width)?;
            validate_range_boundary(right, right_boundary, total_width)?;

            let next_lsb = left_boundary.min(right_boundary);
            if next_lsb <= lsb {
                return Err(RangeStoreError::new(
                    "range store boundaries are not strictly ordered",
                ));
            }
            let msb = next_lsb - 1;
            let left_access = relative_access(left.1.2, lsb, msb)?;
            let right_access = relative_access(right.1.2, lsb, msb)?;
            parts.push((
                BitAccess::new(lsb, msb),
                (&left.1.0, left_access),
                (&right.1.0, right_access),
            ));

            lsb = next_lsb;
            if left_boundary == lsb && lsb < total_width {
                left = left_ranges
                    .next()
                    .expect("a peeked left range boundary exists");
            }
            if right_boundary == lsb && lsb < total_width {
                right = right_ranges
                    .next()
                    .expect("a peeked right range boundary exists");
            }
        }
        Ok(parts)
    }
}

fn validate_range_boundary<T>(
    current: (&usize, &(T, usize, usize)),
    next_lsb: usize,
    total_width: usize,
) -> Result<(), RangeStoreError> {
    let (lsb, (_, width, _)) = current;
    if *width == 0 {
        return Err(RangeStoreError::new(format!(
            "range at bit {lsb} has zero width"
        )));
    }
    let end = lsb
        .checked_add(*width)
        .ok_or_else(|| RangeStoreError::new("range end overflows usize"))?;
    if end != next_lsb || end > total_width {
        return Err(RangeStoreError::new(format!(
            "range at bit {lsb} does not end at the next boundary {next_lsb}"
        )));
    }
    Ok(())
}

fn relative_access(
    origin: usize,
    absolute_lsb: usize,
    absolute_msb: usize,
) -> Result<BitAccess, RangeStoreError> {
    let lsb = absolute_lsb
        .checked_sub(origin)
        .ok_or_else(|| RangeStoreError::new("range origin is above its aligned LSB"))?;
    let msb = absolute_msb
        .checked_sub(origin)
        .ok_or_else(|| RangeStoreError::new("range origin is above its aligned MSB"))?;
    Ok(BitAccess::new(lsb, msb))
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

    fn validate_access(&self, access: BitAccess) -> Result<usize, RangeStoreError> {
        let width = access
            .msb
            .checked_sub(access.lsb)
            .and_then(|span| span.checked_add(1))
            .ok_or_else(|| {
                RangeStoreError::new(format!(
                    "range access [{}:{}] is malformed",
                    access.msb, access.lsb
                ))
            })?;
        let total_width = self.total_width()?;
        if total_width == 0 || access.msb >= total_width {
            return Err(RangeStoreError::new(format!(
                "range access [{}:{}] is outside store width {total_width}",
                access.msb, access.lsb
            )));
        }
        Ok(width)
    }

    /// Split the range at the specified bit position.
    /// Even if split, origin_lsb (the 3rd element) is maintained.
    pub fn split_at(&mut self, bit: usize) -> Result<(), RangeStoreError> {
        if bit == 0 {
            return Ok(());
        }
        let total_width = self.total_width()?;
        if bit > total_width {
            return Err(RangeStoreError::new(format!(
                "split position {bit} is outside store width {total_width}"
            )));
        }

        let mut split = None;
        if let Some((&lsb, (expr, width, origin))) = self.ranges.range(..bit).next_back() {
            if *width == 0 {
                return Err(RangeStoreError::new(format!(
                    "range at bit {lsb} has zero width"
                )));
            }
            let msb = lsb
                .checked_add(*width - 1)
                .ok_or_else(|| RangeStoreError::new("range end overflows usize"))?;
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
        Ok(())
    }

    /// Update the specified range with a new value.
    /// The origin_lsb of the updated range will match access.lsb of that assignment.
    pub fn update(&mut self, access: BitAccess, value: T) -> Result<(), RangeStoreError> {
        let width = self.validate_access(access)?;
        let end = access
            .msb
            .checked_add(1)
            .ok_or_else(|| RangeStoreError::new("updated range end overflows usize"))?;
        self.split_at(access.lsb)?;
        self.split_at(end)?;

        let keys_to_remove: Vec<usize> = self
            .ranges
            .range(access.lsb..=access.msb)
            .map(|(&k, _)| k)
            .collect();
        for k in keys_to_remove {
            self.ranges.remove(&k);
        }

        // When inserting a new range, record access.lsb as the origin
        self.ranges.insert(access.lsb, (value, width, access.lsb));
        Ok(())
    }

    /// Returns borrowed parts overlapping with the requested range.
    /// relative_access will be the relative position from the origin of that expression.
    pub fn get_parts_ref(
        &self,
        access: BitAccess,
    ) -> Result<Vec<(&T, BitAccess)>, RangeStoreError> {
        self.validate_access(access)?;
        let mut parts = Vec::new();
        let first_lsb = self
            .ranges
            .range(..=access.lsb)
            .next_back()
            .map(|(&lsb, _)| lsb)
            .ok_or_else(|| RangeStoreError::new("range store does not cover access LSB"))?;
        for (&range_lsb, (expr, range_width, origin)) in self.ranges.range(first_lsb..=access.msb) {
            if *range_width == 0 {
                return Err(RangeStoreError::new(format!(
                    "range at bit {range_lsb} has zero width"
                )));
            }
            let range_msb = range_lsb
                .checked_add(*range_width - 1)
                .ok_or_else(|| RangeStoreError::new("range end overflows usize"))?;

            let overlap_lsb = range_lsb.max(access.lsb);
            let overlap_msb = range_msb.min(access.msb);

            if overlap_lsb <= overlap_msb {
                // By subtracting origin from absolute position (overlap),
                // calculate the correct relative index for the original data.
                let relative_lsb = overlap_lsb.checked_sub(*origin).ok_or_else(|| {
                    RangeStoreError::new("range origin is above its overlapping LSB")
                })?;
                let relative_msb = overlap_msb.checked_sub(*origin).ok_or_else(|| {
                    RangeStoreError::new("range origin is above its overlapping MSB")
                })?;
                let relative_access = BitAccess::new(relative_lsb, relative_msb);
                parts.push((expr, relative_access));
            }
        }
        Ok(parts)
    }

    /// Returns owned parts overlapping with the requested range.
    pub fn get_parts(&self, access: BitAccess) -> Result<Vec<(T, BitAccess)>, RangeStoreError> {
        self.get_parts_ref(access).map(|parts| {
            parts
                .into_iter()
                .map(|(value, access)| (value.clone(), access))
                .collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_malformed_and_out_of_bounds_accesses_without_panicking() {
        let mut store = RangeStore::new(0u8, 8);
        let original = store.clone();
        assert!(store.update(BitAccess { lsb: 7, msb: 6 }, 1).is_err());
        assert_eq!(store, original);
        assert!(store.update(BitAccess::new(7, 8), 1).is_err());
        assert_eq!(store, original);
        assert!(store.get_parts(BitAccess::new(0, 8)).is_err());
        assert!(store.split_at(9).is_err());
        assert_eq!(store, original);
    }

    #[test]
    fn checked_split_update_and_read_preserve_ranges() {
        let mut store = RangeStore::new(0u8, 8);
        store.update(BitAccess::new(2, 5), 1).unwrap();
        assert_eq!(
            store.get_parts(BitAccess::new(1, 6)).unwrap(),
            vec![
                (0, BitAccess::new(1, 1)),
                (1, BitAccess::new(0, 3)),
                (0, BitAccess::new(6, 6)),
            ]
        );
    }

    #[test]
    fn reads_from_the_range_containing_the_access_lsb() {
        let mut store = RangeStore::new(0u8, 64);
        for bit in 0..64 {
            store.update(BitAccess::new(bit, bit), bit as u8).unwrap();
        }
        assert_eq!(
            store.get_parts(BitAccess::new(60, 62)).unwrap(),
            vec![
                (60, BitAccess::new(0, 0)),
                (61, BitAccess::new(0, 0)),
                (62, BitAccess::new(0, 0)),
            ]
        );
    }

    #[test]
    fn aligns_two_sparse_partitions_in_boundary_order() {
        let mut left = RangeStore::new(0u8, 8);
        left.update(BitAccess::new(2, 5), 1).unwrap();
        let mut right = RangeStore::new(0u8, 8);
        right.update(BitAccess::new(4, 7), 2).unwrap();

        let parts = left
            .aligned_parts(&right)
            .unwrap()
            .into_iter()
            .map(|(absolute, (left, left_access), (right, right_access))| {
                (absolute, (*left, left_access), (*right, right_access))
            })
            .collect::<Vec<_>>();
        assert_eq!(
            parts,
            vec![
                (
                    BitAccess::new(0, 1),
                    (0, BitAccess::new(0, 1)),
                    (0, BitAccess::new(0, 1)),
                ),
                (
                    BitAccess::new(2, 3),
                    (1, BitAccess::new(0, 1)),
                    (0, BitAccess::new(2, 3)),
                ),
                (
                    BitAccess::new(4, 5),
                    (1, BitAccess::new(2, 3)),
                    (2, BitAccess::new(0, 1)),
                ),
                (
                    BitAccess::new(6, 7),
                    (0, BitAccess::new(6, 7)),
                    (2, BitAccess::new(2, 3)),
                ),
            ]
        );
    }

    #[test]
    fn aligning_partitions_rejects_different_widths() {
        let left = RangeStore::new(0u8, 8);
        let right = RangeStore::new(0u8, 9);

        assert!(left.aligned_parts(&right).is_err());
    }
}
