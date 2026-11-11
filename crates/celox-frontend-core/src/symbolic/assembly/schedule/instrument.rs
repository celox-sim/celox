//! Instrumenting the units of a design to count the occurrences of its signals.

use super::*;

/// Choose the form of the combinational processes that have two, and
/// instrument every unit that writes a probed or counted signal. Returns the
/// counted signals with their counters and the sensitivity probes, once for
/// the declaration startup and once for the runtime.
pub(super) fn instrument_occurrences(
    design: &Design,
    logic: &mut CombLogic,
    units: &mut Units,
    parallel_ff_units: &mut FfParts,
    split_commits: &mut SplitCommits,
    variables: &HashMap<AbsoluteAddr, (usize, bool)>,
    four_state: bool,
) -> (
    BTreeMap<AbsoluteAddr, CounterSlots<AbsoluteAddr>>,
    Vec<(VarAtomBase<AbsoluteAddr>, AbsoluteAddr)>,
    Vec<(VarAtomBase<AbsoluteAddr>, AbsoluteAddr)>,
) {
    let Design {
        bindings,
        instance_modules,
        modules,
        clock_domains,
        unpacked_element_widths,
        ..
    } = design;
    let CombLogic {
        global_arena,
        comb_blocks,
        comb_observers,
    } = logic;
    let Units {
        eval_apply_ffs,
        eval_only_ffs,
        apply_ffs,
        processes,
        functions,
        timing,
        ..
    } = units;

    let declaration_probes;
    let occurrence_probes: Vec<(VarAtomBase<AbsoluteAddr>, AbsoluteAddr)>;
    let counted = {
        // Flip-flop event domains and the aliases that resolve to them.
        let event_addresses: HashSet<AbsoluteAddr> = eval_apply_ffs
            .keys()
            .copied()
            .chain(
                clock_domains
                    .iter()
                    .filter(|(_, canonical)| eval_apply_ffs.contains_key(canonical))
                    .map(|(alias, _)| *alias),
            )
            .collect();
        let counted = counted_signals(timing, &event_addresses);
        // A combinational process writing the signal or the gate of a gated
        // wait keeps its procedural order (IEEE 1800-2023 9.4.2.3).
        let gated: HashSet<AbsoluteAddr> = timing
            .wait_sites
            .iter()
            .flat_map(|site| {
                site.terms
                    .iter()
                    .zip(&site.gates)
                    .filter_map(|(&(signal, _), gate)| {
                        gate.as_ref()
                            .filter(|gate| gate.at_write)
                            .map(|gate| [signal, gate.signal])
                    })
            })
            .flatten()
            .collect();
        choose_comb_alternatives(
            bindings,
            instance_modules,
            modules,
            &counted,
            &gated,
            comb_blocks,
        );
        // Count sensitivity occurrences before any optimizer can coalesce
        // consecutive writes whose final value is unchanged.
        let probes = sensitivity_probes(
            comb_blocks,
            comb_observers,
            global_arena,
            |source, target| !four_state || variables[source].1 == variables[target].1,
        );
        declaration_probes = probes.clone();
        occurrence_probes = probes.clone();
        // Signals a flip-flop evaluation may store twice in its working
        // state. Each store is an update event (IEEE 1800-2023 4.3, 10.4.2)
        // that the commit of the final value alone would not show; the
        // working state is seeded with the stable value first, so a store
        // compares with the value its update replaces.
        let ff_multi_stored = ff_multi_stored(eval_apply_ffs, eval_only_ffs, parallel_ff_units);
        // Each unit is instrumented once for all probes. Probe `p` is pass
        // `2p` over the stable state and pass `2p + 1` over a flip-flop
        // evaluation's working state, the order of instrumenting probe by
        // probe.
        let probe_codes: Vec<_> = probes
            .iter()
            .map(|&(source, counter)| {
                let width = source.access.msb - source.access.lsb + 1;
                let four_state = four_state && variables[&source.id].1;
                let offset = match unpacked_element_widths.get(&source.id) {
                    Some(&element_width)
                        if width > element_width
                            && source.access.lsb.is_multiple_of(element_width)
                            && width.is_multiple_of(element_width) =>
                    {
                        celox_sir::SIROffset::PackedElements {
                            bit_offset: source.access.lsb,
                            element_width,
                        }
                    }
                    _ => celox_sir::SIROffset::Static(source.access.lsb),
                };
                (counter, width, four_state, offset)
            })
            .collect();
        let mut probes_of: HashMap<AbsoluteAddr, Vec<usize>> = HashMap::default();
        for (index, (source, _)) in probes.iter().enumerate() {
            probes_of.entry(source.id).or_default().push(index);
        }
        // The first probe of `signal` whose `pass` is `after` or later.
        let first_probe = |signal: AbsoluteAddr, pass: fn(usize) -> usize, after: usize| {
            let indices = probes_of.get(&signal)?;
            indices
                .get(indices.partition_point(|&index| pass(index) < after))
                .copied()
        };
        fn probe_code<A>(
            probe: &(AbsoluteAddr, usize, bool, celox_sir::SIROffset),
            signal: A,
            counter: A,
        ) -> crate::watch::WatchCode<A> {
            let &(_, width, four_state, ref offset) = probe;
            crate::watch::WatchCode {
                signal,
                width,
                offset: offset.clone(),
                four_state,
                counters: vec![(CounterKind::Changes, counter)],
                gated: Vec::new(),
            }
        }
        let stable = |address| RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, address);
        let working = |address| RegionedAbsoluteAddr::from_absolute_addr(WORKING_REGION, address);
        let stable_code = |instruction: &SIRInstruction<RegionedAbsoluteAddr>, after: usize| {
            let address = crate::watch::write_destination(instruction)?;
            if address.region != STABLE_REGION {
                return None;
            }
            let index = first_probe(address.absolute_addr(), |index| 2 * index, after)?;
            Some((
                2 * index,
                probe_code(
                    &probe_codes[index],
                    stable(address.absolute_addr()),
                    stable(probe_codes[index].0),
                ),
            ))
        };
        // A flip-flop evaluation's stores to a signal of `ff_multi_stored`;
        // the commits that seed its working state are not writes of the
        // process.
        let evaluation_code = |instruction: &SIRInstruction<RegionedAbsoluteAddr>, after: usize| {
            if let SIRInstruction::Store(address, ..) = instruction
                && address.region == WORKING_REGION
                && ff_multi_stored.contains(&address.absolute_addr())
                && let Some(index) =
                    first_probe(address.absolute_addr(), |index| 2 * index + 1, after)
            {
                return Some((
                    2 * index + 1,
                    probe_code(
                        &probe_codes[index],
                        working(address.absolute_addr()),
                        stable(probe_codes[index].0),
                    ),
                ));
            }
            stable_code(instruction, after)
        };
        for process in &mut *processes {
            crate::watch::instrument_writes_in_passes(&mut process.kernel, stable_code);
        }
        // A function unit's stores are a process's, whichever calls it.
        for function in &mut *functions {
            crate::watch::instrument_writes_in_passes(&mut function.unit, stable_code);
        }
        for units in eval_apply_ffs
            .values_mut()
            .chain(eval_only_ffs.values_mut())
        {
            for unit in units {
                crate::watch::instrument_writes_in_passes(unit, evaluation_code);
            }
        }
        for units in apply_ffs.values_mut() {
            for unit in units {
                crate::watch::instrument_writes_in_passes(unit, stable_code);
            }
        }
        for parts in parallel_ff_units.values_mut() {
            for (_, part) in parts {
                crate::watch::instrument_writes_in_passes(&mut part.evaluate, evaluation_code);
                crate::watch::instrument_writes_in_passes(&mut part.apply, stable_code);
            }
        }
        for path in &mut *comb_blocks {
            if let LogicPathTarget::Process { unit, .. } = &mut path.target {
                crate::watch::instrument_writes_in_passes(unit, |instruction, after| {
                    let &address = crate::watch::write_destination(instruction)?;
                    let index = first_probe(address, |index| index, after)?;
                    Some((
                        index,
                        probe_code(&probe_codes[index], address, probe_codes[index].0),
                    ))
                });
            }
        }
        let ff_written: HashSet<AbsoluteAddr> = eval_apply_ffs
            .values()
            .chain(eval_only_ffs.values())
            .chain(apply_ffs.values())
            .flatten()
            .chain(
                parallel_ff_units
                    .values()
                    .flatten()
                    .flat_map(|(_, part)| [&part.evaluate, &part.apply]),
            )
            .chain(
                split_commits
                    .values()
                    .flatten()
                    .flatten()
                    .flat_map(|split| [&split.active, &split.nba]),
            )
            .flat_map(|unit| unit.blocks.values())
            .flat_map(|block| &block.instructions)
            .flat_map(super::super::super::written_objects)
            .map(|destination| destination.absolute_addr())
            .collect();
        let gated = gated_terms(timing, comb_blocks, &ff_written);
        let ff_units = eval_apply_ffs
            .values_mut()
            .chain(eval_only_ffs.values_mut())
            .chain(apply_ffs.values_mut())
            .flatten()
            .chain(
                parallel_ff_units
                    .values_mut()
                    .flatten()
                    .flat_map(|(_, part)| [&mut part.evaluate, &mut part.apply]),
            )
            .chain(
                split_commits
                    .values_mut()
                    .flatten()
                    .flatten()
                    .flat_map(|split| [&mut split.active, &mut split.nba]),
            );
        let counted = instrument_counted_signals(
            timing,
            counted,
            &gated,
            variables,
            four_state,
            processes,
            functions,
            comb_blocks,
            ff_units,
        );
        count_repeated_evaluation_stores(
            &counted,
            timing,
            &ff_multi_stored,
            variables,
            four_state,
            eval_apply_ffs,
            eval_only_ffs,
            parallel_ff_units,
        );
        counted
    };
    (counted, declaration_probes, occurrence_probes)
}

