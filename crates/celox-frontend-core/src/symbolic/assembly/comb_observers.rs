//! Combinational observers: capture paths and their order among logic paths.

use super::*;

pub(super) fn build_comb_observer_capture_paths(
    comb_blocks: &mut Vec<LogicPath<AbsoluteAddr>>,
    observers: &mut [CombObserver<AbsoluteAddr>],
    sites: &[RuntimeEventSite],
    arena: &mut SLTNodeArena<AbsoluteAddr>,
    unpacked_element_widths: &HashMap<AbsoluteAddr, usize>,
) -> Result<(), ParserError> {
    if observers.is_empty() {
        return Ok(());
    }

    annotate_comb_capture_enable_sites(comb_blocks, observers, unpacked_element_widths);

    let mut group_members: HashMap<u32, Vec<usize>> = HashMap::default();
    for (idx, observer) in observers.iter().enumerate() {
        group_members
            .entry(observer.activation_group)
            .or_default()
            .push(idx);
    }
    let mut emitted_group_triggers = HashSet::default();
    let mut previous_primary_capture_path: Option<LogicPathId> = None;
    let mut previous_trigger_capture_path: Option<LogicPathId> = None;
    // The capture paths added below write no variable, so what the logic
    // paths read stays the same for every observer.
    let readers = Readers::new(comb_blocks);
    for observer_idx in 0..observers.len() {
        let observer = &observers[observer_idx];
        let affected = observer_affected_by_preceding_writes(&readers, observer);
        let has_statement_position_dependency =
            observer_has_statement_position_dependency(&affected, observer);
        let order_before = observer_order_before(
            comb_blocks,
            observer,
            has_statement_position_dependency.then_some(&affected),
        );
        let order_after =
            observer_order_after(comb_blocks, observer, has_statement_position_dependency);
        let trigger_paths = if has_statement_position_dependency {
            observer_trigger_paths(comb_blocks, observer, &affected)
        } else {
            Vec::new()
        };
        if observer.captured_in_loop {
            let Some(loop_runner) = observer.loop_runner else {
                continue;
            };
            let sources: HashSet<_> = observer
                .sensitivity
                .iter()
                .copied()
                .filter(|atom| !observer_written_input_overlaps(observer, atom))
                .filter(|atom| !affected.overlaps(atom))
                .collect();
            let path_id = LogicPathId(comb_blocks.len());
            if let Some(prev) = previous_primary_capture_path {
                comb_blocks[prev.0].order_before.insert(path_id);
            }
            for idx in &order_after {
                comb_blocks[idx.0].order_before.insert(path_id);
            }
            comb_blocks.push(LogicPath {
                transparent_copy: false,
                target: LogicPathTarget::CombCaptureEvent {
                    site_id: observer.site_id,
                    guard: None,
                    emit_on_true: true,
                    args: Vec::new(),
                    loop_runner: Some(loop_runner),
                    fatal_error_code: None,
                    consume_enabled: true,
                },
                sources,
                previous_sources: HashSet::default(),
                address_sources: HashSet::default(),
                local_inputs: observer.local_inputs.clone(),
                order_before: order_before.clone(),
                comb_capture_enable_sites: Vec::new(),
                comb_capture_enable_always: false,
                pre_lower_nodes: Vec::new(),
                expr: loop_runner,
            });
            previous_primary_capture_path = Some(path_id);
            for trigger_idx in trigger_paths {
                let Some(trigger_target) = comb_blocks[trigger_idx.0].target.var().copied() else {
                    continue;
                };
                let trigger_order_before =
                    direct_consumers_of_path_target(comb_blocks, trigger_idx);
                let path_id = LogicPathId(comb_blocks.len());
                if let Some(prev) = previous_trigger_capture_path {
                    comb_blocks[prev.0].order_before.insert(path_id);
                }
                comb_blocks[trigger_idx.0].order_before.insert(path_id);
                comb_blocks.push(LogicPath {
                    transparent_copy: false,
                    target: LogicPathTarget::CombCaptureEvent {
                        site_id: observer.site_id,
                        guard: None,
                        emit_on_true: true,
                        args: Vec::new(),
                        loop_runner: Some(loop_runner),
                        fatal_error_code: None,
                        consume_enabled: true,
                    },
                    sources: std::iter::once(trigger_target).collect(),
                    previous_sources: HashSet::default(),
                    address_sources: HashSet::default(),
                    local_inputs: observer.local_inputs.clone(),
                    order_before: trigger_order_before,
                    comb_capture_enable_sites: Vec::new(),
                    comb_capture_enable_always: false,
                    pre_lower_nodes: Vec::new(),
                    expr: loop_runner,
                });
                previous_trigger_capture_path = Some(path_id);
            }
            continue;
        }
        let local_input_ids: HashSet<_> = observer
            .local_inputs
            .iter()
            .map(|(addr, _)| *addr)
            .collect();
        let mut sources: HashSet<_> = observer
            .observed_inputs
            .iter()
            .copied()
            .filter(|atom| !observer_written_input_overlaps(observer, atom))
            .filter(|atom| !local_input_ids.contains(&atom.id))
            .filter(|atom| !affected.overlaps(atom))
            .collect();
        for (_, node) in &observer.local_inputs {
            let mut local_sources = HashSet::default();
            crate::flattening::collect_inputs(*node, arena, &mut local_sources);
            sources.extend(
                local_sources
                    .into_iter()
                    .filter(|atom| !observer_written_input_overlaps(observer, atom))
                    .filter(|atom| !local_input_ids.contains(&atom.id))
                    .filter(|atom| !affected.overlaps(atom)),
            );
        }
        let expr = match observer.guard.or_else(|| observer.args.first().copied()) {
            Some(expr) => expr,
            None => arena.alloc(celox_slt::SLTNode::Constant(
                num_bigint::BigUint::from(1u8),
                num_bigint::BigUint::from(0u8),
                1,
                false,
            ))?,
        };
        let emit_on_true = matches!(
            sites[observer.site_id as usize].kind,
            RuntimeEventKind::Display
                | RuntimeEventKind::Write
                | RuntimeEventKind::Finish
                | RuntimeEventKind::Stop
        );
        // A deferred `$fatal` ends the simulation when it matures.
        let fatal_error_code = (sites[observer.site_id as usize].kind.is_fatal()
            && sites[observer.site_id as usize].deferred.is_none())
        .then_some(observer.site_id as i64);
        let pre_lower_nodes = observer_pre_lower_nodes(observer, arena);
        for idx in &order_after {
            comb_blocks[idx.0]
                .pre_lower_nodes
                .extend(pre_lower_nodes.iter().copied());
        }
        let path_id = LogicPathId(comb_blocks.len());
        if let Some(prev) = previous_primary_capture_path {
            comb_blocks[prev.0].order_before.insert(path_id);
        }
        for idx in &order_after {
            comb_blocks[idx.0].order_before.insert(path_id);
        }
        comb_blocks.push(LogicPath {
            transparent_copy: false,
            target: LogicPathTarget::CombCaptureEvent {
                site_id: observer.site_id,
                guard: observer.guard,
                emit_on_true,
                args: observer.args.clone(),
                loop_runner: None,
                fatal_error_code,
                consume_enabled: !trigger_paths.is_empty(),
            },
            sources,
            previous_sources: HashSet::default(),
            address_sources: HashSet::default(),
            local_inputs: observer.local_inputs.clone(),
            order_before,
            comb_capture_enable_sites: Vec::new(),
            comb_capture_enable_always: false,
            pre_lower_nodes: Vec::new(),
            expr,
        });
        previous_primary_capture_path = Some(path_id);
        for trigger_idx in trigger_paths {
            if !emitted_group_triggers.insert((observer.activation_group, trigger_idx)) {
                continue;
            }
            let Some(trigger_target) = comb_blocks[trigger_idx.0].target.var().copied() else {
                continue;
            };
            let trigger_order_before = direct_consumers_of_path_target(comb_blocks, trigger_idx);
            for &member_idx in &group_members[&observer.activation_group] {
                let member = &observers[member_idx];
                let member_emit_on_true = matches!(
                    sites[member.site_id as usize].kind,
                    RuntimeEventKind::Display
                        | RuntimeEventKind::Write
                        | RuntimeEventKind::Finish
                        | RuntimeEventKind::Stop
                );
                // A deferred `$fatal` ends the simulation when it matures.
                let member_fatal_error_code = (sites[member.site_id as usize].kind.is_fatal()
                    && sites[member.site_id as usize].deferred.is_none())
                .then_some(member.site_id as i64);
                let member_expr = match member
                    .loop_runner
                    .or(member.guard)
                    .or_else(|| member.args.first().copied())
                {
                    Some(expr) => expr,
                    None => arena.alloc(celox_slt::SLTNode::Constant(
                        num_bigint::BigUint::from(1u8),
                        num_bigint::BigUint::from(0u8),
                        1,
                        false,
                    ))?,
                };
                let path_id = LogicPathId(comb_blocks.len());
                if let Some(prev) = previous_trigger_capture_path {
                    comb_blocks[prev.0].order_before.insert(path_id);
                }
                comb_blocks[trigger_idx.0].order_before.insert(path_id);
                comb_blocks.push(LogicPath {
                    transparent_copy: false,
                    target: LogicPathTarget::CombCaptureEvent {
                        site_id: member.site_id,
                        guard: member.guard,
                        emit_on_true: member_emit_on_true,
                        args: member.args.clone(),
                        loop_runner: member.loop_runner,
                        fatal_error_code: member_fatal_error_code,
                        consume_enabled: true,
                    },
                    sources: std::iter::once(trigger_target).collect(),
                    previous_sources: HashSet::default(),
                    address_sources: HashSet::default(),
                    local_inputs: member.local_inputs.clone(),
                    order_before: trigger_order_before.clone(),
                    comb_capture_enable_sites: Vec::new(),
                    comb_capture_enable_always: false,
                    pre_lower_nodes: Vec::new(),
                    expr: member_expr,
                });
                previous_trigger_capture_path = Some(path_id);
            }
        }
    }
    Ok(())
}

