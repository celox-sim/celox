use crate::HashMap;
use crate::ir::*;

fn reg_width(map: &HashMap<RegisterId, RegisterType>, reg: RegisterId) -> Option<usize> {
    map.get(&reg).map(|ty| match ty {
        RegisterType::Logic { width } => *width,
        RegisterType::Bit { width, .. } => *width,
    })
}

fn extract_subreg_from_concat(
    args_msb_to_lsb: &[RegisterId],
    map: &HashMap<RegisterId, RegisterType>,
    rel_off: usize,
    width: usize,
) -> Option<RegisterId> {
    let mut lsb = 0usize;
    for arg in args_msb_to_lsb.iter().rev() {
        let w = reg_width(map, *arg)?;
        let msb = lsb + w;
        if rel_off >= lsb && rel_off + width <= msb {
            if rel_off == lsb && width == w {
                return Some(*arg);
            }
            return None;
        }
        lsb = msb;
    }
    None
}

fn resolve_forward_src_from_pred(
    pred_block: &BasicBlock<RegionedAbsoluteAddr>,
    map: &HashMap<RegisterId, RegisterType>,
    commit_src: RegionedAbsoluteAddr,
    commit_off: usize,
    commit_bits: usize,
) -> Option<RegisterId> {
    let commit_end = commit_off + commit_bits;

    for (idx, inst) in pred_block.instructions.iter().enumerate().rev() {
        let (store_addr, store_off, store_bits, store_src) = match inst {
            SIRInstruction::Store(addr, SIROffset::Static(off), bits, src, _, _) => {
                (*addr, *off, *bits, *src)
            }
            _ => continue,
        };

        if store_addr != commit_src {
            continue;
        }

        let store_end = store_off + store_bits;
        let overlaps = commit_off < store_end && store_off < commit_end;
        if !overlaps {
            continue;
        }

        if commit_off < store_off {
            return None;
        }

        let rel_off = commit_off - store_off;
        if rel_off + commit_bits > store_bits {
            return None;
        }

        if rel_off == 0 && commit_bits == store_bits {
            return Some(store_src);
        }

        for prior in pred_block.instructions[..=idx].iter().rev() {
            if let SIRInstruction::Concat(dst, args) = prior
                && *dst == store_src
            {
                return extract_subreg_from_concat(args, map, rel_off, commit_bits);
            }
        }

        return None;
    }

    None
}

#[derive(Default)]
struct CommitMergePredecessors {
    jump: Vec<BlockId>,
    has_non_jump: bool,
}

fn commit_merge_predecessors(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<BlockId, CommitMergePredecessors> {
    let mut result: HashMap<BlockId, CommitMergePredecessors> = HashMap::default();
    for (&predecessor, block) in &eu.blocks {
        match &block.terminator {
            SIRTerminator::Jump(target, _) if *target != predecessor => {
                result.entry(*target).or_default().jump.push(predecessor);
            }
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } => {
                for target in [true_block.0, false_block.0] {
                    if target != predecessor {
                        result.entry(target).or_default().has_non_jump = true;
                    }
                }
            }
            SIRTerminator::Switch { cases, default, .. } => {
                for target in cases
                    .iter()
                    .map(|case| case.target)
                    .chain(std::iter::once(*default))
                {
                    if target != predecessor {
                        result.entry(target).or_default().has_non_jump = true;
                    }
                }
            }
            SIRTerminator::Jump(..) | SIRTerminator::Return | SIRTerminator::Error(_) => {}
        }
    }
    for predecessors in result.values_mut() {
        predecessors.jump.sort_unstable();
        predecessors.jump.dedup();
    }
    result
}

#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct DirectStableStoreHazards {
    ranges: HashMap<AbsoluteAddr, Vec<(usize, usize)>>,
}

impl DirectStableStoreHazards {
    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    fn insert(&mut self, addr: AbsoluteAddr, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let ranges = self.ranges.entry(addr).or_default();
        ranges.push((start, end));
        ranges.sort_unstable_by_key(|range| range.0);
        let mut merged: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
        for &(start, end) in ranges.iter() {
            if let Some(last) = merged.last_mut()
                && start <= last.1
            {
                last.1 = last.1.max(end);
            } else {
                merged.push((start, end));
            }
        }
        *ranges = merged;
    }

