//! Which temporaries of a process kernel each of its activations keeps.
//!
//! A process that forks keeps a frame for each activation (see
//! `celox_design::ProcessFrame::private`), saved and restored whenever
//! another activation of its kernel runs. Most temporaries of its lowering
//! hold a value only between two suspensions: the run that reads one wrote
//! it first. Such a temporary needs no copy per activation: a run of the
//! kernel is never interleaved with a run of another of its activations.
//! Every run starts at the kernel's entry, which dispatches to the point it
//! continues at, so the temporaries an activation keeps are those live at
//! the entry: those some path from it reads before writing the whole of
//! them.

use super::*;
use celox_sir::effects::StateAccess;

/// The whole width of a variable, when only a store of all of it at offset
/// 0 makes its earlier value dead (`None` keeps it live through any store).
pub(super) type WholeWidth<'a> = &'a dyn Fn(SourceVarId) -> Option<usize>;

fn successors(terminator: &SIRTerminator) -> Vec<BlockId> {
    match terminator {
        SIRTerminator::Jump(target, _) => vec![*target],
        SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } => vec![true_block.0, false_block.0],
        SIRTerminator::Switch { cases, default, .. } => cases
            .iter()
            .map(|case| case.target)
            .chain(std::iter::once(*default))
            .collect(),
        SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
    }
}

/// Whether `access` writes all of variable `id`.
fn writes_whole(access: &celox_sir::effects::MemoryAccess<'_, Addr>, whole: WholeWidth) -> bool {
    matches!(access.offset, SIROffset::Static(0))
        && whole(access.addr.var_id).is_some_and(|width| access.width == width)
}

/// The variables of `candidates` a run of `unit`, from its entry, may read
/// before it has written the whole of them; all of them if the unit may
/// read any state.
pub(super) fn live_at_entry(
    unit: &ExecutionUnit<Addr>,
    candidates: &HashSet<SourceVarId>,
    whole: WholeWidth,
) -> HashSet<SourceVarId> {
    // Per block: the candidates read before written in it, and those it
    // writes whole.
    let mut generated: HashMap<BlockId, HashSet<SourceVarId>> = HashMap::default();
    let mut killed: HashMap<BlockId, HashSet<SourceVarId>> = HashMap::default();
    for (&id, block) in &unit.blocks {
        let mut generate = HashSet::default();
        let mut kill = HashSet::default();
        for instruction in block.instructions.iter().rev() {
            if let StateAccess::Access(access) = instruction.state_writes()
                && candidates.contains(&access.addr.var_id)
                && writes_whole(&access, whole)
            {
                generate.remove(&access.addr.var_id);
                kill.insert(access.addr.var_id);
            }
            match instruction.state_reads() {
                StateAccess::None => {}
                StateAccess::Access(access) => {
                    if candidates.contains(&access.addr.var_id) {
                        generate.insert(access.addr.var_id);
                    }
                }
                StateAccess::Objects(objects) => {
                    generate.extend(
                        objects
                            .iter()
                            .map(|addr| addr.var_id)
                            .filter(|id| candidates.contains(id)),
                    );
                }
                StateAccess::Everything => return candidates.clone(),
            }
        }
        generated.insert(id, generate);
        killed.insert(id, kill);
    }
    let mut predecessors: HashMap<BlockId, Vec<BlockId>> = HashMap::default();
    for (&id, block) in &unit.blocks {
        for successor in successors(&block.terminator) {
            predecessors.entry(successor).or_default().push(id);
        }
    }
    let mut live_in: HashMap<BlockId, HashSet<SourceVarId>> = HashMap::default();
    let mut pending: Vec<BlockId> = unit.blocks.keys().copied().collect();
    let mut queued: HashSet<BlockId> = pending.iter().copied().collect();
    while let Some(id) = pending.pop() {
        queued.remove(&id);
        let block = &unit.blocks[&id];
        let mut live: HashSet<SourceVarId> = successors(&block.terminator)
            .iter()
            .filter_map(|successor| live_in.get(successor))
            .flatten()
            .copied()
            .collect();
        live.retain(|var| !killed[&id].contains(var));
        live.extend(generated[&id].iter().copied());
        if live_in.get(&id) != Some(&live) {
            live_in.insert(id, live);
            for &predecessor in predecessors.get(&id).into_iter().flatten() {
                if queued.insert(predecessor) {
                    pending.push(predecessor);
                }
            }
        }
    }
    live_in.remove(&unit.entry_block_id).unwrap_or_default()
}

/// Check that every run of `unit` from its entry writes each of `excluded`
/// whole before it reads it, on every path: what [`live_at_entry`] found,
/// checked forward.
pub(super) fn assert_written_before_read(
    unit: &ExecutionUnit<Addr>,
    excluded: &HashSet<SourceVarId>,
    whole: WholeWidth,
) {
    if excluded.is_empty() {
        return;
    }
    let mut predecessors: HashMap<BlockId, Vec<BlockId>> = HashMap::default();
    for (&id, block) in &unit.blocks {
        for successor in successors(&block.terminator) {
            predecessors.entry(successor).or_default().push(id);
        }
    }
    // What each block has written whole when it ends, on every path: the
    // intersection over its predecessors, from nothing at the entry.
    let transfer = |id: BlockId, mut written: HashSet<SourceVarId>| {
        for instruction in &unit.blocks[&id].instructions {
            if let StateAccess::Access(access) = instruction.state_writes()
                && excluded.contains(&access.addr.var_id)
                && writes_whole(&access, whole)
            {
                written.insert(access.addr.var_id);
            }
        }
        written
    };
    let order = celox_sir::analysis::reverse_postorder(unit);
    let mut written_out: HashMap<BlockId, HashSet<SourceVarId>> = HashMap::default();
    let entry_in = |id: BlockId, written_out: &HashMap<BlockId, HashSet<SourceVarId>>| {
        if id == unit.entry_block_id {
            return HashSet::default();
        }
        let mut sets = predecessors
            .get(&id)
            .into_iter()
            .flatten()
            .filter_map(|predecessor| written_out.get(predecessor));
        let Some(first) = sets.next() else {
            return excluded.clone();
        };
        let mut all = first.clone();
        for set in sets {
            all.retain(|var| set.contains(var));
        }
        all
    };
    // Unvisited predecessors count as having written everything: the sets
    // only shrink until they settle.
    let mut changed = true;
    while changed {
        changed = false;
        for &id in &order {
            let out = transfer(id, entry_in(id, &written_out));
            if written_out.get(&id) != Some(&out) {
                written_out.insert(id, out);
                changed = true;
            }
        }
    }
    for &id in &order {
        let mut written = entry_in(id, &written_out);
        for instruction in &unit.blocks[&id].instructions {
            let read = |var: SourceVarId| {
                assert!(
                    !excluded.contains(&var) || written.contains(&var),
                    "frame temporary {var} read before it is written in block {id:?}"
                );
            };
            match instruction.state_reads() {
                StateAccess::None => {}
                StateAccess::Access(access) => read(access.addr.var_id),
                StateAccess::Objects(objects) => {
                    objects.iter().for_each(|addr| read(addr.var_id));
                }
                StateAccess::Everything => {
                    panic!("an unknown state access in a kernel whose frame excludes temporaries")
                }
            }
            if let StateAccess::Access(access) = instruction.state_writes()
                && excluded.contains(&access.addr.var_id)
                && writes_whole(&access, whole)
            {
                written.insert(access.addr.var_id);
            }
        }
    }
}
