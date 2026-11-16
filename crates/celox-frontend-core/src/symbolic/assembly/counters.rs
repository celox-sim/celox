//! Occurrence counters: sensitivity probes, counted signals and gated terms, and their instrumentation.

use super::*;

/// [`schedule_symbolic_rtl`], which also reports the ranges the blocks of a
/// combinational loop write when the scheduler rejects one.
#[allow(clippy::too_many_arguments)]
/// The sensitivity atoms whose occurrences the processes and observers of
/// `comb_blocks` and `comb_observers` count, with their counters, in order of
/// discovery: the atoms they list, then the upstream atoms of transparent
/// copies into them. `same_kind` tells whether a copy carries occurrences
/// between two variables.
pub(super) fn sensitivity_probes(
    comb_blocks: &[LogicPath<AbsoluteAddr>],
    comb_observers: &[CombObserver<AbsoluteAddr>],
    arena: &SLTNodeArena<AbsoluteAddr>,
    same_kind: impl Fn(&AbsoluteAddr, &AbsoluteAddr) -> bool,
) -> Vec<(VarAtomBase<AbsoluteAddr>, AbsoluteAddr)> {
    let mut probes = Vec::new();
    let mut probed = HashSet::default();
    for path in comb_blocks {
        if let LogicPathTarget::Process {
            sensitivity_counters,
            ..
        } = &path.target
        {
            for &(source, counter) in sensitivity_counters {
                if probed.insert((source, counter)) {
                    probes.push((source, counter));
                }
            }
        }
    }
    for observer in comb_observers {
        for &(source, counter) in &observer.sensitivity_counters {
            if probed.insert((source, counter)) {
                probes.push((source, counter));
            }
        }
    }
    add_copied_probes(&mut probes, &mut probed, comb_blocks, arena, same_kind);
    probes
}

/// Adds to `probes` the upstream atoms of the transparent copies of
/// `comb_blocks` into atoms of `probes`, transitively, each with the counter
/// of the atom it reaches. `probed` holds the entries of `probes`.
///
/// A direct wire/port connection carries occurrences as well as its settled
/// value. Preserve them when a kernel writes the upstream object twice
/// before the copy path is evaluated. Only equal-width, unindexed copies are
/// transparent here; expressions remain scheduled combinational evaluations.
pub(super) fn add_copied_probes(
    probes: &mut Vec<(VarAtomBase<AbsoluteAddr>, AbsoluteAddr)>,
    probed: &mut HashSet<(VarAtomBase<AbsoluteAddr>, AbsoluteAddr)>,
    comb_blocks: &[LogicPath<AbsoluteAddr>],
    arena: &SLTNodeArena<AbsoluteAddr>,
    same_kind: impl Fn(&AbsoluteAddr, &AbsoluteAddr) -> bool,
) {
    let mut copies_to: HashMap<AbsoluteAddr, Vec<usize>> = HashMap::default();
    for (path_index, path) in comb_blocks.iter().enumerate() {
        if let LogicPathTarget::Var(target) = &path.target
            && path.transparent_copy
        {
            copies_to.entry(target.id).or_default().push(path_index);
        }
    }
    let mut next_probe = 0;
    while next_probe < probes.len() {
        let (source, counter) = probes[next_probe];
        next_probe += 1;
        for &path_index in copies_to.get(&source.id).into_iter().flatten() {
            let path = &comb_blocks[path_index];
            let LogicPathTarget::Var(target) = &path.target else {
                unreachable!("copies_to holds variable targets");
            };
            if !target.access.overlaps(&source.access) {
                continue;
            }
            let celox_slt::SLTNode::Input {
                variable,
                access,
                index,
                ..
            } = arena.get(path.expr)
            else {
                continue;
            };
            if !index.is_empty()
                || !same_kind(variable, &target.id)
                || access.msb - access.lsb != target.access.msb - target.access.lsb
            {
                continue;
            }
            let lsb = source.access.lsb.max(target.access.lsb);
            let msb = source.access.msb.min(target.access.msb);
            let upstream = VarAtomBase::new(
                *variable,
                access.lsb + lsb - target.access.lsb,
                access.lsb + msb - target.access.lsb,
            );
            if probed.insert((upstream, counter)) {
                probes.push((upstream, counter));
            }
        }
    }
}

