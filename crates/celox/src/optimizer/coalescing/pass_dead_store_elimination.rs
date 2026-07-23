use crate::ir::*;
use crate::{HashMap, HashSet};

use super::state_ssa::StatePhaseMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StaticBitRange {
    start: usize,
    end: usize,
}

impl StaticBitRange {
    fn new(start: usize, width: usize) -> Option<Self> {
        let end = start.checked_add(width)?;
        (start < end).then_some(Self { start, end })
    }

    fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}

/// Remove stores from `eval_comb` whose target addresses are not live.
///
/// A store's address is considered live if:
/// - It is in `externally_live` (user-specified observable signals), OR
/// - Any execution unit Loads from it (or Commits from it), OR
/// - It has a dynamic offset (conservative), OR
/// - The store has non-empty triggers (edge-detection side effect), OR
/// - The store has non-empty comb capture sites (observer activation side effect).
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) fn eliminate_dead_stores(
    program: &mut Program,
    externally_live: &HashSet<AbsoluteAddr>,
) {
    // 1. Collect all addresses loaded across ALL execution units.
    let mut loaded_addrs: HashSet<AbsoluteAddr> = HashSet::default();
    let mut dynamic_addrs: HashSet<AbsoluteAddr> = HashSet::default();

    let all_eus = program
        .eval_comb
        .iter()
        .chain(
            program
                .eval_apply_ffs
                .values()
                .flat_map(|units| units.iter()),
        )
        .chain(
            program
                .eval_only_ffs
                .values()
                .flat_map(|units| units.iter()),
        )
        .chain(program.apply_ffs.values().flat_map(|units| units.iter()));

    for eu in all_eus {
        for block in eu.blocks.values() {
            for inst in &block.instructions {
                match inst {
                    SIRInstruction::Load(_, addr, SIROffset::Static(_), _) => {
                        loaded_addrs.insert(addr.absolute_addr());
                    }
                    SIRInstruction::Load(
                        _,
                        addr,
                        SIROffset::Dynamic(_) | SIROffset::Element { .. },
                        _,
                    ) => {
                        let key = addr.absolute_addr();
                        loaded_addrs.insert(key);
                        dynamic_addrs.insert(key);
                    }
                    SIRInstruction::Commit(src, _, SIROffset::Static(_), _, _) => {
                        loaded_addrs.insert(src.absolute_addr());
                    }
                    SIRInstruction::Commit(
                        src,
                        _,
                        SIROffset::Dynamic(_) | SIROffset::Element { .. },
                        _,
                        _,
                    ) => {
                        let key = src.absolute_addr();
                        loaded_addrs.insert(key);
                        dynamic_addrs.insert(key);
                    }
                    _ => {}
                }
            }
        }
    }

    // 2. Remove dead stores from eval_comb.
    for eu in program.eval_comb.iter_mut() {
        for block in eu.blocks.values_mut() {
            block.instructions.retain(|inst| {
                match inst {
                    SIRInstruction::Store(
                        addr,
                        SIROffset::Static(_),
                        _,
                        _,
                        triggers,
                        comb_capture_sites,
                    ) if triggers.is_empty() && comb_capture_sites.is_empty() => {
                        let abs = addr.absolute_addr();
                        externally_live.contains(&abs)
                            || loaded_addrs.contains(&abs)
                            || dynamic_addrs.contains(&abs)
                    }
                    // Keep stores with dynamic offsets or triggers unconditionally.
                    _ => true,
                }
            });
        }
    }
}

