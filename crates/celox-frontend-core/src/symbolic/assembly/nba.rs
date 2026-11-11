//! Choosing the staged or the queued path of switched nonblocking sites.

use super::*;

/// The switches of the switched nonblocking sites that may stage their
/// value: the site's target has no other writer (no store or commit of any
/// unit, no other nonblocking site, not a top-level port the host drives)
/// and is not in `observed`.
pub(super) fn staged_nba_sites<'u>(
    timing: &TimingMetadata,
    units: impl Iterator<Item = &'u ExecutionUnit<RegionedAbsoluteAddr>>,
    top_ports: &HashSet<AbsoluteAddr>,
    observed: &HashSet<AbsoluteAddr>,
) -> HashSet<AbsoluteAddr> {
    let mut written = HashSet::default();
    for unit in units {
        for block in unit.blocks.values() {
            for instruction in &block.instructions {
                written.extend(
                    super::super::written_objects(instruction).map(|dst| dst.absolute_addr()),
                );
            }
        }
    }
    let mut sites_per_target: HashMap<AbsoluteAddr, usize> = HashMap::default();
    for site in &timing.nba_sites {
        *sites_per_target.entry(site.target).or_default() += 1;
    }
    let targets: HashMap<AbsoluteAddr, AbsoluteAddr> = timing
        .nba_sites
        .iter()
        .map(|site| (site.value, site.target))
        .collect();
    timing
        .nba_switches
        .iter()
        .filter(|switch| {
            let target = targets[&switch.value];
            sites_per_target[&target] == 1
                && !written.contains(&target)
                && !top_ports.contains(&target)
                && !observed.contains(&target)
        })
        .map(|switch| switch.switch)
        .collect()
}

/// Keep one path of every switched nonblocking site: the staged one if its
/// switch is in `staged`, otherwise the queued one, whose value object
/// becomes the site's and which has no `pending` flag. The kernels select
/// the path by a jump; the other path is removed.
pub(super) fn select_nba_paths(
    timing: &mut TimingMetadata,
    staged: &HashSet<AbsoluteAddr>,
    processes: &mut [RelocatedProcess],
) {
    let mut staged = staged.clone();
    // Future updates must be queued at issue time rather than staged for
    // publication in the current slot. Fork activations can reach one NBA site
    // concurrently. Always queue its
    // captured value, rather than leaving one shared pending slot per kernel.
    for site in &timing.nba_sites {
        if site.delay > 0 || processes[site.process].frame.is_some() {
            for switch in &timing.nba_switches {
                if switch.value == site.value {
                    staged.remove(&switch.switch);
                }
            }
        }
    }
    let switches: HashMap<AbsoluteAddr, NbaSwitch<AbsoluteAddr>> = timing
        .nba_switches
        .iter()
        .map(|switch| (switch.value, *switch))
        .collect();
    for site in &mut timing.nba_sites {
        let Some(switch) = switches.get(&site.value) else {
            continue;
        };
        if !staged.contains(&switch.switch) {
            site.value = switch.queued;
            site.position = switch.queued_position;
            site.pending = None;
        }
    }
    let selections: HashMap<AbsoluteAddr, bool> = switches
        .values()
        .map(|switch| (switch.switch, staged.contains(&switch.switch)))
        .collect();
    let mut folded = 0;
    for process in processes {
        folded += fold_nba_switches(&mut process.kernel, &selections);
    }
    assert_eq!(
        folded,
        selections.len(),
        "every nonblocking switch selects its path once"
    );
}

/// Replace each branch on a load of a switch in `selections` by a jump to
/// the selected path and remove the blocks no longer reachable. Returns the
/// number of branches replaced.
pub(super) fn fold_nba_switches(
    unit: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    selections: &HashMap<AbsoluteAddr, bool>,
) -> usize {
    let mut folded = 0;
    for block in unit.blocks.values_mut() {
        let SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } = &block.terminator
        else {
            continue;
        };
        let Some((position, staged)) =
            block
                .instructions
                .iter()
                .enumerate()
                .find_map(|(position, instruction)| match instruction {
                    SIRInstruction::Load(register, addr, SIROffset::Static(0), 1)
                        if register == cond =>
                    {
                        selections
                            .get(&addr.absolute_addr())
                            .map(|&staged| (position, staged))
                    }
                    _ => None,
                })
        else {
            continue;
        };
        let (target, args) = if staged { true_block } else { false_block };
        block.terminator = SIRTerminator::Jump(*target, args.clone());
        block.instructions.remove(position);
        folded += 1;
    }
    if folded > 0 {
        let mut reachable = HashSet::default();
        let mut pending = vec![unit.entry_block_id];
        while let Some(id) = pending.pop() {
            if !reachable.insert(id) {
                continue;
            }
            match &unit.blocks[&id].terminator {
                SIRTerminator::Jump(next, _) => pending.push(*next),
                SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } => pending.extend([true_block.0, false_block.0]),
                SIRTerminator::Switch { cases, default, .. } => {
                    pending.extend(cases.iter().map(|case| case.target));
                    pending.push(*default);
                }
                SIRTerminator::Return | SIRTerminator::Error(_) => {}
            }
        }
        unit.blocks.retain(|id, _| reachable.contains(id));
    }
    folded
}