pub(super) fn apply_always_comb_previous_source_ordering(
    comb_blocks: &mut [LogicPath<AbsoluteAddr>],
) {
    let targets: Vec<_> = comb_blocks
        .iter()
        .map(|path| path.target.var().copied())
        .collect();

    for (idx, path) in comb_blocks.iter_mut().enumerate() {
        if path.previous_sources.is_empty() {
            continue;
        }

        let previous_sources = path.previous_sources.clone();
        let address_sources = path.address_sources.clone();
        path.sources.retain(|source| {
            let is_previous = previous_sources.iter().any(|previous| {
                previous.id == source.id && previous.access.overlaps(&source.access)
            });
            let is_address = address_sources
                .iter()
                .any(|address| address.id == source.id && address.access.overlaps(&source.access));
            !is_previous || is_address
        });

        let mut order_before = Vec::new();
        for (target_idx, target) in targets.iter().enumerate() {
            if target_idx == idx {
                continue;
            }
            let Some(target) = target else {
                continue;
            };
            if previous_sources.iter().any(|previous| {
                previous.id == target.id && previous.access.overlaps(&target.access)
            }) {
                order_before.push(LogicPathId(target_idx));
            }
        }
        path.order_before.extend(order_before);
    }
}

pub(super) fn observer_written_input_overlaps(
    observer: &CombObserver<AbsoluteAddr>,
    atom: &VarAtomBase<AbsoluteAddr>,
) -> bool {
    observer
        .written_input_atoms
        .iter()
        .any(|written| written.id == atom.id && written.access.overlaps(&atom.access))
}