pub(super) type FfUnits = HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>;

pub(super) type FfParts = HashMap<AbsoluteAddr, Vec<(InstanceId, FfPart<RegionedAbsoluteAddr>)>>;

/// The signals some flip-flop evaluation may store twice in its working
/// state in one execution.
pub(super) fn ff_multi_stored(
    eval_apply_ffs: &FfUnits,
    eval_only_ffs: &FfUnits,
    parallel_ff_units: &FfParts,
) -> HashSet<AbsoluteAddr> {
    let working = |address: &RegionedAbsoluteAddr| {
        (address.region == WORKING_REGION).then(|| address.absolute_addr())
    };
    eval_apply_ffs
        .values()
        .chain(eval_only_ffs.values())
        .flatten()
        .chain(
            parallel_ff_units
                .values()
                .flatten()
                .map(|(_, part)| &part.evaluate),
        )
        .flat_map(|unit| crate::watch::multi_store_only_candidates(unit, working))
        .collect()
}

/// Run `f` on every flip-flop evaluation.
pub(super) fn for_each_ff_evaluation(
    eval_apply_ffs: &mut FfUnits,
    eval_only_ffs: &mut FfUnits,
    parallel_ff_units: &mut FfParts,
    mut f: impl FnMut(&mut ExecutionUnit<RegionedAbsoluteAddr>),
) {
    for unit in eval_apply_ffs
        .values_mut()
        .chain(eval_only_ffs.values_mut())
        .flatten()
    {
        f(unit);
    }
    for (_, part) in parallel_ff_units.values_mut().flatten() {
        f(&mut part.evaluate);
    }
}

/// Counted signals: those a unit may store twice between two runtime checks
/// (a frontend declared counters for them) and that a flip-flop domain or a
/// wait watches, by signal.
pub(super) fn counted_signals(
    timing: &TimingMetadata,
    event_addresses: &HashSet<AbsoluteAddr>,
) -> BTreeMap<AbsoluteAddr, CounterSlots<AbsoluteAddr>> {
    let waited: HashSet<AbsoluteAddr> = timing
        .wait_sites
        .iter()
        .flat_map(|site| site.terms.iter().map(|(signal, _)| *signal))
        .collect();
    timing
        .counter_slots
        .iter()
        .filter(|slots| event_addresses.contains(&slots.signal) || waited.contains(&slots.signal))
        .map(|slots| (slots.signal, *slots))
        .collect()
}

/// Keep, for every instance of a process in `comb_alternatives`, either
/// its imperative unit, when a variable it may store twice is `counted`, or
/// its symbolic definitions, and remove the paths of the other form from
/// `comb_blocks`. Paths of other drivers of the same variables stay.
pub(super) fn choose_comb_alternatives(
    bindings: &PackageBindings,
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
    counted: &BTreeMap<AbsoluteAddr, CounterSlots<AbsoluteAddr>>,
    gated: &HashSet<AbsoluteAddr>,
    comb_blocks: &mut Vec<LogicPath<AbsoluteAddr>>,
) {
    let mut imperative = HashSet::default();
    let mut symbolic = HashMap::<AbsoluteAddr, Vec<_>>::default();
    for (&instance, module) in instance_modules {
        for alternative in &modules[module].comb_alternatives {
            let locate = |var_id| bindings.locate(instance, var_id);
            if alternative.candidates.iter().any(|&candidate| {
                let candidate = locate(candidate);
                counted.contains_key(&candidate)
            }) || alternative
                .targets
                .iter()
                .any(|target| gated.contains(&locate(target.id)))
            {
                for target in &alternative.targets {
                    symbolic
                        .entry(locate(target.id))
                        .or_default()
                        .push(target.access);
                }
            } else {
                imperative.insert(locate(alternative.process));
            }
        }
    }
    if imperative.is_empty() && symbolic.is_empty() {
        return;
    }
    // Flattening may have split a definition at the boundaries of its
    // readers, so a path of the process is any piece of one of its ranges.
    // No other driver writes those bits: the SV frontend rejects overlapping
    // drivers (`validate_variable_driver_ranges`).
    comb_blocks.retain(|path| match &path.target {
        LogicPathTarget::Var(target) => !symbolic
            .get(&target.id)
            .is_some_and(|ranges| ranges.iter().any(|range| range.overlaps(&target.access))),
        LogicPathTarget::Process { process, .. } => !imperative.contains(process),
        LogicPathTarget::CombCaptureEvent { .. } => true,
    });
}

