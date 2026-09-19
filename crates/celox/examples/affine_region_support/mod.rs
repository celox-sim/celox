//! Logical bit-addressed oracle and native state transfer for region experiments.

use celox::{InterpError, InterpMachine, ResolvedAccess, StoreSnapshot};
use celox_design::RegionedStateAddr;
use celox_sir::{SIROffset, SIRValue, TriggerIdWithKind, affine::MemoryObject};
use fxhash::FxHashMap as HashMap;
use num_bigint::BigUint;
use num_traits::{One, ToPrimitive, Zero};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Machine {
    pub values: HashMap<RegionedStateAddr, SIRValue>,
    pub observations: Vec<(String, Vec<SIRValue>)>,
}

pub fn mask(bits: usize) -> BigUint {
    (BigUint::one() << bits) - BigUint::one()
}

/// Re-enter the complete ordinary SIR pipeline after replacing one clock unit.
/// Keeping the other phases supplies the same whole-program memory facts as
/// baseline compilation. This prototype pays for that complete optimization
/// again; benchmarks must include it in candidate compilation cost.
#[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
pub fn optimize_clock_candidate(
    pre: &celox::UnoptimizedSir,
    baseline: &celox::OptimizedSir,
    clock: celox_design::StateAddr,
    candidate: celox_sir::ExecutionUnit<RegionedStateAddr>,
    four_state: bool,
) -> celox_sir::ExecutionUnit<RegionedStateAddr> {
    let mut program = pre.clone();
    assert_eq!(program.sir.eval_apply_ffs[&clock].len(), 1);
    program.sir.eval_apply_ffs.get_mut(&clock).unwrap()[0] = candidate;
    celox_sir_opt::optimize(
        &mut celox_sir_opt::OptimizationContext {
            sir: &mut program.sir,
            design: &program.runtime.design,
            runtime_schema: &program.runtime.runtime_schema,
            layout_requirements: &mut program.layout_requirements,
        },
        four_state,
        &celox::OptimizeOptions::all(),
        false,
    );
    assert_eq!(
        program.layout_requirements.state_aliases(),
        baseline.layout_requirements.state_aliases(),
        "candidate changed layout requirements"
    );
    let mut units = program.sir.eval_apply_ffs.remove(&clock).unwrap();
    assert_eq!(units.len(), 1);
    let unit = units.pop().unwrap();
    unit.verify();
    unit
}

fn bit_offset(access: ResolvedAccess<'_>) -> usize {
    let dynamic = |i: usize| access.dynamics[i].unwrap().payload.to_usize().unwrap();
    match access.offset {
        SIROffset::Static(bit)
        | SIROffset::PackedElements {
            bit_offset: bit, ..
        } => *bit,
        SIROffset::Dynamic(_) => dynamic(0),
        SIROffset::Element {
            element_width,
            bit_offset,
            dynamic_bit_offset,
            ..
        } => {
            dynamic(0) * element_width
                + bit_offset
                + if dynamic_bit_offset.is_some() {
                    dynamic(1)
                } else {
                    0
                }
        }
    }
}

impl Machine {
    #[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
    pub fn restricted(&self, objects: &HashMap<RegionedStateAddr, MemoryObject>) -> Self {
        Self {
            values: self
                .values
                .iter()
                .filter(|(a, _)| objects.contains_key(a))
                .map(|(a, v)| (*a, v.clone()))
                .collect(),
            observations: vec![],
        }
    }

    pub fn new(
        objects: &HashMap<RegionedStateAddr, MemoryObject>,
        seed: u64,
        unknown: bool,
    ) -> Self {
        let mut addresses = objects.keys().copied().collect::<Vec<_>>();
        addresses.sort_unstable();
        let mut random = seed;
        let mut next = || {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            random as u32
        };
        let values = addresses
            .into_iter()
            .map(|a| {
                let width = objects[&a].element_width * objects[&a].elements;
                let payload = BigUint::new((0..width.div_ceil(32)).map(|_| next()).collect());
                let unknowns = if unknown {
                    BigUint::new((0..width.div_ceil(32)).map(|_| next()).collect())
                } else {
                    BigUint::zero()
                };
                (
                    a,
                    SIRValue::new_four_state(payload & mask(width), unknowns & mask(width)),
                )
            })
            .collect();
        Self {
            values,
            observations: vec![],
        }
    }
}