pub(super) fn observer_has_statement_position_dependency(
    affected: &Affected,
    observer: &CombObserver<AbsoluteAddr>,
) -> bool {
    if observer.preceding_writes.is_empty() {
        return false;
    }
    observer
        .position_inputs
        .iter()
        .chain(observer.observed_inputs.iter())
        .any(|input| affected.overlaps(input))
}

pub(super) fn observer_trigger_paths(
    paths: &[LogicPath<AbsoluteAddr>],
    observer: &CombObserver<AbsoluteAddr>,
    affected: &Affected,
) -> Vec<LogicPathId> {
    let mut seen_targets = HashSet::default();
    paths
        .iter()
        .enumerate()
        .filter_map(|(idx, path)| {
            let target = path.target.var()?;
            if observer_written_input_overlaps(observer, target) {
                return None;
            }
            let matches_observer_operand = observer
                .position_inputs
                .iter()
                .chain(observer.observed_inputs.iter())
                .any(|atom| target.id == atom.id && target.access.overlaps(&atom.access));
            if !matches_observer_operand
                || !affected.overlaps(target)
                || !seen_targets.insert((target.id, target.access.lsb, target.access.msb))
            {
                return None;
            }
            Some(LogicPathId(idx))
        })
        .collect()
}

pub(super) fn observer_pre_lower_nodes(
    observer: &CombObserver<AbsoluteAddr>,
    arena: &SLTNodeArena<AbsoluteAddr>,
) -> Vec<NodeId> {
    let local_input_ids: HashSet<_> = observer.local_inputs.iter().map(|(id, _)| *id).collect();
    let mut nodes = Vec::with_capacity(observer.args.len() + usize::from(observer.guard.is_some()));
    if let Some(guard) = observer.guard {
        nodes.push(guard);
    }
    nodes.extend(observer.args.iter().copied());
    nodes.extend(observer.local_inputs.iter().filter_map(|(_, node)| {
        matches!(arena.get(*node), celox_slt::SLTNode::Capture { .. }).then_some(*node)
    }));
    nodes
        .into_iter()
        .filter(|node| {
            let mut inputs = HashSet::default();
            crate::flattening::collect_inputs(*node, arena, &mut inputs);
            // A capture independent of local bindings can be materialized at
            // its statement position. Bound captures still need the event's
            // environment, unless they contain an earlier formal snapshot.
            // Constants need no early materialization.
            !inputs.is_empty()
                && (capture_contains_nested_snapshot(*node, arena)
                    || inputs
                        .iter()
                        .all(|input| !local_input_ids.contains(&input.id)))
        })
        .collect()
}

