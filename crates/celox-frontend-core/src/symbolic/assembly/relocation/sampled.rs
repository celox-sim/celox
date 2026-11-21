//! Publish graph results from the registers that sampled them. Procedural
//! publications and guarded RAM writes retain their original positions.
use super::*;
use celox_design::SPARSE_WORKING_REGION;
use celox_sir::{BlockId, cfg::SirDominance};
use celox_slt::next_state::sample_transition;

type Unit = ExecutionUnit<RegionedAbsoluteAddr>;

fn publication_target(
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
    slots: &HashMap<AbsoluteAddr, Option<BitAccess>>,
) -> Option<AbsoluteAddr> {
    let SIRInstruction::Commit(src, dst, SIROffset::Static(offset), width, triggers) = inst else {
        return None;
    };
    let target = dst.absolute_addr();
    if dst.region != STABLE_REGION || src.absolute_addr() != target || !triggers.is_empty() {
        return None;
    }
    let matches = match slots.get(&target)? {
        Some(range) => {
            src.region == WORKING_REGION
                && *offset == range.lsb
                && *width == range.msb - range.lsb + 1
        }
        None => src.region == SPARSE_WORKING_REGION && *offset == 0,
    };
    matches.then_some(target)
}

/// An opaque call can access any staging slot. A call with known effects
/// excludes only the slots it actually reaches.
fn exclude_accesses(
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
    excluded: &mut HashSet<AbsoluteAddr>,
) -> Option<()> {
    for access in [inst.state_reads(), inst.state_writes()] {
        excluded.extend(
            access
                .objects()?
                .filter(|a| a.region != STABLE_REGION)
                .map(|a| a.absolute_addr()),
        );
    }
    Some(())
}

/// One graph writer and one unconditional publication own the working slot.
/// An earlier writer of the same sparse array could leave other dirty entries.
fn private_slots(
    form: &NextStateForm<AbsoluteAddr>,
    before: &[Unit],
    apply: &[Unit],
) -> Option<HashSet<AbsoluteAddr>> {
    let mut slots = HashMap::default();
    let mut excluded = HashSet::default();
    for (id, shape) in form
        .roots
        .iter()
        .map(|r| (r.target, Some(r.access)))
        .chain(form.ports.iter().map(|p| (p.array, None)))
    {
        if slots.insert(id, shape).is_some() {
            excluded.insert(id);
        }
    }
    for inst in before
        .iter()
        .flat_map(|u| u.blocks.values())
        .flat_map(|b| &b.instructions)
    {
        exclude_accesses(inst, &mut excluded)?;
    }
    let mut published = HashSet::default();
    for unit in apply {
        let mut dominance = None;
        for block in unit.blocks.values() {
            if matches!(block.terminator, SIRTerminator::Error(_)) {
                return None;
            }
            let mut unconditional = None;
            for inst in &block.instructions {
                if inst.may_fail() {
                    return None;
                }
                if let Some(id) = publication_target(inst, &slots) {
                    let always = unconditional.get_or_insert_with(|| {
                        block.id == unit.entry_block_id
                            || dominance
                                .get_or_insert_with(|| SirDominance::analyze(unit).ok())
                                .as_ref()
                                .is_some_and(|dom| {
                                    unit.blocks
                                        .values()
                                        .filter(|b| matches!(b.terminator, SIRTerminator::Return))
                                        .all(|b| dom.dominates(block.id, b.id))
                                })
                    });
                    if !published.insert(id) || !*always {
                        excluded.insert(id);
                    }
                } else {
                    exclude_accesses(inst, &mut excluded)?;
                }
            }
        }
    }
    published.retain(|id| !excluded.contains(id));
    Some(published)
}