    pub(crate) fn overlaps(&self, addr: AbsoluteAddr, start: usize, bits: usize) -> bool {
        let end = start.saturating_add(bits);
        self.ranges.get(&addr).is_some_and(|ranges| {
            ranges
                .iter()
                .any(|&(hazard_start, hazard_end)| start < hazard_end && hazard_start < end)
        })
    }

    pub(crate) fn contains_addr(&self, addr: AbsoluteAddr) -> bool {
        self.ranges
            .get(&addr)
            .is_some_and(|ranges| !ranges.is_empty())
    }
}

fn register_value_count(
    register_map: &HashMap<RegisterId, RegisterType>,
    register: RegisterId,
) -> Option<usize> {
    let width = reg_width(register_map, register)?;
    if width >= usize::BITS as usize {
        return None;
    }
    1usize.checked_shl(width as u32)
}

fn instruction_range(
    register_map: &HashMap<RegisterId, RegisterType>,
    offset: &SIROffset,
    bits: usize,
) -> (usize, usize) {
    match offset {
        SIROffset::Static(start)
        | SIROffset::PackedElements {
            bit_offset: start, ..
        } => (*start, start.saturating_add(bits)),
        SIROffset::Dynamic(register) => {
            let Some(value_count) = register_value_count(register_map, *register) else {
                return (0, usize::MAX);
            };
            (0, value_count.saturating_sub(1).saturating_add(bits))
        }
        SIROffset::Element {
            index,
            element_width,
            bit_offset,
            dynamic_bit_offset: None,
        } => {
            let Some(element_count) = register_value_count(register_map, *index) else {
                return (0, usize::MAX);
            };
            let end = element_count
                .saturating_sub(1)
                .saturating_mul(*element_width)
                .saturating_add(*bit_offset)
                .saturating_add(bits);
            (*bit_offset, end)
        }
        SIROffset::Element {
            dynamic_bit_offset: Some(_),
            ..
        } => (0, usize::MAX),
    }
}

struct PendingSegments {
    by_addr: HashMap<AbsoluteAddr, (usize, Vec<usize>)>,
    count: usize,
}