/// Remove unread combinational publications from a fused comb/FF clone.
///
/// `tick_deferred_comb` marks simulator state dirty immediately after this
/// function returns. External signal reads therefore settle `eval_comb` before
/// observing state, so a comb-prefix Store is not an exit root merely because
/// the standalone combinational function publishes the same signal.
///
/// This initial subset is deliberately stronger than address-only program
/// DSE, but weaker than full MemorySSA DSE: it removes a static, effect-free
/// Store only when no instruction anywhere in the fused function reads an
/// overlapping static range. Any dynamic access to the same object blocks the
/// rewrite. Stores in the FF suffix remain persistent-state publications.
pub(crate) fn eliminate_unread_fused_comb_stores(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    ff_entry: BlockId,
) -> Result<usize, String> {
    let cfg = crate::ir::cfg::SirCfg::analyze(eu).map_err(|error| error.to_string())?;
    let phases = StatePhaseMap::fused(eu, &cfg, ff_entry).map_err(|error| error.to_string())?;
    let ff_blocks = phases
        .ff_blocks()
        .expect("a fused phase map always classifies FF blocks");

    let mut reads = HashMap::<RegionedAbsoluteAddr, Vec<StaticBitRange>>::default();
    let mut unknown_objects = HashSet::<RegionedAbsoluteAddr>::default();
    for block in eu.blocks.values() {
        for instruction in &block.instructions {
            match instruction {
                SIRInstruction::Load(_, address, offset, width)
                    if address.region == STABLE_REGION =>
                {
                    record_fused_read(*address, offset, *width, &mut reads, &mut unknown_objects);
                }
                SIRInstruction::Commit(source, _, offset, width, _)
                    if source.region == STABLE_REGION =>
                {
                    record_fused_read(*source, offset, *width, &mut reads, &mut unknown_objects);
                }
                SIRInstruction::Store(address, offset, width, _, triggers, captures)
                    if address.region == STABLE_REGION
                        && (!triggers.is_empty() || !captures.is_empty()) =>
                {
                    // Trigger/capture publication compares or otherwise
                    // observes the previous value of its written range.
                    record_fused_read(*address, offset, *width, &mut reads, &mut unknown_objects);
                }
                SIRInstruction::Store(
                    address,
                    SIROffset::Dynamic(_) | SIROffset::Element { .. },
                    _,
                    _,
                    _,
                    _,
                ) if address.region == STABLE_REGION => {
                    unknown_objects.insert(*address);
                }
                _ => {}
            }
        }
    }
    for ranges in reads.values_mut() {
        normalize_read_ranges(ranges);
    }

    let mut removed = 0usize;
    for (&block_id, block) in &mut eu.blocks {
        if ff_blocks.contains(&block_id) {
            continue;
        }
        block.instructions.retain(|instruction| {
            let SIRInstruction::Store(
                address,
                SIROffset::Static(start),
                width,
                _,
                triggers,
                captures,
            ) = instruction
            else {
                return true;
            };
            if address.region != STABLE_REGION
                || !triggers.is_empty()
                || !captures.is_empty()
                || unknown_objects.contains(address)
            {
                return true;
            }
            let Some(stored) = StaticBitRange::new(*start, *width) else {
                return true;
            };
            let observed = reads
                .get(address)
                .is_some_and(|ranges| contains_overlapping_range(ranges, stored));
            removed += usize::from(!observed);
            observed
        });
    }
    Ok(removed)
}

fn record_fused_read(
    address: RegionedAbsoluteAddr,
    offset: &SIROffset,
    width: usize,
    reads: &mut HashMap<RegionedAbsoluteAddr, Vec<StaticBitRange>>,
    unknown_objects: &mut HashSet<RegionedAbsoluteAddr>,
) {
    match offset {
        SIROffset::Static(start) => {
            if let Some(range) = StaticBitRange::new(*start, width) {
                reads.entry(address).or_default().push(range);
            } else {
                unknown_objects.insert(address);
            }
        }
        SIROffset::Dynamic(_) | SIROffset::Element { .. } => {
            unknown_objects.insert(address);
        }
    }
}

fn normalize_read_ranges(ranges: &mut Vec<StaticBitRange>) {
    ranges.sort_unstable_by_key(|range| (range.start, range.end));
    let mut write = 0usize;
    for read in 0..ranges.len() {
        if write != 0 && ranges[write - 1].end >= ranges[read].start {
            ranges[write - 1].end = ranges[write - 1].end.max(ranges[read].end);
        } else {
            ranges[write] = ranges[read];
            write += 1;
        }
    }
    ranges.truncate(write);
}

