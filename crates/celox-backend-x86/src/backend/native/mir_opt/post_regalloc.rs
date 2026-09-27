//! Peepholes and cleanup after physical register assignment.

use super::*;

/// Run peepholes that are safe after register allocation.
///
/// Regalloc rematerializes constants as fresh `LoadImm` instructions. When such
/// a constant has exactly one nearby use, we can fold it back into an existing
/// immediate-form MIR instruction without changing liveness or adding new
/// VRegs. The assignment map may still contain the removed VReg; it is simply
/// no longer referenced by emitted code.
pub fn post_regalloc_peephole(func: &mut MFunction, assignment: &AssignmentMap) {
    const IMM_FOLD_SCAN_LIMIT: usize = 8;

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
        let mut remove = vec![false; block.insts.len()];
        let mut replacements: HashMap<usize, MInst> = HashMap::default();

        // A state-home rematerialization immediately before a forwarded
        // width-normalizing copy is one machine load, not a load followed by
        // another normalization.  Keep the copy form while allocating so the
        // stored value may remain resident; once allocation chose the memory
        // recipe, retarget the unsigned load directly to the copy result.
        for idx in 0..block.insts.len().saturating_sub(1) {
            let MInst::Load {
                dst: loaded,
                base,
                offset,
                size,
            } = block.insts[idx]
            else {
                continue;
            };
            if use_counts.get(&loaded).copied().unwrap_or(0) != 1 {
                continue;
            }
            let destination = match (size, &block.insts[idx + 1]) {
                (
                    OpSize::S8,
                    MInst::AndImm32 {
                        dst,
                        src,
                        imm: 0xff,
                    },
                ) if *src == loaded => Some(*dst),
                (
                    OpSize::S16,
                    MInst::AndImm32 {
                        dst,
                        src,
                        imm: 0xffff,
                    },
                ) if *src == loaded => Some(*dst),
                (OpSize::S32, MInst::Mov32 { dst, src }) if *src == loaded => Some(*dst),
                (_, MInst::Mov { dst, src }) if *src == loaded => Some(*dst),
                _ => None,
            };
            let Some(destination) = destination else {
                continue;
            };
            replacements.insert(
                idx,
                MInst::Load {
                    dst: destination,
                    base,
                    offset,
                    size,
                },
            );
            remove[idx + 1] = true;
        }

        for (idx, remove_imm) in remove.iter_mut().enumerate() {
            let MInst::LoadImm {
                dst: imm_vreg,
                value,
            } = block.insts[idx]
            else {
                continue;
            };
            if use_counts.get(&imm_vreg).copied().unwrap_or(0) != 1 {
                continue;
            }

            let end = (idx + IMM_FOLD_SCAN_LIMIT + 1).min(block.insts.len());
            for use_idx in idx + 1..end {
                if !block.insts[use_idx].uses().contains(&imm_vreg) {
                    continue;
                }
                if let Some(folded) = fold_imm_use(&block.insts[use_idx], imm_vreg, value) {
                    *remove_imm = true;
                    replacements.insert(use_idx, folded);
                }
                break;
            }
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

    eliminate_loads_clobbered_by_same_register_constants(func, assignment);
}

/// Drop machine loads whose value is immediately overwritten by a
/// rematerialized constant in the same physical register.
///
/// Allocation rematerializes constants as fresh `LoadImm` definitions and may
/// coalesce one with an adjacent load's destination. The two back-to-back
/// definitions leave no observable window between them: nothing can read the
/// shared register before the constant lands, and every later read observes
/// the constant either way, so the load is pure overhead (one memory access
/// per occurrence in the emitted tick loop).
///
/// The load's VReg is single-definition (enforced by the MIR verifier), so
/// redirecting all of its uses to the constant VReg preserves semantics:
/// those uses already read the shared physical register containing the
/// constant. Elimination only fires when every use of the loaded value is a
/// plain instruction use later in the same block; phi sources and cross-block
/// uses keep the load alive.
fn eliminate_loads_clobbered_by_same_register_constants(
    func: &mut MFunction,
    assignment: &AssignmentMap,
) {
    let mut global_uses: HashMap<VReg, usize> = HashMap::default();
    for block in &func.blocks {
        for phi in &block.phis {
            for (_, src) in &phi.sources {
                *global_uses.entry(*src).or_default() += 1;
            }
        }
        for inst in &block.insts {
            for used in inst.uses() {
                *global_uses.entry(used).or_default() += 1;
            }
        }
    }

    for block in &mut func.blocks {
        let mut local_uses: HashMap<VReg, Vec<usize>> = HashMap::default();
        for (idx, inst) in block.insts.iter().enumerate() {
            for used in inst.uses() {
                local_uses.entry(used).or_default().push(idx);
            }
        }

        let mut eliminated = Vec::<usize>::new();
        for idx in 0..block.insts.len().saturating_sub(1) {
            let (load_dst, constant_dst) = match (&block.insts[idx], &block.insts[idx + 1]) {
                (
                    MInst::Load {
                        dst: loaded,
                        base: _,
                        offset: _,
                        size: _,
                    },
                    MInst::LoadImm {
                        dst: constant,
                        value: _,
                    },
                ) => (*loaded, *constant),
                _ => continue,
            };
            let (Some(loaded_reg), Some(constant_reg)) =
                (assignment.get(load_dst), assignment.get(constant_dst))
            else {
                continue;
            };
            if loaded_reg != constant_reg {
                continue;
            }
            let Some(use_indices) = local_uses.get(&load_dst) else {
                // Fully dead loads are handled by ordinary DCE in
                // post_regalloc_cleanup; leave them alone here.
                continue;
            };
            if use_indices.first().is_some_and(|first| *first <= idx + 1)
                || global_uses.get(&load_dst).copied().unwrap_or(0) != use_indices.len()
            {
                continue;
            }
            for &use_idx in use_indices {
                block.insts[use_idx].rewrite_use(load_dst, constant_dst);
            }
            eliminated.push(idx);
        }

        if !eliminated.is_empty() {
            let mut next = eliminated.iter();
            let mut pending = next.next();
            block.insts = std::mem::take(&mut block.insts)
                .into_iter()
                .enumerate()
                .filter_map(|(idx, inst)| {
                    if pending == Some(&idx) {
                        pending = next.next();
                        None
                    } else {
                        Some(inst)
                    }
                })
                .collect();
        }
    }
}

/// Remove allocation-created trivial values before machine emission.
///
/// Allocation can rewrite two distinct incoming values to the same split
/// representative. Preserve the assigned destination with a Mov, but do not
/// leave the now-irrelevant predicate graph or rematerializations in the
/// emitted function. Copy propagation is intentionally not run after
/// allocation because the source physical register may be clobbered after the
/// copy; ordinary DCE is safe.
pub fn post_regalloc_cleanup(func: &mut MFunction) {
    simplify_equal_value_selects(func);
    dead_code_eliminate_preserving_phis(func);
    simplify_cfg(func);
}

/// Reuse an exact direct load while its assigned physical register still
/// contains that value.
///
/// CSSA intentionally gives interfering phi rows distinct edge snapshots.
/// Several snapshots can nevertheless have the same MemorySSA recipe, and
/// allocation can materialize each snapshot as an independent state load.
/// Spill splitting can likewise reload one stack home repeatedly inside a
/// block. At this late boundary the completed assignment tells us exactly
/// whether a prior loaded value remains physically available, so a duplicate
/// load can become a copy without guessing at an extended live range.
///
/// Availability is local to one block. Any definition or explicit target
/// clobber kills the value in its assigned register; overlapping or unknown
/// writes to the same direct base kill the corresponding memory value. At most
/// one value is tracked per allocatable register, making the pass
/// O(instructions * target-registers) time and O(target-registers) space per
/// block.
pub(crate) fn post_regalloc_direct_load_cse(
    func: &mut MFunction,
    assignment: &AssignmentMap,
) -> usize {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct DirectLoadKey {
        base: BaseReg,
        offset: i32,
        size: OpSize,
    }

    #[derive(Debug, Clone, Copy)]
    struct AvailableDirectLoad {
        key: DirectLoadKey,
        value: VReg,
        available_since: usize,
    }

    fn overlaps(key: DirectLoadKey, range: memory_effect::MemoryRange) -> bool {
        if key.base != range.base {
            return false;
        }
        let load_start = i64::from(key.offset);
        let load_end = load_start + i64::from(key.size.bytes());
        range
            .end()
            .is_none_or(|write_end| load_start < write_end && range.offset < load_end)
    }

    let mut reused = 0usize;
    for block in &mut func.blocks {
        let mut available = HashMap::<PhysReg, AvailableDirectLoad>::default();
        let mut replacements = Vec::<(usize, VReg, VReg)>::new();
        for (position, instruction) in block.insts.iter().enumerate() {
            let writes = memory_effect::writes(instruction);
            if let Some(memory_effect::UnknownMemory::Direct(base)) = writes.unknown_memory() {
                available.retain(|_, loaded| loaded.key.base != base);
            }
            for range in writes.ranges() {
                available.retain(|_, loaded| !overlaps(loaded.key, range));
            }
            for &register in clobbers(instruction) {
                available.remove(&register);
            }

            let direct_load = match instruction {
                MInst::Load {
                    dst,
                    base,
                    offset,
                    size,
                } => Some((
                    *dst,
                    DirectLoadKey {
                        base: *base,
                        offset: *offset,
                        size: *size,
                    },
                )),
                _ => None,
            };
            let definition_register = instruction.def().and_then(|value| assignment.get(value));
            let source = direct_load.and_then(|(_, key)| {
                available
                    .iter()
                    .filter(|(_, loaded)| loaded.key == key)
                    .min_by_key(|(register, loaded)| {
                        (
                            Some(**register) != definition_register,
                            loaded.available_since,
                            **register,
                        )
                    })
                    .map(|(_, loaded)| loaded.value)
            });

            if let Some(register) = definition_register {
                available.remove(&register);
            }

            let Some((destination, key)) = direct_load else {
                continue;
            };
            let Some(register) = definition_register else {
                continue;
            };
            if let Some(source) = source {
                replacements.push((position, destination, source));
                reused += 1;
            }
            available.insert(
                register,
                AvailableDirectLoad {
                    key,
                    value: destination,
                    available_since: position,
                },
            );
        }

        for (position, destination, source) in replacements {
            block.insts[position] = MInst::Mov {
                dst: destination,
                src: source,
            };
        }
    }
    reused
}
