//! What the units of a design read, write and return, as the plan and the
//! generator ask.
use super::*;

/// Whether every two stable homes of `layout` either start at the same
/// offset or do not overlap.
pub(super) fn homes_overlap_only_whole(layout: &crate::backend::MemoryLayout) -> bool {
    let mut homes: Vec<(usize, usize)> = layout
        .offsets
        .iter()
        .map(|(address, &offset)| {
            let bytes = match layout.unpacked_arrays.get(address) {
                Some(array) => array.plane_size,
                None => layout
                    .widths
                    .get(address)
                    .map_or(0, |width| width.div_ceil(8)),
            };
            (offset, offset + bytes)
        })
        .collect();
    // The widest of the homes that start at one offset stands for them.
    homes.sort_unstable_by_key(|&(start, end)| (start, std::cmp::Reverse(end)));
    homes.dedup_by_key(|home| home.0);
    homes.windows(2).all(|pair| pair[0].1 <= pair[1].0)
}

/// The storage `unit` reads.
pub(super) fn loads(unit: &Unit) -> impl Iterator<Item = AbsoluteAddr> + '_ {
    instructions(unit).flat_map(loaded_addresses)
}

pub(super) fn loaded_addresses(
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
) -> impl Iterator<Item = AbsoluteAddr> + '_ {
    read_storage(inst).map(RegionedAbsoluteAddr::absolute_addr)
}

/// Whether a settle of `units` may read what an earlier settle left: a load
/// of storage they write (`stores`, by representative) that stores earlier
/// in the same settle do not cover on every path.
pub(super) fn retains_state(
    units: &[Unit],
    storage: &impl Fn(AbsoluteAddr) -> AbsoluteAddr,
    stores: &HashSet<AbsoluteAddr>,
) -> bool {
    !retained_storage(units, storage, stores).is_empty()
}

