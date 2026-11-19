//! Cutting a partitioned settle into units at an instruction budget,
//! checked by running the settle in the SIR interpreter.

use super::tests::simple_path;
use super::work::UnitLimits;
use crate::HashMap;
use crate::{SLTNode, SLTNodeArena};
use celox_design::{BitAccess, VarAtomBase};
use celox_sir::interp::{InterpError, InterpMachine, ResolvedAccess, StoreSnapshot, execute_unit};
use celox_sir::{ExecutionUnit, SIROffset, SIRValue, TriggerIdWithKind};
use num_bigint::BigUint;
use num_traits::{One, Zero};

const FLAGS: u32 = 99;

/// Memory addressed by bit: each address holds one unbounded value, so a
/// load of a whole partition group's flag bytes sees the stores of each
/// flag byte.
#[derive(Clone, Default, Debug, PartialEq, Eq)]
struct Memory(HashMap<u32, BigUint>);

impl Memory {
    fn read(&self, addr: u32, offset: usize, bits: usize) -> BigUint {
        let value = self.0.get(&addr).cloned().unwrap_or_default();
        (value >> offset) & ((BigUint::one() << bits) - 1u8)
    }

    fn write(&mut self, addr: u32, offset: usize, bits: usize, value: &BigUint) {
        let mask = ((BigUint::one() << bits) - 1u8) << offset;
        let old = self.0.get(&addr).cloned().unwrap_or_default();
        let kept = &old ^ (&old & &mask);
        self.0.insert(addr, kept | ((value << offset) & mask));
    }
}

fn static_offset(access: &ResolvedAccess<'_>) -> Result<usize, InterpError> {
    match access.offset {
        SIROffset::Static(offset) => Ok(*offset),
        other => Err(InterpError::Machine(format!("dynamic offset {other}"))),
    }
}

impl InterpMachine<u32> for Memory {
    fn load(
        &mut self,
        addr: &u32,
        access: ResolvedAccess<'_>,
        bits: usize,
    ) -> Result<SIRValue, InterpError> {
        Ok(SIRValue::new(self.read(
            *addr,
            static_offset(&access)?,
            bits,
        )))
    }

    fn store(
        &mut self,
        addr: &u32,
        access: ResolvedAccess<'_>,
        bits: usize,
        value: &SIRValue,
    ) -> Result<(), InterpError> {
        self.write(*addr, static_offset(&access)?, bits, &value.payload);
        Ok(())
    }

    fn commit(
        &mut self,
        src: &u32,
        dst: &u32,
        access: ResolvedAccess<'_>,
        bits: usize,
    ) -> Result<(), InterpError> {
        let offset = static_offset(&access)?;
        let value = self.read(*src, offset, bits);
        self.write(*dst, offset, bits, &value);
        Ok(())
    }

    fn notify_triggers(
        &mut self,
        _addr: &u32,
        _access: ResolvedAccess<'_>,
        _bits: usize,
        _triggers: &[TriggerIdWithKind],
    ) -> Result<(), InterpError> {
        Ok(())
    }

    fn notify_trigger_only_store(
        &mut self,
        _addr: &u32,
        _triggers: &[TriggerIdWithKind],
    ) -> Result<(), InterpError> {
        Ok(())
    }

    fn capture_store_range(
        &mut self,
        _addr: &u32,
        _access: ResolvedAccess<'_>,
        _bits: usize,
    ) -> Result<StoreSnapshot, InterpError> {
        Ok(StoreSnapshot {
            value_words: Vec::new(),
            mask_words: Vec::new(),
        })
    }

    fn enable_comb_captures(
        &mut self,
        _addr: &u32,
        _access: ResolvedAccess<'_>,
        _bits: usize,
        _before: &StoreSnapshot,
        _sites: &[u32],
    ) -> Result<(), InterpError> {
        Ok(())
    }

    fn emit_runtime_event(&mut self, _site_id: u32, _args: &[SIRValue]) -> Result<(), InterpError> {
        Ok(())
    }

    fn emit_comb_capture_event(
        &mut self,
        _site_id: u32,
        _args: &[SIRValue],
        _fatal_error_code: Option<i64>,
        _consume_enabled: bool,
    ) -> Result<(), InterpError> {
        Ok(())
    }

    fn enable_comb_capture_if_changed(
        &mut self,
        _old: &SIRValue,
        _new: &SIRValue,
        _sites: &[u32],
    ) -> Result<(), InterpError> {
        Ok(())
    }
}

/// The units of a settle copying variable 10 along a chain of `length`
/// variables, one partition per copy, cut at `limits`.
fn chain_settle(length: u32, limits: Option<UnitLimits>) -> Vec<ExecutionUnit<u32>> {
    let mut arena = SLTNodeArena::new();
    let paths = (1..length)
        .map(|step| simple_path(&mut arena, 10 + step, Some(10 + step - 1)))
        .collect();
    let widths = (10..10 + length).map(|id| (id, 8)).collect();
    let result = super::sort_with_unit_limits(
        paths,
        &arena,
        &crate::HashSet::default(),
        &crate::HashMap::default(),
        false,
        &widths,
        &crate::HashMap::default(),
        1,
        Some(super::SettlePartitioning {
            flags: FLAGS,
            target_cost: 1,
            external_readers: crate::HashMap::default(),
            clock_nets: crate::HashSet::default(),
            profile: None,
        }),
        limits.unwrap_or(UnitLimits::DEFAULT),
    )
    .unwrap();
    assert_eq!(result.partitions.len(), length as usize - 1);
    result.execution_units
}