pub(super) fn capture_contains_nested_snapshot(
    node: NodeId,
    arena: &SLTNodeArena<AbsoluteAddr>,
) -> bool {
    let celox_slt::SLTNode::Capture { expr, .. } = arena.get(node) else {
        return false;
    };
    let mut work = vec![*expr];
    let mut visited = HashSet::default();
    while let Some(node) = work.pop() {
        if !visited.insert(node) {
            continue;
        }
        match arena.get(node) {
            celox_slt::SLTNode::Capture { .. } => return true,
            celox_slt::SLTNode::Input { index, .. } => {
                work.extend(index.iter().map(|entry| entry.node));
            }
            celox_slt::SLTNode::Constant(..) => {}
            celox_slt::SLTNode::Binary(lhs, _, rhs) => {
                work.push(*lhs);
                work.push(*rhs);
            }
            celox_slt::SLTNode::Unary(_, inner) => work.push(*inner),
            celox_slt::SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                work.push(*cond);
                work.push(*then_expr);
                work.push(*else_expr);
            }
            celox_slt::SLTNode::Concat(parts) => {
                work.extend(parts.iter().map(|(part, _)| *part));
            }
            celox_slt::SLTNode::Slice { expr, .. } => work.push(*expr),
            celox_slt::SLTNode::ForFold {
                start,
                end,
                result,
                initials,
                updates,
                effects,
                continue_cond,
                ..
            } => {
                if let celox_slt::SLTLoopBound::Expr(node)
                | celox_slt::SLTLoopBound::TypedExpr { node, .. } = start
                {
                    work.push(*node);
                }
                if let celox_slt::SLTLoopBound::Expr(node)
                | celox_slt::SLTLoopBound::TypedExpr { node, .. } = end
                {
                    work.push(*node);
                }
                if let celox_slt::SLTForFoldResult::Transient { initial, update } = result {
                    work.push(*initial);
                    work.push(*update);
                }
                work.extend(initials.iter().map(|state| state.expr));
                work.extend(updates.iter().map(|state| state.expr));
                for effect in effects {
                    match effect {
                        celox_slt::SLTForEffect::Event { guard, args, .. } => {
                            work.extend(*guard);
                            work.extend(args.iter().copied());
                        }
                        celox_slt::SLTForEffect::Runner(runner) => work.push(*runner),
                    }
                }
                work.push(*continue_cond);
            }
            celox_slt::SLTNode::ForFoldGroup {
                entry_guard,
                states,
                ..
            } => {
                work.push(*entry_guard);
                for state in states {
                    work.push(state.initial);
                    work.push(state.update);
                }
            }
        }
    }
    false
}