impl PendingSegments {
    fn build(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> Self {
        let mut endpoints: HashMap<AbsoluteAddr, Vec<usize>> = HashMap::default();
        for block in eu.blocks.values() {
            for instruction in &block.instructions {
                match instruction {
                    SIRInstruction::Store(addr, offset, bits, _, _, _)
                        if addr.region == WORKING_REGION
                            || addr.region == SPARSE_WORKING_REGION =>
                    {
                        let (start, end) = instruction_range(&eu.register_map, offset, *bits);
                        if start < end {
                            endpoints
                                .entry(addr.absolute_addr())
                                .or_default()
                                .extend([start, end]);
                        }
                    }
                    SIRInstruction::Commit(src, dst, offset, bits, _)
                        if dst.region == STABLE_REGION
                            && (src.region == WORKING_REGION
                                || src.region == SPARSE_WORKING_REGION)
                            && src.absolute_addr() == dst.absolute_addr() =>
                    {
                        let (start, end) = instruction_range(&eu.register_map, offset, *bits);
                        if start < end {
                            endpoints
                                .entry(src.absolute_addr())
                                .or_default()
                                .extend([start, end]);
                        }
                    }
                    _ => {}
                }
            }
        }

        let mut addresses = endpoints.into_iter().collect::<Vec<_>>();
        addresses.sort_unstable_by_key(|(address, _)| *address);
        let mut by_addr = HashMap::default();
        let mut count = 0usize;
        for (address, mut points) in addresses {
            points.sort_unstable();
            points.dedup();
            if points.len() < 2 {
                continue;
            }
            let base = count;
            count += points.len() - 1;
            by_addr.insert(address, (base, points));
        }
        Self { by_addr, count }
    }

    fn segment_range(
        &self,
        address: AbsoluteAddr,
        start: usize,
        end: usize,
    ) -> Option<std::ops::Range<usize>> {
        if start >= end {
            return None;
        }
        let (base, endpoints) = self.by_addr.get(&address)?;
        let first = endpoints.binary_search(&start).ok()?;
        let limit = endpoints.binary_search(&end).ok()?;
        Some((base + first)..(base + limit))
    }

    fn record_observation(
        &self,
        hazards: &mut DirectStableStoreHazards,
        written: &[u64],
        address: AbsoluteAddr,
        start: usize,
        end: usize,
    ) {
        let Some((base, endpoints)) = self.by_addr.get(&address) else {
            return;
        };
        for index in 0..endpoints.len() - 1 {
            let segment_start = endpoints[index];
            let segment_end = endpoints[index + 1];
            if start >= segment_end || end <= segment_start {
                continue;
            }
            let bit = base + index;
            if written[bit / 64] & (1u64 << (bit % 64)) != 0 {
                hazards.insert(address, start.max(segment_start), end.min(segment_end));
            }
        }
    }

    fn record_pending(&self, hazards: &mut DirectStableStoreHazards, written: &[u64]) {
        for (&address, &(base, ref endpoints)) in &self.by_addr {
            for index in 0..endpoints.len() - 1 {
                let bit = base + index;
                if written[bit / 64] & (1u64 << (bit % 64)) != 0 {
                    hazards.insert(address, endpoints[index], endpoints[index + 1]);
                }
            }
        }
    }
}

fn update_segments(bits: &mut [u64], range: std::ops::Range<usize>, set: bool) {
    for bit in range {
        let mask = 1u64 << (bit % 64);
        if set {
            bits[bit / 64] |= mask;
        } else {
            bits[bit / 64] &= !mask;
        }
    }
}

/// Bit ranges for which replacing a WORKING/SPARSE write with an immediate
/// STABLE write could change an observation in this complete event CFG.
///
/// `written` is the may-set of state writes which have not reached their
/// publishing Commit yet.  A STABLE read observes an old value in the source
/// program, while a competing STABLE write can change which value the later
/// Commit publishes.  The publishing Commit closes the interval; reads after
/// it observe the same value in either program and are not hazards.
pub(crate) fn direct_stable_store_hazards(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> DirectStableStoreHazards {
    use std::collections::VecDeque;

    let mut hazards = DirectStableStoreHazards::default();
    let segments = PendingSegments::build(eu);
    if segments.count == 0 {
        return hazards;
    }
    let word_count = segments.count.div_ceil(64);
    let mut in_written: HashMap<BlockId, Vec<u64>> = HashMap::default();
    let mut worklist = VecDeque::new();
    in_written.insert(eu.entry_block_id, vec![0; word_count]);
    worklist.push_back(eu.entry_block_id);

    while let Some(bid) = worklist.pop_front() {
        let Some(block) = eu.blocks.get(&bid) else {
            continue;
        };
        let mut written = in_written
            .get(&bid)
            .cloned()
            .unwrap_or_else(|| vec![0; word_count]);
        for inst in &block.instructions {
            // Record old-state reads before updating the pending-write state
            // for this instruction.
            match inst {
                SIRInstruction::Load(_, addr, offset, bits) if addr.region == STABLE_REGION => {
                    let (start, end) = instruction_range(&eu.register_map, offset, *bits);
                    segments.record_observation(
                        &mut hazards,
                        &written,
                        addr.absolute_addr(),
                        start,
                        end,
                    );
                }
                SIRInstruction::Commit(src, _, offset, bits, _) if src.region == STABLE_REGION => {
                    let (start, end) = instruction_range(&eu.register_map, offset, *bits);
                    segments.record_observation(
                        &mut hazards,
                        &written,
                        src.absolute_addr(),
                        start,
                        end,
                    );
                }
                _ => {}
            }

            match inst {
                SIRInstruction::Store(addr, offset, bits, _, _, _)
                    if addr.region == WORKING_REGION || addr.region == SPARSE_WORKING_REGION =>
                {
                    let (start, end) = instruction_range(&eu.register_map, offset, *bits);
                    if let Some(range) = segments.segment_range(addr.absolute_addr(), start, end) {
                        update_segments(&mut written, range, true);
                    }
                }
                SIRInstruction::Store(addr, offset, bits, _, _, _)
                    if addr.region == STABLE_REGION =>
                {
                    let (start, end) = instruction_range(&eu.register_map, offset, *bits);
                    segments.record_observation(
                        &mut hazards,
                        &written,
                        addr.absolute_addr(),
                        start,
                        end,
                    );
                }
                SIRInstruction::Commit(src, dst, offset, bits, _)
                    if dst.region == STABLE_REGION
                        && (src.region == WORKING_REGION
                            || src.region == SPARSE_WORKING_REGION)
                        && src.absolute_addr() == dst.absolute_addr() =>
                {
                    let addr = src.absolute_addr();
                    if src.region == SPARSE_WORKING_REGION && matches!(offset, SIROffset::Static(0))
                    {
                        // The SIR region contract defines this as a full-range
                        // sparse publication, even when the preceding indexed
                        // Store had no statically bounded bit range.
                        if let Some((base, endpoints)) = segments.by_addr.get(&addr) {
                            update_segments(
                                &mut written,
                                *base..(*base + endpoints.len() - 1),
                                false,
                            );
                        }
                    } else {
                        let (start, end) = instruction_range(&eu.register_map, offset, *bits);
                        if let Some(range) = segments.segment_range(addr, start, end) {
                            update_segments(&mut written, range, false);
                        }
                    }
                }
                SIRInstruction::Commit(_, dst, offset, bits, _) if dst.region == STABLE_REGION => {
                    let (start, end) = instruction_range(&eu.register_map, offset, *bits);
                    segments.record_observation(
                        &mut hazards,
                        &written,
                        dst.absolute_addr(),
                        start,
                        end,
                    );
                }
                _ => {}
            }
        }

        let mut propagate = |succ: BlockId| {
            let is_new = !in_written.contains_key(&succ);
            let entry = in_written
                .entry(succ)
                .or_insert_with(|| vec![0; word_count]);
            let mut changed = is_new;
            for (destination, source) in entry.iter_mut().zip(&written) {
                let merged = *destination | *source;
                changed |= merged != *destination;
                *destination = merged;
            }
            if changed {
                worklist.push_back(succ);
            }
        };
        match &block.terminator {
            SIRTerminator::Jump(dst, _) => propagate(*dst),
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } => {
                propagate(true_block.0);
                propagate(false_block.0);
            }
            SIRTerminator::Switch { cases, default, .. } => {
                for case in cases {
                    propagate(case.target);
                }
                propagate(*default);
            }
            SIRTerminator::Return | SIRTerminator::Error(_) => {
                // Publishing a redirected state Store on a path where the
                // source program never reaches its Commit changes the final
                // state (and can expose state before an Error is reported).
                segments.record_pending(&mut hazards, &written);
            }
        }
    }
    hazards
}

pub(crate) fn inline_commit_forwarding_with_hazards(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    hazards: &DirectStableStoreHazards,
) {
    let block_ids: Vec<_> = eu.blocks.keys().copied().collect();

    for bid in block_ids {
        let Some(block) = eu.blocks.get(&bid) else {
            continue;
        };
        let mut commit_replacements: Vec<(usize, Vec<(usize, RegionedAbsoluteAddr)>)> = Vec::new();

        for (ci, inst) in block.instructions.iter().enumerate() {
            let (src_addr, dst_addr, off, bits) = match inst {
                SIRInstruction::Commit(src, dst, SIROffset::Static(off), bits, _) => {
                    (*src, *dst, *off, *bits)
                }
                _ => continue,
            };
            if src_addr.region != WORKING_REGION
                || dst_addr.region != STABLE_REGION
                || hazards.overlaps(dst_addr.absolute_addr(), off, bits)
            {
                continue;
            }

            let mut found_stores = Vec::new();
            let mut safe = true;
            for si in (0..ci).rev() {
                match &block.instructions[si] {
                    SIRInstruction::Store(
                        addr,
                        SIROffset::Static(store_off),
                        store_bits,
                        _,
                        _,
                        _,
                    ) if *addr == src_addr
                        && *store_off >= off
                        && store_off + store_bits <= off + bits =>
                    {
                        found_stores.push((si, *store_off, *store_bits));
                    }
                    SIRInstruction::Store(
                        addr,
                        SIROffset::Dynamic(_) | SIROffset::Element { .. },
                        _,
                        _,
                        _,
                        _,
                    )
                    | SIRInstruction::Load(
                        _,
                        addr,
                        SIROffset::Dynamic(_) | SIROffset::Element { .. },
                        _,
                    ) if *addr == src_addr => {
                        safe = false;
                        break;
                    }
                    _ => {}
                }
            }
            if !safe {
                continue;
            }
            found_stores.sort_by_key(|(_, offset, _)| *offset);
            if found_stores
                .iter()
                .map(|(_, _, width)| *width)
                .sum::<usize>()
                != bits
            {
                continue;
            }
            let mut expected = off;
            if !found_stores.iter().all(|(_, store_off, store_bits)| {
                let contiguous = *store_off == expected;
                expected += *store_bits;
                contiguous
            }) {
                continue;
            }
            commit_replacements.push((
                ci,
                found_stores
                    .iter()
                    .map(|(index, _, _)| (*index, dst_addr))
                    .collect(),
            ));
        }

        let Some(block) = eu.blocks.get_mut(&bid) else {
            continue;
        };
        let mut remove_indices = Vec::new();
        for (ci, store_updates) in &commit_replacements {
            remove_indices.push(*ci);
            for (si, new_dst) in store_updates {
                if let SIRInstruction::Store(addr, _, _, _, _, _) = &mut block.instructions[*si] {
                    *addr = *new_dst;
                }
            }
        }
        remove_indices.sort_unstable();
        remove_indices.dedup();
        for index in remove_indices.into_iter().rev() {
            block.instructions.remove(index);
        }
    }
}

pub(super) fn split_wide_commits(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    let block_ids: Vec<_> = eu.blocks.keys().copied().collect();
    let predecessors = commit_merge_predecessors(eu);

    for merge_id in block_ids {
        let Some(merge_block) = eu.blocks.get(&merge_id) else {
            continue;
        };
        let Some(predecessors) = predecessors.get(&merge_id) else {
            continue;
        };
        if predecessors.has_non_jump || predecessors.jump.is_empty() {
            continue;
        }
        let jump_preds = &predecessors.jump;

        let mut replacements: Vec<(usize, Vec<SIRInstruction<RegionedAbsoluteAddr>>)> = Vec::new();

        for (idx, inst) in merge_block.instructions.iter().enumerate() {
            let (src_addr, dst_addr, off, bits) = match inst {
                SIRInstruction::Commit(src, dst, SIROffset::Static(off), bits, _) => {
                    (*src, *dst, *off, *bits)
                }
                _ => continue,
            };

            let already_sinkable = jump_preds.iter().all(|pred_id| {
                eu.blocks.get(pred_id).is_some_and(|pb| {
                    resolve_forward_src_from_pred(pb, &eu.register_map, src_addr, off, bits)
                        .is_some()
                })
            });
            if already_sinkable {
                continue;
            }

            let Some(first_block) = eu.blocks.get(&jump_preds[0]) else {
                continue;
            };
            let mut sub_ranges: Vec<(usize, usize)> = Vec::new();
            for pred_inst in &first_block.instructions {
                if let SIRInstruction::Store(
                    addr,
                    SIROffset::Static(store_off),
                    store_bits,
                    _,
                    _,
                    _,
                ) = pred_inst
                    && *addr == src_addr
                    && *store_off >= off
                    && store_off + store_bits <= off + bits
                {
                    sub_ranges.push((*store_off, *store_bits));
                }
            }
            sub_ranges.sort_by_key(|(o, _)| *o);
            sub_ranges.dedup();

            let total: usize = sub_ranges.iter().map(|(_, b)| b).sum();
            if total != bits {
                continue;
            }
            let mut expected = off;
            let mut contiguous = true;
            for (sub_off, sub_bits) in &sub_ranges {
                if *sub_off != expected {
                    contiguous = false;
                    break;
                }
                expected += sub_bits;
            }
            if !contiguous {
                continue;
            }

            let mut all_ok = true;
            for pred_id in &jump_preds[1..] {
                let Some(pred_block) = eu.blocks.get(pred_id) else {
                    all_ok = false;
                    break;
                };
                for (sub_off, sub_bits) in &sub_ranges {
                    let has_match = pred_block.instructions.iter().any(|pi| {
                        matches!(
                            pi,
                            SIRInstruction::Store(addr, SIROffset::Static(so), sb, _, _, _)
                            if *addr == src_addr && *so == *sub_off && *sb == *sub_bits
                        )
                    });
                    if !has_match {
                        all_ok = false;
                        break;
                    }
                }
                if !all_ok {
                    break;
                }
            }
            if !all_ok {
                continue;
            }

            let new_commits: Vec<SIRInstruction<RegionedAbsoluteAddr>> = sub_ranges
                .iter()
                .map(|(sub_off, sub_bits)| {
                    SIRInstruction::Commit(
                        src_addr,
                        dst_addr,
                        SIROffset::Static(*sub_off),
                        *sub_bits,
                        Default::default(),
                    )
                })
                .collect();
            replacements.push((idx, new_commits));
        }

        if replacements.is_empty() {
            continue;
        }

        if let Some(merge_block) = eu.blocks.get_mut(&merge_id) {
            for (idx, new_insts) in replacements.into_iter().rev() {
                merge_block.instructions.remove(idx);
                for (j, inst) in new_insts.into_iter().enumerate() {
                    merge_block.instructions.insert(idx + j, inst);
                }
            }
        }
    }
}

pub(crate) fn optimize_commit_sinking_with_hazards(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    hazards: &DirectStableStoreHazards,
) {
    let block_ids: Vec<_> = eu.blocks.keys().copied().collect();
    let predecessors = commit_merge_predecessors(eu);

    for merge_id in block_ids {
        let Some(merge_block) = eu.blocks.get(&merge_id) else {
            continue;
        };
        let Some(predecessors) = predecessors.get(&merge_id) else {
            continue;
        };
        if predecessors.has_non_jump || predecessors.jump.is_empty() {
            continue;
        }
        let jump_preds = &predecessors.jump;

        let mut sinkable = Vec::new();

        for (idx, inst) in merge_block.instructions.iter().enumerate() {
            let (src_addr, dst_addr, off, bits) = match inst {
                SIRInstruction::Commit(src, dst, SIROffset::Static(off), bits, _) => {
                    (*src, *dst, *off, *bits)
                }
                _ => continue,
            };
            if src_addr.region != WORKING_REGION
                || dst_addr.region != STABLE_REGION
                || hazards.overlaps(dst_addr.absolute_addr(), off, bits)
            {
                continue;
            }

            let mut pred_sources = Vec::new();
            let mut ok = true;

            for pred_id in jump_preds {
                let Some(pred_block) = eu.blocks.get(pred_id) else {
                    ok = false;
                    break;
                };
                let Some(src_reg) = resolve_forward_src_from_pred(
                    pred_block,
                    &eu.register_map,
                    src_addr,
                    off,
                    bits,
                ) else {
                    ok = false;
                    break;
                };
                pred_sources.push((*pred_id, src_reg));
            }

            if ok {
                sinkable.push((idx, dst_addr, SIROffset::Static(off), bits, pred_sources));
            }
        }

        if sinkable.is_empty() {
            continue;
        }

        for (_, dst_addr, off, bits, pred_sources) in &sinkable {
            for (pred_id, src_reg) in pred_sources {
                if let Some(pred_block) = eu.blocks.get_mut(pred_id) {
                    pred_block.instructions.push(SIRInstruction::Store(
                        *dst_addr,
                        off.clone(),
                        *bits,
                        *src_reg,
                        Default::default(),
                        Vec::new(),
                    ));
                }
            }
        }

        if let Some(merge_block) = eu.blocks.get_mut(&merge_id) {
            for (idx, _, _, _, _) in sinkable.into_iter().rev() {
                merge_block.instructions.remove(idx);
            }
        }
    }
}

pub(crate) fn optimize_commit_sinking(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    let hazards = direct_stable_store_hazards(eu);
    optimize_commit_sinking_with_hazards(eu, &hazards);
}

#[cfg(test)]
mod tests {
    use super::*;
    use num_bigint::BigUint;
    use veryl_analyzer::ir::VarId;

