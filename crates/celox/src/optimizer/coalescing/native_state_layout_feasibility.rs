use crate::backend::MemoryLayout;
use crate::ir::{
    AbsoluteAddr, BlockId, ExecutionUnit, Program, RegionedAbsoluteAddr, SIRInstruction, SIROffset,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

#[derive(Debug, Clone, Copy, Default)]
struct GlobalObjectFacts {
    arbitrary_dynamic_access: bool,
    element_access: bool,
    element_width: Option<usize>,
    conflicting_element_width: bool,
    effectful_store: bool,
    commit_sites: usize,
}

fn record_offset_kind(facts: &mut GlobalObjectFacts, offset: &SIROffset) {
    match offset {
        SIROffset::Static(_) => {}
        SIROffset::Dynamic(_) => facts.arbitrary_dynamic_access = true,
        SIROffset::Element { element_width, .. } => {
            facts.element_access = true;
            if facts
                .element_width
                .is_some_and(|known| known != *element_width)
            {
                facts.conflicting_element_width = true;
            } else {
                facts.element_width = Some(*element_width);
            }
        }
    }
}

/// Program-wide facts which cannot be recovered from one merged native
/// function. Construction is linear in the SIR and stores one record per
/// touched object.
#[derive(Debug, Clone, Default)]
pub(crate) struct ProgramStateAccessSummary {
    objects: BTreeMap<AbsoluteAddr, GlobalObjectFacts>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccessKind {
    Load,
    Store,
}

#[derive(Debug, Clone, Copy)]
struct HotAccess {
    object: AbsoluteAddr,
    start: usize,
    width: usize,
    kind: AccessKind,
    samples: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct ObjectScore {
    hot_samples: u64,
    hot_accesses: usize,
    current_instructions: u128,
    scalarized_instructions: u128,
    packed_bytes: usize,
    scalarized_bytes: usize,
    fragments: usize,
    arbitrary_dynamic_access: bool,
    element_access: bool,
    element_width: Option<usize>,
    conflicting_element_width: bool,
    element_layout_compatible: bool,
    already_element_strided: bool,
    effectful_store: bool,
    four_state: bool,
    aliased_storage: bool,
    commit_sites: usize,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct NativeStateLayoutFeasibilityReport {
    elapsed: std::time::Duration,
    rss_before_kib: usize,
    rss_after_kib: usize,
    peak_after_kib: usize,
    selected_blocks: usize,
    selected_samples: u64,
    missing_blocks: Vec<BlockId>,
    hot_accesses: usize,
    scores: BTreeMap<AbsoluteAddr, ObjectScore>,
}

impl ProgramStateAccessSummary {
    pub(crate) fn build(program: &Program) -> Self {
        let mut summary = Self::default();
        let mut inspect = |instruction: &SIRInstruction<RegionedAbsoluteAddr>| match instruction {
            SIRInstruction::Load(_, address, offset, _) => {
                record_offset_kind(
                    summary.objects.entry(address.absolute_addr()).or_default(),
                    offset,
                );
            }
            SIRInstruction::Store(address, offset, _, _, triggers, capture_sites) => {
                let facts = summary.objects.entry(address.absolute_addr()).or_default();
                record_offset_kind(facts, offset);
                facts.effectful_store |= !triggers.is_empty() || !capture_sites.is_empty();
            }
            SIRInstruction::Commit(source, destination, offset, _, _) => {
                for address in [source, destination] {
                    let facts = summary.objects.entry(address.absolute_addr()).or_default();
                    record_offset_kind(facts, offset);
                    facts.commit_sites += 1;
                }
            }
            _ => {}
        };
        for unit in program
            .eval_comb
            .iter()
            .chain(program.eval_apply_ffs.values().flatten())
            .chain(program.eval_only_ffs.values().flatten())
            .chain(program.apply_ffs.values().flatten())
        {
            for block in unit.blocks.values() {
                for instruction in &block.instructions {
                    inspect(instruction);
                }
            }
        }
        summary
    }
}

fn native_bytes(width: usize) -> usize {
    match width {
        0 => 0,
        1..=8 => 1,
        9..=16 => 2,
        17..=32 => 4,
        _ => 8,
    }
}

fn current_load_cost(start: usize, width: usize, full_direct: bool) -> usize {
    if width == 0 {
        return 0;
    }
    let intra = start % 8;
    if full_direct || (width <= 64 && intra == 0 && matches!(width, 8 | 16 | 32 | 64)) {
        return 1;
    }
    if width <= 64 && intra + width <= 64 {
        return 1 + usize::from(intra != 0) + usize::from(width < 64);
    }
    let words = (intra + width).div_ceil(64);
    words.saturating_mul(2).saturating_add(1)
}

fn current_store_cost(start: usize, width: usize, full_direct: bool) -> usize {
    if width == 0 {
        return 0;
    }
    let intra = start % 8;
    if full_direct || (width <= 64 && intra == 0 && matches!(width, 8 | 16 | 32 | 64)) {
        return 1;
    }
    if width <= 64 && intra + width <= 64 {
        // old load, clear, source mask, optional shift, merge, store
        return 5 + usize::from(intra != 0);
    }
    let words = (intra + width).div_ceil(64);
    words.saturating_mul(5)
}

fn scalarized_access_cost(kind: AccessKind, fragments: usize) -> usize {
    match kind {
        AccessKind::Load => fragments + fragments.saturating_sub(1) * 2,
        AccessKind::Store => {
            if fragments <= 1 {
                fragments
            } else {
                // Extract each fragment and store it. This is deliberately a
                // conservative machine-independent estimate.
                fragments * 2
            }
        }
    }
}

fn add_static_access(
    accesses: &mut Vec<HotAccess>,
    endpoints: &mut BTreeMap<AbsoluteAddr, BTreeSet<usize>>,
    address: RegionedAbsoluteAddr,
    start: usize,
    width: usize,
    kind: AccessKind,
    samples: u64,
) {
    if width == 0 {
        return;
    }
    let Some(end) = start.checked_add(width) else {
        return;
    };
    let points = endpoints.entry(address.absolute_addr()).or_default();
    points.insert(start);
    points.insert(end);
    accesses.push(HotAccess {
        object: address.absolute_addr(),
        start,
        width,
        kind,
        samples,
    });
}

pub(crate) fn analyze_native_state_layout(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    layout: &MemoryLayout,
    program_facts: &ProgramStateAccessSummary,
    selected_blocks: &[(BlockId, u64)],
) -> NativeStateLayoutFeasibilityReport {
    let start_time = std::time::Instant::now();
    let rss_before_kib = super::resident_memory_kib().map_or(0, |value| value.0);
    let mut report = NativeStateLayoutFeasibilityReport {
        rss_before_kib,
        selected_blocks: selected_blocks.len(),
        selected_samples: selected_blocks.iter().map(|(_, samples)| *samples).sum(),
        ..NativeStateLayoutFeasibilityReport::default()
    };
    let mut accesses = Vec::new();
    let mut endpoints = BTreeMap::<AbsoluteAddr, BTreeSet<usize>>::new();

    for &(block_id, samples) in selected_blocks {
        let Some(block) = eu.blocks.get(&block_id) else {
            report.missing_blocks.push(block_id);
            continue;
        };
        for instruction in &block.instructions {
            match instruction {
                SIRInstruction::Load(_, address, SIROffset::Static(start), width) => {
                    add_static_access(
                        &mut accesses,
                        &mut endpoints,
                        *address,
                        *start,
                        *width,
                        AccessKind::Load,
                        samples,
                    );
                }
                SIRInstruction::Store(address, SIROffset::Static(start), width, _, _, _) => {
                    add_static_access(
                        &mut accesses,
                        &mut endpoints,
                        *address,
                        *start,
                        *width,
                        AccessKind::Store,
                        samples,
                    );
                }
                _ => {}
            }
        }
    }
    report.hot_accesses = accesses.len();

    let candidate_objects = endpoints.keys().copied().collect::<BTreeSet<_>>();
    for block in eu.blocks.values() {
        for instruction in &block.instructions {
            match instruction {
                SIRInstruction::Load(_, address, SIROffset::Static(start), width)
                | SIRInstruction::Store(address, SIROffset::Static(start), width, _, _, _)
                    if candidate_objects.contains(&address.absolute_addr()) =>
                {
                    if let Some(end) = start.checked_add(*width) {
                        let points = endpoints.entry(address.absolute_addr()).or_default();
                        points.insert(*start);
                        points.insert(end);
                    }
                }
                SIRInstruction::Commit(source, destination, SIROffset::Static(start), width, _) => {
                    for address in [source, destination] {
                        if candidate_objects.contains(&address.absolute_addr())
                            && let Some(end) = start.checked_add(*width)
                        {
                            let points = endpoints.entry(address.absolute_addr()).or_default();
                            points.insert(*start);
                            points.insert(end);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    let mut storage_offsets = BTreeMap::<usize, usize>::new();
    for offset in layout.offsets.values() {
        *storage_offsets.entry(*offset).or_default() += 1;
    }
    for (&object, points) in &mut endpoints {
        let width = layout.widths.get(&object).copied().unwrap_or(0);
        points.insert(0);
        points.insert(width);
        let mut split_points = Vec::new();
        for pair in points.iter().copied().collect::<Vec<_>>().windows(2) {
            let mut point = pair[0].saturating_add(64);
            while point < pair[1] {
                split_points.push(point);
                point = point.saturating_add(64);
            }
        }
        points.extend(split_points);

        let fragments = points
            .iter()
            .copied()
            .collect::<Vec<_>>()
            .windows(2)
            .filter(|pair| pair[0] < pair[1])
            .map(|pair| pair[1] - pair[0])
            .collect::<Vec<_>>();
        let facts = program_facts
            .objects
            .get(&object)
            .copied()
            .unwrap_or_default();
        let array_layout = layout.unpacked_arrays.get(&object);
        let element_layout_compatible = facts.element_access
            && !facts.conflicting_element_width
            && array_layout.is_some_and(|array| Some(array.element_width) == facts.element_width);
        report.scores.insert(
            object,
            ObjectScore {
                packed_bytes: width.div_ceil(8),
                scalarized_bytes: fragments.iter().map(|width| native_bytes(*width)).sum(),
                fragments: fragments.len(),
                arbitrary_dynamic_access: facts.arbitrary_dynamic_access,
                element_access: facts.element_access,
                element_width: facts.element_width,
                conflicting_element_width: facts.conflicting_element_width,
                element_layout_compatible,
                already_element_strided: array_layout.is_some(),
                effectful_store: facts.effectful_store,
                four_state: layout.four_state
                    && layout.is_4states.get(&object).copied().unwrap_or(false),
                aliased_storage: layout
                    .offsets
                    .get(&object)
                    .and_then(|offset| storage_offsets.get(offset))
                    .is_some_and(|count| *count > 1),
                commit_sites: facts.commit_sites,
                ..ObjectScore::default()
            },
        );
    }

    for access in accesses {
        let points = &endpoints[&access.object];
        let end = access.start + access.width;
        let fragments = points
            .range(access.start..=end)
            .copied()
            .collect::<Vec<_>>()
            .windows(2)
            .filter(|pair| pair[0] < pair[1])
            .count();
        let whole_object_direct = access.start == 0
            && layout
                .widths
                .get(&access.object)
                .is_some_and(|width| *width == access.width);
        let whole_element_direct =
            layout
                .unpacked_arrays
                .get(&access.object)
                .is_some_and(|array| {
                    access.start.is_multiple_of(array.element_width)
                        && access.width == array.element_width
                });
        let full_direct = whole_object_direct || whole_element_direct;
        let physical_start = layout
            .unpacked_arrays
            .get(&access.object)
            .map(|_| {
                let (byte, intra) = layout.map_static_bit_offset(&access.object, access.start);
                byte * 8 + intra
            })
            .unwrap_or(access.start);
        let current = match access.kind {
            AccessKind::Load => current_load_cost(physical_start, access.width, full_direct),
            AccessKind::Store => current_store_cost(physical_start, access.width, full_direct),
        };
        let scalarized = scalarized_access_cost(access.kind, fragments);
        let score = report.scores.get_mut(&access.object).unwrap();
        score.hot_samples = score.hot_samples.saturating_add(access.samples);
        score.hot_accesses += 1;
        score.current_instructions = score
            .current_instructions
            .saturating_add(current as u128 * access.samples as u128);
        score.scalarized_instructions = score
            .scalarized_instructions
            .saturating_add(scalarized as u128 * access.samples as u128);
    }
    report.elapsed = start_time.elapsed();
    if let Some((resident, peak)) = super::resident_memory_kib() {
        report.rss_after_kib = resident;
        report.peak_after_kib = peak;
    }
    report
}

impl fmt::Display for NativeStateLayoutFeasibilityReport {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            out,
            "selected_blocks={} selected_samples={} missing_blocks={} hot_static_accesses={} elapsed_ms={} rss_before_kib={} rss_after_kib={} peak_after_kib={}",
            self.selected_blocks,
            self.selected_samples,
            self.missing_blocks.len(),
            self.hot_accesses,
            self.elapsed.as_millis(),
            self.rss_before_kib,
            self.rss_after_kib,
            self.peak_after_kib,
        )?;
        for block in &self.missing_blocks {
            writeln!(out, "missing block=b{}", block.0)?;
        }
        let rejected = |score: &ObjectScore| {
            score.arbitrary_dynamic_access
                || score.conflicting_element_width
                || (score.element_access && !score.element_layout_compatible)
                || score.effectful_store
                || score.four_state
                || score.aliased_storage
        };
        let mut admitted_objects = 0usize;
        let mut all_current = 0u128;
        let mut all_scalarized = 0u128;
        let mut admitted_current = 0u128;
        let mut admitted_scalarized = 0u128;
        let mut admitted_packed_bytes = 0usize;
        let mut admitted_scalarized_bytes = 0usize;
        let mut profitable_objects = 0usize;
        let mut profitable_current = 0u128;
        let mut profitable_scalarized = 0u128;
        for score in self.scores.values() {
            all_current = all_current.saturating_add(score.current_instructions);
            all_scalarized = all_scalarized.saturating_add(score.scalarized_instructions);
            if !rejected(score) {
                admitted_objects += 1;
                admitted_current = admitted_current.saturating_add(score.current_instructions);
                admitted_scalarized =
                    admitted_scalarized.saturating_add(score.scalarized_instructions);
                admitted_packed_bytes = admitted_packed_bytes.saturating_add(score.packed_bytes);
                admitted_scalarized_bytes =
                    admitted_scalarized_bytes.saturating_add(score.scalarized_bytes);
                if score.current_instructions > score.scalarized_instructions {
                    profitable_objects += 1;
                    profitable_current =
                        profitable_current.saturating_add(score.current_instructions);
                    profitable_scalarized =
                        profitable_scalarized.saturating_add(score.scalarized_instructions);
                }
            }
        }
        writeln!(
            out,
            "objects={} semantic_objects={} profitable_objects={} all_weighted_current={} all_weighted_scalarized={} all_weighted_net={} semantic_weighted_current={} semantic_weighted_scalarized={} semantic_weighted_net={} profitable_weighted_current={} profitable_weighted_scalarized={} profitable_weighted_net={} semantic_packed_bytes={} semantic_scalarized_bytes={}",
            self.scores.len(),
            admitted_objects,
            profitable_objects,
            all_current,
            all_scalarized,
            all_current as i128 - all_scalarized as i128,
            admitted_current,
            admitted_scalarized,
            admitted_current as i128 - admitted_scalarized as i128,
            profitable_current,
            profitable_scalarized,
            profitable_current as i128 - profitable_scalarized as i128,
            admitted_packed_bytes,
            admitted_scalarized_bytes,
        )?;
        let mut scores = self.scores.iter().collect::<Vec<_>>();
        scores.sort_by_key(|(object, score)| {
            (
                std::cmp::Reverse(
                    score
                        .current_instructions
                        .saturating_sub(score.scalarized_instructions),
                ),
                **object,
            )
        });
        for (object, score) in scores {
            let saving = score
                .current_instructions
                .saturating_sub(score.scalarized_instructions);
            let is_rejected = rejected(score);
            writeln!(
                out,
                "object={object} hot_samples={} hot_accesses={} weighted_current={} weighted_scalarized={} weighted_net={} fragments={} packed_bytes={} scalarized_bytes={} commit_sites={} arbitrary_dynamic={} element_access={} element_width={:?} conflicting_element_width={} element_layout_compatible={} already_element_strided={} effectful={} four_state={} aliased={} semantic_subset={} profitable={}",
                score.hot_samples,
                score.hot_accesses,
                score.current_instructions,
                score.scalarized_instructions,
                score.current_instructions as i128 - score.scalarized_instructions as i128,
                score.fragments,
                score.packed_bytes,
                score.scalarized_bytes,
                score.commit_sites,
                score.arbitrary_dynamic_access,
                score.element_access,
                score.element_width,
                score.conflicting_element_width,
                score.element_layout_compatible,
                score.already_element_strided,
                score.effectful_store,
                score.four_state,
                score.aliased_storage,
                if is_rejected { "reject" } else { "admit" },
                if saving > 0 { "yes" } else { "no" },
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HashMap;
    use crate::backend::memory_layout::MemoryLayoutMode;
    use crate::ir::{
        BasicBlock, InstanceId, RegisterId, RegisterType, SIRTerminator, STABLE_REGION,
    };
    use veryl_analyzer::ir::VarId;

    fn address(variable: u32) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr::from_absolute_addr(
            STABLE_REGION,
            AbsoluteAddr {
                instance_id: InstanceId(0),
                var_id: VarId::from_raw(variable),
            },
        )
    }

    fn layout(objects: &[(RegionedAbsoluteAddr, usize)]) -> MemoryLayout {
        let mut offsets = HashMap::default();
        let mut widths = HashMap::default();
        let mut is_4states = HashMap::default();
        let mut offset = 32usize;
        for (object, width) in objects {
            offsets.insert(object.absolute_addr(), offset);
            widths.insert(object.absolute_addr(), *width);
            is_4states.insert(object.absolute_addr(), false);
            offset += width.div_ceil(8);
        }
        MemoryLayout {
            four_state: false,
            mode: MemoryLayoutMode::Packed,
            offsets,
            widths,
            is_4states,
            unpacked_arrays: HashMap::default(),
            total_size: offset,
            working_offsets: HashMap::default(),
            working_base_offset: offset,
            sparse_offsets: HashMap::default(),
            sparse_base_offset: offset,
            sparse_layouts: HashMap::default(),
            sparse_active_bits_offset: offset,
            sparse_active_capacity: 0,
            merged_total_size: offset,
            triggered_bits_offset: offset,
            triggered_bits_total_size: 0,
            scratch_base_offset: offset,
            scratch_size: 0,
            runtime_event_capacity: 0,
            runtime_event_slot_size: 0,
            runtime_event_buffer_size: 0,
            runtime_event_site_layouts: Vec::new(),
        }
    }

    #[test]
    fn exact_hot_subranges_are_scored_as_scalar_slots() {
        let object = address(1);
        let block = BasicBlock {
            id: BlockId(7),
            params: Vec::new(),
            instructions: vec![
                SIRInstruction::Load(RegisterId(0), object, SIROffset::Static(5), 5),
                SIRInstruction::Store(
                    object,
                    SIROffset::Static(17),
                    3,
                    RegisterId(0),
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            terminator: SIRTerminator::Return,
        };
        let eu = ExecutionUnit {
            entry_block_id: block.id,
            blocks: [(block.id, block)].into_iter().collect(),
            register_map: [(RegisterId(0), RegisterType::Logic { width: 5 })]
                .into_iter()
                .collect(),
        };
        let report = analyze_native_state_layout(
            &eu,
            &layout(&[(object, 32)]),
            &ProgramStateAccessSummary::default(),
            &[(BlockId(7), 10)],
        );
        let score = report.scores.get(&object.absolute_addr()).unwrap();
        assert_eq!(score.hot_accesses, 2);
        assert_eq!(score.scalarized_instructions, 20);
        assert!(score.current_instructions > score.scalarized_instructions);
    }

    #[test]
    fn program_wide_dynamic_access_rejects_local_static_candidate() {
        let object = address(2);
        let mut facts = ProgramStateAccessSummary::default();
        facts.objects.insert(
            object.absolute_addr(),
            GlobalObjectFacts {
                arbitrary_dynamic_access: true,
                ..GlobalObjectFacts::default()
            },
        );
        let block = BasicBlock {
            id: BlockId(3),
            params: Vec::new(),
            instructions: vec![SIRInstruction::Load(
                RegisterId(0),
                object,
                SIROffset::Static(1),
                1,
            )],
            terminator: SIRTerminator::Return,
        };
        let eu = ExecutionUnit {
            entry_block_id: block.id,
            blocks: [(block.id, block)].into_iter().collect(),
            register_map: [(RegisterId(0), RegisterType::Logic { width: 1 })]
                .into_iter()
                .collect(),
        };
        let report =
            analyze_native_state_layout(&eu, &layout(&[(object, 8)]), &facts, &[(BlockId(3), 1)]);
        assert!(
            report.scores[&object.absolute_addr()].arbitrary_dynamic_access,
            "a local exact access cannot hide a program-wide dynamic alias"
        );
    }

    #[test]
    fn element_access_is_distinct_from_arbitrary_dynamic_access() {
        let mut facts = GlobalObjectFacts::default();
        record_offset_kind(
            &mut facts,
            &SIROffset::Element {
                index: RegisterId(0),
                element_width: 3,
                bit_offset: 0,
                dynamic_bit_offset: None,
            },
        );
        assert!(facts.element_access);
        assert_eq!(facts.element_width, Some(3));
        assert!(!facts.arbitrary_dynamic_access);
        assert!(!facts.conflicting_element_width);

        record_offset_kind(&mut facts, &SIROffset::Dynamic(RegisterId(1)));
        assert!(facts.arbitrary_dynamic_access);
    }

    #[test]
    fn conflicting_element_widths_are_not_treated_as_one_layout() {
        let mut facts = GlobalObjectFacts::default();
        for element_width in [3, 1] {
            record_offset_kind(
                &mut facts,
                &SIROffset::Element {
                    index: RegisterId(0),
                    element_width,
                    bit_offset: 0,
                    dynamic_bit_offset: None,
                },
            );
        }
        assert!(facts.conflicting_element_width);
    }

    #[test]
    fn whole_narrow_variable_is_already_a_direct_native_access() {
        let object = address(3);
        let block = BasicBlock {
            id: BlockId(4),
            params: Vec::new(),
            instructions: vec![
                SIRInstruction::Load(RegisterId(0), object, SIROffset::Static(0), 1),
                SIRInstruction::Store(
                    object,
                    SIROffset::Static(0),
                    1,
                    RegisterId(0),
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            terminator: SIRTerminator::Return,
        };
        let eu = ExecutionUnit {
            entry_block_id: block.id,
            blocks: [(block.id, block)].into_iter().collect(),
            register_map: [(RegisterId(0), RegisterType::Logic { width: 1 })]
                .into_iter()
                .collect(),
        };
        let report = analyze_native_state_layout(
            &eu,
            &layout(&[(object, 1)]),
            &ProgramStateAccessSummary::default(),
            &[(BlockId(4), 7)],
        );
        let score = report.scores.get(&object.absolute_addr()).unwrap();
        assert_eq!(score.current_instructions, 14);
        assert_eq!(score.scalarized_instructions, 14);
    }
}
