use crate::ir::BitAccess;
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

    /// Returns parts overlapping with the requested range.
    /// relative_access will be the relative position from the origin of that expression.
    pub fn get_parts(&self, access: BitAccess) -> Result<Vec<(T, BitAccess)>, RangeStoreError> {
        self.validate_access(access)?;
        let mut parts = Vec::new();
        for (&range_lsb, (expr, range_width, origin)) in self.ranges.range(..=access.msb) {
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
                parts.push((expr.clone(), relative_access));
            }
        }
        Ok(parts)
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
}
