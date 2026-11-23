//! Sample a conditional scalar bank and drain its pending writes in NBA.
use super::*;

pub(super) fn bind(
    raw: &SLTNodeArena<SourceVarId>,
    arena: &mut SLTNodeArena<SourceVarId>,
    form: &NextState,
    enables: Vec<(SourceVarId, NodeId, NodeId)>,
    pm: &mut procedural::ProcModule<'_>,
) -> Option<NextState> {
    let mut copier = Reinterner::new(raw);
    let mut form = form.map(|id| *id, |node| copier.copy(arena, node)).ok()?;
    let mut by_condition = std::collections::BTreeMap::<_, Vec<_>>::new();
    for (target, cond, data) in enables {
        let cond = copier.copy(arena, cond).ok()?;
        let data = copier.copy(arena, data).ok()?;
        by_condition.entry(cond).or_default().push((target, data));
    }
    // One sampled flag and one publication branch serve the whole bank.
    let mut enabled = HashMap::default();
    for (condition, bank) in by_condition {
        if bank.len() >= 2 {
            let (slot, _) = pm.module_temp("$nba_enable", 1, false, false);
            for (target, data) in bank {
                enabled.insert(
                    target,
                    celox_slt::next_state::WriteEnable {
                        slot,
                        condition,
                        data,
                    },
                );
            }
        }
    }
    for root in &mut form.roots {
        root.enable = enabled.remove(&root.target);
    }
    Some(form)
}

pub(super) fn publication(
    targets: &[VarAtomBase<SourceVarId>],
    enables: &HashMap<SourceVarId, SourceVarId>,
    memories: &HashSet<SourceVarId>,
    pm: &procedural::ProcModule<'_>,
) -> ExecutionUnit<RegionedVarAddr> {
    let mut b = SIRBuilder::new();
    // Another edge may sample this bank before NBA runs. Consume the flag
    // only after publishing the last target that shares it.
    let last_target: HashMap<_, _> = targets
        .iter()
        .filter_map(|t| enables.get(&t.id).map(|slot| (*slot, t.id)))
        .collect();
    for run in targets.chunk_by(|a, b| enables.get(&a.id) == enables.get(&b.id)) {
        if let Some(&slot) = enables.get(&run[0].id) {
            let addr = RegionedVarAddr {
                region: WORKING_REGION,
                var_id: slot,
            };
            let guard = b.alloc_bit(1, false);
            b.emit(SIRInstruction::Load(guard, addr, SIROffset::Static(0), 1));
            let write = b.new_block();
            let next = b.new_block();
            b.seal_block(SIRTerminator::Branch {
                cond: guard,
                true_block: (write, vec![]),
                false_block: (next, vec![]),
            });
            b.switch_to_block(write);
            ff::emit_sparse_memory_commits(&mut b, run, memories, pm);
            if run.last().unwrap().id == last_target[&slot] {
                let zero = b.alloc_bit(1, false);
                b.emit(SIRInstruction::Imm(zero, SIRValue::new(0u64)));
                b.emit(SIRInstruction::Store(
                    addr,
                    SIROffset::Static(0),
                    1,
                    zero,
                    vec![],
                    vec![],
                ));
            }
            b.seal_block(SIRTerminator::Jump(next, vec![]));
            b.switch_to_block(next);
        } else {
            ff::emit_sparse_memory_commits(&mut b, run, memories, pm);
        }
    }
    seal_builder(b)
}
