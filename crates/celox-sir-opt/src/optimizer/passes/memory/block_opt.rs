use super::shared::{def_reg, replace_reg_in_terminator};
use crate::ir::*;
use crate::{HashMap, HashSet};
use num_bigint::BigUint;
use std::collections::BTreeMap;

const MAX_SCALAR_COALESCED_STORE_BITS: usize = 64;

fn union_u32<I: IntoIterator<Item = u32>>(items: I) -> Vec<u32> {
    let mut out = Vec::new();
    for item in items {
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

pub(in crate::optimizer) fn aggregate_static_offset(
    bit_offset: usize,
    width: usize,
    element_width: Option<usize>,
) -> Option<SIROffset> {
    match element_width {
        Some(0) => None,
        Some(element_width) if width != 0 => {
            let end = bit_offset.checked_add(width)?;
            if bit_offset / element_width == end.saturating_sub(1) / element_width {
                Some(SIROffset::Static(bit_offset))
            } else if bit_offset.is_multiple_of(element_width)
                && width.is_multiple_of(element_width)
            {
                Some(SIROffset::PackedElements {
                    bit_offset,
                    element_width,
                })
            } else {
                None
            }
        }
        _ => Some(SIROffset::Static(bit_offset)),
    }
}

fn is_complete_element_access(bit_offset: usize, width: usize, element_width: usize) -> bool {
    element_width != 0
        && width != 0
        && bit_offset.is_multiple_of(element_width)
        && width.is_multiple_of(element_width)
}

fn collect_used_regs<A>(inst: &SIRInstruction<A>, out: &mut Vec<RegisterId>) {
    match inst {
        SIRInstruction::Imm(_, _) => {}
        SIRInstruction::Binary(_, lhs, _, rhs) => {
            out.push(*lhs);
            out.push(*rhs);
        }
        SIRInstruction::Unary(_, _, src) => {
            out.push(*src);
        }
        SIRInstruction::Load(_, _, offset, _) => {
            out.extend(offset.dynamic_registers().into_iter().flatten());
        }
        SIRInstruction::Store(_, offset, _, src, _, _) => {
            out.extend(offset.dynamic_registers().into_iter().flatten());
            out.push(*src);
        }
        SIRInstruction::Commit(_, _, offset, _, _) => {
            out.extend(offset.dynamic_registers().into_iter().flatten());
        }
        SIRInstruction::Concat(_, args) => out.extend(args.iter().copied()),
        SIRInstruction::Slice(_, src, _, _) => {
            out.push(*src);
        }
        SIRInstruction::Mux(_, cond, then_val, else_val) => {
            out.push(*cond);
            out.push(*then_val);
            out.push(*else_val);
        }
        SIRInstruction::RuntimeEvent { args, .. }
        | SIRInstruction::CombCaptureEvent { args, .. } => out.extend(args.iter().copied()),
        SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => {
            out.push(*old);
            out.push(*new);
        }
    }
}

fn is_memory_barrier<A>(inst: &SIRInstruction<A>) -> bool {
    matches!(
        inst,
        SIRInstruction::Commit(_, _, _, _, _)
            | SIRInstruction::RuntimeEvent { .. }
            | SIRInstruction::CombCaptureEvent { .. }
            | SIRInstruction::CombCaptureEnableIfChanged { .. }
    )
}

fn mem_access_info<A>(inst: &SIRInstruction<A>) -> Option<(&A, Option<usize>, usize, bool)> {
    match inst {
        SIRInstruction::Load(_, addr, SIROffset::Static(off), bits) => {
            Some((addr, Some(*off), *bits, false))
        }
        SIRInstruction::Load(_, addr, SIROffset::PackedElements { bit_offset, .. }, bits) => {
            Some((addr, Some(*bit_offset), *bits, false))
        }
        SIRInstruction::Load(_, addr, SIROffset::Dynamic(_) | SIROffset::Element { .. }, bits) => {
            Some((addr, None, *bits, false))
        }
        SIRInstruction::Store(addr, SIROffset::Static(off), bits, _, _, _) => {
            Some((addr, Some(*off), *bits, true))
        }
        SIRInstruction::Store(
            addr,
            SIROffset::PackedElements { bit_offset, .. },
            bits,
            _,
            _,
            _,
        ) => Some((addr, Some(*bit_offset), *bits, true)),
        SIRInstruction::Store(
            addr,
            SIROffset::Dynamic(_) | SIROffset::Element { .. },
            bits,
            _,
            _,
            _,
        ) => Some((addr, None, *bits, true)),
        _ => None,
    }
}

/// Check if two memory ranges at the same address may alias (offset overlap check only).
/// Used when the address equality is already guaranteed by HashMap bucketing.
fn ranges_alias(
    off_a: Option<usize>,
    width_a: usize,
    off_b: Option<usize>,
    width_b: usize,
) -> bool {
    match (off_a, off_b) {
        (Some(a), Some(b)) => a < b + width_b && b < a + width_a,
        _ => true,
    }
}

fn schedule_block_interleaved<A: Clone + PartialEq + Eq + std::hash::Hash>(
    window: &[SIRInstruction<A>],
    max_inflight_loads: usize,
) -> Vec<SIRInstruction<A>> {
    let n = window.len();
    if n <= 1 {
        return window.to_vec();
    }

    // Build def-use information
    let mut defs: Vec<Option<RegisterId>> = Vec::with_capacity(n);
    let mut uses: Vec<Vec<RegisterId>> = Vec::with_capacity(n);
    for inst in window {
        defs.push(def_reg(inst));
        let mut u = Vec::new();
        collect_used_regs(inst, &mut u);
        uses.push(u);
    }

    // Build dependency graph using def-use chains: O(n * avg_uses) instead of O(n²)
    let mut def_map: HashMap<RegisterId, usize> = HashMap::default();
    let mut succs: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut indeg = vec![0usize; n];

    let add_edge = |from: usize, to: usize, succs: &mut Vec<Vec<usize>>, indeg: &mut Vec<usize>| {
        if !succs[from].contains(&to) {
            succs[from].push(to);
            indeg[to] += 1;
        }
    };

    // Track memory accesses indexed by address for O(n*k) instead of O(n²).
    // In large designs, most addresses are distinct so only a few entries per bucket.
    let mut mem_writes: HashMap<A, Vec<usize>> = HashMap::default();
    let mut mem_reads: HashMap<A, Vec<usize>> = HashMap::default();

    // Pre-extract memory access info to avoid redundant pattern matching
    let mem_infos: Vec<Option<(A, Option<usize>, usize, bool)>> = window
        .iter()
        .map(|inst| {
            mem_access_info(inst)
                .map(|(addr, off, width, is_write)| (addr.clone(), off, width, is_write))
        })
        .collect();

    for j in 0..n {
        // Data dependencies: for each register used by j, add edge from its def
        for reg in &uses[j] {
            if let Some(&def_idx) = def_map.get(reg) {
                add_edge(def_idx, j, &mut succs, &mut indeg);
            }
        }
        if let Some(d) = defs[j] {
            def_map.insert(d, j);
        }

        // Memory dependencies — only check entries with the same address
        if let Some(ref info_j) = mem_infos[j] {
            let j_write = info_j.3;

            if j_write {
                // WAW: depend on previous writes to the same address that alias
                if let Some(prev_writes) = mem_writes.get(&info_j.0) {
                    for &prev in prev_writes {
                        if let Some(ref info_prev) = mem_infos[prev] {
                            if ranges_alias(info_prev.1, info_prev.2, info_j.1, info_j.2) {
                                add_edge(prev, j, &mut succs, &mut indeg);
                            }
                        }
                    }
                }
                // WAR: depend on previous reads to the same address that alias
                if let Some(prev_reads) = mem_reads.get(&info_j.0) {
                    for &prev in prev_reads {
                        if let Some(ref info_prev) = mem_infos[prev] {
                            if ranges_alias(info_prev.1, info_prev.2, info_j.1, info_j.2) {
                                add_edge(prev, j, &mut succs, &mut indeg);
                            }
                        }
                    }
                }
                mem_writes.entry(info_j.0.clone()).or_default().push(j);
            } else {
                // RAW: depend on previous writes to the same address that alias
                if let Some(prev_writes) = mem_writes.get(&info_j.0) {
                    for &prev in prev_writes {
                        if let Some(ref info_prev) = mem_infos[prev] {
                            if ranges_alias(info_prev.1, info_prev.2, info_j.1, info_j.2) {
                                add_edge(prev, j, &mut succs, &mut indeg);
                            }
                        }
                    }
                }
                mem_reads.entry(info_j.0.clone()).or_default().push(j);
            }
        }
    }

    // Scheduling loop with incremental ready set
    let mut out = Vec::with_capacity(n);
    let mut inflight_loads: HashSet<RegisterId> = HashSet::default();
    let mut ready: Vec<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();

    while !ready.is_empty() {
        let pick = ready
            .iter()
            .copied()
            .find(|&i| matches!(window[i], SIRInstruction::Store(_, _, _, _, _, _)))
            .or_else(|| {
                if inflight_loads.len() < max_inflight_loads {
                    ready
                        .iter()
                        .copied()
                        .find(|&i| matches!(window[i], SIRInstruction::Load(_, _, _, _)))
                } else {
                    None
                }
            })
            .unwrap_or(ready[0]);

        ready.retain(|&x| x != pick);

        let inst = window[pick].clone();
        if let SIRInstruction::Load(dst, _, _, _) = inst {
            inflight_loads.insert(dst);
        }

        for r in &uses[pick] {
            inflight_loads.remove(r);
        }

        out.push(inst);

        // Update successors and add newly ready ones
        for &s in &succs[pick] {
            indeg[s] -= 1;
            if indeg[s] == 0 {
                let pos = ready.partition_point(|&x| x < s);
                ready.insert(pos, s);
            }
        }
    }

    out
}

pub(in crate::optimizer) fn schedule_instructions<A: Clone + PartialEq + Eq + std::hash::Hash>(
    instructions: &mut [SIRInstruction<A>],
    max_inflight_loads: usize,
) {
    let n = instructions.len();
    if n <= 2 {
        return;
    }

    let mut out: Vec<SIRInstruction<A>> = Vec::with_capacity(n);
    let mut begin = 0usize;

    for i in 0..n {
        if is_memory_barrier(&instructions[i]) {
            out.extend(schedule_block_interleaved(
                &instructions[begin..i],
                max_inflight_loads,
            ));
            out.push(instructions[i].clone());
            begin = i + 1;
        }
    }

    if begin < n {
        out.extend(schedule_block_interleaved(
            &instructions[begin..n],
            max_inflight_loads,
        ));
    }

    for (dst, src) in instructions.iter_mut().zip(out) {
        *dst = src;
    }
}

/// Coalesce contiguous static stores to the same address into a single wide
/// Concat + Store. Returns true if any coalescing was performed.
fn coalesce_static_stores<A: Clone + std::fmt::Debug + PartialEq + Ord + std::hash::Hash>(
    instructions: &mut Vec<SIRInstruction<A>>,
    register_map: &mut HashMap<RegisterId, RegisterType>,
    reg_counter: &mut usize,
    element_widths: &HashMap<A, usize>,
) -> bool {
    let next_id = reg_counter;

    type StoreGroupKey<A> = A;
    let mut groups: HashMap<StoreGroupKey<A>, Vec<usize>> = HashMap::default();

    for (idx, inst) in instructions.iter().enumerate() {
        if let SIRInstruction::Store(addr, SIROffset::Static(_), _, _, _, _) = inst {
            let key = addr.clone();
            groups.entry(key).or_default().push(idx);
        }
    }
    // A contiguous run made entirely of naturally aligned native-width
    // stores is deliberately left unchanged below. Exclude those groups
    // before building the load index and sorting detailed store metadata.
    groups.retain(|_, indices| {
        indices.len() >= 2
            && indices.iter().any(|&index| {
                matches!(
                    &instructions[index],
                    SIRInstruction::Store(
                        _,
                        SIROffset::Static(offset),
                        width,
                        _,
                        _,
                        _
                    ) if *offset % 8 != 0 || !matches!(*width, 8 | 16 | 32 | 64)
                )
            })
    });
    if groups.is_empty() {
        return false;
    }

    let mut replaced_indices = std::collections::HashSet::new();
    let mut insertions: HashMap<usize, Vec<SIRInstruction<A>>> = HashMap::default();

    // Pre-index loads by address for efficient safety checks.
    // Each entry is (instruction_index, offset, width, is_dynamic).
    let mut load_index: HashMap<A, Vec<(usize, Option<usize>, usize)>> = HashMap::default();
    for (idx, inst) in instructions.iter().enumerate() {
        match inst {
            SIRInstruction::Load(_, addr, SIROffset::Static(off), w) => {
                load_index
                    .entry(addr.clone())
                    .or_default()
                    .push((idx, Some(*off), *w));
            }
            SIRInstruction::Load(_, addr, SIROffset::PackedElements { bit_offset, .. }, w) => {
                load_index
                    .entry(addr.clone())
                    .or_default()
                    .push((idx, Some(*bit_offset), *w));
            }
            SIRInstruction::Load(_, addr, SIROffset::Dynamic(_) | SIROffset::Element { .. }, w) => {
                load_index
                    .entry(addr.clone())
                    .or_default()
                    .push((idx, None, *w));
            }
            _ => {}
        }
    }

    for (addr, indices) in groups {
        if indices.len() < 2 {
            continue;
        }

        struct StoreInfo {
            offset: usize,
            width: usize,
            index: usize,
            src: RegisterId,
            triggers: Vec<crate::ir::TriggerIdWithKind>,
            comb_capture_sites: Vec<u32>,
        }
        let mut details: Vec<StoreInfo> = Vec::new();

        for &idx in &indices {
            if let SIRInstruction::Store(_, SIROffset::Static(o), w, s, t, sites) =
                &instructions[idx]
            {
                details.push(StoreInfo {
                    offset: *o,
                    width: *w,
                    index: idx,
                    src: *s,
                    triggers: t.clone(),
                    comb_capture_sites: sites.clone(),
                });
            }
        }

        details.sort_by_key(|d| d.offset);

        // When the same (offset, width) is stored multiple times (e.g. SCC
        // unrolling stores to v[0] twice), only the LAST store matters — it
        // overwrites the earlier one.  Keep only the store with the highest
        // instruction index for each (offset, width) pair to prevent merging
        // stale first-pass values with fresh second-pass values.
        {
            let mut best: HashMap<(usize, usize), usize> = HashMap::default();
            for (i, d) in details.iter().enumerate() {
                best.entry((d.offset, d.width))
                    .and_modify(|prev| {
                        if details[*prev].index < d.index {
                            *prev = i;
                        }
                    })
                    .or_insert(i);
            }
            let keep: std::collections::HashSet<usize> = best.into_values().collect();
            let mut i = 0;
            details.retain(|_| {
                let k = keep.contains(&i);
                i += 1;
                k
            });
            // Re-sort after filtering
            details.sort_by_key(|d| d.offset);
        }

        // Get loads for this address once (empty slice if none)
        let addr_loads = load_index.get(&addr);

        let mut segment_start = 0;
        while segment_start < details.len() {
            let mut segment_end = segment_start;
            let mut expected_next_offset =
                details[segment_start].offset + details[segment_start].width;

            for (k, detail) in details.iter().enumerate().skip(segment_start + 1) {
                if detail.offset != expected_next_offset {
                    break;
                }
                expected_next_offset += detail.width;
                let aggregate_width = expected_next_offset - details[segment_start].offset;
                if aggregate_width > MAX_SCALAR_COALESCED_STORE_BITS {
                    break;
                }
                if aggregate_static_offset(
                    details[segment_start].offset,
                    aggregate_width,
                    element_widths.get(&addr).copied(),
                )
                .is_some()
                {
                    segment_end = k;
                } else if element_widths.get(&addr).is_some_and(|element_width| {
                    !details[segment_start].offset.is_multiple_of(*element_width)
                }) {
                    break;
                }
            }

            if segment_end > segment_start {
                let segment = &details[segment_start..=segment_end];

                let all_native = segment
                    .iter()
                    .all(|s| s.offset % 8 == 0 && matches!(s.width, 8 | 16 | 32 | 64));
                if all_native {
                    segment_start = segment_end + 1;
                    continue;
                }

                let insert_at_index = segment.iter().map(|s| s.index).max().unwrap();

                // Safety check: ensure no conflicting load between store and insert point.
                // Use pre-indexed loads to avoid scanning all instructions.
                let safe = if let Some(loads) = addr_loads {
                    segment.iter().all(|s| {
                        if s.index == insert_at_index {
                            return true;
                        }
                        // Check loads to this address in range (s.index, insert_at_index]
                        !loads.iter().any(|&(load_idx, load_off, load_w)| {
                            if load_idx <= s.index || load_idx > insert_at_index {
                                return false;
                            }
                            match load_off {
                                None => true, // dynamic offset — conservatively unsafe
                                Some(lo) => {
                                    let range1 = s.offset..(s.offset + s.width);
                                    let range2 = lo..(lo + load_w);
                                    range1.start < range2.end && range2.start < range1.end
                                }
                            }
                        })
                    })
                } else {
                    true // no loads to this address at all — always safe
                };

                if safe {
                    let total_width: usize = segment.iter().map(|s| s.width).sum();
                    let start_offset = segment[0].offset;
                    let args: Vec<RegisterId> = segment.iter().rev().map(|s| s.src).collect();
                    let triggers: Vec<crate::ir::TriggerIdWithKind> =
                        segment.iter().flat_map(|s| s.triggers.clone()).collect();
                    let comb_capture_sites = union_u32(
                        segment
                            .iter()
                            .flat_map(|s| s.comb_capture_sites.iter().copied()),
                    );

                    *next_id += 1;
                    while register_map.contains_key(&RegisterId(*next_id)) {
                        *next_id += 1;
                    }
                    let new_reg_id = RegisterId(*next_id);
                    register_map.insert(new_reg_id, RegisterType::Logic { width: total_width });

                    for s in segment {
                        replaced_indices.insert(s.index);
                    }

                    let Some(offset) = aggregate_static_offset(
                        start_offset,
                        total_width,
                        element_widths.get(&addr).copied(),
                    ) else {
                        segment_start = segment_end + 1;
                        continue;
                    };
                    let new_ops = vec![
                        SIRInstruction::Concat(new_reg_id, args),
                        SIRInstruction::Store(
                            addr.clone(),
                            offset,
                            total_width,
                            new_reg_id,
                            triggers,
                            comb_capture_sites,
                        ),
                    ];

                    insertions
                        .entry(insert_at_index)
                        .or_default()
                        .extend(new_ops);
                }
            }

            segment_start = segment_end + 1;
        }
    }

    if replaced_indices.is_empty() {
        return false;
    }

    let mut new_instructions = Vec::with_capacity(instructions.len());
    for (i, inst) in instructions.iter().enumerate() {
        if !replaced_indices.contains(&i) {
            new_instructions.push(inst.clone());
        }
        if let Some(ops) = insertions.remove(&i) {
            new_instructions.extend(ops);
        }
    }

    *instructions = new_instructions;
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AvailableStaticLoad {
    dst: RegisterId,
    load_offset: usize,
    load_width: usize,
    valid_start: usize,
    valid_end: usize,
}

#[derive(Default)]
struct AvailableStaticLoads {
    by_valid_start: BTreeMap<usize, Vec<AvailableStaticLoad>>,
    max_valid_width: usize,
}

impl AvailableStaticLoads {
    fn insert(&mut self, load: AvailableStaticLoad) {
        self.max_valid_width = self
            .max_valid_width
            .max(load.valid_end.saturating_sub(load.valid_start));
        self.by_valid_start
            .entry(load.valid_start)
            .or_default()
            .push(load);
    }

    fn containing(&self, start: usize, end: usize) -> impl Iterator<Item = &AvailableStaticLoad> {
        let earliest_start = start.saturating_sub(self.max_valid_width);
        self.by_valid_start
            .range(earliest_start..=start)
            .flat_map(|(_, loads)| loads)
            .filter(move |load| load.valid_start <= start && end <= load.valid_end)
    }

    /// Remove a statically written range from the still-current portions of
    /// prior loads. A write can split one loaded range into two valid
    /// fragments: the register still contains the old value on both
    /// non-overlapping sides.
    fn subtract_write(&mut self, write_start: usize, write_end: usize) {
        if write_start >= write_end {
            return;
        }

        let earliest_start = write_start.saturating_sub(self.max_valid_width.saturating_sub(1));
        let affected_starts = self
            .by_valid_start
            .range(earliest_start..write_end)
            .filter(|(_, loads)| {
                loads
                    .iter()
                    .any(|load| write_start < load.valid_end && load.valid_start < write_end)
            })
            .map(|(&start, _)| start)
            .collect::<Vec<_>>();

        for start in affected_starts {
            let loads = self
                .by_valid_start
                .remove(&start)
                .expect("affected load bucket must exist");
            for load in loads {
                if write_start >= load.valid_end || load.valid_start >= write_end {
                    self.insert(load);
                    continue;
                }
                if load.valid_start < write_start {
                    self.insert(AvailableStaticLoad {
                        valid_end: write_start,
                        ..load
                    });
                }
                if write_end < load.valid_end {
                    self.insert(AvailableStaticLoad {
                        valid_start: write_end,
                        ..load
                    });
                }
            }
        }
    }
}

/// Replace a later static Load with a Slice of a prior wider static Load when
/// the requested memory range has not changed in between. This keeps the
/// original destination register (and therefore its Bit/Logic type), while a
/// Slice preserves both value and mask planes in four-state execution.
fn subsume_static_loads<A: Clone + Eq + std::hash::Hash>(
    instructions: &mut [SIRInstruction<A>],
    register_map: &HashMap<RegisterId, RegisterType>,
) -> bool {
    let mut available: HashMap<A, AvailableStaticLoads> = HashMap::default();
    let mut changed = false;

    for inst in instructions {
        match inst {
            SIRInstruction::Load(dst, addr, SIROffset::Static(offset), width) => {
                let (dst, offset, width) = (*dst, *offset, *width);
                let Some(end) = offset.checked_add(width) else {
                    // An unrepresentable range cannot safely participate in
                    // containment arithmetic. Leave it to the verifier/runtime.
                    continue;
                };
                if width == 0 {
                    continue;
                }

                let exact_is_available = available.get(addr).is_some_and(|loads| {
                    loads
                        .containing(offset, end)
                        .any(|load| load.load_offset == offset && load.load_width == width)
                });
                if exact_is_available {
                    // Preserve the existing exact-load elimination path, which
                    // aliases the destination instead of introducing a Slice.
                    continue;
                }

                let source = available.get(addr).and_then(|loads| {
                    loads
                        .containing(offset, end)
                        .filter(|load| {
                            load.load_width > width
                                && match (register_map.get(&load.dst), register_map.get(&dst)) {
                                    (
                                        Some(RegisterType::Logic {
                                            width: source_width,
                                        }),
                                        Some(RegisterType::Logic {
                                            width: destination_width,
                                        }),
                                    ) => {
                                        *source_width == load.load_width
                                            && *destination_width == width
                                    }
                                    (
                                        Some(RegisterType::Bit {
                                            width: source_width,
                                            signed: source_signed,
                                        }),
                                        Some(RegisterType::Bit {
                                            width: destination_width,
                                            signed: destination_signed,
                                        }),
                                    ) => {
                                        *source_width == load.load_width
                                            && *destination_width == width
                                            && source_signed == destination_signed
                                    }
                                    _ => false,
                                }
                        })
                        .min_by_key(|load| load.load_width)
                        .copied()
                });
                if let Some(source) = source {
                    let Some(relative_offset) = offset.checked_sub(source.load_offset) else {
                        continue;
                    };
                    let Some(relative_end) = relative_offset.checked_add(width) else {
                        continue;
                    };
                    if relative_end <= source.load_width {
                        *inst = SIRInstruction::Slice(dst, source.dst, relative_offset, width);
                        changed = true;
                        continue;
                    }
                }

                available
                    .entry(addr.clone())
                    .or_default()
                    .insert(AvailableStaticLoad {
                        dst,
                        load_offset: offset,
                        load_width: width,
                        valid_start: offset,
                        valid_end: end,
                    });
            }
            SIRInstruction::Load(_, _, SIROffset::Dynamic(_), _)
            | SIRInstruction::Load(_, _, SIROffset::Element { .. }, _)
            | SIRInstruction::Load(_, _, SIROffset::PackedElements { .. }, _)
            | SIRInstruction::Store(_, SIROffset::Dynamic(_), _, _, _, _)
            | SIRInstruction::Store(_, SIROffset::Element { .. }, _, _, _, _)
            | SIRInstruction::Store(_, SIROffset::PackedElements { .. }, _, _, _, _)
            | SIRInstruction::Commit(_, _, SIROffset::Dynamic(_), _, _)
            | SIRInstruction::Commit(_, _, SIROffset::Element { .. }, _, _)
            | SIRInstruction::Commit(_, _, SIROffset::PackedElements { .. }, _, _) => {
                // Dynamic ranges are deliberately a global barrier. The address
                // is known, but keeping this rule conservative avoids depending
                // on alias properties not represented in SIR.
                available.clear();
            }
            SIRInstruction::Store(addr, SIROffset::Static(offset), width, _, triggers, sites) => {
                if !triggers.is_empty() || !sites.is_empty() {
                    available.clear();
                    continue;
                }
                let Some(write_end) = offset.checked_add(*width) else {
                    available.clear();
                    continue;
                };
                if let Some(loads) = available.get_mut(addr) {
                    loads.subtract_write(*offset, write_end);
                }
            }
            SIRInstruction::Commit(_, dst, SIROffset::Static(offset), width, triggers) => {
                if !triggers.is_empty() {
                    available.clear();
                    continue;
                }
                let Some(write_end) = offset.checked_add(*width) else {
                    available.clear();
                    continue;
                };
                if let Some(loads) = available.get_mut(dst) {
                    loads.subtract_write(*offset, write_end);
                }
            }
            SIRInstruction::RuntimeEvent { .. }
            | SIRInstruction::CombCaptureEvent { .. }
            | SIRInstruction::CombCaptureEnableIfChanged { .. } => {
                available.clear();
            }
            SIRInstruction::Imm(..)
            | SIRInstruction::Binary(..)
            | SIRInstruction::Unary(..)
            | SIRInstruction::Concat(..)
            | SIRInstruction::Slice(..)
            | SIRInstruction::Mux(..) => {}
        }
    }

    changed
}

pub(in crate::optimizer) fn optimize_block<
    A: Clone + std::fmt::Debug + PartialEq + Eq + Ord + std::hash::Hash,
>(
    block: &mut BasicBlock<A>,
    register_map: &mut HashMap<RegisterId, RegisterType>,
    unit_replacement_map: &mut HashMap<RegisterId, RegisterId>,
    reg_counter: &mut usize,
    skip_final_schedule: bool,
    four_state: bool,
    element_widths: &HashMap<A, usize>,
) {
    const MAX_INFLIGHT_LOADS: usize = 8;
    coalesce_static_loads(
        &mut block.instructions,
        register_map,
        reg_counter,
        four_state,
        element_widths,
    );

    // First pass: coalesce stores that are safe even with intermediate loads present
    coalesce_static_stores(
        &mut block.instructions,
        register_map,
        reg_counter,
        element_widths,
    );

    // Reuse already-loaded wide static regions before exact-load forwarding.
    // Turning contained loads into pure Slices can also make more store groups
    // eligible for the second coalescing pass below.
    subsume_static_loads(&mut block.instructions, register_map);

    let mut local_replacement_map = HashMap::default();
    eliminate_redundant_loads(
        &mut block.instructions,
        &mut local_replacement_map,
        register_map,
    );

    // Second pass: after eliminate_redundant_loads removed store-forwarded loads,
    // previously-unsafe store groups may now be safe to coalesce
    coalesce_static_stores(
        &mut block.instructions,
        register_map,
        reg_counter,
        element_widths,
    );

    for (from, to) in local_replacement_map {
        unit_replacement_map.insert(from, to);
        replace_reg_in_terminator(&mut block.terminator, from, to);
    }

    // Skip scheduling if the reschedule pass will run afterward on this EU
    if !skip_final_schedule {
        schedule_instructions(block.instructions.as_mut_slice(), MAX_INFLIGHT_LOADS);
    }
}

fn coalesce_static_loads<A: Clone + std::fmt::Debug + PartialEq + Ord + std::hash::Hash>(
    instructions: &mut Vec<SIRInstruction<A>>,
    register_map: &mut HashMap<RegisterId, RegisterType>,
    reg_counter: &mut usize,
    four_state: bool,
    element_widths: &HashMap<A, usize>,
) {
    #[derive(Clone)]
    struct LoadInfo {
        index: usize,
        dst: RegisterId,
        offset: usize,
        width: usize,
    }

    #[derive(Clone)]
    struct Segment<A> {
        addr: A,
        loads: Vec<LoadInfo>,
    }

    fn next_reg_id(map: &HashMap<RegisterId, RegisterType>, counter: &mut usize) -> RegisterId {
        *counter += 1;
        while map.contains_key(&RegisterId(*counter)) {
            *counter += 1;
        }
        RegisterId(*counter)
    }

    let mut segments: Vec<Segment<A>> = Vec::new();
    let mut active: HashMap<A, usize> = HashMap::default();

    for (idx, inst) in instructions.iter().enumerate() {
        match inst {
            SIRInstruction::Load(dst, addr, SIROffset::Static(off), width) if *width > 0 => {
                let seg_id = if let Some(seg_id) = active.get(addr).copied() {
                    seg_id
                } else {
                    let seg_id = segments.len();
                    segments.push(Segment {
                        addr: addr.clone(),
                        loads: Vec::new(),
                    });
                    active.insert(addr.clone(), seg_id);
                    seg_id
                };
                segments[seg_id].loads.push(LoadInfo {
                    index: idx,
                    dst: *dst,
                    offset: *off,
                    width: *width,
                });
            }
            SIRInstruction::Store(addr, _, _, _, _, _) => {
                active.remove(addr);
            }
            SIRInstruction::Commit(_, dst, _, _, _) => {
                active.remove(dst);
            }
            _ => {}
        }
    }

    if segments.is_empty() {
        return;
    }

    let mut insertions: HashMap<usize, Vec<SIRInstruction<A>>> = HashMap::default();
    let mut replacements: HashMap<usize, Vec<SIRInstruction<A>>> = HashMap::default();

    for seg in segments {
        if seg.loads.len() < 2
            || seg
                .loads
                .iter()
                .all(|load| load.offset % 8 == 0 && matches!(load.width, 8 | 16 | 32 | 64))
        {
            continue;
        }

        let element_width = element_widths.get(&seg.addr).copied();
        let scalar_observed_end = element_width.is_none().then(|| {
            seg.loads
                .iter()
                .filter_map(|load| load.offset.checked_add(load.width))
                .max()
                .unwrap_or(0)
        });
        let mut by_word: HashMap<(usize, usize), Vec<LoadInfo>> = HashMap::default();
        for ld in seg.loads {
            if ld.width == 0 || ld.width > 64 {
                continue;
            }
            let (word_base, word_width) = if element_width.is_some_and(|element_width| {
                is_complete_element_access(ld.offset, ld.width, element_width)
            }) {
                // Keep the bucket bounded, then choose the exact covered span
                // below. Unlike an ordinary Static load, the resulting
                // PackedElements access may legally cross semantic elements.
                ((ld.offset / 64) * 64, 0)
            } else if let Some(element_width) = element_width {
                if element_width == 0 {
                    continue;
                }
                let element_base = (ld.offset / element_width) * element_width;
                let within_element = ld.offset - element_base;
                let chunk_base = (within_element / 64) * 64;
                (
                    element_base + chunk_base,
                    (element_width - chunk_base).min(64),
                )
            } else {
                ((ld.offset / 64) * 64, 64)
            };
            if word_width == 0
                || ld
                    .offset
                    .checked_add(ld.width)
                    .zip(word_base.checked_add(word_width))
                    .is_some_and(|(load_end, word_end)| load_end <= word_end)
            {
                by_word.entry((word_base, word_width)).or_default().push(ld);
            }
        }

        for ((mut word_base, mut word_width), mut loads) in by_word {
            if loads.len() < 2 {
                continue;
            }
            // A scalar has no entry in `element_widths`.  The 64-bit bucket is
            // only a grouping aid; it is not evidence that the backing object
            // is 64 bits wide. Keep the combined load within the extent proven
            // readable by any original load from this scalar. This preserves a
            // native word load when a covering wide load already proves it safe.
            if let Some(observed_end) = scalar_observed_end {
                word_width = observed_end.saturating_sub(word_base).min(word_width);
            }
            if word_width == 0 {
                word_base = loads.iter().map(|load| load.offset).min().unwrap();
                let Some(word_end) = loads
                    .iter()
                    .filter_map(|load| load.offset.checked_add(load.width))
                    .max()
                else {
                    continue;
                };
                word_width = word_end - word_base;
                if word_width == 0 || word_width > 64 {
                    continue;
                }
            }

            let all_native = loads
                .iter()
                .all(|ld| ld.offset % 8 == 0 && matches!(ld.width, 8 | 16 | 32 | 64));
            if all_native {
                continue;
            }

            loads.sort_by_key(|x| x.index);
            let insert_idx = loads[0].index;

            let Some(offset) = aggregate_static_offset(word_base, word_width, element_width) else {
                continue;
            };
            let wide_reg = next_reg_id(register_map, reg_counter);
            register_map.insert(wide_reg, RegisterType::Logic { width: word_width });
            insertions
                .entry(insert_idx)
                .or_default()
                .push(SIRInstruction::Load(
                    wide_reg,
                    seg.addr.clone(),
                    offset,
                    word_width,
                ));

            for ld in loads {
                let rel_off = ld.offset - word_base;
                if four_state || element_width.is_some() {
                    replacements.insert(
                        ld.index,
                        vec![SIRInstruction::Slice(ld.dst, wide_reg, rel_off, ld.width)],
                    );
                    continue;
                }
                let mut ops: Vec<SIRInstruction<A>> = Vec::new();
                let mut source_reg = wide_reg;

                if rel_off != 0 {
                    let shift_reg = next_reg_id(register_map, reg_counter);
                    register_map.insert(shift_reg, RegisterType::Logic { width: 64 });
                    ops.push(SIRInstruction::Imm(
                        shift_reg,
                        SIRValue::new(rel_off as u64),
                    ));

                    let shifted_reg = next_reg_id(register_map, reg_counter);
                    register_map.insert(shifted_reg, RegisterType::Logic { width: 64 });
                    ops.push(SIRInstruction::Binary(
                        shifted_reg,
                        source_reg,
                        BinaryOp::Shr,
                        shift_reg,
                    ));
                    source_reg = shifted_reg;
                }

                if ld.width < 64 {
                    let mask_reg = next_reg_id(register_map, reg_counter);
                    register_map.insert(mask_reg, RegisterType::Logic { width: 64 });
                    let mask = if ld.width == 64 {
                        BigUint::from(u64::MAX)
                    } else {
                        let one = BigUint::from(1u8);
                        (one.clone() << ld.width) - one
                    };
                    ops.push(SIRInstruction::Imm(mask_reg, SIRValue::new(mask)));
                    ops.push(SIRInstruction::Binary(
                        ld.dst,
                        source_reg,
                        BinaryOp::And,
                        mask_reg,
                    ));
                } else {
                    let zero_reg = next_reg_id(register_map, reg_counter);
                    register_map.insert(zero_reg, RegisterType::Logic { width: 64 });
                    ops.push(SIRInstruction::Imm(zero_reg, SIRValue::new(0u8)));
                    ops.push(SIRInstruction::Binary(
                        ld.dst,
                        source_reg,
                        BinaryOp::Or,
                        zero_reg,
                    ));
                }

                replacements.insert(ld.index, ops);
            }
        }
    }

    if insertions.is_empty() && replacements.is_empty() {
        return;
    }

    let mut out = Vec::with_capacity(instructions.len() * 2);
    for (i, inst) in instructions.iter().enumerate() {
        if let Some(ops) = insertions.remove(&i) {
            out.extend(ops);
        }

        if let Some(ops) = replacements.remove(&i) {
            out.extend(ops);
        } else {
            out.push(inst.clone());
        }
    }

    *instructions = out;
}

#[derive(Default)]
struct KnownAddressValues {
    static_values: BTreeMap<usize, (RegisterId, usize)>,
    other_values: HashMap<SIROffset, (RegisterId, usize)>,
    max_static_width: usize,
}

impl KnownAddressValues {
    fn get(&self, offset: &SIROffset) -> Option<(RegisterId, usize)> {
        match offset {
            SIROffset::Static(start) => self.static_values.get(start).copied(),
            _ => self.other_values.get(offset).copied(),
        }
    }

    fn insert(&mut self, offset: SIROffset, value: (RegisterId, usize)) {
        match offset {
            SIROffset::Static(start) => {
                self.max_static_width = self.max_static_width.max(value.1);
                self.static_values.insert(start, value);
            }
            offset => {
                self.other_values.insert(offset, value);
            }
        }
    }

    fn invalidate_store(&mut self, offset: &SIROffset, width: usize) {
        let SIROffset::Static(store_start) = offset else {
            self.static_values.clear();
            self.other_values.clear();
            self.max_static_width = 0;
            return;
        };

        self.other_values.clear();
        let store_end = *store_start + width;
        let earliest_overlap = store_start.saturating_sub(self.max_static_width.saturating_sub(1));
        let invalidated = self
            .static_values
            .range(earliest_overlap..store_end)
            .filter_map(|(&load_start, &(_, load_width))| {
                let load_end = load_start + load_width;
                (*store_start < load_end && load_start < store_end).then_some(load_start)
            })
            .collect::<Vec<_>>();
        for start in invalidated {
            self.static_values.remove(&start);
        }
    }
}

fn eliminate_redundant_loads<A: Clone + std::fmt::Debug + PartialEq + Ord + std::hash::Hash>(
    instructions: &mut Vec<SIRInstruction<A>>,
    replacement_map: &mut HashMap<RegisterId, RegisterId>,
    register_map: &HashMap<RegisterId, RegisterType>,
) {
    let mut known_values: HashMap<A, KnownAddressValues> = HashMap::default();
    let mut new_instructions = Vec::with_capacity(instructions.len());

    for mut inst in instructions.drain(..) {
        match &mut inst {
            SIRInstruction::Binary(_, lhs, _, rhs) => {
                if let Some(r) = replacement_map.get(lhs) {
                    *lhs = *r;
                }
                if let Some(r) = replacement_map.get(rhs) {
                    *rhs = *r;
                }
            }
            SIRInstruction::Unary(_, _, src) => {
                if let Some(r) = replacement_map.get(src) {
                    *src = *r;
                }
            }
            SIRInstruction::Store(_, offset, _, src, _, _) => {
                for register in offset.dynamic_registers().into_iter().flatten() {
                    if let Some(replacement) = replacement_map.get(&register) {
                        match offset {
                            SIROffset::Dynamic(current) if *current == register => {
                                *current = *replacement;
                            }
                            SIROffset::Element {
                                index,
                                dynamic_bit_offset,
                                ..
                            } => {
                                if *index == register {
                                    *index = *replacement;
                                }
                                if dynamic_bit_offset.as_ref() == Some(&register) {
                                    *dynamic_bit_offset = Some(*replacement);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                if let Some(r) = replacement_map.get(src) {
                    *src = *r;
                }
            }
            SIRInstruction::Load(_, _, offset, _) | SIRInstruction::Commit(_, _, offset, _, _) => {
                match offset {
                    SIROffset::Static(_) | SIROffset::PackedElements { .. } => {}
                    SIROffset::Dynamic(register) => {
                        if let Some(replacement) = replacement_map.get(register) {
                            *register = *replacement;
                        }
                    }
                    SIROffset::Element {
                        index,
                        dynamic_bit_offset,
                        ..
                    } => {
                        if let Some(replacement) = replacement_map.get(index) {
                            *index = *replacement;
                        }
                        if let Some(dynamic) = dynamic_bit_offset
                            && let Some(replacement) = replacement_map.get(dynamic)
                        {
                            *dynamic = *replacement;
                        }
                    }
                }
            }
            SIRInstruction::Concat(_, args) => {
                for arg in args {
                    if let Some(r) = replacement_map.get(arg) {
                        *arg = *r;
                    }
                }
            }
            _ => {}
        }

        match &inst {
            SIRInstruction::Load(dst, addr, offset, width) => {
                if let Some((existing_reg, existing_width)) =
                    known_values.get(addr).and_then(|values| values.get(offset))
                    && existing_width == *width
                    && register_map
                        .get(&existing_reg)
                        .zip(register_map.get(dst))
                        .is_some_and(|(existing, destination)| existing == destination)
                {
                    replacement_map.insert(*dst, existing_reg);
                    continue;
                }

                known_values
                    .entry(addr.clone())
                    .or_default()
                    .insert(offset.clone(), (*dst, *width));
                new_instructions.push(inst);
            }
            SIRInstruction::Store(addr, offset, width, src, _, _) => {
                let values = known_values.entry(addr.clone()).or_default();
                values.invalidate_store(offset, *width);
                if matches!(offset, SIROffset::Static(_)) {
                    values.insert(offset.clone(), (*src, *width));
                }

                new_instructions.push(inst);
            }
            SIRInstruction::Commit(src_addr, dst_addr, offset, width, triggers) => {
                known_values
                    .entry(dst_addr.clone())
                    .or_default()
                    .invalidate_store(offset, *width);

                if let Some((src_reg, src_width)) = known_values
                    .get(src_addr)
                    .and_then(|values| values.get(offset))
                    && src_width == *width
                    && register_map.get(&src_reg) == Some(&RegisterType::Logic { width: *width })
                {
                    known_values
                        .entry(dst_addr.clone())
                        .or_default()
                        .insert(offset.clone(), (src_reg, *width));
                    new_instructions.push(SIRInstruction::Store(
                        dst_addr.clone(),
                        offset.clone(),
                        *width,
                        src_reg,
                        triggers.clone(),
                        Vec::new(),
                    ));
                    continue;
                }

                new_instructions.push(inst);
            }
            _ => {
                new_instructions.push(inst);
            }
        }
    }

    *instructions = new_instructions;
}

#[cfg(test)]
mod tests {
    use super::{
        AvailableStaticLoad, AvailableStaticLoads, KnownAddressValues,
        MAX_SCALAR_COALESCED_STORE_BITS, aggregate_static_offset,
        coalesce_static_loads as coalesce_static_loads_with_types,
        coalesce_static_stores as coalesce_static_stores_with_types, optimize_block,
        subsume_static_loads as subsume_static_loads_with_types,
    };
    use crate::HashMap;
    use crate::ir::{
        BasicBlock, BlockId, ExecutionUnit, RegisterId, RegisterType, SIRInstruction, SIROffset,
        SIRTerminator, SIRValue,
    };

    fn logic(width: usize) -> RegisterType {
        RegisterType::Logic { width }
    }

    fn subsume_static_loads(instructions: &mut [SIRInstruction<u32>]) -> bool {
        let register_map = instructions
            .iter()
            .filter_map(|inst| match inst {
                SIRInstruction::Load(dst, _, _, width) => Some((*dst, logic(*width))),
                SIRInstruction::Imm(dst, _) => Some((
                    *dst,
                    RegisterType::Bit {
                        width: 64,
                        signed: false,
                    },
                )),
                _ => None,
            })
            .collect();
        subsume_static_loads_with_types(instructions, &register_map)
    }

    #[test]
    fn known_values_invalidate_only_overlapping_static_ranges() {
        let mut values = KnownAddressValues::default();
        values.insert(SIROffset::Static(0), (RegisterId(0), 128));
        values.insert(SIROffset::Static(160), (RegisterId(1), 32));
        values.insert(SIROffset::Dynamic(RegisterId(9)), (RegisterId(2), 32));

        values.invalidate_store(&SIROffset::Static(96), 32);

        assert_eq!(values.get(&SIROffset::Static(0)), None);
        assert_eq!(
            values.get(&SIROffset::Static(160)),
            Some((RegisterId(1), 32))
        );
        assert_eq!(values.get(&SIROffset::Dynamic(RegisterId(9))), None);
    }

    #[test]
    fn dynamic_store_invalidates_every_known_range() {
        let mut values = KnownAddressValues::default();
        values.insert(SIROffset::Static(0), (RegisterId(0), 32));
        values.insert(
            SIROffset::PackedElements {
                bit_offset: 32,
                element_width: 8,
            },
            (RegisterId(1), 32),
        );

        values.invalidate_store(&SIROffset::Dynamic(RegisterId(9)), 32);

        assert_eq!(values.get(&SIROffset::Static(0)), None);
        assert_eq!(
            values.get(&SIROffset::PackedElements {
                bit_offset: 32,
                element_width: 8,
            }),
            None
        );
        assert_eq!(values.max_static_width, 0);
    }

    #[test]
    fn available_load_index_matches_naive_range_updates() {
        let original = (0..12usize)
            .map(|index| {
                let start = index * 11 % 70;
                let width = index * 13 % 31 + 1;
                AvailableStaticLoad {
                    dst: RegisterId(index),
                    load_offset: start,
                    load_width: width,
                    valid_start: start,
                    valid_end: start + width,
                }
            })
            .collect::<Vec<_>>();
        let sort = |loads: &mut Vec<AvailableStaticLoad>| {
            loads.sort_by_key(|load| {
                (
                    load.dst,
                    load.valid_start,
                    load.valid_end,
                    load.load_offset,
                    load.load_width,
                )
            });
        };

        let mut untouched = AvailableStaticLoads::default();
        for &load in &original {
            untouched.insert(load);
        }
        for start in 0..96 {
            for width in 0..24 {
                let end = start + width;
                let mut expected = original
                    .iter()
                    .filter(|load| load.valid_start <= start && end <= load.valid_end)
                    .copied()
                    .collect::<Vec<_>>();
                let mut actual = untouched
                    .containing(start, end)
                    .copied()
                    .collect::<Vec<_>>();
                sort(&mut expected);
                sort(&mut actual);
                assert_eq!(actual, expected, "containing range {start}..{end}");
            }
        }

        for write_start in 0..96 {
            for write_width in 0..24 {
                let write_end = write_start + write_width;
                let mut expected = Vec::new();
                for &load in &original {
                    if write_start >= write_end
                        || write_start >= load.valid_end
                        || load.valid_start >= write_end
                    {
                        expected.push(load);
                        continue;
                    }
                    if load.valid_start < write_start {
                        expected.push(AvailableStaticLoad {
                            valid_end: write_start,
                            ..load
                        });
                    }
                    if write_end < load.valid_end {
                        expected.push(AvailableStaticLoad {
                            valid_start: write_end,
                            ..load
                        });
                    }
                }

                let mut indexed = AvailableStaticLoads::default();
                for &load in &original {
                    indexed.insert(load);
                }
                indexed.subtract_write(write_start, write_end);
                let mut actual = indexed
                    .by_valid_start
                    .into_values()
                    .flatten()
                    .collect::<Vec<_>>();
                sort(&mut expected);
                sort(&mut actual);
                assert_eq!(
                    actual, expected,
                    "subtracting write range {write_start}..{write_end}"
                );
            }
        }
    }

    fn verify(
        instructions: Vec<SIRInstruction<u32>>,
        registers: impl IntoIterator<Item = (RegisterId, RegisterType)>,
    ) {
        let block = BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions,
            terminator: SIRTerminator::Return,
        };
        let unit = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(BlockId(0), block)].into_iter().collect(),
            register_map: registers.into_iter().collect(),
        };
        unit.verify_result().unwrap();
    }

    #[test]
    fn many_contained_static_loads_become_slices_of_one_full_load() {
        let mut instructions = vec![SIRInstruction::Load(
            RegisterId(0),
            7,
            SIROffset::Static(0),
            128,
        )];
        let mut registers = vec![(RegisterId(0), logic(128))];
        for index in 0..1_018usize {
            let width = index % 127 + 1;
            let dst = RegisterId(index + 1);
            instructions.push(SIRInstruction::Load(dst, 7, SIROffset::Static(0), width));
            registers.push((dst, logic(width)));
        }

        assert!(subsume_static_loads(&mut instructions));
        assert_eq!(
            instructions
                .iter()
                .filter(|inst| matches!(inst, SIRInstruction::Load(..)))
                .count(),
            1
        );
        assert_eq!(
            instructions
                .iter()
                .filter(|inst| matches!(inst, SIRInstruction::Slice(..)))
                .count(),
            1_018
        );
        verify(instructions, registers);
    }

    #[test]
    fn covering_wide_load_does_not_disable_word_load_coalescing() {
        let mut instructions = vec![
            SIRInstruction::Load(RegisterId(0), 7u32, SIROffset::Static(0), 128),
            SIRInstruction::Load(RegisterId(1), 7, SIROffset::Static(65), 3),
            SIRInstruction::Load(RegisterId(2), 7, SIROffset::Static(66), 2),
        ];
        let mut register_map = [
            (RegisterId(0), logic(128)),
            (RegisterId(1), logic(3)),
            (RegisterId(2), logic(2)),
        ]
        .into_iter()
        .collect();
        let mut reg_counter = 2;

        coalesce_static_loads_with_types(
            &mut instructions,
            &mut register_map,
            &mut reg_counter,
            false,
            &HashMap::default(),
        );

        assert_eq!(
            instructions
                .iter()
                .filter(|instruction| matches!(instruction, SIRInstruction::Load(..)))
                .count(),
            2
        );
        assert!(instructions.iter().any(|instruction| matches!(
            instruction,
            SIRInstruction::Load(_, 7, SIROffset::Static(64), 64)
        )));
        assert!(instructions.iter().all(|instruction| !matches!(
            instruction,
            SIRInstruction::Load(RegisterId(1) | RegisterId(2), _, _, _)
        )));
        verify(instructions, register_map);
    }

    #[test]
    fn scalar_partial_load_coalescing_stays_within_the_accessed_span() {
        let mut instructions = vec![
            SIRInstruction::Load(RegisterId(0), 7u32, SIROffset::Static(0), 16),
            SIRInstruction::Load(RegisterId(1), 7, SIROffset::Static(16), 2),
            SIRInstruction::Load(RegisterId(2), 7, SIROffset::Static(18), 14),
        ];
        let mut register_map = [
            (RegisterId(0), logic(16)),
            (RegisterId(1), logic(2)),
            (RegisterId(2), logic(14)),
        ]
        .into_iter()
        .collect();
        let mut reg_counter = 2;

        coalesce_static_loads_with_types(
            &mut instructions,
            &mut register_map,
            &mut reg_counter,
            false,
            &HashMap::default(),
        );

        assert!(instructions.iter().any(|instruction| matches!(
            instruction,
            SIRInstruction::Load(_, 7, SIROffset::Static(0), 32)
        )));
        assert!(instructions.iter().all(|instruction| !matches!(
            instruction,
            SIRInstruction::Load(_, 7, SIROffset::Static(0), 64)
        )));
        verify(instructions, register_map);
    }

    #[test]
    fn unpacked_array_load_coalescing_uses_explicit_packed_elements() {
        let mut instructions = vec![
            SIRInstruction::Load(RegisterId(0), 7u32, SIROffset::Static(0), 12),
            SIRInstruction::Load(RegisterId(1), 7, SIROffset::Static(12), 12),
        ];
        let mut register_map = [(RegisterId(0), logic(12)), (RegisterId(1), logic(12))]
            .into_iter()
            .collect();
        let mut reg_counter = 1;

        coalesce_static_loads_with_types(
            &mut instructions,
            &mut register_map,
            &mut reg_counter,
            false,
            &[(7u32, 12usize)].into_iter().collect(),
        );

        assert_eq!(
            instructions,
            vec![
                SIRInstruction::Load(
                    RegisterId(2),
                    7,
                    SIROffset::PackedElements {
                        bit_offset: 0,
                        element_width: 12,
                    },
                    24,
                ),
                SIRInstruction::Slice(RegisterId(0), RegisterId(2), 0, 12),
                SIRInstruction::Slice(RegisterId(1), RegisterId(2), 12, 12),
            ]
        );
        verify(instructions, register_map);
    }

    #[test]
    fn four_state_load_coalescing_extracts_with_slices() {
        let mut instructions = vec![
            SIRInstruction::Load(RegisterId(0), 7u32, SIROffset::Static(0), 5),
            SIRInstruction::Load(RegisterId(1), 7, SIROffset::Static(4), 1),
        ];
        let mut register_map = [(RegisterId(0), logic(5)), (RegisterId(1), logic(1))]
            .into_iter()
            .collect();
        let mut reg_counter = 1;

        coalesce_static_loads_with_types(
            &mut instructions,
            &mut register_map,
            &mut reg_counter,
            true,
            &HashMap::default(),
        );

        assert_eq!(
            instructions
                .iter()
                .filter(|instruction| matches!(instruction, SIRInstruction::Slice(..)))
                .count(),
            2
        );
        assert!(
            instructions
                .iter()
                .all(|instruction| !matches!(instruction, SIRInstruction::Binary(..)))
        );
        verify(instructions, register_map);
    }

    #[test]
    fn naturally_aligned_native_loads_skip_coalescing() {
        let mut instructions = vec![
            SIRInstruction::Load(RegisterId(0), 7u32, SIROffset::Static(0), 32),
            SIRInstruction::Load(RegisterId(1), 7u32, SIROffset::Static(32), 32),
        ];
        let original = instructions.clone();
        let mut register_map = [(RegisterId(0), logic(32)), (RegisterId(1), logic(32))]
            .into_iter()
            .collect();
        let mut reg_counter = 1;

        coalesce_static_loads_with_types(
            &mut instructions,
            &mut register_map,
            &mut reg_counter,
            false,
            &HashMap::default(),
        );

        assert_eq!(instructions, original);
        assert_eq!(reg_counter, 1);
    }

    #[test]
    fn store_coalescing_marks_cross_element_transfer_explicitly() {
        let mut instructions = (0..4)
            .map(|index| SIRInstruction::Imm(RegisterId(index), SIRValue::new(index as u8)))
            .chain((0..4).map(|index| {
                SIRInstruction::Store(
                    7u32,
                    SIROffset::Static(index * 6),
                    6,
                    RegisterId(index),
                    vec![],
                    vec![],
                )
            }))
            .collect::<Vec<_>>();
        let mut register_map = (0..4).map(|index| (RegisterId(index), logic(6))).collect();
        let mut reg_counter = 3;

        assert!(coalesce_static_stores_with_types(
            &mut instructions,
            &mut register_map,
            &mut reg_counter,
            &[(7u32, 12usize)].into_iter().collect(),
        ));

        let stores = instructions
            .iter()
            .filter_map(|instruction| match instruction {
                SIRInstruction::Store(
                    _,
                    SIROffset::PackedElements {
                        bit_offset,
                        element_width,
                    },
                    width,
                    ..,
                ) => Some((*bit_offset, *element_width, *width)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(stores, vec![(0, 12, 24)]);
        verify(instructions, register_map);
    }

    #[test]
    fn naturally_aligned_native_stores_skip_coalescing() {
        let mut instructions = vec![
            SIRInstruction::Store(
                7u32,
                SIROffset::Static(0),
                32,
                RegisterId(0),
                vec![],
                vec![],
            ),
            SIRInstruction::Store(
                7u32,
                SIROffset::Static(32),
                32,
                RegisterId(1),
                vec![],
                vec![],
            ),
        ];
        let original = instructions.clone();
        let mut register_map = [(RegisterId(0), logic(32)), (RegisterId(1), logic(32))]
            .into_iter()
            .collect();
        let mut reg_counter = 1;

        assert!(!coalesce_static_stores_with_types(
            &mut instructions,
            &mut register_map,
            &mut reg_counter,
            &HashMap::default(),
        ));
        assert_eq!(instructions, original);
        assert_eq!(reg_counter, 1);
    }

    #[test]
    fn scalar_store_coalescing_stops_at_64_bits() {
        let mut instructions = (0..8)
            .map(|index| SIRInstruction::Imm(RegisterId(index), SIRValue::new(index as u8)))
            .chain((0..8).map(|index| {
                SIRInstruction::Store(
                    7u32,
                    SIROffset::Static(index * 10),
                    10,
                    RegisterId(index),
                    vec![],
                    vec![],
                )
            }))
            .collect::<Vec<_>>();
        let mut register_map = (0..8).map(|index| (RegisterId(index), logic(10))).collect();
        let mut reg_counter = 7;

        assert!(coalesce_static_stores_with_types(
            &mut instructions,
            &mut register_map,
            &mut reg_counter,
            &[(7u32, 10usize)].into_iter().collect(),
        ));

        let stores = instructions
            .iter()
            .filter_map(|instruction| match instruction {
                SIRInstruction::Store(_, _, width, ..) => Some(*width),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(stores, vec![60, 20]);
        assert!(
            stores
                .iter()
                .all(|width| *width <= MAX_SCALAR_COALESCED_STORE_BITS)
        );
        verify(instructions, register_map);
    }

    #[test]
    fn packed_element_aggregate_requires_complete_aligned_elements() {
        assert_eq!(
            aggregate_static_offset(27, 54, Some(27)),
            Some(SIROffset::PackedElements {
                bit_offset: 27,
                element_width: 27,
            })
        );
        assert_eq!(
            aggregate_static_offset(774, 63, Some(27)),
            None,
            "a partial first and last element is not an array-copy operation"
        );
        assert_eq!(
            aggregate_static_offset(774, 9, Some(27)),
            Some(SIROffset::Static(774)),
            "an ordinary access within one element remains static"
        );
        assert_eq!(
            aggregate_static_offset(0, 64, Some(1)),
            Some(SIROffset::PackedElements {
                bit_offset: 0,
                element_width: 1,
            })
        );
    }

    #[test]
    fn wide_cross_chunk_logic_load_preserves_destination_and_verifies() {
        let mut instructions = vec![
            SIRInstruction::Load(RegisterId(0), 1, SIROffset::Static(100), 256),
            SIRInstruction::Load(RegisterId(1), 1, SIROffset::Static(163), 129),
        ];

        assert!(subsume_static_loads(&mut instructions));
        assert_eq!(
            instructions[1],
            SIRInstruction::Slice(RegisterId(1), RegisterId(0), 63, 129)
        );
        verify(
            instructions,
            [(RegisterId(0), logic(256)), (RegisterId(1), logic(129))],
        );
    }

    #[test]
    fn overlapping_static_store_blocks_subsumption() {
        let mut instructions = vec![
            SIRInstruction::Load(RegisterId(0), 1, SIROffset::Static(0), 128),
            SIRInstruction::Store(
                1,
                SIROffset::Static(32),
                8,
                RegisterId(0),
                Vec::new(),
                Vec::new(),
            ),
            SIRInstruction::Load(RegisterId(1), 1, SIROffset::Static(28), 16),
        ];

        assert!(!subsume_static_loads(&mut instructions));
        assert!(matches!(instructions[2], SIRInstruction::Load(..)));
        verify(
            instructions,
            [(RegisterId(0), logic(128)), (RegisterId(1), logic(16))],
        );
    }

    #[test]
    fn nonoverlapping_part_survives_static_store_to_same_wide_load() {
        let mut instructions = vec![
            SIRInstruction::Load(RegisterId(0), 1, SIROffset::Static(0), 128),
            SIRInstruction::Store(
                1,
                SIROffset::Static(64),
                8,
                RegisterId(0),
                Vec::new(),
                Vec::new(),
            ),
            SIRInstruction::Load(RegisterId(1), 1, SIROffset::Static(0), 16),
        ];

        assert!(subsume_static_loads(&mut instructions));
        assert_eq!(
            instructions[2],
            SIRInstruction::Slice(RegisterId(1), RegisterId(0), 0, 16)
        );
        verify(
            instructions,
            [(RegisterId(0), logic(128)), (RegisterId(1), logic(16))],
        );
    }

    #[test]
    fn overlapping_commit_blocks_subsumption() {
        let mut instructions = vec![
            SIRInstruction::Load(RegisterId(0), 1, SIROffset::Static(0), 128),
            SIRInstruction::Commit(2, 1, SIROffset::Static(32), 8, Vec::new()),
            SIRInstruction::Load(RegisterId(1), 1, SIROffset::Static(28), 16),
        ];

        assert!(!subsume_static_loads(&mut instructions));
        assert!(matches!(instructions[2], SIRInstruction::Load(..)));
        verify(
            instructions,
            [(RegisterId(0), logic(128)), (RegisterId(1), logic(16))],
        );
    }

    #[test]
    fn dynamic_memory_access_is_a_conservative_barrier() {
        let mut instructions = vec![
            SIRInstruction::Imm(RegisterId(2), SIRValue::new(0u8)),
            SIRInstruction::Load(RegisterId(0), 1, SIROffset::Static(0), 128),
            SIRInstruction::Load(RegisterId(3), 9, SIROffset::Dynamic(RegisterId(2)), 8),
            SIRInstruction::Load(RegisterId(1), 1, SIROffset::Static(0), 16),
        ];

        assert!(!subsume_static_loads(&mut instructions));
        assert!(matches!(instructions[3], SIRInstruction::Load(..)));
        verify(
            instructions,
            [
                (RegisterId(0), logic(128)),
                (RegisterId(1), logic(16)),
                (
                    RegisterId(2),
                    RegisterType::Bit {
                        width: 64,
                        signed: false,
                    },
                ),
                (RegisterId(3), logic(8)),
            ],
        );
    }

    #[test]
    fn event_is_a_conservative_barrier() {
        let mut instructions = vec![
            SIRInstruction::Load(RegisterId(0), 1, SIROffset::Static(0), 128),
            SIRInstruction::RuntimeEvent {
                site_id: 3,
                args: Vec::new(),
            },
            SIRInstruction::Load(RegisterId(1), 1, SIROffset::Static(0), 16),
        ];

        assert!(!subsume_static_loads(&mut instructions));
        assert!(matches!(instructions[2], SIRInstruction::Load(..)));
        verify(
            instructions,
            [(RegisterId(0), logic(128)), (RegisterId(1), logic(16))],
        );
    }

    #[test]
    fn overflowing_static_range_is_left_unchanged_without_panicking() {
        let offset = usize::MAX - 3;
        let mut instructions = vec![
            SIRInstruction::Load(RegisterId(0), 1, SIROffset::Static(offset), 8),
            SIRInstruction::Load(RegisterId(1), 1, SIROffset::Static(offset), 4),
        ];

        assert!(!subsume_static_loads(&mut instructions));
        assert!(
            instructions
                .iter()
                .all(|inst| matches!(inst, SIRInstruction::Load(..)))
        );
        verify(
            instructions,
            [(RegisterId(0), logic(8)), (RegisterId(1), logic(4))],
        );
    }

    #[test]
    fn exact_static_load_is_left_for_existing_alias_elimination() {
        let mut instructions = vec![
            SIRInstruction::Load(RegisterId(0), 1, SIROffset::Static(0), 128),
            SIRInstruction::Load(RegisterId(1), 1, SIROffset::Static(0), 128),
        ];

        assert!(!subsume_static_loads(&mut instructions));
        assert!(matches!(instructions[1], SIRInstruction::Load(..)));
        verify(
            instructions,
            [(RegisterId(0), logic(128)), (RegisterId(1), logic(128))],
        );
    }

    #[test]
    fn logic_and_bit_loads_are_not_subsumed_across_value_plane_kinds() {
        let mut instructions = vec![
            SIRInstruction::Load(RegisterId(0), 1, SIROffset::Static(0), 128),
            SIRInstruction::Load(RegisterId(1), 1, SIROffset::Static(0), 16),
        ];
        let register_map = [
            (
                RegisterId(0),
                RegisterType::Bit {
                    width: 128,
                    signed: false,
                },
            ),
            (RegisterId(1), logic(16)),
        ]
        .into_iter()
        .collect::<HashMap<_, _>>();

        assert!(!subsume_static_loads_with_types(
            &mut instructions,
            &register_map
        ));
        assert!(matches!(instructions[1], SIRInstruction::Load(..)));
        verify(instructions, register_map);
    }

    #[test]
    fn optimize_block_pipeline_applies_subsumption_and_remains_valid() {
        let mut block = BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions: vec![
                SIRInstruction::Load(RegisterId(0), 1u32, SIROffset::Static(0), 128),
                SIRInstruction::Load(RegisterId(1), 1u32, SIROffset::Static(17), 31),
            ],
            terminator: SIRTerminator::Return,
        };
        let mut register_map = [(RegisterId(0), logic(128)), (RegisterId(1), logic(31))]
            .into_iter()
            .collect::<HashMap<_, _>>();
        let mut replacements = HashMap::default();
        let mut reg_counter = 1;

        optimize_block(
            &mut block,
            &mut register_map,
            &mut replacements,
            &mut reg_counter,
            true,
            false,
            &HashMap::default(),
        );

        assert!(replacements.is_empty());
        assert_eq!(
            block.instructions[1],
            SIRInstruction::Slice(RegisterId(1), RegisterId(0), 17, 31)
        );
        verify(block.instructions, register_map);
    }

    #[test]
    fn exact_load_alias_requires_full_register_type_match() {
        let mut block = BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions: vec![
                SIRInstruction::Load(RegisterId(0), 1u32, SIROffset::Static(0), 8),
                SIRInstruction::Load(RegisterId(1), 1u32, SIROffset::Static(0), 8),
            ],
            terminator: SIRTerminator::Return,
        };
        let mut register_map = [
            (
                RegisterId(0),
                RegisterType::Bit {
                    width: 8,
                    signed: false,
                },
            ),
            (RegisterId(1), logic(8)),
        ]
        .into_iter()
        .collect::<HashMap<_, _>>();
        let mut replacements = HashMap::default();
        let mut reg_counter = 1;

        optimize_block(
            &mut block,
            &mut register_map,
            &mut replacements,
            &mut reg_counter,
            true,
            false,
            &HashMap::default(),
        );

        assert!(replacements.is_empty());
        assert_eq!(
            block
                .instructions
                .iter()
                .filter(|inst| matches!(inst, SIRInstruction::Load(..)))
                .count(),
            2
        );
        verify(block.instructions, register_map);
    }

    #[test]
    fn commit_forwarding_rejects_bit_source_but_accepts_logic_source() {
        let optimize = |source_type: RegisterType| {
            let mut block = BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Load(RegisterId(0), 1u32, SIROffset::Static(0), 8),
                    SIRInstruction::Commit(1u32, 2u32, SIROffset::Static(0), 8, Vec::new()),
                ],
                terminator: SIRTerminator::Return,
            };
            let mut register_map = [(RegisterId(0), source_type)]
                .into_iter()
                .collect::<HashMap<_, _>>();
            let mut replacements = HashMap::default();
            let mut reg_counter = 0;
            optimize_block(
                &mut block,
                &mut register_map,
                &mut replacements,
                &mut reg_counter,
                true,
                false,
                &HashMap::default(),
            );
            verify(block.instructions.clone(), register_map);
            block.instructions
        };

        let bit_instructions = optimize(RegisterType::Bit {
            width: 8,
            signed: false,
        });
        assert!(matches!(bit_instructions[1], SIRInstruction::Commit(..)));

        let logic_instructions = optimize(logic(8));
        assert!(matches!(
            logic_instructions[1],
            SIRInstruction::Store(2, SIROffset::Static(0), 8, RegisterId(0), _, _)
        ));
    }
}