pub(super) fn combined(
    arena: &SLTNodeArena<AbsoluteAddr>,
    form: &NextStateForm<AbsoluteAddr>,
    elements: &HashMap<AbsoluteAddr, usize>,
    before: &[Unit],
    apply: &[Unit],
) -> Result<Option<Unit>, ParserError> {
    let Some(slots) = private_slots(form, before, apply).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let (sample, stores) = sample_transition(
        arena,
        form,
        |a, r| RegionedAbsoluteAddr::from_absolute_addr(r, a),
        elements,
        &slots,
    )
    .map_err(|e| ParserError::illegal_context("clock samples", e, None))?;
    let phases: Vec<_> = before.iter().chain([&sample]).chain(apply).collect();
    let (mut unit, mapping) = celox_sir::merge_sir_eu_refs_with_provenance(&phases);
    let mut samples = HashMap::default();
    for sampled in stores {
        let store = mapping.instruction(before.len(), &sampled.store);
        let addr = *store.memory_write().expect("a sampled graph store").addr;
        let guard = sampled.guard.map(|r| mapping.register(before.len(), r));
        samples.insert(addr, (store, guard));
    }
    let mut next = unit.blocks.keys().map(|b| b.0).max().unwrap() + 1;
    let mut guards = Vec::new();
    for (id, block) in &mut unit.blocks {
        if mapping.block_units[id] <= before.len() {
            continue;
        }
        for (index, inst) in block.instructions.iter_mut().enumerate() {
            if let SIRInstruction::Commit(src, dst, ..) = inst
                && src.region != STABLE_REGION
                && dst.region == STABLE_REGION
                && src.absolute_addr() == dst.absolute_addr()
                && let Some((store, guard)) = samples.remove(dst)
            {
                *inst = store;
                if let Some(guard) = guard {
                    guards.push((*id, index, guard));
                }
            }
        }
    }
    debug_assert!(samples.is_empty());
    // Split backwards so earlier instruction positions remain valid.
    guards.sort_unstable_by_key(|(block, index, _)| (*block, std::cmp::Reverse(*index)));
    for (id, index, guard) in guards {
        let write = BlockId(next);
        let join = BlockId(next + 1);
        next += 2;
        let block = unit.blocks.get_mut(&id).unwrap();
        let tail = block.instructions.split_off(index + 1);
        let store = block.instructions.pop().unwrap();
        let terminator = std::mem::replace(
            &mut block.terminator,
            SIRTerminator::Branch {
                cond: guard,
                true_block: (write, Vec::new()),
                false_block: (join, Vec::new()),
            },
        );
        unit.blocks.insert(
            write,
            BasicBlock {
                id: write,
                params: Vec::new(),
                instructions: vec![store],
                terminator: SIRTerminator::Jump(join, Vec::new()),
            },
        );
        unit.blocks.insert(
            join,
            BasicBlock {
                id: join,
                params: Vec::new(),
                instructions: tail,
                terminator,
            },
        );
    }
    tracing::debug!(target: "celox::edge_graph", sampled = slots.len(), "published graph samples");
    Ok(Some(unit))
}

#[cfg(test)]
mod tests {
    use super::*;
    use celox_sir::SIRBuilder;
    use celox_slt::next_state::{RamWritePort, ValueRoot};

    #[test]
    fn a_shared_sparse_slot_cannot_be_replaced_by_one_port() {
        let array = AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: SourceVarId(1),
        };
        let q = AbsoluteAddr {
            var_id: SourceVarId(2),
            ..array
        };
        let stable = |id| RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, id);
        let sparse = RegionedAbsoluteAddr::from_absolute_addr(SPARSE_WORKING_REGION, array);
        let working = RegionedAbsoluteAddr::from_absolute_addr(WORKING_REGION, q);
        let form = NextStateForm {
            roots: vec![ValueRoot {
                target: q,
                access: BitAccess::new(0, 7),
                value: NodeId(0),
            }],
            ports: vec![RamWritePort {
                array,
                index: NodeId(1),
                element_width: 8,
                access: BitAccess::new(0, 7),
                value: NodeId(2),
                guard: NodeId(3),
            }],
        };
        let mut b = SIRBuilder::new();
        b.emit(SIRInstruction::Commit(
            sparse,
            stable(array),
            SIROffset::Static(0),
            32,
            Vec::new(),
        ));
        b.emit(SIRInstruction::Commit(
            working,
            stable(q),
            SIROffset::Static(0),
            8,
            Vec::new(),
        ));
        let apply = [b.flush_eu().unwrap()];
        assert_eq!(private_slots(&form, &[], &apply).unwrap().len(), 2);
        b.emit(SIRInstruction::Commit(
            stable(array),
            sparse,
            SIROffset::Static(8),
            8,
            Vec::new(),
        ));
        let before = [b.flush_eu().unwrap()];
        assert_eq!(
            private_slots(&form, &before, &apply).unwrap(),
            [q].into_iter().collect()
        );
        let reads = [stable(array)].into_iter().collect();
        let writes = [sparse].into_iter().collect();
        assert!(!can_delay_samples(&before[0], &reads, &writes));
        let mut b = SIRBuilder::new();
        b.emit(SIRInstruction::Commit(
            stable(q),
            stable(array),
            SIROffset::Static(0),
            8,
            Vec::new(),
        ));
        assert!(!can_delay_samples(&b.flush_eu().unwrap(), &reads, &writes));
    }
}