impl InterpMachine<RegionedStateAddr> for Machine {
    fn load(
        &mut self,
        a: &RegionedStateAddr,
        at: ResolvedAccess<'_>,
        bits: usize,
    ) -> Result<SIRValue, InterpError> {
        let value = &self.values[a];
        let bit = bit_offset(at);
        Ok(SIRValue::new_four_state(
            (&value.payload >> bit) & mask(bits),
            (&value.mask >> bit) & mask(bits),
        ))
    }
    fn store(
        &mut self,
        a: &RegionedStateAddr,
        at: ResolvedAccess<'_>,
        bits: usize,
        v: &SIRValue,
    ) -> Result<(), InterpError> {
        let value = self.values.get_mut(a).unwrap();
        let bit = bit_offset(at);
        let range = mask(bits) << bit;
        value.payload ^= &value.payload & &range;
        value.mask ^= &value.mask & &range;
        value.payload |= (&v.payload & mask(bits)) << bit;
        value.mask |= (&v.mask & mask(bits)) << bit;
        Ok(())
    }
    fn commit(
        &mut self,
        src: &RegionedStateAddr,
        dst: &RegionedStateAddr,
        at: ResolvedAccess<'_>,
        bits: usize,
    ) -> Result<(), InterpError> {
        let value = self.load(src, at, bits)?;
        self.store(dst, at, bits, &value)?;
        self.observations.push((
            format!("commit {src:?} {dst:?} {} {bits}", bit_offset(at)),
            vec![value],
        ));
        Ok(())
    }
    fn notify_triggers(
        &mut self,
        a: &RegionedStateAddr,
        at: ResolvedAccess<'_>,
        bits: usize,
        triggers: &[TriggerIdWithKind],
    ) -> Result<(), InterpError> {
        let value = self.load(a, at, bits)?;
        self.observations.push((
            format!("triggers {a:?} {} {bits} {triggers:?}", bit_offset(at)),
            vec![value],
        ));
        Ok(())
    }
    fn notify_trigger_only_store(
        &mut self,
        a: &RegionedStateAddr,
        triggers: &[TriggerIdWithKind],
    ) -> Result<(), InterpError> {
        self.observations
            .push((format!("trigger-only {a:?} {triggers:?}"), vec![]));
        Ok(())
    }
    fn capture_store_range(
        &mut self,
        a: &RegionedStateAddr,
        at: ResolvedAccess<'_>,
        bits: usize,
    ) -> Result<StoreSnapshot, InterpError> {
        let before = self.load(a, at, bits)?;
        Ok(StoreSnapshot {
            value_words: before.payload.to_u64_digits(),
            mask_words: before.mask.to_u64_digits(),
        })
    }
    fn enable_comb_captures(
        &mut self,
        a: &RegionedStateAddr,
        at: ResolvedAccess<'_>,
        bits: usize,
        before: &StoreSnapshot,
        sites: &[u32],
    ) -> Result<(), InterpError> {
        let after = self.load(a, at, bits)?;
        self.observations.push((
            format!(
                "capture {a:?} {} {bits} {before:?} {sites:?}",
                bit_offset(at)
            ),
            vec![after],
        ));
        Ok(())
    }
    fn emit_runtime_event(&mut self, site: u32, args: &[SIRValue]) -> Result<(), InterpError> {
        self.observations
            .push((format!("event {site}"), args.to_vec()));
        Ok(())
    }
    fn emit_comb_capture_event(
        &mut self,
        site: u32,
        args: &[SIRValue],
        fatal: Option<i64>,
        consume: bool,
    ) -> Result<(), InterpError> {
        assert!(fatal.is_none(), "fixture has no fatal event");
        self.observations
            .push((format!("capture-event {site} {consume}"), args.to_vec()));
        Ok(())
    }
    fn enable_comb_capture_if_changed(
        &mut self,
        old: &SIRValue,
        new: &SIRValue,
        sites: &[u32],
    ) -> Result<(), InterpError> {
        self.observations.push((
            format!("capture-change {sites:?}"),
            vec![old.clone(), new.clone()],
        ));
        Ok(())
    }
}

#[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
pub mod native {
    use super::*;
    use crate::affine_support::native::Executable;
    use celox_design::StateAddr;
    use celox_sir::{ExecutionUnit, SIRInstruction};
    use celox_state_layout::MemoryLayout;

