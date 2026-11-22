//! Notifications whose reader is already pending when the publisher runs.
use super::*;
use std::borrow::Cow;

struct Publication {
    producer: usize,
    guard: BlockId,
    publish: BlockId,
    done: BlockId,
    stores: Vec<(usize, Vec<usize>)>,
}

/// The full compiled settle visits partitions in increasing order. If every
/// way of marking P also marks a later Q, Q is still pending when P runs.
/// Keep the original observation units and the clock-only entry independent.
pub(crate) fn full_settle<'a>(
    units: &'a [Unit],
    flags: RegionedAbsoluteAddr,
    settle: &celox_state_layout::SettleLayout,
    offsets: &crate::HashMap<AbsoluteAddr, usize>,
) -> Cow<'a, [Unit]> {
    let Some(edits) = edits(units, &flags, settle, offsets) else {
        return Cow::Borrowed(units);
    };
    let before = units
        .iter()
        .map(|u| {
            u.blocks
                .values()
                .map(|b| b.instructions.len())
                .sum::<usize>()
        })
        .sum::<usize>();
    let mut stats = PublishStats::default();
    let mut units = units.to_vec();
    for (unit, edits) in units.iter_mut().zip(edits) {
        if !edits.is_empty() {
            stats.add(&apply_edits(unit, &flags, edits));
            celox_sir_opt::optimizer::remove_dead_sir_definitions(unit);
        }
    }
    let after = units
        .iter()
        .map(|u| {
            u.blocks
                .values()
                .map(|b| b.instructions.len())
                .sum::<usize>()
        })
        .sum::<usize>();
    tracing::debug!(
        "[coactivation] full settle {before} -> {after} instructions, {} stores dropped, {} branches bypassed",
        stats.dropped,
        stats.bypassed
    );
    Cow::Owned(units)
}

