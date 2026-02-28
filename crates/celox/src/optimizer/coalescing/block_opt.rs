use super::shared::replace_reg_in_terminator;
use crate::ir::*;
use crate::{HashMap, HashSet};
use malachite_bigint::BigUint;

fn def_reg<A>(inst: &SIRInstruction<A>) -> Option<RegisterId> {
    match inst {
        SIRInstruction::Imm(dst, _)
        | SIRInstruction::Binary(dst, _, _, _)
        | SIRInstruction::Unary(dst, _, _)
        | SIRInstruction::Load(dst, _, _, _)
        | SIRInstruction::Concat(dst, _) => Some(*dst),
        SIRInstruction::Store(_, _, _, _, _) | SIRInstruction::Commit(_, _, _, _, _) => None,
    }
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

fn may_alias<A: PartialEq>(a: (&A, Option<usize>, usize), b: (&A, Option<usize>, usize)) -> bool {
    if a.0 != b.0 {
        return false;
    }
    match (a.1, b.1) {
        (Some(off_a), Some(off_b)) => off_a < off_b + b.2 && off_b < off_a + a.2,
        _ => true,
    }
}

fn schedule_block_interleaved<A: Clone + PartialEq>(
    window: &[SIRInstruction<A>],
    max_inflight_loads: usize,
) -> Vec<SIRInstruction<A>> {
    let n = window.len();
    if n <= 1 {
        return window.to_vec();
    }

    let mut defs: Vec<Option<RegisterId>> = Vec::with_capacity(n);
    let mut uses: Vec<Vec<RegisterId>> = Vec::with_capacity(n);
    for inst in window {
        defs.push(def_reg(inst));
        let mut u = Vec::new();
        collect_used_regs(inst, &mut u);
        uses.push(u);
    }

    let mut succs: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut indeg = vec![0usize; n];

    let add_edge = |from: usize, to: usize, succs: &mut Vec<Vec<usize>>, indeg: &mut Vec<usize>| {
        if !succs[from].contains(&to) {
            succs[from].push(to);
            indeg[to] += 1;
        }
    };

    for i in 0..n {
        for j in (i + 1)..n {
            let mut dep = false;

            if let Some(d) = defs[i]
                && uses[j].contains(&d)
            {
                dep = true;
            }

            if let (Some(ai), Some(aj)) = (mem_access_info(&window[i]), mem_access_info(&window[j]))
            {
                let alias = may_alias((ai.0, ai.1, ai.2), (aj.0, aj.1, aj.2));
                if alias {
                    let i_write = ai.3;
                    let j_write = aj.3;
                    if i_write || j_write {
                        dep = true;
                    }
                }
            }

            if dep {
                add_edge(i, j, &mut succs, &mut indeg);
            }
        }
    }

    let mut out = Vec::with_capacity(n);
    let mut scheduled = vec![false; n];
    let mut scheduled_count = 0usize;
    let mut inflight_loads: HashSet<RegisterId> = HashSet::default();

    while scheduled_count < n {
        let mut ready: Vec<usize> = (0..n).filter(|&i| !scheduled[i] && indeg[i] == 0).collect();

        if ready.is_empty() {
            let k = (0..n).find(|&i| !scheduled[i]).unwrap();
            ready.push(k);
        }

        ready.sort_unstable();

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

        let inst = window[pick].clone();
        if let SIRInstruction::Load(dst, _, _, _) = inst {
            inflight_loads.insert(dst);
        }

        for r in &uses[pick] {
            inflight_loads.remove(r);
        }

        out.push(inst);
        scheduled[pick] = true;
        scheduled_count += 1;

        for &s in &succs[pick] {
            indeg[s] = indeg[s].saturating_sub(1);
        }
    }

    out
}

pub(super) fn schedule_instructions<A: Clone + PartialEq>(
    instructions: &mut [SIRInstruction<A>],
    max_inflight_loads: usize,
) {
    let n = instructions.len();
    if n <= 1 {
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

pub(super) fn optimize_block<A: Clone + std::fmt::Debug + PartialEq + Ord + std::hash::Hash>(
    block: &mut BasicBlock<A>,
    register_map: &mut HashMap<RegisterId, RegisterType>,
    unit_replacement_map: &mut HashMap<RegisterId, RegisterId>,
) {
    const MAX_INFLIGHT_LOADS: usize = 8;
    coalesce_static_loads(&mut block.instructions, register_map);

    let mut new_instructions = Vec::with_capacity(block.instructions.len());
    let mut replaced_indices = std::collections::HashSet::new();
    let mut insertions: HashMap<usize, Vec<SIRInstruction<A>>> = HashMap::default();

    type StoreGroupKey<A> = A;
    let mut groups: HashMap<StoreGroupKey<A>, Vec<usize>> = HashMap::default();

    for (idx, inst) in block.instructions.iter().enumerate() {
        if let SIRInstruction::Store(addr, SIROffset::Static(_), _, _, _) = inst {
            let key = addr.clone();
            groups.entry(key).or_default().push(idx);
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
            if let SIRInstruction::Store(_, SIROffset::Static(o), w, s, t) =
                &block.instructions[idx]
            {
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

        let mut segment_start = 0;
        while segment_start < details.len() {
            let mut segment_end = segment_start;
            let mut expected_next_offset =
                details[segment_start].offset + details[segment_start].width;

            for k in (segment_start + 1)..details.len() {
                if details[k].offset == expected_next_offset {
                    segment_end = k;
                    expected_next_offset += details[k].width;
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

                let mut safe = true;
                for s in segment {
                    if s.index == insert_at_index {
                        continue;
                    }
                    for check_idx in (s.index + 1)..=insert_at_index {
                        let inst = &block.instructions[check_idx];
                        if let SIRInstruction::Load(_, a, SIROffset::Static(o), w) = inst
                            && *a == addr
                        {
                            let range1 = s.offset..(s.offset + s.width);
                            let range2 = *o..(*o + *w);
                            if range1.start < range2.end && range2.start < range1.end {
                                safe = false;
                                break;
                            }
                        }
                        if let SIRInstruction::Load(_, a, SIROffset::Dynamic(_), _) = inst
                            && *a == addr
                        {
                            safe = false;
                            break;
                        }
                    }
                    if !safe {
                        break;
                    }
                }

                if safe {
                    let total_width: usize = segment.iter().map(|s| s.width).sum();
                    let start_offset = segment[0].offset;
                    let args: Vec<RegisterId> = segment.iter().rev().map(|s| s.src).collect();
                    let triggers: Vec<crate::ir::TriggerIdWithKind> =
                        segment.iter().flat_map(|s| s.triggers.clone()).collect();

                    let new_reg_id =
                        RegisterId(register_map.keys().map(|r| r.0).max().unwrap_or(0) + 1);
                    register_map.insert(new_reg_id, RegisterType::Logic { width: total_width });

                    for s in segment {
                        replaced_indices.insert(s.index);
                    }

                    let mut new_ops = Vec::new();
                    new_ops.push(SIRInstruction::Concat(new_reg_id, args));
                    new_ops.push(SIRInstruction::Store(
                        addr.clone(),
                        SIROffset::Static(start_offset),
                        total_width,
                        new_reg_id,
                        triggers,
                    ));

                    insertions
                        .entry(insert_at_index)
                        .or_default()
                        .extend(new_ops);
                }
            }

            segment_start = segment_end + 1;
        }
    }

    for i in 0..block.instructions.len() {
        if !replaced_indices.contains(&i) {
            new_instructions.push(block.instructions[i].clone());
        }
        if let Some(ops) = insertions.remove(&i) {
            new_instructions.extend(ops);
        }
    }

    let mut local_replacement_map = HashMap::default();
    eliminate_redundant_loads(&mut new_instructions, &mut local_replacement_map);

    for (from, to) in local_replacement_map {
        unit_replacement_map.insert(from, to);
        replace_reg_in_terminator(&mut block.terminator, from, to);
    }

    schedule_instructions(new_instructions.as_mut_slice(), MAX_INFLIGHT_LOADS);

    block.instructions = new_instructions;
}

fn coalesce_static_loads<A: Clone + std::fmt::Debug + PartialEq + Ord + std::hash::Hash>(
    instructions: &mut Vec<SIRInstruction<A>>,
    register_map: &mut HashMap<RegisterId, RegisterType>,
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

    fn next_reg_id(map: &HashMap<RegisterId, RegisterType>) -> RegisterId {
        RegisterId(map.keys().map(|r| r.0).max().unwrap_or(0) + 1)
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

            let wide_reg = next_reg_id(register_map);
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
                    let shift_reg = next_reg_id(register_map);
                    register_map.insert(shift_reg, RegisterType::Logic { width: 64 });
                    ops.push(SIRInstruction::Imm(
                        shift_reg,
                        SIRValue::new(rel_off as u64),
                    ));

                    let shifted_reg = next_reg_id(register_map);
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
                    let mask_reg = next_reg_id(register_map);
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
                    let zero_reg = next_reg_id(register_map);
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
    for i in 0..instructions.len() {
        if let Some(ops) = insertions.remove(&i) {
            out.extend(ops);
        }

        if let Some(ops) = replacements.remove(&i) {
            out.extend(ops);
        } else {
            out.push(instructions[i].clone());
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