pub(super) fn retained_storage(
    units: &[Unit],
    storage: &impl Fn(AbsoluteAddr) -> AbsoluteAddr,
    stores: &HashSet<AbsoluteAddr>,
) -> HashSet<AbsoluteAddr> {
    /// The bits of each home a settle has stored so far, as sorted,
    /// disjoint `[start, end)` ranges. A persistent map: the paths of a
    /// settle share what they stored before they split, so passing it on to
    /// a block copies nothing.
    type Covered = imbl::GenericHashMap<
        (u32, AbsoluteAddr),
        Vec<(usize, usize)>,
        fxhash::FxBuildHasher,
        imbl::shared_ptr::DefaultSharedPtr,
    >;
    let mut retained = HashSet::default();
    fn add(covered: &mut Covered, key: (u32, AbsoluteAddr), start: usize, end: usize) {
        let mut ranges = covered.get(&key).cloned().unwrap_or_default();
        ranges.push((start, end));
        ranges.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
        for &(start, end) in ranges.iter() {
            match merged.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        covered.insert(key, merged);
    }
    /// What both `known` and `other` cover; `known` itself when that is
    /// all of it, so that an unchanged block need not run again.
    fn meet(known: &Covered, other: &Covered) -> Covered {
        if known.ptr_eq(other) {
            return known.clone();
        }
        let (small, large) = if known.len() <= other.len() {
            (known, other)
        } else {
            (other, known)
        };
        let mut met = Covered::default();
        let mut same = known.len() <= other.len();
        for (key, left) in small {
            let Some(right) = large.get(key) else {
                same = false;
                continue;
            };
            if left == right {
                met.insert(*key, left.clone());
                continue;
            }
            let both: Vec<(usize, usize)> = left
                .iter()
                .flat_map(|&(ls, le)| {
                    right.iter().filter_map(move |&(rs, re)| {
                        let (start, end) = (ls.max(rs), le.min(re));
                        (start < end).then_some((start, end))
                    })
                })
                .collect();
            if known.get(key) != Some(&both) {
                same = false;
            }
            if !both.is_empty() {
                met.insert(*key, both);
            }
        }
        if same && met.len() == known.len() {
            known.clone()
        } else {
            met
        }
    }
    // Record every storage whose entry value can reach a read before a
    // covering write. Keep scanning so independent homes are analyzed once.
    let mut step = |block: &celox_sir::BasicBlock<RegionedAbsoluteAddr>, covered: &mut Covered| {
        for inst in &block.instructions {
            // A call may read any part of what it reads before anything in
            // the settle stored it, and what it writes it need not write on
            // every path, so it covers nothing.
            if let StateAccess::Objects(sources) = inst.state_reads() {
                retained.extend(
                    sources
                        .iter()
                        .map(|source| storage(source.absolute_addr()))
                        .filter(|home| stores.contains(home)),
                );
            }
            match inst {
                SIRInstruction::Load(_, source, offset, width)
                | SIRInstruction::Commit(source, _, offset, width, _) => {
                    let home = storage(source.absolute_addr());
                    if stores.contains(&home) {
                        let read = offset.constant_bit_offset().is_some_and(|start| {
                            covered.get(&(source.region, home)).is_some_and(|ranges| {
                                ranges
                                    .iter()
                                    .any(|&(s, e)| s <= start && start + width <= e)
                            })
                        });
                        if !read {
                            retained.insert(home);
                        }
                    }
                }
                _ => {}
            }
            match inst {
                SIRInstruction::Store(target, offset, width, ..)
                | SIRInstruction::Commit(_, target, offset, width, _) => {
                    let home = storage(target.absolute_addr());
                    if stores.contains(&home)
                        && let Some(start) = offset.constant_bit_offset()
                    {
                        add(covered, (target.region, home), start, start + width);
                    }
                }
                _ => {}
            }
        }
    };
    let mut entry = Covered::default();
    for unit in units {
        // What every path stores before each block, from the paths found
        // so far: a fixed point from above, as loops only remove. A block
        // runs again only when what reaches it changed, lowest first.
        let mut before: HashMap<celox_sir::BlockId, Covered> = HashMap::default();
        before.insert(unit.entry_block_id, entry.clone());
        let mut after: HashMap<celox_sir::BlockId, Covered> = HashMap::default();
        let mut pending: BTreeSet<celox_sir::BlockId> = BTreeSet::from([unit.entry_block_id]);
        while let Some(id) = pending.pop_first() {
            let Some(block) = unit.blocks.get(&id) else {
                continue;
            };
            let mut covered = before[&id].clone();
            step(block, &mut covered);
            if after
                .get(&id)
                .is_some_and(|known| known.ptr_eq(&covered) || *known == covered)
            {
                continue;
            }
            let successors: Vec<_> = match &block.terminator {
                SIRTerminator::Jump(target, _) => vec![*target],
                SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } => vec![true_block.0, false_block.0],
                SIRTerminator::Switch { cases, default, .. } => cases
                    .iter()
                    .map(|case| case.target)
                    .chain([*default])
                    .collect(),
                SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
            };
            for successor in successors {
                let next = match before.get(&successor) {
                    Some(known) => {
                        let next = meet(known, &covered);
                        if next.ptr_eq(known) {
                            continue;
                        }
                        next
                    }
                    None => covered.clone(),
                };
                before.insert(successor, next);
                pending.insert(successor);
            }
            after.insert(id, covered);
        }
        // The next unit starts from what every return of this one stored.
        let mut returns = unit
            .blocks
            .iter()
            .filter(|(_, block)| matches!(block.terminator, SIRTerminator::Return))
            .filter_map(|(id, _)| after.get(id));
        entry = match returns.next() {
            Some(first) => returns.fold(first.clone(), |met, next| meet(&met, next)),
            None => Covered::default(),
        };
    }
    #[cfg(test)]
    assert_eq!(
        retained,
        retained_storage_reference(units, storage, stores),
        "the worklist finds what the full passes find"
    );
    retained
}