pub(super) fn annotate_comb_capture_enable_sites(
    comb_blocks: &mut [LogicPath<AbsoluteAddr>],
    observers: &[CombObserver<AbsoluteAddr>],
    unpacked_element_widths: &HashMap<AbsoluteAddr, usize>,
) {
    let mut group_sites: HashMap<u32, Vec<u32>> = HashMap::default();
    for observer in observers {
        group_sites
            .entry(observer.activation_group)
            .or_default()
            .push(observer.site_id);
    }
    for observer in observers {
        for atom in &observer.sensitivity {
            for path in comb_blocks.iter_mut() {
                let Some(target) = path.target.var() else {
                    continue;
                };
                if target.id == atom.id && target.access.overlaps(&atom.access) {
                    for site_id in &group_sites[&observer.activation_group] {
                        if !path.comb_capture_enable_sites.contains(site_id) {
                            path.comb_capture_enable_sites.push(*site_id);
                        }
                    }
                }
            }
        }
    }
    // A process executes all its writes on its first output projection.
    // Compare exactly the observed bits around each store: a dynamic select
    // may change another bit of the same object without waking the observer.
    use celox_sir::{RegisterId, RegisterType, SIROffset};
    let mut process_writes: HashMap<AbsoluteAddr, Vec<VarAtomBase<AbsoluteAddr>>> =
        HashMap::default();
    for path in comb_blocks.iter() {
        if let LogicPathTarget::Process {
            process, target, ..
        } = &path.target
        {
            process_writes.entry(*process).or_default().push(*target);
        }
    }
    for path in comb_blocks.iter_mut() {
        let LogicPathTarget::Process { process, unit, .. } = &mut path.target else {
            continue;
        };
        let mut probes: Vec<(VarAtomBase<AbsoluteAddr>, Vec<u32>)> = Vec::new();
        for observer in observers {
            for atom in &observer.sensitivity {
                if !process_writes[&*process]
                    .iter()
                    .any(|written| written.id == atom.id && written.access.overlaps(&atom.access))
                {
                    continue;
                }
                let index = match probes.iter().position(|(probe, _)| probe == atom) {
                    Some(index) => index,
                    None => {
                        probes.push((*atom, Vec::new()));
                        probes.len() - 1
                    }
                };
                for site in &group_sites[&observer.activation_group] {
                    if !probes[index].1.contains(site) {
                        probes[index].1.push(*site);
                    }
                }
            }
        }
        let unit = &mut **unit;
        let mut next_register = unit
            .register_map
            .keys()
            .map(|reg| reg.0 + 1)
            .max()
            .unwrap_or(0);
        for block in unit.blocks.values_mut() {
            let mut instructions = Vec::new();
            for inst in std::mem::take(&mut block.instructions) {
                let mut captures = Vec::new();
                if let SIRInstruction::Store(address, offset, width, ..) = &inst {
                    let written = offset.constant_bit_offset().and_then(|lsb| {
                        Some(BitAccess::new(lsb, lsb.checked_add(width.checked_sub(1)?)?))
                    });
                    for (atom, sites) in &probes {
                        if atom.id != *address
                            || written.is_some_and(|written| !written.overlaps(&atom.access))
                        {
                            continue;
                        }
                        let width = atom.access.msb - atom.access.lsb + 1;
                        let offset = match unpacked_element_widths.get(address) {
                            Some(element_width)
                                if width > *element_width
                                    && atom.access.lsb.is_multiple_of(*element_width)
                                    && width.is_multiple_of(*element_width) =>
                            {
                                SIROffset::PackedElements {
                                    bit_offset: atom.access.lsb,
                                    element_width: *element_width,
                                }
                            }
                            _ => SIROffset::Static(atom.access.lsb),
                        };
                        let old = RegisterId(next_register);
                        let new = RegisterId(next_register + 1);
                        next_register += 2;
                        unit.register_map.insert(old, RegisterType::Logic { width });
                        unit.register_map.insert(new, RegisterType::Logic { width });
                        instructions.push(SIRInstruction::Load(
                            old,
                            *address,
                            offset.clone(),
                            width,
                        ));
                        captures.push((old, new, *address, offset, width, sites));
                    }
                }
                instructions.push(inst);
                for (old, new, address, offset, width, sites) in captures {
                    instructions.push(SIRInstruction::Load(new, address, offset, width));
                    instructions.push(SIRInstruction::CombCaptureEnableIfChanged {
                        old,
                        new,
                        sites: sites.clone(),
                    });
                }
            }
            block.instructions = instructions;
        }
    }
}

pub(super) fn observer_order_after(
    paths: &[LogicPath<AbsoluteAddr>],
    observer: &CombObserver<AbsoluteAddr>,
    has_statement_position_dependency: bool,
) -> HashSet<LogicPathId> {
    let mut result = HashSet::default();
    if !has_statement_position_dependency {
        return result;
    }
    for written in &observer.preceding_writes {
        for (idx, path) in paths.iter().enumerate() {
            let Some(target) = path.target.var() else {
                continue;
            };
            if target.id == written.id && target.access.overlaps(&written.access) {
                result.insert(LogicPathId(idx));
            }
        }
    }
    result
}

