use super::shared::{def_reg, replace_reg_in_terminator};
use crate::ir::*;
use crate::{HashMap, HashSet};
use num_bigint::BigUint;
use std::collections::{BTreeMap, BTreeSet};

const MAX_SCALAR_COALESCED_STORE_BITS: usize = 64;

/// A Store consumes only its low `width` bits, whereas Concat consumes the
/// entire register. Preserve each store's truncation before joining lanes.
pub(in crate::optimizer) fn store_concat_source<A>(
    source: RegisterId,
    width: usize,
    register_map: &mut HashMap<RegisterId, RegisterType>,
    counter: &mut usize,
    instructions: &mut Vec<SIRInstruction<A>>,
) -> RegisterId {
    if register_map[&source].width() == width {
        return source;
    }
    *counter += 1;
    while register_map.contains_key(&RegisterId(*counter)) {
        *counter += 1;
    }
    let narrowed = RegisterId(*counter);
    register_map.insert(narrowed, RegisterType::Logic { width });
    instructions.push(SIRInstruction::Slice(narrowed, source, 0, width));
    narrowed
}

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
    inst.for_each_use(|register| out.push(register));
}

fn is_memory_barrier<A>(inst: &SIRInstruction<A>) -> bool {
    // This pass tracks loads and stores precisely but not commits, so a
    // commit orders every state access here. State accesses are not moved
    // across host interactions either.
    inst.is_host_interaction() || matches!(inst, SIRInstruction::Commit(..))
}