/// [`retained_storage`] by full passes over every block with copied maps,
/// as it was computed before: the tests check that both agree.
#[cfg(test)]
fn retained_storage_reference(
    units: &[Unit],
    storage: &impl Fn(AbsoluteAddr) -> AbsoluteAddr,
    stores: &HashSet<AbsoluteAddr>,
) -> HashSet<AbsoluteAddr> {
    /// The bits of each home a settle has stored so far, as sorted,
    /// disjoint `[start, end)` ranges.
    type Covered = HashMap<(u32, AbsoluteAddr), Vec<(usize, usize)>>;
    let mut retained = HashSet::default();
    fn add(covered: &mut Covered, key: (u32, AbsoluteAddr), start: usize, end: usize) {
        let ranges = covered.entry(key).or_default();
        ranges.push((start, end));
        ranges.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
        for &(start, end) in ranges.iter() {
            match merged.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        *ranges = merged;
    }
    fn meet(a: &Covered, b: &Covered) -> Covered {
        let mut met = Covered::default();
        for (key, left) in a {
            let Some(right) = b.get(key) else { continue };
            let both: Vec<(usize, usize)> = left
                .iter()
                .flat_map(|&(ls, le)| {
                    right.iter().filter_map(move |&(rs, re)| {
                        let (start, end) = (ls.max(rs), le.min(re));
                        (start < end).then_some((start, end))
                    })
                })
                .collect();
            if !both.is_empty() {
                met.insert(*key, both);
            }
        }
        met
    }
    // Record every storage whose entry value can reach a read before a
    // covering write. Keep scanning so independent homes are analyzed once.
    let mut step = |block: &celox_sir::BasicBlock<RegionedAbsoluteAddr>, covered: &mut Covered| {
        for inst in &block.instructions {
            // A call may read any part of what it reads before anything in
            // the settle stored it, and what it writes it need not write on
            // every path, so it covers nothing.
            if let StateAccess::Objects(sources) = inst.state_reads() {
                retained.extend(
                    sources
                        .iter()
                        .map(|source| storage(source.absolute_addr()))
                        .filter(|home| stores.contains(home)),
                );
            }
            match inst {
                SIRInstruction::Load(_, source, offset, width)
                | SIRInstruction::Commit(source, _, offset, width, _) => {
                    let home = storage(source.absolute_addr());
                    if stores.contains(&home) {
                        let read = offset.constant_bit_offset().is_some_and(|start| {
                            covered.get(&(source.region, home)).is_some_and(|ranges| {
                                ranges
                                    .iter()
                                    .any(|&(s, e)| s <= start && start + width <= e)
                            })
                        });
                        if !read {
                            retained.insert(home);
                        }
                    }
                }
                _ => {}
            }
            match inst {
                SIRInstruction::Store(target, offset, width, ..)
                | SIRInstruction::Commit(_, target, offset, width, _) => {
                    let home = storage(target.absolute_addr());
                    if stores.contains(&home)
                        && let Some(start) = offset.constant_bit_offset()
                    {
                        add(covered, (target.region, home), start, start + width);
                    }
                }
                _ => {}
            }
        }
    };
    let mut entry = Covered::default();
    for unit in units {
        // What every path stores before each block, from the paths found
        // so far: a fixed point from above, as loops only remove.
        let mut before: HashMap<celox_sir::BlockId, Covered> = HashMap::default();
        before.insert(unit.entry_block_id, entry.clone());
        let mut after: HashMap<celox_sir::BlockId, Covered> = HashMap::default();
        let mut changed = true;
        while changed {
            changed = false;
            let mut ids: Vec<_> = unit.blocks.keys().copied().collect();
            ids.sort_unstable();
            for id in ids {
                let Some(start) = before.get(&id).cloned() else {
                    continue;
                };
                let mut covered = start;
                step(&unit.blocks[&id], &mut covered);
                if after.get(&id) == Some(&covered) {
                    continue;
                }
                let successors: Vec<_> = match &unit.blocks[&id].terminator {
                    SIRTerminator::Jump(target, _) => vec![*target],
                    SIRTerminator::Branch {
                        true_block,
                        false_block,
                        ..
                    } => vec![true_block.0, false_block.0],
                    SIRTerminator::Switch { cases, default, .. } => cases
                        .iter()
                        .map(|case| case.target)
                        .chain([*default])
                        .collect(),
                    SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
                };
                for successor in successors {
                    let next = match before.get(&successor) {
                        Some(known) => meet(known, &covered),
                        None => covered.clone(),
                    };
                    before.insert(successor, next);
                }
                after.insert(id, covered);
                changed = true;
            }
        }
        // The next unit starts from what every return of this one stored.
        let mut returns = unit
            .blocks
            .iter()
            .filter(|(_, block)| matches!(block.terminator, SIRTerminator::Return))
            .filter_map(|(id, _)| after.get(id));
        entry = match returns.next() {
            Some(first) => returns.fold(first.clone(), |met, next| meet(&met, next)),
            None => Covered::default(),
        };
    }
    retained
}

/// Whether `unit` may end with an error: at one of its own error exits, or
/// at a call whose callee may fail and ends it with the callee's error.
pub(super) fn fails(unit: &Unit) -> bool {
    unit.blocks
        .values()
        .any(|block| matches!(block.terminator, SIRTerminator::Error(_)))
        || instructions(unit).any(SIRInstruction::may_fail)
}

/// The signals `statement` writes that may be reset events: those of
/// `rst.assert` and of assignments.
pub(super) fn testbench_resets(
    statement: &celox_testbench::SemanticStatement<AbsoluteAddr>,
) -> Vec<AbsoluteAddr> {
    use celox_testbench::SemanticStatement as S;
    match statement {
        S::ResetAssert {
            reset_event: Some(event),
            ..
        } => vec![*event],
        S::Assign { dst, .. } => vec![dst.signal.address],
        S::If {
            then_block,
            else_block,
            ..
        } => then_block
            .iter()
            .chain(else_block)
            .flat_map(testbench_resets)
            .collect(),
        S::For { body, .. } => body.iter().flat_map(testbench_resets).collect(),
        _ => vec![],
    }
}

/// Whether `unit` may record a `$finish`, one of `finish_sites`.
pub(super) fn emits_finish(unit: &Unit, finish_sites: &HashSet<u32>) -> bool {
    instructions(unit).any(|inst| {
        matches!(inst, SIRInstruction::RuntimeEvent { site_id, .. }
            if finish_sites.contains(site_id))
            || !finish_sites.is_empty() && calls_runtime_events(inst)
    })
}

/// The longest delay `unit` stores into `delay`: `u64::MAX` unless every
/// one is a constant.
pub(super) fn delay_bound(unit: &Unit, delay: AbsoluteAddr) -> u64 {
    let constants: HashMap<RegisterId, u64> = instructions(unit)
        .filter_map(|inst| match inst {
            SIRInstruction::Imm(register, value) => u64::try_from(&value.payload)
                .ok()
                .map(|value| (*register, value)),
            _ => None,
        })
        .collect();
    let delay = stable(delay);
    instructions(unit)
        .filter_map(|inst| match inst {
            SIRInstruction::Store(target, offset, _, value, ..) if *target == delay => {
                Some(match offset {
                    SIROffset::Static(0) => constants.get(value).copied().unwrap_or(u64::MAX),
                    _ => u64::MAX,
                })
            }
            SIRInstruction::Commit(_, target, ..) if *target == delay => Some(u64::MAX),
            // A call may store any delay.
            _ if inst.call_effects().is_some() && inst.state_writes().touches(&delay) => {
                Some(u64::MAX)
            }
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

/// The shortest delay `unit` can store in its delay slot `delay`: 0 unless
/// every store of it is a known constant.
pub(super) fn shortest_delay(unit: &Unit, delay: AbsoluteAddr) -> u64 {
    let constants: HashMap<RegisterId, u64> = instructions(unit)
        .filter_map(|inst| match inst {
            SIRInstruction::Imm(register, value) => u64::try_from(&value.payload)
                .ok()
                .map(|value| (*register, value)),
            _ => None,
        })
        .collect();
    let delay = stable(delay);
    instructions(unit)
        .filter_map(|inst| match inst {
            SIRInstruction::Store(target, offset, _, value, ..) if *target == delay => {
                Some(match offset {
                    SIROffset::Static(0) => constants.get(value).copied().unwrap_or(0),
                    _ => 0,
                })
            }
            SIRInstruction::Commit(_, target, ..) if *target == delay => Some(0),
            _ if inst.call_effects().is_some() && inst.state_writes().touches(&delay) => Some(0),
            _ => None,
        })
        .min()
        .unwrap_or(0)
}

pub(super) fn stable(address: AbsoluteAddr) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, address)
}

pub(super) fn instructions(
    unit: &Unit,
) -> impl Iterator<Item = &SIRInstruction<RegionedAbsoluteAddr>> {
    unit.blocks.values().flat_map(|block| &block.instructions)
}

/// The storage `inst` may read: a Load's or Commit's source, or every object
/// a call's effects read, each of which it may read in any part.
/// [`Plan::new`] rejects units that may read any state (a native kernel),
/// the one case no list names, so none is left out here.
pub(super) fn read_storage(
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
) -> impl Iterator<Item = &RegionedAbsoluteAddr> {
    inst.state_reads().objects().into_iter().flatten()
}

/// The storage `inst` may write: a Store's or Commit's destination, or
/// every object a call's effects write (see [`read_storage`]).
pub(super) fn written_storage(
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
) -> impl Iterator<Item = &RegionedAbsoluteAddr> {
    inst.state_writes().objects().into_iter().flatten()
}

/// Whether `inst` makes an external call of any function, itself or in a
/// function it calls. That is more than [`makes_host_calls`] asks: a native
/// memory operation and a call of the coverage helper are external calls
/// too. A compiled kernel that makes one has every wait checked after its
/// run, not only the waits its writes select.
pub(super) fn makes_extern_calls(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    inst.host_effects().extern_calls
}

/// Whether `inst` calls into the host, which may read or change state its
/// explicit loads and stores do not name: an extern call, or a call of a
/// function that makes one, other than a native memory operation (the settle
/// instrumentation's wide commits, whose writes the typed commits after them
/// name) and a call of the coverage helper, which reads and writes only the
/// heap (`celox_sir::HostEffects::state_calls`).
fn makes_host_calls(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    inst.host_effects().state_calls
}

/// Whether `inst` calls a function of the host at all, itself or in a
/// function it calls: [`makes_host_calls`], and a call of the coverage
/// helper too. Combinational logic is classified with this. Every run of a
/// settle makes the calls of its units again, so how often and where the
/// settles run is observable through a call that touches no simulation
/// state as much as through one that does, and nothing is planned from the
/// difference there.
///
/// A summary does not tell a coverage helper call from a native memory
/// operation, so a call of a function that holds only those counts as well;
/// no function unit holds one.
fn calls_host_functions(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    match inst {
        SIRInstruction::ExternCall { func, .. } => {
            celox_sir::native_memory::NativeMemoryOp::decode(*func).is_none()
        }
        _ => makes_extern_calls(inst),
    }
}

/// Whether `inst` is a call of an isolated function of `functions`, such as
/// a noncontext DPI-C import: it reaches nothing of the design but its
/// arguments and its result (see [`celox_design::ExternFunction::isolated`]).
fn calls_isolated_function(
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
    functions: &[celox_design::ExternFunction],
) -> bool {
    matches!(
        inst,
        SIRInstruction::ExternCall { func, .. }
            if functions
                .get(*func as usize)
                .is_some_and(|function| function.isolated)
    )
}

/// Whether `inst` is a host call that may read or write state of the design
/// beyond its arguments and its result, as [`makes_host_calls`] says of
/// every host call but a call of an isolated function of `functions`. The
/// units that run once per event or per wake-up are classified with this:
/// the domains of the events and the kernels.
pub(super) fn makes_opaque_calls(
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
    functions: &[celox_design::ExternFunction],
) -> bool {
    !calls_isolated_function(inst, functions) && makes_host_calls(inst)
}

/// [`makes_opaque_calls`] for combinational logic: [`calls_host_functions`]
/// of every call but a call of an isolated function of `functions`, so a
/// call of the coverage helper in a settle counts as a call of the heap
/// helper there does.
pub(super) fn makes_opaque_calls_in_settle(
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
    functions: &[celox_design::ExternFunction],
) -> bool {
    !calls_isolated_function(inst, functions) && calls_host_functions(inst)
}

/// Whether `inst` is a call of a container operation of the heap, whose
/// code `constants` (the immediates of its unit) give: it changes no state
/// of the design but its result, a value or the handle of a new container
/// that the unit stores where the container lives
/// ([`celox_heap::containers::is_container_value_op`]). No operation of the
/// coverage helper is one, so only the heap helper's own `func` is asked.
pub(super) fn container_value_call(
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
    constants: &HashMap<RegisterId, u64>,
) -> bool {
    matches!(
        inst,
        SIRInstruction::ExternCall { func, args, .. }
            if *func == celox_sir::extern_abi::HEAP_HELPER_ID
                && args
                    .first()
                    .and_then(|code| constants.get(code))
                    .is_some_and(|&code| celox_heap::containers::is_container_value_op(code))
    )
}

/// The immediates of `unit` that fit 64 bits, by register.
pub(super) fn unit_constants(unit: &Unit) -> HashMap<RegisterId, u64> {
    instructions(unit)
        .filter_map(|inst| match inst {
            SIRInstruction::Imm(register, value) => u64::try_from(&value.payload)
                .ok()
                .map(|value| (*register, value)),
            _ => None,
        })
        .collect()
}

/// Whether `inst` is a call whose callee may record runtime events, and so
/// may record a `$finish`, which the callee flags itself
/// (`instrument_recording_functions`).
fn calls_runtime_events(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    inst.call_effects().is_some() && inst.host_effects().runtime_events
}

pub(super) fn yielding_status(unit: &Unit, status: AbsoluteAddr) -> bool {
    returns_status(unit, status, ProcessStatus::Yield)
}

pub(super) fn returns_status(unit: &Unit, status: AbsoluteAddr, wanted: ProcessStatus) -> bool {
    let constants: HashMap<RegisterId, u64> = instructions(unit)
        .filter_map(|inst| match inst {
            SIRInstruction::Imm(register, value) => u64::try_from(&value.payload)
                .ok()
                .map(|value| (*register, value)),
            _ => None,
        })
        .collect();
    instructions(unit).any(|inst| {
        matches!(inst,
            SIRInstruction::Store(target, SIROffset::Static(0), _, value, ..)
                if *target == stable(status) && constants.get(value) == Some(&u64::from(wanted.code()))
        )
            // A call may store any status.
            || inst.call_effects().is_some() && inst.state_writes().touches(&stable(status))
    })
}

/// Local output definitions at each returning block. A merged exit, partial
/// write or opaque call yields an unknown word.
pub(super) fn returned_words(
    unit: &Unit,
    status: AbsoluteAddr,
    delay: AbsoluteAddr,
) -> Vec<(Option<u64>, Option<u64>)> {
    let constants = unit_constants(unit);
    let word = |block: &celox_sir::BasicBlock<RegionedAbsoluteAddr>, address, width| {
        let target = stable(address);
        let last = block
            .instructions
            .iter()
            .rev()
            .find(|inst| inst.state_writes().touches(&target))?;
        match last {
            SIRInstruction::Store(to, SIROffset::Static(0), bits, reg, ..)
                if *to == target && *bits == width =>
            {
                constants.get(reg).copied()
            }
            _ => None,
        }
    };
    unit.blocks
        .values()
        .filter(|b| matches!(b.terminator, SIRTerminator::Return))
        .map(|b| {
            (
                word(b, status, celox_design::PROCESS_STATUS_WIDTH),
                word(b, delay, celox_design::PROCESS_DELAY_WIDTH),
            )
        })
        .collect()
}

pub(super) fn is_delay_code(code: u64) -> bool {
    code == u64::from(ProcessStatus::Delay.code())
        || code == u64::from(ProcessStatus::InactiveDelay.code())
}

/// Given the frontend guarantee that steady-state delays are positive,
/// a kernel that can only return a zero delay only delays at startup.
pub(super) fn only_zero_delay_exits(
    unit: &Unit,
    status: AbsoluteAddr,
    delay: AbsoluteAddr,
) -> bool {
    returned_words(unit, status, delay)
        .into_iter()
        .all(|(code, amount)| code.is_some_and(|c| !is_delay_code(c) || amount == Some(0)))
}

/// Whether every status `unit` stores into `status` is a constant among
/// `allowed`.
pub(super) fn statuses_only(unit: &Unit, status: AbsoluteAddr, allowed: &[ProcessStatus]) -> bool {
    let constants: HashMap<RegisterId, u64> = instructions(unit)
        .filter_map(|inst| match inst {
            SIRInstruction::Imm(register, value) => u64::try_from(&value.payload)
                .ok()
                .map(|value| (*register, value)),
            _ => None,
        })
        .collect();
    let allowed: Vec<u64> = allowed
        .iter()
        .map(|status| u64::from(status.code()))
        .collect();
    let status = stable(status);
    instructions(unit).all(|inst| match inst {
        SIRInstruction::Store(target, _, _, value, ..) if *target == status => constants
            .get(value)
            .is_some_and(|code| allowed.contains(code)),
        SIRInstruction::Commit(_, target, ..) => *target != status,
        // A call may store any status.
        _ if inst.call_effects().is_some() => !inst.state_writes().touches(&status),
        _ => true,
    })
}
