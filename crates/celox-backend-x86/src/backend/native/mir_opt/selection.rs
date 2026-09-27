//! Compare/select fusion and selected indexed-load sinking.

use super::*;

pub(super) fn fuse_compare_selects(func: &mut MFunction) {
    let mut use_counts: HashMap<VReg, usize> = HashMap::default();
    for block in &func.blocks {
        for phi in &block.phis {
            for (_, src) in &phi.sources {
                *use_counts.entry(*src).or_default() += 1;
            }
        }
        for inst in &block.insts {
            for use_vreg in inst.uses() {
                *use_counts.entry(use_vreg).or_default() += 1;
            }
        }
    }

    for block in &mut func.blocks {
        let mut def_pos: HashMap<VReg, usize> = HashMap::default();
        for (idx, inst) in block.insts.iter().enumerate() {
            if let Some(def) = inst.def() {
                def_pos.insert(def, idx);
            }
        }

        let mut remove = vec![false; block.insts.len()];
        let mut replacements: HashMap<usize, MInst> = HashMap::default();

        for (idx, inst) in block.insts.iter().enumerate() {
            let MInst::Select {
                dst,
                cond,
                true_val,
                false_val,
            } = inst
            else {
                continue;
            };
            if use_counts.get(cond).copied().unwrap_or(0) != 1 {
                continue;
            }
            let Some(&cmp_idx) = def_pos.get(cond) else {
                continue;
            };
            if cmp_idx >= idx || remove[cmp_idx] {
                continue;
            }

            let fused = match block.insts[cmp_idx] {
                MInst::Cmp { lhs, rhs, kind, .. } => Some(MInst::CmpSelect {
                    dst: *dst,
                    lhs,
                    rhs,
                    kind,
                    true_val: *true_val,
                    false_val: *false_val,
                }),
                MInst::CmpImm { lhs, imm, kind, .. } => Some(MInst::CmpImmSelect {
                    dst: *dst,
                    lhs,
                    imm,
                    kind,
                    true_val: *true_val,
                    false_val: *false_val,
                }),
                _ => None,
            };

            if let Some(fused) = fused {
                remove[cmp_idx] = true;
                replacements.insert(idx, fused);
            }
        }

        if replacements.is_empty() {
            continue;
        }

        let mut rewritten = Vec::with_capacity(block.insts.len());
        for (idx, inst) in block.insts.iter().enumerate() {
            if remove[idx] {
                continue;
            }
            rewritten.push(replacements.remove(&idx).unwrap_or_else(|| inst.clone()));
        }
        block.insts = rewritten;
    }
}

pub(super) fn select_values(inst: &MInst) -> Option<(VReg, VReg)> {
    match inst {
        MInst::Select {
            true_val,
            false_val,
            ..
        }
        | MInst::CmpSelect {
            true_val,
            false_val,
            ..
        }
        | MInst::CmpImmSelect {
            true_val,
            false_val,
            ..
        }
        | MInst::GuardedCmpSelect {
            true_val,
            false_val,
            ..
        } => Some((*true_val, *false_val)),
        _ => None,
    }
}

fn rebuild_select(inst: &MInst, dst: VReg, true_val: VReg, false_val: VReg) -> Option<MInst> {
    match inst {
        MInst::Select { cond, .. } => Some(MInst::Select {
            dst,
            cond: *cond,
            true_val,
            false_val,
        }),
        MInst::CmpSelect { lhs, rhs, kind, .. } => Some(MInst::CmpSelect {
            dst,
            lhs: *lhs,
            rhs: *rhs,
            kind: *kind,
            true_val,
            false_val,
        }),
        MInst::CmpImmSelect { lhs, imm, kind, .. } => Some(MInst::CmpImmSelect {
            dst,
            lhs: *lhs,
            imm: *imm,
            kind: *kind,
            true_val,
            false_val,
        }),
        MInst::GuardedCmpSelect {
            guard,
            lhs,
            rhs,
            kind,
            ..
        } => Some(MInst::GuardedCmpSelect {
            dst,
            guard: *guard,
            lhs: *lhs,
            rhs: *rhs,
            kind: *kind,
            true_val,
            false_val,
        }),
        _ => None,
    }
}

struct IndexedLoadSelectionPlan {
    root: usize,
    remove: Vec<usize>,
    replacement: Vec<MInst>,
}

#[derive(Clone, Copy)]
struct IndexedLoadTreeSummary {
    base: BaseReg,
    index: VReg,
    size: OpSize,
    first_definition: usize,
}

impl IndexedLoadTreeSummary {
    fn same_address_shape(self, other: Self) -> bool {
        self.base == other.base && self.index == other.index && self.size == other.size
    }
}

