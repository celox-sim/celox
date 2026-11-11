//! What units read and write, and the commits of observed blocking results.

use super::*;

/// For every combinational value, the variables it is computed from.
pub(super) fn comb_sources(
    comb_blocks: &[celox_slt::LogicPath<AbsoluteAddr>],
    arena: &SLTNodeArena<AbsoluteAddr>,
) -> HashMap<AbsoluteAddr, HashSet<AbsoluteAddr>> {
    let mut sources: HashMap<AbsoluteAddr, HashSet<AbsoluteAddr>> = HashMap::default();
    for path in comb_blocks {
        let Some(target) = path.target.var() else {
            continue;
        };
        let mut inputs = HashSet::default();
        if matches!(path.target, celox_slt::LogicPathTarget::Process { .. }) {
            inputs.extend(path.sources.iter().cloned());
        } else {
            crate::flattening::collect_inputs(path.expr, arena, &mut inputs);
        }
        sources
            .entry(target.id)
            .or_default()
            .extend(inputs.into_iter().map(|atom| atom.id));
    }
    sources
}

/// The objects `instruction` may write, each with the bits it writes when a
/// constant offset gives them; `None` when it may write any bits: at a
/// dynamic offset, or in a call. The frontend makes no native kernels (see
/// [`super::super::written_objects`]).
pub(super) fn written_bits<A>(
    instruction: &SIRInstruction<A>,
) -> impl Iterator<Item = (&A, Option<BitAccess>)> {
    let (exact, whole) = match instruction.state_writes() {
        celox_sir::StateAccess::Access(write) => {
            let bits = write
                .offset
                .constant_bit_offset()
                .zip(write.width.checked_sub(1))
                .and_then(|(lsb, tail)| Some(BitAccess::new(lsb, lsb.checked_add(tail)?)));
            (Some((write.addr, bits)), None)
        }
        access => (
            None,
            Some(
                access
                    .objects()
                    .expect("the frontend makes no native kernels"),
            ),
        ),
    };
    exact
        .into_iter()
        .chain(whole.into_iter().flatten().map(|object| (object, None)))
}

/// The variables `unit` loads, and those its calls read.
pub(super) fn unit_loads(
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    loads: &mut HashSet<AbsoluteAddr>,
) {
    for block in unit.blocks.values() {
        for instruction in &block.instructions {
            if matches!(
                instruction,
                SIRInstruction::Load(..) | SIRInstruction::Call { .. }
            ) {
                loads.extend(
                    super::super::read_objects(instruction).map(|addr| addr.absolute_addr()),
                );
            }
        }
    }
}

/// `reads` closed over combinational logic: with a value, the variables it
/// is computed from.
pub(super) fn through_comb(
    mut reads: HashSet<AbsoluteAddr>,
    comb_sources: &HashMap<AbsoluteAddr, HashSet<AbsoluteAddr>>,
) -> HashSet<AbsoluteAddr> {
    let mut pending: Vec<AbsoluteAddr> = reads.iter().copied().collect();
    while let Some(read) = pending.pop() {
        for source in comb_sources.get(&read).into_iter().flatten() {
            if reads.insert(*source) {
                pending.push(*source);
            }
        }
    }
    reads
}

/// Move the commits of the blocking results of every observed domain from
/// its apply units to its evaluation: a blocking result is observed when
/// it, or a combinational value computed from it, is read by a process
/// kernel or by another domain's evaluation, is a wait term, or is another
/// domain's event signal. Domains whose apply units the frontend did not
/// split keep them.
#[allow(clippy::too_many_arguments)]
pub(super) fn split_observed_commits<'k>(
    eval_only_ffs: &mut HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    apply_ffs: &mut HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    split_commits: SplitCommits,
    comb_sources: &HashMap<AbsoluteAddr, HashSet<AbsoluteAddr>>,
    kernels: impl Iterator<Item = &'k ExecutionUnit<RegionedAbsoluteAddr>>,
    wait_terms: impl Iterator<Item = AbsoluteAddr>,
    events: &HashSet<AbsoluteAddr>,
    clock_domains: &HashMap<AbsoluteAddr, AbsoluteAddr>,
) -> Vec<AbsoluteAddr> {
    let mut split = Vec::new();
    let mut shared: HashSet<AbsoluteAddr> = wait_terms.collect();
    for kernel in kernels {
        unit_loads(kernel, &mut shared);
    }
    let evaluation_loads: HashMap<AbsoluteAddr, HashSet<AbsoluteAddr>> = eval_only_ffs
        .iter()
        .map(|(event, units)| {
            let mut loads = HashSet::default();
            for unit in units {
                unit_loads(unit, &mut loads);
            }
            (*event, loads)
        })
        .collect();
    let canonical = |event: &AbsoluteAddr| clock_domains.get(event).copied().unwrap_or(*event);
    for (domain, splits) in split_commits {
        let Some(splits) = splits.into_iter().collect::<Option<Vec<_>>>() else {
            continue;
        };
        if apply_ffs.get(&domain).map_or(0, Vec::len) != splits.len() {
            continue;
        }
        let mut blocking = HashSet::default();
        for split in &splits {
            for block in split.active.blocks.values() {
                for instruction in &block.instructions {
                    if let SIRInstruction::Commit(_, dst, ..) = instruction {
                        blocking.insert(dst.absolute_addr());
                    }
                }
            }
        }
        if blocking.is_empty() {
            continue;
        }
        let mut reads = shared.clone();
        reads.extend(
            events
                .iter()
                .filter(|event| canonical(event) != domain)
                .copied(),
        );
        for (event, loads) in &evaluation_loads {
            if *event != domain {
                reads.extend(loads.iter().copied());
            }
        }
        let reads = through_comb(reads, comb_sources);
        if blocking.is_disjoint(&reads) {
            continue;
        }
        let (active, nba): (Vec<_>, Vec<_>) = splits
            .into_iter()
            .map(|split| (split.active, split.nba))
            .unzip();
        eval_only_ffs.entry(domain).or_default().extend(active);
        apply_ffs.insert(domain, nba);
        split.push(domain);
    }
    split.sort_unstable();
    split
}