/// Forget the facts `facts` keeps about the objects `inst` may write
/// other than through a Load, Store or Commit: those a call's effects
/// list, or all of them for a native kernel.
fn forget_opaque_writes<A: Eq + std::hash::Hash, V>(
    inst: &SIRInstruction<A>,
    facts: &mut HashMap<A, V>,
) {
    match inst.opaque_written_objects() {
        Some(objects) => {
            for object in objects {
                facts.remove(object);
            }
        }
        None => facts.clear(),
    }
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

/// The earlier accesses of a scheduling window by object: those with a
/// static offset by their start bit, those with a dynamic offset apart.
struct AccessIndex<A> {
    objects: HashMap<A, ObjectAccesses>,
}

#[derive(Default)]
struct ObjectAccesses {
    /// The accesses at each static start bit, as instruction index and width.
    fixed: std::collections::BTreeMap<usize, Vec<(usize, usize)>>,
    /// The widest static access, which bounds how far below a start bit an
    /// aliasing access can begin.
    max_width: usize,
    /// Instruction indices of the accesses with a dynamic offset.
    dynamic: Vec<usize>,
}

impl<A> Default for AccessIndex<A> {
    fn default() -> Self {
        Self {
            objects: HashMap::default(),
        }
    }
}

impl<A: Clone + PartialEq + Eq + std::hash::Hash> AccessIndex<A> {
    fn insert(&mut self, addr: A, off: Option<usize>, width: usize, index: usize) {
        let accesses = self.objects.entry(addr).or_default();
        match off {
            Some(off) => {
                accesses.fixed.entry(off).or_default().push((index, width));
                accesses.max_width = accesses.max_width.max(width);
            }
            None => accesses.dynamic.push(index),
        }
    }

    /// Append the indices of the recorded accesses of `addr` that may alias
    /// the range `off`/`width` (see `ranges_alias`) to `out`.
    fn aliasing(&self, addr: &A, off: Option<usize>, width: usize, out: &mut Vec<usize>) {
        let Some(accesses) = self.objects.get(addr) else {
            return;
        };
        out.extend_from_slice(&accesses.dynamic);
        match off {
            Some(off) => {
                let first = off.saturating_sub(accesses.max_width.saturating_sub(1));
                let last = off.saturating_add(width.max(1) - 1);
                for (&start, accesses) in accesses.fixed.range(first..=last) {
                    out.extend(accesses.iter().filter_map(|&(index, access_width)| {
                        ranges_alias(Some(start), access_width, Some(off), width).then_some(index)
                    }));
                }
            }
            None => out.extend(accesses.fixed.values().flatten().map(|&(index, _)| index)),
        }
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
        // Every call below targets the current j of the ascending instruction
        // scan. Successor lists therefore grow in order; a duplicate data or
        // memory edge can only equal the most recently appended destination.
        if succs[from].last() != Some(&to) {
            succs[from].push(to);
            indeg[to] += 1;
        }
    };

    // Track memory accesses by address, and within an address by static
    // offset, so that a window of thousands of static accesses of one object
    // (a memory initialization, the partition flags of a settle) finds only
    // the accesses that can alias each one instead of every earlier access of
    // the object.
    let mut mem_writes: AccessIndex<A> = AccessIndex::default();
    let mut mem_reads: AccessIndex<A> = AccessIndex::default();

    // Pre-extract memory access info to avoid redundant pattern matching
    let mem_infos: Vec<Option<(A, Option<usize>, usize, bool)>> = window
        .iter()
        .map(|inst| {
            mem_access_info(inst)
                .map(|(addr, off, width, is_write)| (addr.clone(), off, width, is_write))
        })
        .collect();

    let mut aliasing = Vec::new();
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
        if let Some((addr, off, width, j_write)) = &mem_infos[j] {
            // Every earlier write of an aliasing range orders a read (RAW) or
            // a write (WAW); every earlier read orders a write (WAR). An
            // instruction is a read or a write, so no source repeats.
            aliasing.clear();
            mem_writes.aliasing(addr, *off, *width, &mut aliasing);
            if *j_write {
                mem_reads.aliasing(addr, *off, *width, &mut aliasing);
            }
            aliasing.sort_unstable();
            for &prev in &aliasing {
                add_edge(prev, j, &mut succs, &mut indeg);
            }
            if *j_write {
                mem_writes.insert(addr.clone(), *off, *width, j);
            } else {
                mem_reads.insert(addr.clone(), *off, *width, j);
            }
        }
    }

    // Scheduling loop with incremental ready set
    let mut out = Vec::with_capacity(n);
    let mut inflight_loads: HashSet<RegisterId> = HashSet::default();
    let mut ready = (0..n).filter(|&i| indeg[i] == 0).collect::<BTreeSet<_>>();
    let mut ready_stores = ready
        .iter()
        .copied()
        .filter(|&i| matches!(window[i], SIRInstruction::Store(_, _, _, _, _, _)))
        .collect::<BTreeSet<_>>();
    let mut ready_loads = ready
        .iter()
        .copied()
        .filter(|&i| matches!(window[i], SIRInstruction::Load(_, _, _, _)))
        .collect::<BTreeSet<_>>();

    while !ready.is_empty() {
        let pick = ready_stores
            .first()
            .copied()
            .or_else(|| {
                (inflight_loads.len() < max_inflight_loads)
                    .then(|| ready_loads.first().copied())
                    .flatten()
            })
            .unwrap_or_else(|| *ready.first().expect("ready set must not be empty"));

        ready.remove(&pick);
        ready_stores.remove(&pick);
        ready_loads.remove(&pick);

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
                ready.insert(s);
                match window[s] {
                    SIRInstruction::Store(_, _, _, _, _, _) => {
                        ready_stores.insert(s);
                    }
                    SIRInstruction::Load(_, _, _, _) => {
                        ready_loads.insert(s);
                    }
                    _ => {}
                }
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
        // The window scheduler orders one access per instruction, so an
        // instruction reaching state through a call's effects orders every
        // access around it.
        if is_memory_barrier(&instructions[i]) || instructions[i].has_opaque_state_access() {
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

    let mut replaced_indices = HashSet::default();
    let mut insertions: HashMap<usize, Vec<SIRInstruction<A>>> = HashMap::default();

    // Moving a store to the end of a merged run must cross neither a read
    // nor a later overlapping write. In particular, a narrow overwrite can
    // sit between two adjacent candidate ranges in instruction order.
    let mut access_index: HashMap<A, Vec<(usize, Option<usize>, usize)>> = HashMap::default();
    let mut barriers = Vec::new();
    for (idx, inst) in instructions.iter().enumerate() {
        if is_memory_barrier(inst) {
            barriers.push(idx);
        }
        if let Some((addr, offset, width, _)) = mem_access_info(inst) {
            access_index
                .entry(addr.clone())
                .or_default()
                .push((idx, offset, width));
        }
        // A call accesses any part of the objects its effects list, which
        // no store of them may move across.
        match inst.opaque_accessed_objects() {
            Some(objects) => {
                for object in objects {
                    let accesses = access_index.entry(object.clone()).or_default();
                    if accesses.last().is_none_or(|&(last, _, _)| last != idx) {
                        accesses.push((idx, None, 0));
                    }
                }
            }
            None => barriers.push(idx),
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
            let keep: crate::HashSet<usize> = best.into_values().collect();
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
        let addr_accesses = access_index.get(&addr);

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

                // Preserve the ordering of every range moved to the anchor.
                let safe = if let Some(accesses) = addr_accesses {
                    segment.iter().all(|s| {
                        if s.index == insert_at_index {
                            return true;
                        }
                        let first_barrier = barriers.partition_point(|&idx| idx <= s.index);
                        if barriers
                            .get(first_barrier)
                            .is_some_and(|&idx| idx <= insert_at_index)
                        {
                            return false;
                        }
                        let first = accesses.partition_point(|&(idx, _, _)| idx <= s.index);
                        let end = accesses.partition_point(|&(idx, _, _)| idx <= insert_at_index);
                        !accesses[first..end].iter().any(|&(_, load_off, load_w)| {
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
                    let mut new_ops = Vec::new();
                    let args: Vec<RegisterId> = segment
                        .iter()
                        .rev()
                        .map(|s| {
                            store_concat_source(s.src, s.width, register_map, next_id, &mut new_ops)
                        })
                        .collect();
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
                    new_ops.extend([
                        SIRInstruction::Concat(new_reg_id, args),
                        SIRInstruction::Store(
                            addr.clone(),
                            offset,
                            total_width,
                            new_reg_id,
                            triggers,
                            comb_capture_sites,
                        ),
                    ]);

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
            _ if inst.is_host_interaction() => available.clear(),
            _ => forget_opaque_writes(inst, &mut available),
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
            // A load after a call that may write its object starts a new
            // segment: the two loads can see different values.
            other => forget_opaque_writes(other, &mut active),
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
            other => {
                forget_opaque_writes(other, &mut known_values);
                new_instructions.push(inst);
            }
        }
    }

    *instructions = new_instructions;
}

#[cfg(test)]
mod tests;