/// Turn a tree of selected indexed loads into one indexed load from a selected
/// address.
///
/// This is the machine-level form of LLVM's load/select sinking:
///
/// `select p, load [base + a + i], load [base + b + i]`
/// becomes
/// `load [base + select(p, a, b) + i]`.
///
/// Simulator memory is non-faulting inside the alias envelopes carried by the
/// original loads.  The rewrite nevertheless stays within one block, requires
/// every arm and intermediate select to be exclusively consumed by the tree,
/// and rejects any intervening write.  The selected load receives the union of
/// the original alias envelopes.  Each instruction and operand edge belongs to
/// at most one accepted component, so discovery and rewriting are linear in
/// the MIR size.  A forward summary rejects malformed subtrees once, before
/// maximal roots are traversed, so failed nested candidates cannot cause
/// quadratic rediscovery.
pub(super) fn sink_selected_indexed_loads(func: &mut MFunction) {
    let mut use_counts = HashMap::<VReg, usize>::default();
    for block in &func.blocks {
        for phi in &block.phis {
            for (_, source) in &phi.sources {
                *use_counts.entry(*source).or_default() += 1;
            }
        }
        for inst in &block.insts {
            for source in inst.uses() {
                *use_counts.entry(source).or_default() += 1;
            }
        }
    }

    let mut plans = Vec::<Vec<IndexedLoadSelectionPlan>>::with_capacity(func.blocks.len());
    for block in &func.blocks {
        let mut definitions = HashMap::<VReg, usize>::default();
        for (index, inst) in block.insts.iter().enumerate() {
            if let Some(dst) = inst.def() {
                definitions.insert(dst, index);
            }
        }

        let mut write_prefix = Vec::with_capacity(block.insts.len() + 1);
        write_prefix.push(0usize);
        for inst in &block.insts {
            write_prefix.push(
                write_prefix.last().copied().unwrap()
                    + usize::from(memory_effect::writes(inst).has_effect()),
            );
        }
        let mut selectable = HashMap::<VReg, IndexedLoadTreeSummary>::default();
        for (instruction, inst) in block.insts.iter().enumerate() {
            if let MInst::LoadIndexed {
                dst,
                base,
                index,
                scale,
                size,
                ..
            } = inst
            {
                if *scale != 1 {
                    continue;
                }
                selectable.insert(
                    *dst,
                    IndexedLoadTreeSummary {
                        base: *base,
                        index: *index,
                        size: *size,
                        first_definition: instruction,
                    },
                );
                continue;
            }
            let Some((true_val, false_val)) = select_values(inst) else {
                continue;
            };
            if use_counts.get(&true_val).copied() != Some(1)
                || use_counts.get(&false_val).copied() != Some(1)
            {
                continue;
            }
            let Some(true_tree) = selectable.get(&true_val).copied() else {
                continue;
            };
            let Some(false_tree) = selectable.get(&false_val).copied() else {
                continue;
            };
            if !true_tree.same_address_shape(false_tree) {
                continue;
            }
            let first_definition = true_tree.first_definition.min(false_tree.first_definition);
            if write_prefix[instruction] != write_prefix[first_definition] {
                continue;
            }
            selectable.insert(
                inst.def().unwrap(),
                IndexedLoadTreeSummary {
                    first_definition,
                    ..true_tree
                },
            );
        }

        let mut claimed = HashSet::<usize>::default();
        let mut block_plans = Vec::new();
        for root in (0..block.insts.len()).rev() {
            if claimed.contains(&root)
                || select_values(&block.insts[root]).is_none()
                || block.insts[root]
                    .def()
                    .is_none_or(|dst| !selectable.contains_key(&dst))
            {
                continue;
            }

            let mut pending = Vec::new();
            let (true_val, false_val) = select_values(&block.insts[root]).unwrap();
            pending.push(true_val);
            pending.push(false_val);
            let mut select_indices = vec![root];
            let mut load_indices = Vec::new();
            let mut seen_values = HashSet::default();
            let mut failed = false;

            while let Some(value) = pending.pop() {
                if !seen_values.insert(value) {
                    continue;
                }
                let Some(&definition) = definitions.get(&value) else {
                    failed = true;
                    break;
                };
                if definition >= root || claimed.contains(&definition) {
                    failed = true;
                    break;
                }
                match &block.insts[definition] {
                    MInst::LoadIndexed { .. } => {
                        if use_counts.get(&value).copied() != Some(1) {
                            failed = true;
                            break;
                        }
                        load_indices.push(definition);
                    }
                    inst if select_values(inst).is_some()
                        && use_counts.get(&value).copied() == Some(1) =>
                    {
                        select_indices.push(definition);
                        let (true_val, false_val) = select_values(inst).unwrap();
                        pending.push(true_val);
                        pending.push(false_val);
                    }
                    _ => {
                        failed = true;
                        break;
                    }
                }
            }
            if failed || load_indices.len() < 2 {
                continue;
            }

            let MInst::LoadIndexed {
                base,
                offset: first_offset,
                index,
                size,
                ..
            } = block.insts[load_indices[0]]
            else {
                unreachable!()
            };
            let mut min_offset = first_offset;
            let mut alias_start = i64::MAX;
            let mut alias_end = i64::MIN;
            let mut complete_alias = true;
            for &load_index in &load_indices {
                let MInst::LoadIndexed {
                    base: candidate_base,
                    offset,
                    index: candidate_index,
                    size: candidate_size,
                    alias_range,
                    ..
                } = block.insts[load_index]
                else {
                    unreachable!()
                };
                if candidate_base != base || candidate_index != index || candidate_size != size {
                    failed = true;
                    break;
                }
                min_offset = min_offset.min(offset);
                if let Some(range) = alias_range {
                    alias_start = alias_start.min(i64::from(range.offset()));
                    alias_end = alias_end.max(range.end());
                } else {
                    complete_alias = false;
                }
            }
            if failed {
                continue;
            }

            let first_definition = load_indices.iter().copied().min().unwrap();
            if block.insts[first_definition..root]
                .iter()
                .any(|inst| memory_effect::writes(inst).has_effect())
            {
                continue;
            }

            select_indices.sort_unstable();
            load_indices.sort_unstable();
            let mut replacement = Vec::new();
            let mut remapped = HashMap::<VReg, VReg>::default();
            let mut offset_constants = HashMap::<i32, VReg>::default();
            for &load_index in &load_indices {
                let MInst::LoadIndexed { dst, offset, .. } = block.insts[load_index] else {
                    unreachable!()
                };
                let delta = offset
                    .checked_sub(min_offset)
                    .map(|value| value as u32 as u64);
                let Some(delta) = delta else {
                    failed = true;
                    break;
                };
                let selected_offset = *offset_constants.entry(offset).or_insert_with(|| {
                    let value = func.vregs.alloc();
                    while func.spill_descs.len() <= value.0 as usize {
                        func.spill_descs.push(SpillDesc::remat(delta));
                    }
                    replacement.push(MInst::LoadImm {
                        dst: value,
                        value: delta,
                    });
                    value
                });
                remapped.insert(dst, selected_offset);
            }
            if failed {
                continue;
            }

            for &select_index in &select_indices {
                let inst = &block.insts[select_index];
                let (old_true, old_false) = select_values(inst).unwrap();
                let Some(&true_val) = remapped.get(&old_true) else {
                    failed = true;
                    break;
                };
                let Some(&false_val) = remapped.get(&old_false) else {
                    failed = true;
                    break;
                };
                let old_dst = inst.def().unwrap();
                let selected_offset = func.vregs.alloc();
                while func.spill_descs.len() <= selected_offset.0 as usize {
                    func.spill_descs.push(SpillDesc::transient());
                }
                replacement
                    .push(rebuild_select(inst, selected_offset, true_val, false_val).unwrap());
                remapped.insert(old_dst, selected_offset);
            }
            if failed {
                continue;
            }

            let root_dst = block.insts[root].def().unwrap();
            let selected_offset = remapped[&root_dst];
            let combined_index = func.vregs.alloc();
            while func.spill_descs.len() <= combined_index.0 as usize {
                func.spill_descs.push(SpillDesc::transient());
            }
            replacement.push(MInst::Add {
                dst: combined_index,
                lhs: index,
                rhs: selected_offset,
            });
            let alias_range = if complete_alias {
                usize::try_from(alias_end - alias_start)
                    .ok()
                    .and_then(|byte_len| i32::try_from(alias_start).ok().zip(Some(byte_len)))
                    .and_then(|(offset, byte_len)| MemoryAliasRange::new(offset, byte_len))
            } else {
                None
            };
            replacement.push(MInst::LoadIndexed {
                dst: root_dst,
                base,
                offset: min_offset,
                index: combined_index,
                scale: 1,
                size,
                alias_range,
            });

            let mut remove = select_indices;
            remove.extend(load_indices);
            remove.sort_unstable();
            remove.dedup();
            claimed.extend(remove.iter().copied());
            block_plans.push(IndexedLoadSelectionPlan {
                root,
                remove,
                replacement,
            });
        }
        plans.push(block_plans);
    }

    for (block, block_plans) in func.blocks.iter_mut().zip(plans) {
        if block_plans.is_empty() {
            continue;
        }
        let mut removals = HashSet::default();
        let mut replacements = HashMap::default();
        for plan in block_plans {
            removals.extend(plan.remove);
            replacements.insert(plan.root, plan.replacement);
        }
        let original = std::mem::take(&mut block.insts);
        let mut rewritten = Vec::with_capacity(original.len());
        for (index, inst) in original.into_iter().enumerate() {
            if let Some(replacement) = replacements.remove(&index) {
                rewritten.extend(replacement);
            } else if !removals.contains(&index) {
                rewritten.push(inst);
            }
        }
        block.insts = rewritten;
    }
}