fn settle(units: &[ExecutionUnit<u32>], memory: &mut Memory) {
    for unit in units {
        execute_unit(unit, memory, &[], false).unwrap();
    }
}

/// Units cut at a budget, also within the groups of eight partitions that
/// share a flag test, run the settle the uncut unit runs: a full settle,
/// and one that only the input's reader is flagged for, which reaches the
/// end of the chain through the flags its partitions set.
#[test]
fn settles_cut_at_an_instruction_budget_run_like_the_uncut_settle() {
    let length = 24;
    let partitions = length as usize - 1;
    let whole = chain_settle(length, None);
    assert_eq!(whole.len(), 1);
    let cut = chain_settle(
        length,
        Some(UnitLimits {
            blocks: usize::MAX,
            instructions: 20,
        }),
    );
    // Three groups of eight partitions would give at most four units if
    // groups were never cut.
    assert!(cut.len() > 4, "{} units", cut.len());
    let runs = |units: &[ExecutionUnit<u32>]| {
        let mut memory = Memory::default();
        memory.write(10, 0, 8, &BigUint::from(0x5au8));
        memory.write(
            FLAGS,
            celox_design::settle_untracked_flag(partitions) * 8,
            8,
            &BigUint::one(),
        );
        settle(units, &mut memory);
        let full = memory.clone();
        memory.write(
            FLAGS,
            celox_design::settle_untracked_flag(partitions) * 8,
            8,
            &BigUint::zero(),
        );
        memory.write(10, 0, 8, &BigUint::from(0x33u8));
        memory.write(FLAGS, 0, 8, &BigUint::one());
        settle(units, &mut memory);
        (full, memory)
    };
    let (whole_full, whole_woken) = runs(&whole);
    let (cut_full, cut_woken) = runs(&cut);
    assert_eq!(cut_full, whole_full);
    assert_eq!(cut_woken, whole_woken);
    for variable in 10..10 + length {
        assert_eq!(whole_full.read(variable, 0, 8), BigUint::from(0x5au8));
        assert_eq!(whole_woken.read(variable, 0, 8), BigUint::from(0x33u8));
    }
    // Every partition's flag is clear after the settles.
    assert!(whole_woken.read(FLAGS, 0, partitions * 8).is_zero());
}

/// The units of a settle without partitions of `regions` guarded regions:
/// twelve paths each, choosing between two constants on condition `100 + r`.
fn guarded_settle(regions: u32, limits: Option<UnitLimits>) -> Vec<ExecutionUnit<u32>> {
    let mut arena = SLTNodeArena::new();
    let mut paths = Vec::new();
    for region in 0..regions {
        let condition = arena
            .alloc(SLTNode::Input {
                variable: 100 + region,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(0, 0),
            })
            .unwrap();
        for step in 0..12 {
            let target = region * 12 + step;
            let constant = |arena: &mut SLTNodeArena<u32>, value: u32| {
                arena
                    .alloc(SLTNode::Constant(
                        BigUint::from(value),
                        BigUint::from(0u8),
                        8,
                        false,
                    ))
                    .unwrap()
            };
            let then_expr = constant(&mut arena, target);
            let else_expr = constant(&mut arena, target + 128);
            let expr = arena
                .alloc(SLTNode::Mux {
                    cond: condition,
                    then_expr,
                    else_expr,
                })
                .unwrap();
            let mut path = simple_path(&mut arena, target, None);
            path.expr = expr;
            path.sources = [VarAtomBase::new(100 + region, 0, 0)].into_iter().collect();
            paths.push(path);
        }
    }
    let result = super::sort_with_unit_limits(
        paths,
        &arena,
        &crate::HashSet::default(),
        &crate::HashMap::default(),
        false,
        &crate::HashMap::default(),
        &crate::HashMap::default(),
        1,
        None,
        limits.unwrap_or(UnitLimits::DEFAULT),
    )
    .unwrap();
    result.execution_units
}

/// A settle of guarded regions is cut between them at a budget, each region
/// kept whole, and runs like the uncut settle.
#[test]
fn settles_of_guarded_regions_are_cut_between_them() {
    let regions = 4;
    let whole = guarded_settle(regions, None);
    assert_eq!(whole.len(), 1);
    let cut = guarded_settle(
        regions,
        Some(UnitLimits {
            blocks: usize::MAX,
            instructions: 20,
        }),
    );
    assert!(cut.len() >= regions as usize, "{} units", cut.len());
    // Each region is one guard: a unit holds whole regions only.
    for unit in &cut {
        let branches = unit
            .blocks
            .values()
            .filter(|block| matches!(block.terminator, celox_sir::SIRTerminator::Branch { .. }))
            .count();
        assert!(branches <= 1, "{branches} guards in one unit");
    }
    for conditions in [0b0000u32, 0b1010, 0b0111, 0b1111] {
        let run = |units: &[ExecutionUnit<u32>]| {
            let mut memory = Memory::default();
            for region in 0..regions {
                memory.write(
                    100 + region,
                    0,
                    1,
                    &BigUint::from((conditions >> region) & 1),
                );
            }
            settle(units, &mut memory);
            memory
        };
        let whole = run(&whole);
        assert_eq!(run(&cut), whole, "conditions {conditions:04b}");
        for target in 0..regions * 12 {
            let chosen = if (conditions >> (target / 12)) & 1 == 1 {
                target
            } else {
                target + 128
            };
            assert_eq!(
                whole.read(target, 0, 8),
                BigUint::from(chosen),
                "target {target}"
            );
        }
    }
}
