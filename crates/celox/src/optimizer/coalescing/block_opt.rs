use super::shared::{def_reg, replace_reg_in_terminator};
use crate::ir::*;
use crate::{HashMap, HashSet};
use malachite_bigint::BigUint;

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
        SIRInstruction::Load(_, _, SIROffset::Dynamic(off), _) => {
            out.push(*off);
        }
        SIRInstruction::Load(_, _, SIROffset::Static(_), _) => {}
        SIRInstruction::Store(_, SIROffset::Dynamic(off), _, src, _) => {
            out.push(*off);
            out.push(*src);
        }
        SIRInstruction::Store(_, SIROffset::Static(_), _, src, _) => {
            out.push(*src);
        }
        SIRInstruction::Commit(_, _, SIROffset::Dynamic(off), _, _) => {
            out.push(*off);
        }
        SIRInstruction::Commit(_, _, SIROffset::Static(_), _, _) => {}
        SIRInstruction::Concat(_, args) => out.extend(args.iter().copied()),
    }
}

fn is_memory_barrier<A>(inst: &SIRInstruction<A>) -> bool {
    matches!(inst, SIRInstruction::Commit(_, _, _, _, _))
}

fn mem_access_info<A>(inst: &SIRInstruction<A>) -> Option<(&A, Option<usize>, usize, bool)> {
    match inst {
        SIRInstruction::Load(_, addr, SIROffset::Static(off), bits) => {
            Some((addr, Some(*off), *bits, false))
        }
        SIRInstruction::Load(_, addr, SIROffset::Dynamic(_), bits) => {
            Some((addr, None, *bits, false))
        }
        SIRInstruction::Store(addr, SIROffset::Static(off), bits, _, _) => {
            Some((addr, Some(*off), *bits, true))
        }
        SIRInstruction::Store(addr, SIROffset::Dynamic(_), bits, _, _) => {
            Some((addr, None, *bits, true))
        }
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
            .find(|&i| matches!(window[i], SIRInstruction::Store(_, _, _, _, _)))
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

    for (dst, src) in instructions.iter_mut().zip(out.into_iter()) {
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
        if let SIRInstruction::Store(addr, SIROffset::Static(_), _, _, _) = inst {
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
            SIRInstruction::Load(_, addr, SIROffset::Dynamic(_), w) => {
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
        }
        let mut details: Vec<StoreInfo> = Vec::new();

        for &idx in &indices {
            if let SIRInstruction::Store(_, SIROffset::Static(o), w, s, t) = &instructions[idx] {
                details.push(StoreInfo {
                    offset: *o,
                    width: *w,
                    index: idx,
                    src: *s,
                    triggers: t.clone(),
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

    let mut local_replacement_map = HashMap::default();
    eliminate_redundant_loads(&mut block.instructions, &mut local_replacement_map);

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
            SIRInstruction::Store(addr, _, _, _, _) => {
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
            let prev_end = sorted[i - 1].offset + sorted[i - 1].width;
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
            if ld.offset + ld.width <= word_base + 64 {
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
            SIRInstruction::Store(_, SIROffset::Dynamic(off_reg), _, src, _) => {
                if let Some(r) = replacement_map.get(off_reg) {
                    *off_reg = *r;
                }
                if let Some(r) = replacement_map.get(src) {
                    *src = *r;
                }
            }
            SIRInstruction::Store(_, SIROffset::Static(_), _, src, _) => {
                if let Some(r) = replacement_map.get(src) {
                    *src = *r;
                }
            }
            SIRInstruction::Load(_, _, SIROffset::Dynamic(off_reg), _) => {
                if let Some(r) = replacement_map.get(off_reg) {
                    *off_reg = *r;
                }
            }
            SIRInstruction::Commit(_, _, SIROffset::Dynamic(off_reg), _, _) => {
                if let Some(r) = replacement_map.get(off_reg) {
                    *off_reg = *r;
                }
            }
            SIRInstruction::Commit(_, _, SIROffset::Static(_), _, _) => {}
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
                {
                    replacement_map.insert(*dst, *existing_reg);
                    continue;
                }

                known_values.insert(key, (*dst, *width));
                new_instructions.push(inst);
            }
            SIRInstruction::Store(addr, offset, width, src, _) => {
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
                {
                    known_values.insert((dst_addr.clone(), offset.clone()), (src_reg, *width));
                    new_instructions.push(SIRInstruction::Store(
                        dst_addr.clone(),
                        offset.clone(),
                        *width,
                        src_reg,
                        triggers.clone(),
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