    fn addr(region: u32) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region,
            instance_id: InstanceId(0),
            var_id: VarId::from_raw(0),
        }
    }

    fn forwarding_eu(read_old_after: bool) -> ExecutionUnit<RegionedAbsoluteAddr> {
        let stable = addr(STABLE_REGION);
        let working = addr(WORKING_REGION);
        let mut blocks = HashMap::default();
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Store(
                        working,
                        SIROffset::Static(0),
                        8,
                        RegisterId(0),
                        Vec::new(),
                        Vec::new(),
                    ),
                    SIRInstruction::Commit(working, stable, SIROffset::Static(0), 8, Vec::new()),
                ],
                terminator: if read_old_after {
                    SIRTerminator::Jump(BlockId(1), Vec::new())
                } else {
                    SIRTerminator::Return
                },
            },
        );
        if read_old_after {
            blocks.insert(
                BlockId(1),
                BasicBlock {
                    id: BlockId(1),
                    params: Vec::new(),
                    instructions: vec![SIRInstruction::Load(
                        RegisterId(1),
                        stable,
                        SIROffset::Static(0),
                        8,
                    )],
                    terminator: SIRTerminator::Return,
                },
            );
        }
        let mut register_map = HashMap::default();
        register_map.insert(RegisterId(0), RegisterType::Logic { width: 8 });
        register_map.insert(RegisterId(1), RegisterType::Logic { width: 8 });
        ExecutionUnit {
            blocks,
            entry_block_id: BlockId(0),
            register_map,
        }
    }

    #[test]
    fn forwarding_remains_enabled_when_no_old_stable_read_follows() {
        let mut eu = forwarding_eu(false);
        let hazards = direct_stable_store_hazards(&eu);
        assert!(hazards.is_empty());
        inline_commit_forwarding_with_hazards(&mut eu, &hazards);
        let instructions = &eu.blocks[&BlockId(0)].instructions;
        assert!(matches!(
            instructions.as_slice(),
            [SIRInstruction::Store(addr, ..)] if addr.region == STABLE_REGION
        ));
    }

    #[test]
    fn forwarding_crosses_a_stable_read_after_publication() {
        let mut eu = forwarding_eu(true);
        let hazards = direct_stable_store_hazards(&eu);
        assert!(hazards.is_empty());
        inline_commit_forwarding_with_hazards(&mut eu, &hazards);
        let instructions = &eu.blocks[&BlockId(0)].instructions;
        assert!(matches!(
            instructions.as_slice(),
            [SIRInstruction::Store(a, ..)] if a.region == STABLE_REGION
        ));
    }

    #[test]
    fn forwarding_preserves_working_commit_when_old_stable_is_read_before_it() {
        let stable = addr(STABLE_REGION);
        let working = addr(WORKING_REGION);
        let mut eu = forwarding_eu(false);
        eu.blocks.get_mut(&BlockId(0)).unwrap().instructions.insert(
            1,
            SIRInstruction::Load(RegisterId(1), stable, SIROffset::Static(0), 8),
        );

        let hazards = direct_stable_store_hazards(&eu);
        assert!(hazards.overlaps(stable.absolute_addr(), 0, 8));
        inline_commit_forwarding_with_hazards(&mut eu, &hazards);
        let instructions = &eu.blocks[&BlockId(0)].instructions;
        assert!(matches!(instructions[0], SIRInstruction::Store(a, ..) if a == working));
        assert!(matches!(instructions[2], SIRInstruction::Commit(..)));
    }

    #[test]
    fn forwarding_ignores_a_later_read_of_a_disjoint_bit_range() {
        let stable = addr(STABLE_REGION);
        let mut eu = forwarding_eu(false);
        eu.blocks.get_mut(&BlockId(0)).unwrap().instructions.insert(
            1,
            SIRInstruction::Load(RegisterId(1), stable, SIROffset::Static(8), 8),
        );

        let hazards = direct_stable_store_hazards(&eu);
        assert!(!hazards.overlaps(addr(STABLE_REGION).absolute_addr(), 0, 8));
        inline_commit_forwarding_with_hazards(&mut eu, &hazards);
        let instructions = &eu.blocks[&BlockId(0)].instructions;
        assert!(
            matches!(instructions[0], SIRInstruction::Store(addr, ..) if addr.region == STABLE_REGION)
        );
        assert!(matches!(instructions[1], SIRInstruction::Load(..)));
    }

    #[test]
    fn sparse_write_makes_an_old_stable_read_hazardous_until_publication() {
        let stable = addr(STABLE_REGION);
        let sparse = addr(SPARSE_WORKING_REGION);
        let mut eu = forwarding_eu(false);
        eu.blocks.get_mut(&BlockId(0)).unwrap().instructions = vec![
            SIRInstruction::Store(
                sparse,
                SIROffset::Element {
                    index: RegisterId(1),
                    element_width: 8,
                    bit_offset: 0,
                    dynamic_bit_offset: None,
                },
                8,
                RegisterId(0),
                Vec::new(),
                Vec::new(),
            ),
            SIRInstruction::Load(RegisterId(2), stable, SIROffset::Static(0), 8),
            SIRInstruction::Commit(sparse, stable, SIROffset::Static(0), 64, Vec::new()),
        ];
        eu.register_map
            .insert(RegisterId(2), RegisterType::Logic { width: 8 });

        let hazards = direct_stable_store_hazards(&eu);
        assert!(hazards.contains_addr(stable.absolute_addr()));
    }

    #[test]
    fn sparse_publication_closes_the_old_stable_read_interval() {
        let stable = addr(STABLE_REGION);
        let sparse = addr(SPARSE_WORKING_REGION);
        let mut eu = forwarding_eu(false);
        eu.blocks.get_mut(&BlockId(0)).unwrap().instructions = vec![
            SIRInstruction::Store(
                sparse,
                SIROffset::Element {
                    index: RegisterId(1),
                    element_width: 8,
                    bit_offset: 0,
                    dynamic_bit_offset: None,
                },
                8,
                RegisterId(0),
                Vec::new(),
                Vec::new(),
            ),
            SIRInstruction::Commit(sparse, stable, SIROffset::Static(0), 64, Vec::new()),
            SIRInstruction::Load(RegisterId(2), stable, SIROffset::Static(0), 8),
        ];
        eu.register_map
            .insert(RegisterId(2), RegisterType::Logic { width: 8 });

        assert!(direct_stable_store_hazards(&eu).is_empty());
    }

    #[test]
    fn stable_write_competing_with_an_unpublished_sparse_write_is_hazardous() {
        let stable = addr(STABLE_REGION);
        let sparse = addr(SPARSE_WORKING_REGION);
        let mut eu = forwarding_eu(false);
        eu.blocks.get_mut(&BlockId(0)).unwrap().instructions = vec![
            SIRInstruction::Store(
                sparse,
                SIROffset::Static(0),
                8,
                RegisterId(0),
                Vec::new(),
                Vec::new(),
            ),
            SIRInstruction::Store(
                stable,
                SIROffset::Static(0),
                8,
                RegisterId(1),
                Vec::new(),
                Vec::new(),
            ),
            SIRInstruction::Commit(sparse, stable, SIROffset::Static(0), 64, Vec::new()),
        ];

        assert!(direct_stable_store_hazards(&eu).overlaps(stable.absolute_addr(), 0, 8));
    }

    #[test]
    fn switch_predecessor_prevents_commit_sinking() {
        let stable = addr(STABLE_REGION);
        let working = addr(WORKING_REGION);
        let mut blocks = HashMap::default();
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Switch {
                    selector: RegisterId(0),
                    cases: vec![SIRSwitchCase {
                        value: BigUint::from(0u8),
                        target: BlockId(2),
                    }],
                    default: BlockId(1),
                },
            },
        );
        blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: vec![SIRInstruction::Store(
                    working,
                    SIROffset::Static(0),
                    8,
                    RegisterId(1),
                    Vec::new(),
                    Vec::new(),
                )],
                terminator: SIRTerminator::Jump(BlockId(2), Vec::new()),
            },
        );
        let commit = SIRInstruction::Commit(working, stable, SIROffset::Static(0), 8, Vec::new());
        blocks.insert(
            BlockId(2),
            BasicBlock {
                id: BlockId(2),
                params: Vec::new(),
                instructions: vec![commit.clone()],
                terminator: SIRTerminator::Return,
            },
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map: [
                (
                    RegisterId(0),
                    RegisterType::Bit {
                        width: 1,
                        signed: false,
                    },
                ),
                (RegisterId(1), RegisterType::Logic { width: 8 }),
            ]
            .into_iter()
            .collect(),
        };

        optimize_commit_sinking(&mut eu);

        assert_eq!(eu.blocks[&BlockId(2)].instructions, vec![commit]);
        assert!(matches!(
            eu.blocks[&BlockId(1)].instructions.as_slice(),
            [SIRInstruction::Store(address, ..)] if address.region == WORKING_REGION
        ));
    }
}