/// Count each store of a flip-flop evaluation to a counted signal it may
/// store twice, for the signals whose commit alone is counted.
fn count_repeated_evaluation_stores(
    counted: &BTreeMap<AbsoluteAddr, CounterSlots<AbsoluteAddr>>,
    timing: &TimingMetadata,
    ff_multi_stored: &HashSet<AbsoluteAddr>,
    variables: &HashMap<AbsoluteAddr, (usize, bool)>,
    four_state: bool,
    eval_apply_ffs: &mut FfUnits,
    eval_only_ffs: &mut FfUnits,
    parallel_ff_units: &mut FfParts,
) {
    // A counted signal without pending slots (a design without waits)
    // is counted at its commit, which sees only the final value. Count
    // each store of an evaluation that may store it twice as well.
    let pending: HashSet<AbsoluteAddr> = timing
        .pending_slots
        .iter()
        .map(|slots| slots.signal)
        .collect();
    // One pass per signal, in signal order; each pass watches only its
    // own signal.
    let regioned = RegionedAbsoluteAddr::from_absolute_addr;
    let multi_stored: HashMap<AbsoluteAddr, (usize, crate::watch::WatchCode<_>)> = counted
        .iter()
        .filter(|(signal, _)| ff_multi_stored.contains(signal) && !pending.contains(signal))
        .enumerate()
        .map(|(pass, (&signal, slots))| {
            let (width, is_4state) = variables[&signal];
            let code = crate::watch::WatchCode {
                signal: regioned(WORKING_REGION, signal),
                offset: celox_sir::SIROffset::Static(0),
                width,
                four_state: four_state && is_4state,
                counters: CounterKind::ALL
                    .into_iter()
                    .map(|kind| (kind, regioned(STABLE_REGION, slots.counter(kind))))
                    .collect(),
                gated: Vec::new(),
            };
            (signal, (pass, code))
        })
        .collect();
    if !multi_stored.is_empty() {
        for_each_ff_evaluation(eval_apply_ffs, eval_only_ffs, parallel_ff_units, |unit| {
            crate::watch::instrument_writes_in_passes(unit, |instruction, after| {
                let SIRInstruction::Store(address, ..) = instruction else {
                    return None;
                };
                if address.region != WORKING_REGION {
                    return None;
                }
                let (pass, code) = multi_stored.get(&address.absolute_addr())?;
                (*pass >= after).then(|| (*pass, code.clone()))
            })
        });
    }
}