fn edits(
    units: &[Unit],
    flags: &RegionedAbsoluteAddr,
    settle: &celox_state_layout::SettleLayout,
    offsets: &crate::HashMap<AbsoluteAddr, usize>,
) -> Option<Vec<Vec<Edit>>> {
    let n = settle.partitions;
    let mut guarded = vec![false; n];
    let mut candidates = vec![crate::HashSet::default(); n];
    let mut activations = Vec::new();
    let mut publications = Vec::new();
    for unit in units {
        let a = Analysis::new(unit, flags, n, offsets);
        let mut owners = crate::HashMap::default();
        let mut predecessors = crate::HashMap::<_, usize>::default();
        for (&id, block) in &unit.blocks {
            visit_targets(&block.terminator, |target| {
                *predecessors.entry(target).or_default() += 1;
            });
            if let Some((p, body, merge)) = a.guard(id) {
                if std::mem::replace(&mut guarded[p], true) {
                    return None;
                }
                for inner in a.body(body, merge) {
                    if owners.insert(inner, p).is_some() {
                        return None;
                    }
                }
            }
        }
        let mut sites = Vec::new();
        for (&id, block) in &unit.blocks {
            let owner = owners.get(&id).copied();
            let mut marks = crate::HashSet::default();
            for inst in &block.instructions {
                if !inst.state_writes().overlaps(flags, 0, n * 8) {
                    continue;
                }
                // A flag is cleared only by its own partition. Decline
                // dynamic writes, calls and flag words with unknown bytes.
                let SIRInstruction::Store(addr, SIROffset::Static(bit), width, reg, tr, sites) =
                    inst
                else {
                    return None;
                };
                if addr != flags
                    || !bit.is_multiple_of(8)
                    || !width.is_multiple_of(8)
                    || !tr.is_empty()
                    || !sites.is_empty()
                {
                    return None;
                }
                if *width == 8 && a.is_zero(*reg) && owner == Some(bit / 8) {
                    continue;
                }
                let SIRInstruction::Imm(_, value) = a.defs.get(reg)? else {
                    return None;
                };
                let bytes = value.payload.to_bytes_le();
                if value.mask.bits() != 0
                    || bytes.len() != width / 8
                    || bytes.iter().any(|b| *b != 1)
                {
                    return None;
                }
                marks.extend((bit / 8..bit / 8 + bytes.len()).take_while(|&p| p < n));
            }
            if !marks.is_empty() {
                if let Some(producer) = owner {
                    activations.push((producer, marks));
                } else if marks.len() != n {
                    // Only the untracked-entry fill can mark flags outside a partition.
                    return None;
                }
            }
            let Some(producer) = owner else { continue };
            let SIRTerminator::Branch {
                true_block: (publish, args),
                false_block: (done, done_args),
                ..
            } = &block.terminator
            else {
                continue;
            };
            if !args.is_empty()
                || !done_args.is_empty()
                || owners.get(publish) != Some(&producer)
                || predecessors.get(publish) != Some(&1)
            {
                continue;
            }
            let Some(stores) = a.publish_stores(*publish, *done) else {
                continue;
            };
            if stores.is_empty() || stores.iter().any(|(index, readers)| {
                !matches!(&unit.blocks[publish].instructions[*index], SIRInstruction::Store(_, _, width, ..) if readers.len() * 8 == *width)
            }) {
                continue;
            }
            candidates[producer].extend(
                stores
                    .iter()
                    .flat_map(|(_, rs)| rs)
                    .copied()
                    .filter(|&r| producer < r && r < n),
            );
            sites.push(Publication {
                producer,
                guard: id,
                publish: *publish,
                done: *done,
                stores,
            });
        }
        publications.push(sites);
    }
    if !guarded.into_iter().all(|g| g) {
        return None;
    }
    // Host writes, register commits and procedural updates all mark the
    // complete reader set of the home they write. An internal publisher
    // at or after P could leave P pending for the next sweep.
    for object in &settle.objects {
        for &p in &object.partitions {
            if let Some(readers) = candidates.get_mut(p as usize) {
                readers.retain(|&q| object.partitions.binary_search(&(q as u32)).is_ok());
            }
        }
    }
    for (owner, marks) in activations {
        for &p in &marks {
            candidates[p].retain(|q| owner < p && marks.contains(q));
        }
    }
    let mut count = 0;
    let edits = publications
        .into_iter()
        .map(|sites| {
            sites
                .into_iter()
                .filter_map(|site| {
                    let total = site.stores.len();
                    let changes: Vec<_> = site
                        .stores
                        .into_iter()
                        .filter_map(|(index, rs)| {
                            (!rs.is_empty()
                                && rs.iter().all(|r| candidates[site.producer].contains(r)))
                            .then(|| (index, Vec::new()))
                        })
                        .collect();
                    if changes.is_empty() {
                        return None;
                    }
                    count += 1;
                    Some(Edit {
                        guard: site.guard,
                        publish: site.publish,
                        done: site.done,
                        done_args: Vec::new(),
                        bypass: changes.len() == total,
                        changes,
                    })
                })
                .collect()
        })
        .collect();
    (count != 0).then_some(edits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use celox_sir::SIRBuilder;
    use celox_state_layout::{SettleLayout, SettleObject};

    #[test]
    fn every_origin_must_also_wake_the_later_reader() {
        let flags = RegionedAbsoluteAddr::from_absolute_addr(
            STABLE_REGION,
            AbsoluteAddr {
                instance_id: celox_design::InstanceId(0),
                var_id: celox_design::StateObjectId(0),
            },
        );
        let signal = RegionedAbsoluteAddr::from_absolute_addr(
            STABLE_REGION,
            AbsoluteAddr {
                instance_id: celox_design::InstanceId(0),
                var_id: celox_design::StateObjectId(1),
            },
        );
        // P0 publishes to P1, which in turn publishes to P2. A separate
        // change can publish P1 alone even when its external input marks both.
        let mut b = SIRBuilder::new();
        let zero = b.alloc_bit(8, false);
        let one = b.alloc_bit(8, false);
        b.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
        b.emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));
        for p in 0..3 {
            let own = b.alloc_bit(8, false);
            let dirty = b.alloc_bit(1, false);
            let body = b.new_block();
            let done = b.new_block();
            b.emit(SIRInstruction::Load(
                own,
                flags,
                SIROffset::Static(p * 8),
                8,
            ));
            b.emit(SIRInstruction::Binary(dirty, own, BinaryOp::Ne, zero));
            b.seal_block(SIRTerminator::Branch {
                cond: dirty,
                true_block: (body, vec![]),
                false_block: (done, vec![]),
            });
            b.switch_to_block(body);
            b.emit(SIRInstruction::Store(
                flags,
                SIROffset::Static(p * 8),
                8,
                zero,
                vec![],
                vec![],
            ));
            if p < 2 {
                let value = b.alloc_bit(8, false);
                let changed = b.alloc_bit(1, false);
                b.emit(SIRInstruction::Load(value, signal, SIROffset::Static(0), 8));
                b.emit(SIRInstruction::Binary(changed, value, BinaryOp::Ne, zero));
                let publish = b.new_block();
                let merge = b.new_block();
                b.seal_block(SIRTerminator::Branch {
                    cond: changed,
                    true_block: (publish, vec![]),
                    false_block: (merge, vec![]),
                });
                b.switch_to_block(publish);
                b.emit(SIRInstruction::Store(
                    flags,
                    SIROffset::Static((p + 1) * 8),
                    8,
                    one,
                    vec![],
                    vec![],
                ));
                b.seal_block(SIRTerminator::Jump(merge, vec![]));
                b.switch_to_block(merge);
            }
            b.seal_block(SIRTerminator::Jump(done, vec![]));
            b.switch_to_block(done);
        }
        let units = [b.flush_eu().unwrap()];
        let object = |start, partitions| SettleObject {
            start,
            end: start + 1,
            partitions,
        };
        let layout = SettleLayout::new(32, 3, vec![object(0, vec![0, 1]), object(1, vec![1, 2])]);
        let chosen = edits(&units, &flags, &layout, &Default::default()).unwrap();
        assert_eq!(chosen[0].len(), 1);
        assert!(chosen[0][0].bypass);
        let arm = &units[0].blocks[&chosen[0][0].publish];
        assert!(matches!(
            arm.instructions.as_slice(),
            [SIRInstruction::Store(_, SIROffset::Static(8), ..)]
        ));
        let mut separate = layout;
        separate.objects.push(object(2, vec![0]));
        assert!(edits(&units, &flags, &separate, &Default::default()).is_none());
    }
}