fn contains_overlapping_range(ranges: &[StaticBitRange], stored: StaticBitRange) -> bool {
    let candidate = ranges.partition_point(|range| range.end <= stored.start);
    ranges
        .get(candidate)
        .is_some_and(|range| stored.overlaps(*range))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{InstanceId, RegisterType};
    use veryl_analyzer::ir::VarId;

    fn address(instance: usize) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region: STABLE_REGION,
            instance_id: InstanceId(instance),
            var_id: VarId::default(),
        }
    }

    fn fused_unit(
        comb_instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
        ff_instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    ) -> ExecutionUnit<RegionedAbsoluteAddr> {
        let blocks = [
            (
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: Vec::new(),
                    instructions: comb_instructions,
                    terminator: SIRTerminator::Jump(BlockId(1), Vec::new()),
                },
            ),
            (
                BlockId(1),
                BasicBlock {
                    id: BlockId(1),
                    params: Vec::new(),
                    instructions: ff_instructions,
                    terminator: SIRTerminator::Return,
                },
            ),
        ]
        .into_iter()
        .collect();
        let register_map = (0..8)
            .map(|register| {
                (
                    RegisterId(register),
                    RegisterType::Bit {
                        width: 64,
                        signed: false,
                    },
                )
            })
            .collect();
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        }
    }

    fn store(
        object: usize,
        start: usize,
        width: usize,
        source: usize,
    ) -> SIRInstruction<RegionedAbsoluteAddr> {
        SIRInstruction::Store(
            address(object),
            SIROffset::Static(start),
            width,
            RegisterId(source),
            Vec::new(),
            Vec::new(),
        )
    }

    #[test]
    fn fused_dse_removes_only_unread_comb_ranges() {
        let mut eu = fused_unit(
            vec![store(0, 0, 8, 0), store(1, 0, 8, 1), store(2, 0, 8, 2)],
            vec![
                SIRInstruction::Load(RegisterId(3), address(1), SIROffset::Static(0), 8),
                SIRInstruction::Load(RegisterId(4), address(2), SIROffset::Static(8), 8),
            ],
        );

        assert_eq!(
            eliminate_unread_fused_comb_stores(&mut eu, BlockId(1)).unwrap(),
            2
        );
        assert_eq!(eu.blocks[&BlockId(0)].instructions, vec![store(1, 0, 8, 1)]);
    }

    #[test]
    fn fused_dse_keeps_effectful_and_dynamically_aliased_comb_stores() {
        let mut effectful = store(0, 0, 8, 0);
        let SIRInstruction::Store(_, _, _, _, _, captures) = &mut effectful else {
            unreachable!();
        };
        captures.push(7);
        let mut eu = fused_unit(
            vec![effectful.clone(), store(1, 0, 8, 1)],
            vec![SIRInstruction::Load(
                RegisterId(2),
                address(1),
                SIROffset::Dynamic(RegisterId(3)),
                8,
            )],
        );

        assert_eq!(
            eliminate_unread_fused_comb_stores(&mut eu, BlockId(1)).unwrap(),
            0
        );
        assert_eq!(
            eu.blocks[&BlockId(0)].instructions,
            vec![effectful, store(1, 0, 8, 1)]
        );
    }

    #[test]
    fn normalized_read_ranges_answer_overlap_at_exact_boundaries() {
        let mut ranges = vec![
            StaticBitRange { start: 16, end: 24 },
            StaticBitRange { start: 0, end: 8 },
            StaticBitRange { start: 7, end: 17 },
        ];
        normalize_read_ranges(&mut ranges);

        assert_eq!(ranges, vec![StaticBitRange { start: 0, end: 24 }]);
        assert!(contains_overlapping_range(
            &ranges,
            StaticBitRange { start: 23, end: 25 }
        ));
        assert!(!contains_overlapping_range(
            &ranges,
            StaticBitRange { start: 24, end: 32 }
        ));
    }
}
