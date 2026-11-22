//! Publish graph results from the registers that sampled them. Procedural
//! publications and guarded RAM writes retain their original positions.
use super::*;
use celox_design::SPARSE_WORKING_REGION;
use celox_sir::{BlockId, cfg::SirDominance};
use celox_slt::next_state::sample_enabled_transition;

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

struct PrivateSlots {
    captured: HashSet<AbsoluteAddr>,
    /// No other publication can change these destinations after sampling.
    unchanged: HashSet<AbsoluteAddr>,
}

/// One graph writer and one unconditional publication own the working slot.
/// An earlier writer of the same sparse array could leave other dirty entries.
fn private_slots(
    form: &NextStateForm<AbsoluteAddr>,
    before: &[Unit],
    apply: &[Unit],
) -> Option<PrivateSlots> {
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
    let mut changed = HashSet::default();
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
                    changed.extend(
                        inst.state_writes()
                            .objects()?
                            .filter(|a| a.region == STABLE_REGION)
                            .map(|a| a.absolute_addr()),
                    );
                }
            }
        }
    }
    published.retain(|id| !excluded.contains(id));
    let unchanged = published.difference(&changed).copied().collect();
    Some(PrivateSlots {
        captured: published,
        unchanged,
    })
}

pub(super) fn combined(
    arena: &SLTNodeArena<AbsoluteAddr>,
    form: &NextStateForm<AbsoluteAddr>,
    elements: &HashMap<AbsoluteAddr, usize>,
    before: &[Unit],
    apply: &[Unit],
) -> Result<Option<Unit>, ParserError> {
    let Some(slots) = private_slots(form, before, apply).filter(|s| !s.captured.is_empty()) else {
        return Ok(None);
    };
    let (sample, stores) = sample_enabled_transition(
        arena,
        form,
        |a, r| RegionedAbsoluteAddr::from_absolute_addr(r, a),
        elements,
        &slots.captured,
        &slots.unchanged,
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
    for run in guards.chunk_by(|(a, i, x), (b, j, y)| a == b && *i == j + 1 && x == y) {
        let (id, last, guard) = run[0];
        let first = run.last().unwrap().1;
        let write = BlockId(next);
        let join = BlockId(next + 1);
        next += 2;
        let block = unit.blocks.get_mut(&id).unwrap();
        let tail = block.instructions.split_off(last + 1);
        let stores = block.instructions.split_off(first);
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
                instructions: stores,
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
    tracing::debug!(target: "celox::edge_graph", sampled = slots.captured.len(), "published graph samples");
    Ok(Some(unit))
}

#[cfg(test)]
mod tests {
    use super::*;
    use celox_sir::SIRBuilder;
    use celox_slt::next_state::{RamWritePort, ValueRoot};

    #[test]
    fn shared_feedback_is_guarded_only_while_the_sampled_destination_is_unchanged() {
        use celox_slt::SLTNode;
        let addr = |n| AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: SourceVarId(n),
        };
        let region = RegionedAbsoluteAddr::from_absolute_addr;
        let access = BitAccess::new(0, 0);
        let mut arena = SLTNodeArena::new();
        let mut input = |n| {
            arena
                .alloc(SLTNode::Input {
                    variable: addr(n),
                    access,
                    index: Vec::new(),
                    signed: false,
                })
                .unwrap()
        };
        let cond = input(0);
        let data = input(1);
        let old = [input(2), input(3), input(4)];
        let mut form = NextStateForm::default();
        let mut apply = SIRBuilder::new();
        // Another writer replaces q4 before its NBA. On a disabled edge
        // q4 must publish the value sampled before this competing write.
        apply.emit(SIRInstruction::Commit(
            region(STABLE_REGION, addr(1)),
            region(STABLE_REGION, addr(4)),
            SIROffset::Static(0),
            1,
            Vec::new(),
        ));
        for (i, old) in old.into_iter().enumerate() {
            let target = addr(i as u32 + 2);
            let value = arena
                .alloc(SLTNode::Mux {
                    cond,
                    then_expr: old,
                    else_expr: data,
                })
                .unwrap();
            form.roots.push(ValueRoot {
                target,
                access,
                value,
            });
            apply.emit(SIRInstruction::Commit(
                region(WORKING_REGION, target),
                region(STABLE_REGION, target),
                SIROffset::Static(0),
                1,
                Vec::new(),
            ));
        }
        let unit = combined(
            &arena,
            &form,
            &HashMap::default(),
            &[],
            &[apply.flush_eu().unwrap()],
        )
        .unwrap()
        .unwrap();
        unit.verify();
        let branch = unit
            .blocks
            .values()
            .find_map(|block| match &block.terminator {
                SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } => Some((true_block.0, false_block.0)),
                _ => None,
            })
            .unwrap();
        let stores = |id| {
            unit.blocks[&id]
                .instructions
                .iter()
                .filter_map(|i| match i {
                    SIRInstruction::Store(a, ..) => Some(a.absolute_addr()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(stores(branch.0), [addr(2), addr(3)]);
        assert_eq!(stores(branch.1), [addr(4)]);
        assert!(
            unit.blocks
                .values()
                .flat_map(|b| &b.instructions)
                .any(|i| matches!(i,SIRInstruction::Load(_,a,..) if a.absolute_addr() == addr(4)))
        );
    }

    #[test]
    fn a_procedure_orders_only_the_affected_roots_and_ram() {
        let addr = |n| AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: SourceVarId(n),
        };
        let stable = |id| RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, id);
        let mut arena = SLTNodeArena::new();
        let access = BitAccess::new(0, 0);
        let mut input = |n| {
            arena
                .alloc(celox_slt::SLTNode::Input {
                    variable: addr(n),
                    signed: false,
                    index: vec![],
                    access,
                })
                .unwrap()
        };
        let x = input(0);
        let y = input(1);
        let root = |n, value| ValueRoot {
            target: addr(n),
            access,
            value,
        };
        let port = |index| RamWritePort {
            array: addr(4),
            index,
            guard: y,
            value: y,
            element_width: 1,
            access,
        };
        let mut form = NextStateForm {
            roots: vec![root(2, x), root(3, y)],
            ports: vec![port(y), port(x)],
        };
        let mut b = SIRBuilder::new();
        b.emit(SIRInstruction::Commit(
            stable(addr(1)),
            stable(addr(0)),
            SIROffset::Static(0),
            1,
            Vec::new(),
        ));
        let early = take_conflicting_samples(&mut form, &arena, &b.flush_eu().unwrap(), true);
        assert_eq!(early.roots, [root(2, x)]);
        assert_eq!(early.ports, [port(y), port(x)]);
        assert_eq!(form.roots, [root(3, y)]);
        assert!(form.ports.is_empty());
    }

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
        assert_eq!(private_slots(&form, &[], &apply).unwrap().captured.len(), 2);
        b.emit(SIRInstruction::Commit(
            stable(array),
            sparse,
            SIROffset::Static(8),
            8,
            Vec::new(),
        ));
        let before = [b.flush_eu().unwrap()];
        assert_eq!(
            private_slots(&form, &before, &apply).unwrap().captured,
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