pub(super) fn observer_order_before(
    paths: &[LogicPath<AbsoluteAddr>],
    observer: &CombObserver<AbsoluteAddr>,
    affected_by_preceding_writes: Option<&Affected>,
) -> HashSet<LogicPathId> {
    let preceding_writes = observer.preceding_writes.iter().collect::<Vec<_>>();
    let mut result = HashSet::default();
    for (idx, path) in paths.iter().enumerate() {
        let Some(target) = path.target.var() else {
            continue;
        };
        let already_written = preceding_writes
            .iter()
            .any(|written| target.id == written.id && target.access.overlaps(&written.access));
        let is_later_observed_write = observer_written_input_overlaps(observer, target);
        let is_later_affected_write =
            affected_by_preceding_writes.is_some_and(|affected| affected.overlaps(target));
        if !already_written && (is_later_observed_write || is_later_affected_write) {
            result.insert(LogicPathId(idx));
        }
    }
    result
}

/// Place a trigger-capture between the write which activated it and each
/// immediate dataflow consumer of that write. Transitive consumers remain
/// ordered by the ordinary LogicPath dependency graph.
pub(super) fn direct_consumers_of_path_target(
    paths: &[LogicPath<AbsoluteAddr>],
    trigger: LogicPathId,
) -> HashSet<LogicPathId> {
    let Some(target) = paths.get(trigger.0).and_then(|path| path.target.var()) else {
        return HashSet::default();
    };
    paths
        .iter()
        .enumerate()
        .filter_map(|(index, path)| {
            (index != trigger.0
                && path
                    .sources
                    .iter()
                    .any(|source| source.id == target.id && source.access.overlaps(&target.access)))
            .then_some(LogicPathId(index))
        })
        .collect()
}

/// What each variable that a logic path reads reaches: the part the path
/// reads and the variable part it writes.
pub(super) struct Readers {
    by_id: HashMap<AbsoluteAddr, Vec<(BitAccess, VarAtomBase<AbsoluteAddr>)>>,
}

impl Readers {
    pub(super) fn new(paths: &[LogicPath<AbsoluteAddr>]) -> Self {
        let mut by_id: HashMap<_, Vec<_>> = HashMap::default();
        for path in paths {
            let Some(target) = path.target.var() else {
                continue;
            };
            for source in &path.sources {
                by_id
                    .entry(source.id)
                    .or_default()
                    .push((source.access, *target));
            }
        }
        Self { by_id }
    }
}

/// The variable parts that the writes before an observer reach through the
/// combinational dataflow, by variable.
#[derive(Default)]
pub(super) struct Affected {
    by_id: HashMap<AbsoluteAddr, Vec<BitAccess>>,
}

impl Affected {
    /// Whether `atom` overlaps an affected part of its variable.
    pub(super) fn overlaps(&self, atom: &VarAtomBase<AbsoluteAddr>) -> bool {
        self.by_id
            .get(&atom.id)
            .is_some_and(|parts| parts.iter().any(|part| part.overlaps(&atom.access)))
    }
}

/// The parts that the writes before `observer` reach (see
/// [`affected_by_writes`]).
pub(super) fn observer_affected_by_preceding_writes(
    readers: &Readers,
    observer: &CombObserver<AbsoluteAddr>,
) -> Affected {
    affected_by_writes(readers, &observer.preceding_writes)
}

/// The parts that `writes` reach: those writes, and the targets of the
/// paths that read a part already reached, to a fixpoint. Each reached part
/// is followed to the paths that read its variable once.
pub(super) fn affected_by_writes<'a>(
    readers: &Readers,
    writes: impl IntoIterator<Item = &'a VarAtomBase<AbsoluteAddr>>,
) -> Affected {
    let mut reached: HashSet<VarAtomBase<AbsoluteAddr>> = HashSet::default();
    let mut affected = Affected::default();
    let mut pending: Vec<VarAtomBase<AbsoluteAddr>> = Vec::new();
    for write in writes {
        if reached.insert(*write) {
            pending.push(*write);
        }
    }
    while let Some(atom) = pending.pop() {
        affected.by_id.entry(atom.id).or_default().push(atom.access);
        for (read, target) in readers.by_id.get(&atom.id).into_iter().flatten() {
            if read.overlaps(&atom.access) && reached.insert(*target) {
                pending.push(*target);
            }
        }
    }
    affected
}