    /// The native microbenchmark handles Commit but has no event/capture
    /// plumbing. Check that contract and physical object disjointness before
    /// executing code. Unreferenced aliases elsewhere do not affect this unit.
    pub fn unit_objects(
        unit: &ExecutionUnit<RegionedStateAddr>,
        objects: &HashMap<RegionedStateAddr, MemoryObject>,
        layout: &MemoryLayout<StateAddr>,
    ) -> HashMap<RegionedStateAddr, MemoryObject> {
        let mut touched = HashMap::default();
        for instruction in unit.blocks.values().flat_map(|b| &b.instructions) {
            let addresses = match instruction {
                SIRInstruction::Load(_, a, _, _) => vec![a],
                SIRInstruction::Store(a, _, width, _, triggers, captures) => {
                    assert!(*width > 0 && triggers.is_empty() && captures.is_empty());
                    vec![a]
                }
                SIRInstruction::Commit(src, dst, _, _, triggers) => {
                    assert!(triggers.is_empty());
                    vec![src, dst]
                }
                SIRInstruction::RuntimeEvent { .. }
                | SIRInstruction::CombCaptureEvent { .. }
                | SIRInstruction::CombCaptureEnableIfChanged { .. } => {
                    panic!("native region fixture requires runtime callbacks")
                }
                _ => vec![],
            };
            for a in addresses {
                assert!(
                    a.region <= 1,
                    "native fixture does not initialize sparse metadata"
                );
                touched.insert(*a, objects[a].clone());
            }
        }
        let mut intervals = touched
            .keys()
            .map(|a| {
                let base = layout.region_base_offset(a);
                let size =
                    layout.plane_size(&a.absolute_addr()) * if layout.four_state { 2 } else { 1 };
                (base, base + size)
            })
            .collect::<Vec<_>>();
        intervals.sort_unstable();
        assert!(
            intervals.windows(2).all(|pair| pair[0].1 <= pair[1].0),
            "referenced objects alias in physical layout"
        );
        touched
    }

    /// Copies declared stable and working data only. Header/scratch remain zero.
    pub fn initialize(unit: &mut Executable, layout: &MemoryLayout<StateAddr>, state: &Machine) {
        unit.state.fill(0);
        for (a, value) in &state.values {
            if a.region > 1
                || (a.region == 1 && !layout.working_offsets.contains_key(&a.absolute_addr()))
            {
                continue;
            }
            let absolute = a.absolute_addr();
            let base = layout.region_base_offset(a);
            let plane = layout.plane_size(&absolute);
            for (index, value) in [&value.payload, &value.mask]
                .into_iter()
                .enumerate()
                .take(if layout.four_state { 2 } else { 1 })
            {
                let bytes = value.to_bytes_le();
                for bit in (0..layout.widths[&absolute]).step_by(8) {
                    let (offset, intra) = layout.map_static_bit_offset(&absolute, bit);
                    assert_eq!(intra, 0, "fixture arrays have byte-aligned cells");
                    let byte = base + index * plane + offset;
                    unit.state[byte / 8] |=
                        u64::from(bytes.get(bit / 8).copied().unwrap_or(0)) << (8 * (byte % 8));
                }
            }
        }
    }

    pub fn assert_stable(unit: &Executable, layout: &MemoryLayout<StateAddr>, expected: &Machine) {
        for (a, expected) in &expected.values {
            if a.region != 0 {
                continue;
            }
            let absolute = a.absolute_addr();
            let base = layout.offsets[&absolute];
            let width = layout.widths[&absolute];
            let plane = layout.plane_size(&absolute);
            let mut values = Vec::new();
            for index in 0..if layout.four_state { 2 } else { 1 } {
                let bytes = (0..width)
                    .step_by(8)
                    .map(|bit| {
                        let (offset, intra) = layout.map_static_bit_offset(&absolute, bit);
                        assert_eq!(intra, 0);
                        let byte = base + index * plane + offset;
                        (unit.state[byte / 8] >> (8 * (byte % 8))) as u8
                    })
                    .collect::<Vec<_>>();
                values.push(BigUint::from_bytes_le(&bytes) & mask(width));
            }
            assert_eq!(values[0], expected.payload, "native payload {absolute:?}");
            if layout.four_state {
                assert_eq!(values[1], expected.mask, "native mask {absolute:?}");
            }
        }
    }
}