/// A term with a gate counter (IEEE 1800-2023 9.4.2.3): the kind of its
/// signal's occurrences, the gate, and the counters a write bumps.
pub(super) type GatedTerm = (CounterKind, AbsoluteAddr, GateCounters<AbsoluteAddr>);

/// The gated terms whose counters the writes of their signal bump, by
/// signal. A gate counts at the write only when the signal and the gate are
/// variables that process kernels, combinational processes, the runtime or
/// the host write: a continuous assignment is a scheduled evaluation (4.7),
/// and a flip-flop publishes its writes after the process evaluated them
/// (#1830). The other gates lose their counters, so the runtime reads them
/// when it detects the occurrence.
pub(super) fn gated_terms(
    timing: &mut TimingMetadata,
    comb_blocks: &[LogicPath<AbsoluteAddr>],
    ff_written: &HashSet<AbsoluteAddr>,
) -> HashMap<AbsoluteAddr, Vec<GatedTerm>> {
    let continuous: HashSet<AbsoluteAddr> = comb_blocks
        .iter()
        .filter_map(|path| match &path.target {
            LogicPathTarget::Var(target) => Some(target.id),
            _ => None,
        })
        .collect();
    let mut gated: HashMap<AbsoluteAddr, Vec<GatedTerm>> = HashMap::default();
    for site in &mut timing.wait_sites {
        for (&(signal, kind), gate) in site.terms.iter().zip(&mut site.gates) {
            let Some(gate) = gate else {
                continue;
            };
            if ff_written.contains(&signal)
                || continuous.contains(&signal)
                || continuous.contains(&gate.signal)
            {
                gate.counters = None;
                gate.at_write = false;
                continue;
            }
            let Some(counters) = gate.counters else {
                continue;
            };
            gated
                .entry(signal)
                .or_default()
                .push((kind, gate.signal, counters));
        }
    }
    gated
}

/// The code that counts the writes of `signal` in its `counted` counters
/// and the counters of its `gated` terms, each object addressed through
/// `address`, or `None` if nothing counts them.
pub(super) fn watch_code<A>(
    signal: AbsoluteAddr,
    counted: &BTreeMap<AbsoluteAddr, CounterSlots<AbsoluteAddr>>,
    gated: &HashMap<AbsoluteAddr, Vec<GatedTerm>>,
    variables: &HashMap<AbsoluteAddr, (usize, bool)>,
    four_state: bool,
    address: impl Fn(AbsoluteAddr) -> A,
) -> Option<crate::watch::WatchCode<A>> {
    let slots = counted.get(&signal);
    let terms = gated.get(&signal);
    if slots.is_none() && terms.is_none() {
        return None;
    }
    let (width, is_4state) = variables[&signal];
    Some(crate::watch::WatchCode {
        signal: address(signal),
        offset: celox_sir::SIROffset::Static(0),
        width,
        four_state: four_state && is_4state,
        counters: slots
            .into_iter()
            .flat_map(|slots| {
                CounterKind::ALL
                    .into_iter()
                    .map(|kind| (kind, address(slots.counter(kind))))
            })
            .collect(),
        gated: terms
            .into_iter()
            .flatten()
            .map(|&(kind, gate, counters)| crate::watch::GatedCode {
                kind,
                gate: address(gate),
                gate_four_state: four_state && variables[&gate].1,
                gated: address(counters.gated),
                all: address(counters.all),
            })
            .collect(),
    })
}

