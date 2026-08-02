//! Register-allocation data types and target-independent allocation helpers.

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::hash::Hash;

/// A physical register that can be stored in a compact register set.
pub trait MachineRegister: Copy + Eq + Hash + Ord + fmt::Debug {
    /// Stable target-defined register number in the range `0..64`.
    fn index(self) -> u8;
}

/// Compact set for targets with at most 64 physical registers per class.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RegisterSet(u64);

impl RegisterSet {
    pub const fn new() -> Self {
        Self(0)
    }

    pub fn insert<R: MachineRegister>(&mut self, register: R) {
        self.0 |= register_bit(register);
    }

    pub fn remove<R: MachineRegister>(&mut self, register: R) {
        self.0 &= !register_bit(register);
    }

    pub fn contains<R: MachineRegister>(&self, register: &R) -> bool {
        self.0 & register_bit(*register) != 0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

fn register_bit<R: MachineRegister>(register: R) -> u64 {
    1_u64
        .checked_shl(u32::from(register.index()))
        .expect("physical register index must be below 64")
}

/// Constraint on one machine-instruction operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegConstraint<R> {
    Any,
    Fixed(R),
}

/// Physical location used while lowering SSA edge transfers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueLocation<R> {
    Register(R),
    Stack(i32),
    Immediate(u64),
}

/// Target constraints attached to one instruction after legalization.
#[derive(Debug, Clone)]
pub struct InstructionConstraints<V, R> {
    pub fixed_uses: Vec<(V, R)>,
    pub clobbers: Vec<R>,
}

impl<V, R> Default for InstructionConstraints<V, R> {
    fn default() -> Self {
        Self {
            fixed_uses: Vec::new(),
            clobbers: Vec::new(),
        }
    }
}

/// One inclusive live range in a linearized machine function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveRange<V> {
    pub value: V,
    pub start: u32,
    pub end: u32,
}

/// Register assignment returned by [`allocate_linear_scan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allocation<V: Eq + Hash, R> {
    assignments: HashMap<V, R>,
}

impl<V: Eq + Hash, R: Copy> Allocation<V, R> {
    pub fn get(&self, value: V) -> Option<R> {
        self.assignments.get(&value).copied()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&V, &R)> {
        self.assignments.iter()
    }
}

/// Failure from target-independent linear-scan allocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinearScanError<V> {
    EmptyRegisterFile,
    DuplicateValue(V),
    InvalidRange(LiveRange<V>),
    RegisterPressure { value: V, point: u32 },
}

impl<V: fmt::Debug> fmt::Display for LinearScanError<V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRegisterFile => formatter.write_str("target has no allocatable registers"),
            Self::DuplicateValue(value) => write!(formatter, "duplicate live range for {value:?}"),
            Self::InvalidRange(range) => write!(
                formatter,
                "invalid live range {:?}: {}..{}",
                range.value, range.start, range.end
            ),
            Self::RegisterPressure { value, point } => write!(
                formatter,
                "no register available for {value:?} at program point {point}"
            ),
        }
    }
}

impl<V: fmt::Debug> std::error::Error for LinearScanError<V> {}

/// Allocate non-spilling live ranges using a deterministic linear scan.
///
/// This is the bootstrap allocator for new native targets. The mature SSA
/// splitter remains available to x86 while its target hooks are separated;
/// both allocators share the physical-register and constraint model here.
pub fn allocate_linear_scan<V, R>(
    ranges: &[LiveRange<V>],
    allocatable: &[R],
) -> Result<Allocation<V, R>, LinearScanError<V>>
where
    V: Copy + Eq + Hash + Ord + fmt::Debug,
    R: MachineRegister,
{
    if allocatable.is_empty() {
        return Err(LinearScanError::EmptyRegisterFile);
    }

    let mut ordered = ranges.to_vec();
    ordered.sort_unstable_by_key(|range| (range.start, range.end, range.value));
    let mut seen = BTreeSet::new();
    for range in &ordered {
        if range.start > range.end {
            return Err(LinearScanError::InvalidRange(*range));
        }
        if !seen.insert(range.value) {
            return Err(LinearScanError::DuplicateValue(range.value));
        }
    }

    let mut active = Vec::<(u32, V, R)>::new();
    let mut assignments = HashMap::with_capacity(ordered.len());
    for range in ordered {
        active.retain(|(end, _, _)| *end >= range.start);
        let register = allocatable
            .iter()
            .copied()
            .find(|candidate| active.iter().all(|(_, _, used)| used != candidate))
            .ok_or(LinearScanError::RegisterPressure {
                value: range.value,
                point: range.start,
            })?;
        assignments.insert(range.value, register);
        active.push((range.end, range.value, register));
        active.sort_unstable_by_key(|(end, value, register)| (*end, *value, register.index()));
    }

    Ok(Allocation { assignments })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
    struct Reg(u8);

    impl MachineRegister for Reg {
        fn index(self) -> u8 {
            self.0
        }
    }

    #[test]
    fn register_set_supports_registers_above_x86s_range() {
        let mut set = RegisterSet::new();
        set.insert(Reg(30));
        assert!(set.contains(&Reg(30)));
        set.remove(Reg(30));
        assert!(set.is_empty());
    }

    #[test]
    fn linear_scan_reuses_register_after_last_use() {
        let ranges = [
            LiveRange {
                value: 0,
                start: 0,
                end: 1,
            },
            LiveRange {
                value: 1,
                start: 2,
                end: 3,
            },
        ];
        let allocation = allocate_linear_scan(&ranges, &[Reg(9)]).unwrap();
        assert_eq!(allocation.get(0), Some(Reg(9)));
        assert_eq!(allocation.get(1), Some(Reg(9)));
    }

    #[test]
    fn linear_scan_reports_pressure_without_hidden_spills() {
        let ranges = [
            LiveRange {
                value: 0,
                start: 0,
                end: 2,
            },
            LiveRange {
                value: 1,
                start: 1,
                end: 2,
            },
        ];
        assert_eq!(
            allocate_linear_scan(&ranges, &[Reg(9)]),
            Err(LinearScanError::RegisterPressure { value: 1, point: 1 })
        );
    }
}
