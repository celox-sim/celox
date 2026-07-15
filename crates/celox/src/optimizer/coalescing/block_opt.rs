use super::shared::{def_reg, replace_reg_in_terminator};
use crate::ir::*;
use crate::{HashMap, HashSet};
use num_bigint::BigUint;

fn union_u32<I: IntoIterator<Item = u32>>(items: I) -> Vec<u32> {
    let mut out = Vec::new();
    for item in items {
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out
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
        SIRInstruction::Load(_, addr, SIROffset::Dynamic(_) | SIROffset::Element { .. }, bits) => {
            Some((addr, None, *bits, false))
        }
        SIRInstruction::Store(addr, SIROffset::Static(off), bits, _, _, _) => {
            Some((addr, Some(*off), *bits, true))
        }
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

pub(super) fn schedule_instructions<A: Clone + PartialEq + Eq + std::hash::Hash>(
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
) -> bool {
    let next_id = reg_counter;
    let mut replaced_indices = std::collections::HashSet::new();
    let mut insertions: HashMap<usize, Vec<SIRInstruction<A>>> = HashMap::default();

    type StoreGroupKey<A> = A;
    let mut groups: HashMap<StoreGroupKey<A>, Vec<usize>> = HashMap::default();

    for (idx, inst) in instructions.iter().enumerate() {
        if let SIRInstruction::Store(addr, SIROffset::Static(_), _, _, _, _) = inst {
            let key = addr.clone();
            groups.entry(key).or_default().push(idx);
        }
    }

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
                if detail.offset == expected_next_offset {
                    segment_end = k;
                    expected_next_offset += detail.width;
                } else {
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

                    let new_ops = vec![
                        SIRInstruction::Concat(new_reg_id, args),
                        SIRInstruction::Store(
                            addr.clone(),
                            SIROffset::Static(start_offset),
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

#[derive(Clone, Copy)]
struct AvailableStaticLoad {
    dst: RegisterId,
    load_offset: usize,
    load_width: usize,
    valid_start: usize,
    valid_end: usize,
}

/// Remove a statically written range from the still-current portions of prior
/// loads. A write can split one loaded range into two valid fragments: the
/// register still contains the old value on both non-overlapping sides.
fn subtract_static_write(
    loads: &mut Vec<AvailableStaticLoad>,
    write_start: usize,
    write_end: usize,
) {
    if write_start >= write_end {
        return;
    }
    let mut retained = Vec::with_capacity(loads.len().saturating_add(1));
    for load in loads.drain(..) {
        if write_start >= load.valid_end || load.valid_start >= write_end {
            retained.push(load);
            continue;
        }
        if load.valid_start < write_start {
            retained.push(AvailableStaticLoad {
                valid_end: write_start,
                ..load
            });
        }
        if write_end < load.valid_end {
            retained.push(AvailableStaticLoad {
                valid_start: write_end,
                ..load
            });
        }
    }
    *loads = retained;
}

/// Replace a later static Load with a Slice of a prior wider static Load when
/// the requested memory range has not changed in between. This keeps the
/// original destination register (and therefore its Bit/Logic type), while a
/// Slice preserves both value and mask planes in four-state execution.
fn subsume_static_loads<A: Clone + Eq + std::hash::Hash>(
    instructions: &mut [SIRInstruction<A>],
    register_map: &HashMap<RegisterId, RegisterType>,
) -> bool {
    let mut available: HashMap<A, Vec<AvailableStaticLoad>> = HashMap::default();
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
                    loads.iter().any(|load| {
                        load.load_offset == offset
                            && load.load_width == width
                            && load.valid_start <= offset
                            && end <= load.valid_end
                    })
                });
                if exact_is_available {
                    // Preserve the existing exact-load elimination path, which
                    // aliases the destination instead of introducing a Slice.
                    continue;
                }

                let source = available.get(addr).and_then(|loads| {
                    loads
                        .iter()
                        .filter(|load| {
                            load.load_width > width
                                && load.valid_start <= offset
                                && end <= load.valid_end
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
                    .push(AvailableStaticLoad {
                        dst,
                        load_offset: offset,
                        load_width: width,
                        valid_start: offset,
                        valid_end: end,
                    });
            }
            SIRInstruction::Load(_, _, SIROffset::Dynamic(_), _)
            | SIRInstruction::Load(_, _, SIROffset::Element { .. }, _)
            | SIRInstruction::Store(_, SIROffset::Dynamic(_), _, _, _, _)
            | SIRInstruction::Store(_, SIROffset::Element { .. }, _, _, _, _)
            | SIRInstruction::Commit(_, _, SIROffset::Dynamic(_), _, _)
            | SIRInstruction::Commit(_, _, SIROffset::Element { .. }, _, _) => {
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
                    subtract_static_write(loads, *offset, write_end);
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
                    subtract_static_write(loads, *offset, write_end);
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

pub(super) fn optimize_block<
    A: Clone + std::fmt::Debug + PartialEq + Eq + Ord + std::hash::Hash,
>(
    block: &mut BasicBlock<A>,
    register_map: &mut HashMap<RegisterId, RegisterType>,
    unit_replacement_map: &mut HashMap<RegisterId, RegisterId>,
    reg_counter: &mut usize,
    skip_final_schedule: bool,
) {
    const MAX_INFLIGHT_LOADS: usize = 8;
    coalesce_static_loads(&mut block.instructions, register_map, reg_counter);

    // First pass: coalesce stores that are safe even with intermediate loads present
    coalesce_static_stores(&mut block.instructions, register_map, reg_counter);

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
    coalesce_static_stores(&mut block.instructions, register_map, reg_counter);

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
        if seg.loads.len() < 2 {
            continue;
        }

        let mut sorted = seg.loads.clone();
        sorted.sort_by_key(|x| x.offset);

        let mut overlap = false;
        for i in 1..sorted.len() {
            let Some(prev_end) = sorted[i - 1].offset.checked_add(sorted[i - 1].width) else {
                overlap = true;
                break;
            };
            if sorted[i].offset < prev_end {
                overlap = true;
                break;
            }
        }
        if overlap {
            continue;
        }

        let mut by_word: HashMap<usize, Vec<LoadInfo>> = HashMap::default();
        for ld in seg.loads {
            if ld.width == 0 || ld.width > 64 {
                continue;
            }
            let word_base = (ld.offset / 64) * 64;
            if ld
                .offset
                .checked_add(ld.width)
                .zip(word_base.checked_add(64))
                .is_some_and(|(load_end, word_end)| load_end <= word_end)
            {
                by_word.entry(word_base).or_default().push(ld);
            }
        }

        for (word_base, mut loads) in by_word {
            if loads.len() < 2 {
                continue;
            }

            let all_native = loads
                .iter()
                .all(|ld| ld.offset % 8 == 0 && matches!(ld.width, 8 | 16 | 32 | 64));
            if all_native {
                continue;
            }

            loads.sort_by_key(|x| x.index);
            let insert_idx = loads[0].index;

            let wide_reg = next_reg_id(register_map, reg_counter);
            register_map.insert(wide_reg, RegisterType::Logic { width: 64 });
            insertions
                .entry(insert_idx)
                .or_default()
                .push(SIRInstruction::Load(
                    wide_reg,
                    seg.addr.clone(),
                    SIROffset::Static(word_base),
                    64,
                ));

            for ld in loads {
                let rel_off = ld.offset - word_base;
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

fn eliminate_redundant_loads<A: Clone + std::fmt::Debug + PartialEq + Ord + std::hash::Hash>(
    instructions: &mut Vec<SIRInstruction<A>>,
    replacement_map: &mut HashMap<RegisterId, RegisterId>,
    register_map: &HashMap<RegisterId, RegisterType>,
) {
    let mut known_values: HashMap<(A, SIROffset), (RegisterId, usize)> = HashMap::default();
    let mut new_instructions = Vec::with_capacity(instructions.len());

    for inst in instructions.drain(..) {
        let mut inst = inst.clone();

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
                    SIROffset::Static(_) => {}
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
                let key = (addr.clone(), offset.clone());
                if let Some((existing_reg, existing_width)) = known_values.get(&key)
                    && *existing_width == *width
                    && register_map
                        .get(existing_reg)
                        .zip(register_map.get(dst))
                        .is_some_and(|(existing, destination)| existing == destination)
                {
                    replacement_map.insert(*dst, *existing_reg);
                    continue;
                }

                known_values.insert(key, (*dst, *width));
                new_instructions.push(inst);
            }
            SIRInstruction::Store(addr, offset, width, src, _, _) => {
                let keys_to_remove: Vec<_> = known_values
                    .keys()
                    .filter(|(a, _)| *a == *addr)
                    .cloned()
                    .collect();

                if let SIROffset::Static(store_off) = offset {
                    let store_range = *store_off..(*store_off + *width);

                    for key in keys_to_remove {
                        let (_, key_offset) = &key;
                        if let SIROffset::Static(load_off) = key_offset {
                            let load_width = known_values[&key].1;
                            let load_range = *load_off..(*load_off + load_width);

                            if store_range.start < load_range.end
                                && load_range.start < store_range.end
                            {
                                known_values.remove(&key);
                            }
                        } else {
                            known_values.remove(&key);
                        }
                    }

                    let key = (addr.clone(), offset.clone());
                    known_values.insert(key, (*src, *width));
                } else {
                    for key in keys_to_remove {
                        known_values.remove(&key);
                    }
                }

                new_instructions.push(inst);
            }
            SIRInstruction::Commit(src_addr, dst_addr, offset, width, triggers) => {
                let keys_to_remove: Vec<_> = known_values
                    .keys()
                    .filter(|(a, _)| *a == *dst_addr)
                    .cloned()
                    .collect();

                if let SIROffset::Static(commit_off) = offset {
                    let commit_range = *commit_off..(*commit_off + *width);

                    for key in keys_to_remove {
                        let (_, key_offset) = &key;
                        if let SIROffset::Static(load_off) = key_offset {
                            let load_width = known_values[&key].1;
                            let load_range = *load_off..(*load_off + load_width);
                            if commit_range.start < load_range.end
                                && load_range.start < commit_range.end
                            {
                                known_values.remove(&key);
                            }
                        } else {
                            known_values.remove(&key);
                        }
                    }
                } else {
                    for key in keys_to_remove {
                        known_values.remove(&key);
                    }
                }

                let src_key = (src_addr.clone(), offset.clone());
                if let Some((src_reg, src_width)) = known_values.get(&src_key).copied()
                    && src_width == *width
                    && register_map.get(&src_reg) == Some(&RegisterType::Logic { width: *width })
                {
                    known_values.insert((dst_addr.clone(), offset.clone()), (src_reg, *width));
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
    use super::{optimize_block, subsume_static_loads as subsume_static_loads_with_types};
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
