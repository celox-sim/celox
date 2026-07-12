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

    fn merge_from(&mut self, other: &Self) {
        for (&addr, ranges) in &other.ranges {
            for &(start, end) in ranges {
                self.insert(addr, start, end);
            }
        }
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

fn instruction_range(offset: &SIROffset, bits: usize) -> (usize, usize) {
    match offset {
        SIROffset::Static(start) => (*start, start.saturating_add(bits)),
        SIROffset::Dynamic(_) => (0, usize::MAX),
    }
}

fn intersect_read_with_written(
    hazards: &mut DirectStableStoreHazards,
    written: &DirectStableStoreHazards,
    addr: AbsoluteAddr,
    offset: &SIROffset,
    bits: usize,
) {
    let (read_start, read_end) = instruction_range(offset, bits);
    if let Some(ranges) = written.ranges.get(&addr) {
        for &(write_start, write_end) in ranges {
            let start = read_start.max(write_start);
            let end = read_end.min(write_end);
            hazards.insert(addr, start, end);
        }
    }
}

/// Bit ranges for which replacing a WORKING write with an immediate STABLE
/// write could change an old-state read later in this complete event CFG.
pub(crate) fn direct_stable_store_hazards(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> DirectStableStoreHazards {
    use std::collections::VecDeque;

    let mut hazards = DirectStableStoreHazards::default();
    let mut in_written: HashMap<BlockId, DirectStableStoreHazards> = HashMap::default();
    let mut worklist = VecDeque::new();
    in_written.insert(eu.entry_block_id, DirectStableStoreHazards::default());
    worklist.push_back(eu.entry_block_id);

    while let Some(bid) = worklist.pop_front() {
        let Some(block) = eu.blocks.get(&bid) else {
            continue;
        };
        let mut written = in_written.get(&bid).cloned().unwrap_or_default();
        for inst in &block.instructions {
            match inst {
                SIRInstruction::Load(_, addr, offset, bits) if addr.region == STABLE_REGION => {
                    intersect_read_with_written(
                        &mut hazards,
                        &written,
                        addr.absolute_addr(),
                        offset,
                        *bits,
                    );
                }
                SIRInstruction::Commit(src, _, offset, bits, _) if src.region == STABLE_REGION => {
                    intersect_read_with_written(
                        &mut hazards,
                        &written,
                        src.absolute_addr(),
                        offset,
                        *bits,
                    );
                }
                SIRInstruction::Store(addr, offset, bits, _, _, _)
                    if addr.region == WORKING_REGION =>
                {
                    let (start, end) = instruction_range(offset, *bits);
                    written.insert(addr.absolute_addr(), start, end);
                }
                _ => {}
            }
        }

        let mut propagate = |succ: BlockId| {
            let is_new = !in_written.contains_key(&succ);
            let entry = in_written.entry(succ).or_default();
            let old = entry.clone();
            entry.merge_from(&written);
            if is_new || *entry != old {
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
            SIRTerminator::Return | SIRTerminator::Error(_) => {}
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
                    SIRInstruction::Store(addr, SIROffset::Dynamic(_), _, _, _, _)
                    | SIRInstruction::Load(_, addr, SIROffset::Dynamic(_), _)
                        if *addr == src_addr =>
                    {
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

    for merge_id in block_ids {
        let Some(merge_block) = eu.blocks.get(&merge_id) else {
            continue;
        };

        let mut jump_preds = Vec::new();
        let mut has_non_jump_pred = false;
        for (pred_id, pred_block) in &eu.blocks {
            if *pred_id == merge_id {
                continue;
            }
            match &pred_block.terminator {
                SIRTerminator::Jump(dst, _) if *dst == merge_id => jump_preds.push(*pred_id),
                SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } if true_block.0 == merge_id || false_block.0 == merge_id => {
                    has_non_jump_pred = true;
                }
                _ => {}
            }
        }

        if has_non_jump_pred || jump_preds.is_empty() {
            continue;
        }

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

    for merge_id in block_ids {
        let Some(merge_block) = eu.blocks.get(&merge_id) else {
            continue;
        };

        let mut has_non_jump_pred = false;
        let mut jump_preds = Vec::new();

        for (pred_id, pred_block) in &eu.blocks {
            if *pred_id == merge_id {
                continue;
            }
            match &pred_block.terminator {
                SIRTerminator::Jump(dst, _) if *dst == merge_id => jump_preds.push(*pred_id),
                SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } if true_block.0 == merge_id || false_block.0 == merge_id => {
                    has_non_jump_pred = true;
                }
                _ => {}
            }
        }

        if has_non_jump_pred || jump_preds.is_empty() {
            continue;
        }

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

            for pred_id in &jump_preds {
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
    fn forwarding_preserves_working_commit_when_old_stable_is_read_later() {
        let mut eu = forwarding_eu(true);
        let hazards = direct_stable_store_hazards(&eu);
        assert!(hazards.overlaps(addr(STABLE_REGION).absolute_addr(), 0, 8));
        inline_commit_forwarding_with_hazards(&mut eu, &hazards);
        let instructions = &eu.blocks[&BlockId(0)].instructions;
        assert!(
            matches!(instructions[0], SIRInstruction::Store(a, ..) if a.region == WORKING_REGION)
        );
        assert!(matches!(instructions[1], SIRInstruction::Commit(..)));
    }

    #[test]
    fn forwarding_ignores_a_later_read_of_a_disjoint_bit_range() {
        let mut eu = forwarding_eu(true);
        let SIRInstruction::Load(_, _, offset, _) =
            &mut eu.blocks.get_mut(&BlockId(1)).unwrap().instructions[0]
        else {
            unreachable!()
        };
        *offset = SIROffset::Static(8);

        let hazards = direct_stable_store_hazards(&eu);
        assert!(!hazards.overlaps(addr(STABLE_REGION).absolute_addr(), 0, 8));
        inline_commit_forwarding_with_hazards(&mut eu, &hazards);
        assert!(matches!(
            eu.blocks[&BlockId(0)].instructions.as_slice(),
            [SIRInstruction::Store(addr, ..)] if addr.region == STABLE_REGION
        ));
    }
}
