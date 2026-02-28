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
    for (idx, inst) in pred_block.instructions.iter().enumerate().rev() {
        let (store_addr, store_off, store_bits, store_src) = match inst {
            SIRInstruction::Store(addr, SIROffset::Static(off), bits, src, _) => {
                (*addr, *off, *bits, *src)
            }
            _ => continue,
        };

        if store_addr != commit_src || commit_off < store_off {
            continue;
        }

        let rel_off = commit_off - store_off;
        if rel_off + commit_bits > store_bits {
            continue;
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

pub(super) fn inline_commit_forwarding(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
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

            let mut found_stores: Vec<(usize, usize, usize)> = Vec::new();
            let mut safe = true;

            for si in (0..ci).rev() {
                let sinst = &block.instructions[si];
                match sinst {
                    SIRInstruction::Store(addr, SIROffset::Static(store_off), store_bits, _, _)
                        if *addr == src_addr
                            && *store_off >= off
                            && store_off + store_bits <= off + bits =>
                    {
                        found_stores.push((si, *store_off, *store_bits));
                    }
                    SIRInstruction::Store(addr, SIROffset::Dynamic(_), _, _, _)
                        if *addr == src_addr =>
                    {
                        safe = false;
                        break;
                    }
                    SIRInstruction::Load(_, addr, SIROffset::Dynamic(_), _)
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

            found_stores.sort_by_key(|(_, o, _)| *o);
            let total: usize = found_stores.iter().map(|(_, _, b)| b).sum();
            if total != bits {
                continue;
            }
            let mut expected = off;
            let mut contiguous = true;
            for (_, store_off, store_bits) in &found_stores {
                if *store_off != expected {
                    contiguous = false;
                    break;
                }
                expected += store_bits;
            }
            if !contiguous {
                continue;
            }

            let store_updates: Vec<(usize, RegionedAbsoluteAddr)> = found_stores
                .iter()
                .map(|(idx, _, _)| (*idx, dst_addr))
                .collect();
            commit_replacements.push((ci, store_updates));
        }

        if commit_replacements.is_empty() {
            continue;
        }

        let block = eu.blocks.get_mut(&bid).unwrap();

        let mut remove_indices: Vec<usize> = Vec::new();
        for (ci, store_updates) in &commit_replacements {
            remove_indices.push(*ci);
            for (si, new_dst) in store_updates {
                if let SIRInstruction::Store(addr, _, _, _, _) = &mut block.instructions[*si] {
                    *addr = *new_dst;
                }
            }
        }

        remove_indices.sort_unstable();
        for idx in remove_indices.into_iter().rev() {
            block.instructions.remove(idx);
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
                if let SIRInstruction::Store(addr, SIROffset::Static(store_off), store_bits, _, _) =
                    pred_inst
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
                            SIRInstruction::Store(addr, SIROffset::Static(so), sb, _, _)
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

pub(super) fn optimize_commit_sinking(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
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