/// Instrument the process kernels, function units, combinational process
/// units and flip-flop units that write a `counted` signal, and the process
/// kernels, function units and combinational process units that write the
/// signal of a `gated` term, and return `counted`; no other unit changes.
pub(super) fn instrument_counted_signals<'u>(
    timing: &TimingMetadata,
    counted: BTreeMap<AbsoluteAddr, CounterSlots<AbsoluteAddr>>,
    gated: &HashMap<AbsoluteAddr, Vec<GatedTerm>>,
    variables: &HashMap<AbsoluteAddr, (usize, bool)>,
    four_state: bool,
    processes: &mut [RelocatedProcess],
    functions: &mut [celox_sir::FunctionUnit<RegionedAbsoluteAddr>],
    comb_blocks: &mut [LogicPath<AbsoluteAddr>],
    ff_units: impl Iterator<Item = &'u mut ExecutionUnit<RegionedAbsoluteAddr>>,
) -> BTreeMap<AbsoluteAddr, CounterSlots<AbsoluteAddr>> {
    if counted.is_empty() && gated.is_empty() {
        return counted;
    }
    let code = |signal: AbsoluteAddr, address: &dyn Fn(AbsoluteAddr) -> RegionedAbsoluteAddr| {
        watch_code(signal, &counted, gated, variables, four_state, address)
    };
    let code_comb = |signal: AbsoluteAddr| {
        watch_code(signal, &counted, gated, variables, four_state, |var| var)
    };
    let stable = |var: AbsoluteAddr| RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, var);
    // A function unit's stores are those of the process that calls it,
    // and the counters are the stored signal's, whichever process that is.
    let kernels = processes
        .iter_mut()
        .map(|process| &mut process.kernel)
        .chain(functions.iter_mut().map(|function| &mut function.unit));
    for unit in kernels {
        crate::watch::instrument_unit(unit, |address| {
            if address.region != STABLE_REGION {
                return None;
            }
            code(address.absolute_addr(), &stable)
        });
    }
    for path in comb_blocks {
        if let LogicPathTarget::Process { unit, .. } = &mut path.target {
            crate::watch::instrument_unit(unit, |address| code_comb(*address));
        }
    }
    if counted.is_empty() {
        return counted;
    }
    let code = |signal: AbsoluteAddr| {
        let slots = counted.get(&signal)?;
        let (width, is_4state) = variables[&signal];
        Some((slots, width, four_state && is_4state))
    };
    let working = |var: AbsoluteAddr| RegionedAbsoluteAddr::from_absolute_addr(WORKING_REGION, var);
    let watch = |signal: AbsoluteAddr| {
        let (slots, width, four_state) = code(signal)?;
        Some(crate::watch::WatchCode {
            signal: stable(slots.signal),
            offset: celox_sir::SIROffset::Static(0),
            width,
            four_state,
            counters: CounterKind::ALL
                .into_iter()
                .map(|kind| (kind, stable(slots.counter(kind))))
                .collect(),
            gated: Vec::new(),
        })
    };
    // A flip-flop writes its next state, and a commit publishes it. Repeated
    // writes are held back until the commit; a single write is counted at
    // the commit itself.
    let pending: Vec<_> = timing
        .pending_slots
        .iter()
        .filter_map(|slots| {
            Some(crate::watch::PendingCode {
                watch: watch(slots.signal)?,
                write: working(slots.staged.unwrap_or(slots.signal)),
                staged: slots.staged.is_some(),
                value: stable(slots.value),
                covered: stable(slots.covered),
                first: stable(slots.first),
                changed: stable(slots.changed),
                written: stable(slots.written),
                edges: vec![
                    (CounterKind::Rises, stable(slots.rises)),
                    (CounterKind::Falls, stable(slots.falls)),
                ],
            })
        })
        .collect();
    let held = |staged: bool| -> HashSet<AbsoluteAddr> {
        timing
            .pending_slots
            .iter()
            .filter(|slots| slots.staged.is_some() == staged)
            .map(|slots| slots.signal)
            .collect()
    };
    let (held_next, held_staged) = (held(false), held(true));
    for unit in ff_units {
        crate::watch::instrument_writes(unit, |instruction| match instruction {
            // The commit of the next state; a seed copies the other way.
            SIRInstruction::Commit(source, destination, ..)
                if source.region == WORKING_REGION
                    && destination.region == STABLE_REGION
                    && source.absolute_addr() == destination.absolute_addr()
                    && !held_next.contains(&destination.absolute_addr()) =>
            {
                watch(destination.absolute_addr())
            }
            // The merge of staged nonblocking values.
            SIRInstruction::Store(destination, ..)
                if destination.region == STABLE_REGION
                    && !held_staged.contains(&destination.absolute_addr()) =>
            {
                watch(destination.absolute_addr())
            }
            _ => None,
        });
        crate::watch::instrument_pending(unit, &pending);
    }
    counted
}
