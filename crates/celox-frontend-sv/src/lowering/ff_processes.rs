//! Lowering of the edge-triggered processes of a module.

use super::*;

fn expr_references_ident(expr: &sv::ir::Expr, name: &str) -> bool {
    match expr {
        sv::ir::Expr::Ident(ident) => ident == name,
        sv::ir::Expr::Literal(_) => false,
        sv::ir::Expr::Select { expr, .. }
        | sv::ir::Expr::Resize { expr, .. }
        | sv::ir::Expr::Unary { expr, .. } => expr_references_ident(expr, name),
        sv::ir::Expr::Concat(parts) | sv::ir::Expr::RepeatConcat { parts, .. } => {
            parts.iter().any(|part| expr_references_ident(part, name))
        }
        sv::ir::Expr::Binary { left, right, .. } => {
            expr_references_ident(left, name) || expr_references_ident(right, name)
        }
        sv::ir::Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            expr_references_ident(condition, name)
                || expr_references_ident(then_expr, name)
                || expr_references_ident(else_expr, name)
        }
        sv::ir::Expr::Inside { expr, items } => {
            expr_references_ident(expr, name)
                || items
                    .iter()
                    .flat_map(sv::ir::InsideItem::exprs)
                    .any(|operand| expr_references_ident(operand, name))
        }
        sv::ir::Expr::Call { args, .. } => args
            .iter()
            .flatten()
            .any(|arg| expr_references_ident(arg, name)),
    }
}

type SvFfBlocks = (
    HashMap<TriggerSet<SourceVarId>, ExecutionUnit<RegionedVarAddr>>,
    HashMap<TriggerSet<SourceVarId>, ExecutionUnit<RegionedVarAddr>>,
    HashMap<TriggerSet<SourceVarId>, ExecutionUnit<RegionedVarAddr>>,
    HashMap<
        TriggerSet<SourceVarId>,
        celox_frontend_core::symbolic::artifact::SplitCommit<RegionedVarAddr>,
    >,
    HashMap<SourceVarId, SourceVarId>,
    HashMap<
        TriggerSet<SourceVarId>,
        Vec<celox_frontend_core::symbolic::artifact::FfPart<RegionedVarAddr>>,
    >,
    FfEventVars,
    Vec<celox_design::NbaSite<SourceVarId>>,
);

/// The variables `always_ff` event controls are sensed on.
///
/// A variable carries one domain kind, the edge and role of its event, so a
/// signal is sensed on itself at one edge and role, and at any other on a
/// hidden copy of the signal: `negedge clk` beside `posedge clk`, or
/// `posedge rst` as a clock beside `negedge rst` as an asynchronous reset.
/// Each copy is a whole-variable continuous assignment of the signal, so it
/// changes in the same time step as the signal and is a clock derived from
/// it; only domains sensed on a copy pay for it.
///
/// The signal itself keeps the first of rising clock, falling clock, active
/// high reset, active low reset that it is sensed as, independent of the
/// order of the processes: a clock port stays in the domain of the rising
/// clock that drives it, and `Simulator::tick` of the signal is its rising
/// edge whenever a process uses that.
#[derive(Clone, Default)]
pub(crate) struct FfEventVars {
    /// Per signal, the domain kinds it is sensed at and their variables.
    sensed: HashMap<SourceVarId, Vec<(DomainKind, SourceVarId)>>,
    /// The `(copy, signal)` pairs of the hidden copies, in creation order.
    pub(super) copies: Vec<(SourceVarId, SourceVarId)>,
}

impl FfEventVars {
    /// Sense each signal of `events` on itself at its first kind in
    /// priority order.
    fn new(events: impl Iterator<Item = (SourceVarId, DomainKind)>) -> Self {
        let priority = |kind: DomainKind| match kind {
            DomainKind::ClockPosedge => 0,
            DomainKind::ClockNegedge => 1,
            DomainKind::ResetAsyncHigh => 2,
            DomainKind::ResetAsyncLow => 3,
            DomainKind::Other => 4,
        };
        let mut primary = HashMap::<SourceVarId, DomainKind>::default();
        for (signal, kind) in events {
            primary
                .entry(signal)
                .and_modify(|primary| {
                    if priority(kind) < priority(*primary) {
                        *primary = kind;
                    }
                })
                .or_insert(kind);
        }
        Self {
            sensed: primary
                .into_iter()
                .map(|(signal, kind)| (signal, vec![(kind, signal)]))
                .collect(),
            copies: Vec::new(),
        }
    }

    /// The variable on which `signal` is sensed as `kind`.
    fn sensed(
        &mut self,
        pm: &mut procedural::ProcModule<'_>,
        signal: SourceVarId,
        kind: DomainKind,
    ) -> SourceVarId {
        let sensed = self.sensed.entry(signal).or_default();
        if let Some(&(_, var)) = sensed.iter().find(|(sensed_kind, _)| *sensed_kind == kind) {
            return var;
        }
        let copy = pm.event_copy(signal, kind);
        self.copies.push((copy, signal));
        sensed.push((kind, copy));
        copy
    }

    /// Every sensed variable with its domain kind.
    pub(super) fn kinds(&self) -> impl Iterator<Item = (SourceVarId, DomainKind)> + '_ {
        self.sensed
            .values()
            .flatten()
            .map(|&(kind, var)| (var, kind))
    }
}

/// A signal a flip-flop evaluation writes and, for nonblocking writes staged
/// apart from blocking ones, its staging value; see
/// `celox_frontend_core::watch::PendingSlots`.
pub(super) type FfWriteStream = (SourceVarId, Option<SourceVarId>);

/// The write streams one execution of the flip-flop evaluation `eval` may
/// write twice. `staged` lists its staged variables as (variable, value,
/// mask).
pub(super) fn repeated_ff_writes(
    eval: &ExecutionUnit<RegionedVarAddr>,
    staged: &[(SourceVarId, SourceVarId, SourceVarId)],
) -> Vec<FfWriteStream> {
    // A seed commits the stable state into the next state; only the stores
    // of the body are writes. Every nonblocking write of a staged variable
    // marks its mask, so the value alone tells a repeated write.
    celox_frontend_core::watch::multi_write_candidates(eval, |instruction| match instruction {
        SIRInstruction::Store(address, ..) if address.region == WORKING_REGION => {
            Some(address.var_id)
        }
        _ => None,
    })
    .into_iter()
    .filter_map(
        |id| match staged.iter().find(|&&(_, value, _)| value == id) {
            Some(&(signal, value, _)) => Some((signal, Some(value))),
            None if staged.iter().any(|&(_, _, mask)| mask == id) => None,
            None => Some((id, None)),
        },
    )
    .collect()
}

pub(super) fn lower_ff_processes(
    module: &sv::ir::Module,
    pm: &mut procedural::ProcModule<'_>,
    ff_parts: bool,
    ff_event_roles: &[FfEventRole],
    shared_memories: &HashSet<String>,
    foreign_writes: &HashMap<String, hierarchy::ForeignWrites>,
    shared_variables: &HashSet<String>,
    repeated_writes: &mut Vec<FfWriteStream>,
) -> Result<SvFfBlocks, sv::AnalyzerError> {
    let mut parallel_ff_parts = HashMap::<_, Vec<_>>::default();
    let mut eval_only_ff_blocks = HashMap::default();
    let mut apply_ff_blocks = HashMap::default();
    let mut active_ff_blocks = HashMap::default();
    let mut nba_ff_blocks: HashMap<_, ExecutionUnit<RegionedVarAddr>> = HashMap::default();
    let mut eval_apply_ff_blocks = HashMap::default();
    let mut reset_clock_map = HashMap::default();
    let mut all_delayed_sites: Vec<celox_design::NbaSite<SourceVarId>> = Vec::new();

    // Every event of every process, as (signal, domain kind): the clock, then
    // the resets.
    let mut process_events = Vec::new();
    for process in module.ff_processes() {
        let clock = clock_event_from_ff_process(process)
            .ok_or_else(|| sv::AnalyzerError::Unsupported("always_ff event control".to_string()))?;
        let event_id = |event: &sv::ir::FfEvent| {
            let id = pm.id(event.signal()).ok_or_else(|| {
                sv::AnalyzerError::Unsupported("always_ff event control".to_string())
            })?;
            if pm
                .variables
                .get(&id)
                .is_some_and(|variable| variable.width != 1)
            {
                return Err(sv::AnalyzerError::Unsupported(
                    "multi-bit always_ff event signal".to_string(),
                ));
            }
            Ok(id)
        };
        let clock_kind = match clock.edge() {
            sv::ir::FfEdge::Pos => DomainKind::ClockPosedge,
            sv::ir::FfEdge::Neg => DomainKind::ClockNegedge,
        };
        let mut events = vec![(event_id(clock)?, clock_kind)];
        for reset in process
            .events()
            .iter()
            .filter(|event| event.signal() != clock.signal())
        {
            let reset_kind = match reset.edge() {
                sv::ir::FfEdge::Pos => DomainKind::ResetAsyncHigh,
                sv::ir::FfEdge::Neg => DomainKind::ResetAsyncLow,
            };
            events.push((event_id(reset)?, reset_kind));
        }
        process_events.push(events);
    }
    // Select primary roles globally before creating copies for retained FFs.
    // Scheduled processes still contribute clock/reset roles and must satisfy
    // the same one-bit event restriction, even though they use wait sites.
    let global_events = ff_event_roles
        .iter()
        .map(|role| {
            let id = pm
                .id(&role.signal)
                .ok_or_else(|| sv::AnalyzerError::Unsupported("always_ff event control".into()))?;
            if pm
                .variables
                .get(&id)
                .is_some_and(|variable| variable.width != 1)
            {
                return Err(sv::AnalyzerError::Unsupported(
                    "multi-bit always_ff event signal".into(),
                ));
            }
            Ok((id, role.kind))
        })
        .collect::<Result<Vec<_>, sv::AnalyzerError>>()?;
    // A hierarchical name denotes another instance's object, whose own
    // declaration decides how it is sensed there: sense it on copies here.
    let hierarchical: HashSet<SourceVarId> = module
        .signals()
        .iter()
        .filter(|signal| signal.hierarchical().is_some())
        .filter_map(|signal| pm.id(signal.name()))
        .collect();
    let mut event_vars = FfEventVars::new(
        global_events
            .into_iter()
            .filter(|(id, _)| !hierarchical.contains(id)),
    );
    // Reflection must retain associations from scheduled host processes too.
    // Resolve sensed roles just as retained FF trigger sets do, including aliases
    // when one authored signal has more than one clock/reset role.
    for role in ff_event_roles {
        if matches!(
            role.kind,
            DomainKind::ResetAsyncHigh | DomainKind::ResetAsyncLow
        ) {
            let reset = pm.id(&role.signal).expect("validated FF event signal");
            let clock = pm.id(&role.clock).expect("validated FF clock signal");
            let reset = event_vars.sensed(pm, reset, role.kind);
            let clock = event_vars.sensed(pm, clock, role.clock_kind);
            reset_clock_map.entry(reset).or_insert(clock);
        }
    }

    let edge_memories =
        edge_only_memories(module, pm.constants, pm.parameter_types, foreign_writes);
    let window_writes = kernel_window_writes(module, foreign_writes);
    let random_seeds = process_random_seeds(module);
    for ((index, process), events) in module.ff_processes().iter().enumerate().zip(process_events) {
        pm.call_unit();
        let mut events = events
            .into_iter()
            .map(|(signal, kind)| event_vars.sensed(pm, signal, kind));
        let mut trigger_set = TriggerSet {
            clock: events.next().expect("a process has a clock event"),
            resets: Vec::new(),
        };
        trigger_set.resets.extend(events);
        // A reset shared by several clock domains is associated with the
        // first; the association only names a clock for the reset signal.
        for reset in &trigger_set.resets {
            reset_clock_map.entry(*reset).or_insert(trigger_set.clock);
        }
        let sites_before = pm.runtime.event_sites.len();
        let extern_calls_before = pm.externs.calls;
        let clock = inferred_clock(pm, process);
        let ff::FfProcessUnits {
            eval: eval_only,
            apply,
            active,
            nba,
            targets,
            staged,
            delayed_sites,
        } = ff::Ff::new(pm, random_seeds[&(0, index)])
            .with_sparse_memories(shared_memories)
            .with_shared_variables(shared_variables)
            .with_sparse_candidates(&edge_memories)
            .with_window_writes(&window_writes)
            .with_inferred_clock(clock)
            .lower_process(process.body())?;
        all_delayed_sites.extend(delayed_sites);
        repeated_writes.extend(repeated_ff_writes(&eval_only, &staged));
        // A process without writes still runs for its runtime events and
        // extern calls.
        let has_effects =
            pm.runtime.event_sites.len() > sites_before || pm.externs.calls > extern_calls_before;
        if trigger_set.resets.is_empty() && targets.is_empty() && !has_effects {
            continue;
        }
        if ff_parts {
            parallel_ff_parts
                .entry(trigger_set.clone())
                .or_default()
                .push(celox_frontend_core::symbolic::artifact::FfPart {
                    evaluate: eval_only.clone(),
                    apply: apply.clone(),
                });
        }
        insert_or_merge_ff_unit(&mut eval_only_ff_blocks, trigger_set.clone(), eval_only);
        insert_or_merge_ff_unit(&mut apply_ff_blocks, trigger_set.clone(), apply);
        insert_or_merge_ff_unit(&mut active_ff_blocks, trigger_set.clone(), active);
        insert_or_merge_ff_unit(&mut nba_ff_blocks, trigger_set, nba);
    }
    // Every process of a trigger evaluates against the pre-edge state before
    // any of them commits (IEEE 1800-2023 10.4.2), so the combined unit runs
    // all evaluations first and all commits afterwards.
    for (trigger_set, eval_only) in &eval_only_ff_blocks {
        let apply = &apply_ff_blocks[trigger_set];
        let mut eval_apply = merge_sir_eus(&[eval_only.clone(), apply.clone()]).0;
        ff::prune_unreachable_blocks(&mut eval_apply);
        eval_apply_ff_blocks.insert(trigger_set.clone(), eval_apply);
    }
    // One process is the whole trigger group.
    parallel_ff_parts.retain(|_, parts| parts.len() > 1);

    let split_commit_ff_blocks = active_ff_blocks
        .into_iter()
        .map(|(trigger_set, active)| {
            let nba = nba_ff_blocks
                .remove(&trigger_set)
                .expect("every trigger group has both commits");
            (
                trigger_set,
                celox_frontend_core::symbolic::artifact::SplitCommit { active, nba },
            )
        })
        .collect();
    Ok((
        eval_only_ff_blocks,
        apply_ff_blocks,
        eval_apply_ff_blocks,
        split_commit_ff_blocks,
        reset_clock_map,
        parallel_ff_parts,
        event_vars,
        all_delayed_sites,
    ))
}

fn insert_or_merge_ff_unit(
    blocks: &mut HashMap<TriggerSet<SourceVarId>, ExecutionUnit<RegionedVarAddr>>,
    trigger_set: TriggerSet<SourceVarId>,
    unit: ExecutionUnit<RegionedVarAddr>,
) {
    if let Some(existing) = blocks.remove(&trigger_set) {
        let mut merged = merge_sir_eus(&[existing, unit]).0;
        ff::prune_unreachable_blocks(&mut merged);
        blocks.insert(trigger_set, merged);
    } else {
        blocks.insert(trigger_set, unit);
    }
}

/// A plain `always` sensitive to edges is a flip-flop only in the shape of
/// one: a clock and asynchronous resets, each a one-bit variable. Any other
/// edge-sensitive `always` runs as a process waiting on the same events, and
/// so does an edge process of either keyword whose delayed nonblocking
/// assignments the flip-flop form cannot stage.
pub(super) fn runs_as_process(
    module: &sv::ir::Module,
    process: &sv::ir::FfProcess,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> bool {
    let multi_bit = |name: &str| {
        let r#type = module
            .ports()
            .iter()
            .find(|port| port.name() == name)
            .map(sv::ir::Port::r#type)
            .or_else(|| {
                module
                    .signals()
                    .iter()
                    .find(|signal| signal.name() == name)
                    .map(sv::ir::Signal::r#type)
            });
        r#type
            .and_then(|r#type| signal_type_from_sv(r#type, constants, parameter_types).ok())
            .is_some_and(|type_info| type_info.width != 1)
    };
    // A blocking write through a hierarchical name updates the other
    // instance's object at once, outside that instance's flip-flop commit.
    let writes_other_instance = || {
        let mut hierarchical = module
            .signals()
            .iter()
            .filter(|signal| signal.hierarchical().is_some())
            .peekable();
        if hierarchical.peek().is_none() {
            return false;
        }
        let mut written = HashSet::default();
        procedural::written_names_with_calls(module, process.body(), &mut written);
        hierarchical.any(|signal| written.contains(signal.name()))
    };
    ineligible_delayed_nonblocking(module, process)
        || (!process.is_always_ff()
            && (clock_event_from_ff_process(process).is_none()
                || process
                    .events()
                    .iter()
                    .any(|event| multi_bit(event.signal()))
                || writes_other_instance()))
}

/// Whether edge procedure `process` sets the assertion control state with
/// `$assertoff` or `$asserton`, also through a task it calls. The tasks set
/// it as they execute (IEEE 1800-2023 20.11), and other procedures, also of
/// the same edge, may set it too: it is no register of the procedure, so
/// the procedure runs as a process waiting on its events. Only modules that
/// have the state (`assertion_control`) are asked.
pub(super) fn controls_assertions(module: &sv::ir::Module, process: &sv::ir::FfProcess) -> bool {
    let mut written = HashSet::default();
    procedural::written_names_with_calls(module, process.body(), &mut written);
    written.contains(sv::ASSERTIONS_OFF)
}

/// Whether plain `always` edge procedure `process` writes a variable that
/// repeating processes of other modules write through hierarchical names
/// (`foreign_writes`), other than by nonblocking assignments to a memory.
/// Those update the selected elements of the shared memory (see
/// `shared_edge_memories`); any other write must store into the variable
/// as it executes, since the last write determines the value (IEEE
/// 1800-2023 6.5), so the procedure runs as a process waiting on its events.
pub(super) fn shares_foreign_variable(
    module: &sv::ir::Module,
    process: &sv::ir::FfProcess,
    foreign_writes: &HashMap<String, hierarchy::ForeignWrites>,
) -> bool {
    if process.is_always_ff() || foreign_writes.values().all(|writes| writes.repeating == 0) {
        return false;
    }
    let mut written = HashSet::default();
    procedural::written_names_with_calls(module, process.body(), &mut written);
    written.retain(|name| {
        foreign_writes
            .get(name)
            .is_some_and(|writes| writes.repeating > 0)
    });
    if written.is_empty() {
        return false;
    }
    // The names the body assigns directly, by the kind of assignment.
    let mut blocking = HashSet::default();
    let mut nonblocking = HashSet::default();
    for stmt in process.body() {
        stmt.walk(&mut |stmt| match stmt {
            sv::ir::Stmt::Assign {
                lhs,
                nonblocking: queued,
                ..
            } => {
                if *queued {
                    nonblocking.insert(lhs.name());
                } else {
                    blocking.insert(lhs.name());
                }
            }
            sv::ir::Stmt::AssignConcat {
                parts,
                nonblocking: queued,
                ..
            } => {
                let names = parts.iter().map(sv::ir::LValue::name);
                if *queued {
                    nonblocking.extend(names);
                } else {
                    blocking.extend(names);
                }
            }
            _ => {}
        });
    }
    written.iter().any(|name| {
        let memory = module
            .signals()
            .iter()
            .any(|signal| signal.name() == name && !signal.r#type().unpacked_ranges().is_empty());
        // A write that is no assignment of the body, such as one of a
        // called task, is taken as a blocking one.
        !memory || blocking.contains(name.as_str()) || !nonblocking.contains(name.as_str())
    })
}

/// The `always @*` processes, by index among the combinational ones, that
/// write bits of a variable an edge-sensitive `always` (or an ordinary
/// `always` run as a process) writes too. An `always @*` is an ordinary
/// `always` (IEEE 1800-2023 9.2.2) that may share a variable with other
/// procedures, the last write determining its value (6.5), so it runs as a
/// process waiting on its implicit event list (9.4.2.2), as one sharing a
/// variable with another `always @*` does (see `share_always_variables`);
/// `edge_processes_sharing_variables` then lets the edge procedure share it.
pub(super) fn always_star_processes_sharing_edge_variables(
    module: &sv::ir::Module,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> HashSet<usize> {
    let comb = module.comb_processes();
    let ff = module.ff_processes();
    let mut sharing = HashSet::default();
    let always_star = |driver: usize| {
        comb.get(driver)
            .is_some_and(|process| process.kind() == sv::ir::CombProcessKind::AlwaysStar)
    };
    if !(0..comb.len()).any(always_star) {
        return sharing;
    }
    let edge = comb.len()..comb.len() + ff.len();
    // An edge-sensitive `always`, or an ordinary `always` run as a process.
    let ordinary_edge = |driver: usize| {
        if edge.contains(&driver) {
            !ff[driver - edge.start].is_always_ff()
        } else {
            drivers::shares_variables(module, driver)
        }
    };
    let drivers = ModuleDrivers::new(module, constants, parameter_types);
    for writes in drivers.writes.values() {
        let writers = writes
            .iter()
            .filter_map(|write| Some((write.local?, write.range.or(write.element))))
            .collect::<Vec<_>>();
        for &(left, left_range) in &writers {
            if !always_star(left) {
                continue;
            }
            if writers.iter().any(|&(right, right_range)| {
                right != left
                    && ordinary_edge(right)
                    && net_driver_ranges_overlap(left_range, right_range)
            }) {
                sharing.insert(left);
            }
        }
    }
    sharing
}

/// The edge processes, by index, that are an edge-sensitive `always`
/// writing bits of a variable another ordinary `always` of the module
/// writes too, where the flip-flop form cannot share it. Several such
/// procedures may write one variable, the last write determining its value
/// (IEEE 1800-2023 6.5); only an `always_ff`, an `always_comb` or an
/// `always_latch` must be the only writer (9.2.2). The flip-flop form shares
/// a variable that edge procedures write only by blocking assignments (see
/// `shared_edge_variables`) and a memory that only edge procedures write
/// (see `shared_edge_memories`); for any other, such as one a procedure
/// writes by nonblocking assignments and another by blocking ones, each
/// write must store into the variable as it executes, so the procedure runs
/// as a process waiting on its events, as one writing a variable of another
/// instance does (see `shares_foreign_variable`).
pub(super) fn edge_processes_sharing_variables(
    module: &sv::ir::Module,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> HashSet<usize> {
    let ff = module.ff_processes();
    let mut sharing = HashSet::default();
    if ff.iter().all(sv::ir::FfProcess::is_always_ff) {
        return sharing;
    }
    let edge = module.comb_processes().len()..module.comb_processes().len() + ff.len();
    // An edge-sensitive `always`, or an ordinary `always` run as a process.
    let ordinary = |driver: usize| {
        if edge.contains(&driver) {
            !ff[driver - edge.start].is_always_ff()
        } else {
            drivers::shares_variables(module, driver)
        }
    };
    let drivers = ModuleDrivers::new(module, constants, parameter_types);
    let flip_flop_shared = shared_edge_variables(module, &drivers);
    let memories = module
        .signals()
        .iter()
        .filter(|signal| !signal.r#type().unpacked_ranges().is_empty())
        .map(sv::ir::Signal::name)
        .collect::<HashSet<_>>();
    for (name, writes) in &drivers.writes {
        let mut writers = writes
            .iter()
            .filter_map(|write| Some((write.local?, write.range.or(write.element))))
            .filter(|(driver, _)| ordinary(*driver))
            .collect::<Vec<_>>();
        writers.sort_unstable();
        writers.dedup();
        let only_edges = writers.iter().all(|(driver, _)| edge.contains(driver));
        if writers.iter().all(|(driver, _)| *driver == writers[0].0)
            || (only_edges && (flip_flop_shared.contains(name) || memories.contains(name.as_str())))
        {
            continue;
        }
        for (index, &(left, left_range)) in writers.iter().enumerate() {
            for &(right, right_range) in &writers[index + 1..] {
                if left != right && net_driver_ranges_overlap(left_range, right_range) {
                    sharing.extend(
                        [left, right]
                            .into_iter()
                            .filter(|driver| edge.contains(driver))
                            .map(|driver| driver - edge.start),
                    );
                }
            }
        }
    }
    sharing
}

/// Whether a flip-flop's delayed nonblocking assignments cannot take the
/// fused staging path: an event control or nonconstant delay, a partial or
/// array target, a target one run of the body may assign more than once,
/// or a target another process or assignment also writes. Those keep the
/// process-kernel path, whose queued updates preserve the exact timing and
/// order (IEEE 1800-2023 9.4.5, 10.4.2).
fn ineligible_delayed_nonblocking(module: &sv::ir::Module, process: &sv::ir::FfProcess) -> bool {
    let mut targets: Vec<String> = Vec::new();
    let mut ineligible = false;
    for stmt in process.body() {
        stmt.walk(&mut |stmt| {
            let sv::ir::Stmt::DelayedNonblocking {
                control,
                assignment,
            } = stmt
            else {
                return;
            };
            let sv::ir::Stmt::Delay { amount, .. } = control.as_ref() else {
                ineligible = true;
                return;
            };
            if !matches!(amount, sv::ir::Expr::Literal(_) | sv::ir::Expr::Ident(_)) {
                ineligible = true;
                return;
            }
            match assignment.as_ref() {
                sv::ir::Stmt::Assign {
                    lhs: sv::ir::LValue::Ident(name),
                    nonblocking: true,
                    ..
                } => targets.push(name.clone()),
                _ => ineligible = true,
            }
        });
    }
    if ineligible {
        return true;
    }
    if targets.is_empty() {
        return false;
    }
    // A stage holds one value per run. Two delayed assignments of one run
    // are two updates, each an event of its own (10.4.2), which only the
    // queue keeps apart.
    if delayed_targets(process.body(), &mut HashSet::default()) {
        return true;
    }
    // A target of an unpacked array keeps the kernel path: the fused
    // staging publishes whole variables only.
    let array_target = targets.iter().any(|name| {
        module
            .ports()
            .iter()
            .find(|port| port.name() == name)
            .map(sv::ir::Port::r#type)
            .or_else(|| {
                module
                    .signals()
                    .iter()
                    .find(|signal| signal.name() == name)
                    .map(sv::ir::Signal::r#type)
            })
            .is_some_and(|ty| !ty.unpacked_ranges().is_empty())
    });
    if array_target {
        return true;
    }
    // A target another process or a continuous assignment writes keeps the
    // kernel path: the fused commit publishes its variable on its own.
    let mut written = HashSet::default();
    for other in module.ff_processes() {
        if !std::ptr::eq(other, process) {
            procedural::written_names_with_calls(module, other.body(), &mut written);
        }
    }
    for other in module.comb_processes() {
        procedural::written_names_with_calls(module, other.body(), &mut written);
        for assignment in other.assignments() {
            written.insert(assignment.lhs().to_string());
        }
    }
    for other in module.initial_processes() {
        procedural::written_names_with_calls(module, other.body(), &mut written);
    }
    for assignment in module.assignments() {
        written.insert(assignment.lhs().to_string());
    }
    targets.iter().any(|target| written.contains(target))
}

/// Collect into `targets` the variables the delayed nonblocking assignments
/// of `body` may write in one run, and report whether one run may write one
/// of them more than once: two assignments that are not in alternative
/// branches, or one in a loop.
fn delayed_targets<'a>(body: &'a [sv::ir::Stmt], targets: &mut HashSet<&'a str>) -> bool {
    let mut repeated = false;
    for stmt in body {
        let mut own = HashSet::default();
        match stmt {
            sv::ir::Stmt::DelayedNonblocking { assignment, .. } => {
                if let sv::ir::Stmt::Assign { lhs, .. } = assignment.as_ref() {
                    own.insert(lhs.name());
                }
            }
            sv::ir::Stmt::If {
                then_body,
                else_body,
                ..
            } => {
                repeated |= delayed_targets(then_body, &mut own);
                let mut other = HashSet::default();
                repeated |= delayed_targets(else_body, &mut other);
                own.extend(other);
            }
            sv::ir::Stmt::Case { items, default, .. } => {
                let bodies = items
                    .iter()
                    .map(|item| item.body.as_slice())
                    .chain(default.as_deref());
                for body in bodies {
                    let mut alternative = HashSet::default();
                    repeated |= delayed_targets(body, &mut alternative);
                    own.extend(alternative);
                }
            }
            sv::ir::Stmt::Loop {
                init, step, body, ..
            } => {
                repeated |= delayed_targets(init, &mut own);
                let mut iteration = HashSet::default();
                delayed_targets(body, &mut iteration);
                delayed_targets(step, &mut iteration);
                repeated |= !iteration.is_empty();
                own.extend(iteration);
            }
            sv::ir::Stmt::Fork { branches, .. } => {
                for branch in branches {
                    repeated |= delayed_targets(branch, &mut own);
                }
            }
            _ => {}
        }
        repeated |= own.iter().any(|target| targets.contains(target));
        targets.extend(own);
    }
    repeated
}

pub(super) fn clock_event_from_ff_process(process: &sv::ir::FfProcess) -> Option<&sv::ir::FfEvent> {
    if let [clock] = process.events() {
        return Some(clock);
    }

    // Event OR has no ordering (IEEE 1800-2023 9.4.2.1). For this
    // optimized FF path, resets are the events used as conditions in the
    // body; require exactly one remaining clock candidate.
    let mut clocks = process
        .events()
        .iter()
        .filter(|event| !ff_event_used_as_condition(process, event));
    let clock = clocks.next()?;
    clocks.next().is_none().then_some(clock)
}

fn ff_event_used_as_condition(process: &sv::ir::FfProcess, event: &sv::ir::FfEvent) -> bool {
    let mut used = false;
    for stmt in process.body() {
        stmt.walk(&mut |stmt| match stmt {
            sv::ir::Stmt::If { condition, .. } => {
                used |= expr_references_ident(condition, event.signal());
            }
            sv::ir::Stmt::Case { selector, .. } => {
                used |= expr_references_ident(selector, event.signal());
            }
            sv::ir::Stmt::Assign { rhs, .. } | sv::ir::Stmt::AssignConcat { rhs, .. } => {
                used |= expr_uses_ident_as_condition(rhs, event.signal());
            }
            _ => {}
        });
    }
    used
}

fn expr_uses_ident_as_condition(expr: &sv::ir::Expr, name: &str) -> bool {
    match expr {
        sv::ir::Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            expr_references_ident(condition, name)
                || expr_uses_ident_as_condition(then_expr, name)
                || expr_uses_ident_as_condition(else_expr, name)
        }
        sv::ir::Expr::Select { expr, .. }
        | sv::ir::Expr::Resize { expr, .. }
        | sv::ir::Expr::Unary { expr, .. } => expr_uses_ident_as_condition(expr, name),
        sv::ir::Expr::Concat(parts) | sv::ir::Expr::RepeatConcat { parts, .. } => parts
            .iter()
            .any(|part| expr_uses_ident_as_condition(part, name)),
        sv::ir::Expr::Binary { left, right, .. } => {
            expr_uses_ident_as_condition(left, name) || expr_uses_ident_as_condition(right, name)
        }
        sv::ir::Expr::Inside { expr, items } => {
            expr_uses_ident_as_condition(expr, name)
                || items
                    .iter()
                    .flat_map(sv::ir::InsideItem::exprs)
                    .any(|operand| expr_uses_ident_as_condition(operand, name))
        }
        sv::ir::Expr::Call { args, .. } => args
            .iter()
            .flatten()
            .any(|arg| expr_uses_ident_as_condition(arg, name)),
        sv::ir::Expr::Ident(_) | sv::ir::Expr::Literal(_) => false,
    }
}

pub(super) fn seal_builder(
    mut builder: SIRBuilder<RegionedVarAddr>,
) -> ExecutionUnit<RegionedVarAddr> {
    builder.seal_block(SIRTerminator::Return);
    let (blocks, register_map, _) = builder.drain();
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    }
}

pub(super) fn emit_ff_seeds(
    builder: &mut SIRBuilder<RegionedVarAddr>,
    targets: &[VarAtomBase<SourceVarId>],
) {
    for target in targets {
        builder.emit(SIRInstruction::Commit(
            RegionedVarAddrBase {
                region: STABLE_REGION,
                var_id: target.id,
            },
            RegionedVarAddrBase {
                region: WORKING_REGION,
                var_id: target.id,
            },
            SIROffset::Static(target.access.lsb),
            target.access.msb - target.access.lsb + 1,
            Vec::new(),
        ));
    }
}

/// Publish the next state of `targets`. A target a procedural force
/// controls takes the forced value while it is forced, also when the force
/// came after the process ran (IEEE 1800-2023 10.6.2).
pub(super) fn emit_ff_commits(
    builder: &mut SIRBuilder<RegionedVarAddr>,
    targets: &[VarAtomBase<SourceVarId>],
    pm: &procedural::ProcModule<'_>,
) {
    for target in targets {
        if let Some((enabled, forced)) = pm.force_controls(target.id)
            && pm.procedurally_forced.contains(&target.id)
        {
            let offset = SIROffset::Static(target.access.lsb);
            let width = target.access.msb - target.access.lsb + 1;
            let register = |builder: &mut SIRBuilder<RegionedVarAddr>| {
                if pm.var(target.id).is_4state {
                    builder.alloc_logic(width)
                } else {
                    builder.alloc_bit(width, false)
                }
            };
            let next = register(builder);
            builder.emit(SIRInstruction::Load(
                next,
                RegionedVarAddrBase {
                    region: WORKING_REGION,
                    var_id: target.id,
                },
                offset.clone(),
                width,
            ));
            let hold = pm.force_holds.get(&target.id).copied();
            let selected = force_select(builder, (enabled, forced), hold, offset.clone(), next);
            builder.emit(SIRInstruction::Store(
                RegionedVarAddrBase {
                    region: STABLE_REGION,
                    var_id: target.id,
                },
                offset,
                width,
                selected,
                Vec::new(),
                Vec::new(),
            ));
            continue;
        }
        builder.emit(SIRInstruction::Commit(
            RegionedVarAddrBase {
                region: WORKING_REGION,
                var_id: target.id,
            },
            RegionedVarAddrBase {
                region: STABLE_REGION,
                var_id: target.id,
            },
            SIROffset::Static(target.access.lsb),
            target.access.msb - target.access.lsb + 1,
            Vec::new(),
        ));
    }
}

/// `value`, the bits at `offset` of a variable, with the bits a
/// procedural force `controls` (enable mask, value) forces, or its `hold`
/// mask holds, taken from the forced value (IEEE 1800-2023 10.6.2).
pub(super) fn force_select(
    builder: &mut SIRBuilder<RegionedVarAddr>,
    controls: (SourceVarId, SourceVarId),
    hold: Option<SourceVarId>,
    offset: SIROffset,
    value: celox_sir::RegisterId,
) -> celox_sir::RegisterId {
    let (enabled, forced) = controls;
    let width = builder.register(&value).width();
    let stable = |var_id| RegionedVarAddrBase {
        region: STABLE_REGION,
        var_id,
    };
    let mut mask = builder.alloc_bit(width, false);
    builder.emit(SIRInstruction::Load(
        mask,
        stable(enabled),
        offset.clone(),
        width,
    ));
    if let Some(hold) = hold {
        let held = builder.alloc_bit(width, false);
        builder.emit(SIRInstruction::Load(
            held,
            stable(hold),
            offset.clone(),
            width,
        ));
        let either = builder.alloc_bit(width, false);
        builder.emit(SIRInstruction::Binary(either, mask, BinaryOp::Or, held));
        mask = either;
    }
    let replacement = builder.alloc_reg(builder.register(&value).clone());
    builder.emit(SIRInstruction::Load(
        replacement,
        stable(forced),
        offset,
        width,
    ));
    select_masked(builder, mask, replacement, value)
}

/// The bits of `forced` where the two-state `mask` is set and of `value`
/// elsewhere. A four-state merge selects the value and the X/Z planes
/// apart, so a selected Z stays Z.
pub(super) fn select_masked(
    builder: &mut SIRBuilder<RegionedVarAddr>,
    mask: celox_sir::RegisterId,
    forced: celox_sir::RegisterId,
    value: celox_sir::RegisterId,
) -> celox_sir::RegisterId {
    let width = builder.register(&value).width();
    let merge = |builder: &mut SIRBuilder<RegionedVarAddr>, forced, value, logic: bool| {
        let alloc = |builder: &mut SIRBuilder<RegionedVarAddr>| {
            if logic {
                builder.alloc_logic(width)
            } else {
                builder.alloc_bit(width, false)
            }
        };
        let keep = builder.alloc_bit(width, false);
        builder.emit(SIRInstruction::Unary(keep, UnaryOp::BitNot, mask));
        let taken = alloc(builder);
        builder.emit(SIRInstruction::Binary(taken, forced, BinaryOp::And, mask));
        let kept = alloc(builder);
        builder.emit(SIRInstruction::Binary(kept, value, BinaryOp::And, keep));
        let merged = alloc(builder);
        builder.emit(SIRInstruction::Binary(merged, kept, BinaryOp::Or, taken));
        merged
    };
    if !matches!(
        builder.register(&value),
        celox_sir::RegisterType::Logic { .. }
    ) {
        return merge(builder, forced, value, false);
    }
    // The value and X/Z planes of each operand, as two-state registers.
    let words = width.next_multiple_of(64);
    let split = |builder: &mut SIRBuilder<RegionedVarAddr>, register| {
        let padded = builder.alloc_logic(words);
        if words > width {
            let zero = builder.alloc_logic(words - width);
            builder.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
            builder.emit(SIRInstruction::Concat(padded, vec![zero, register]));
        } else {
            builder.emit(SIRInstruction::Unary(padded, UnaryOp::Ident, register));
        }
        let planes = builder.alloc_bit(2 * words, false);
        builder.emit(SIRInstruction::Unary(planes, UnaryOp::ToPlanes, padded));
        [0, words].map(|lsb| {
            let plane = builder.alloc_bit(width, false);
            builder.emit(SIRInstruction::Slice(plane, planes, lsb, width));
            plane
        })
    };
    let [forced_value, forced_unknown] = split(builder, forced);
    let [value_value, value_unknown] = split(builder, value);
    let merged_value = merge(builder, forced_value, value_value, false);
    let merged_unknown = merge(builder, forced_unknown, value_unknown, false);
    let mut parts = Vec::new();
    for plane in [merged_unknown, merged_value] {
        if words > width {
            let zero = builder.alloc_bit(words - width, false);
            builder.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
            parts.push(zero);
        }
        parts.push(plane);
    }
    let planes = builder.alloc_bit(2 * words, false);
    builder.emit(SIRInstruction::Concat(planes, parts));
    let padded = builder.alloc_logic(words);
    builder.emit(SIRInstruction::Unary(padded, UnaryOp::FromPlanes, planes));
    let joined = builder.alloc_logic(width);
    builder.emit(SIRInstruction::Slice(joined, padded, 0, width));
    joined
}

/// The clock that the sampled value functions of an edge-triggered process
/// infer (IEEE 1800-2023 16.14.6): its event expression of the clock, when
/// it is the only one whose signal the body reads nowhere else. With an
/// asynchronous reset the body reads the reset, so the clock remains.
fn inferred_clock(
    pm: &procedural::ProcModule<'_>,
    process: &sv::ir::FfProcess,
) -> Option<ff::Clock> {
    let clock = clock_event_from_ff_process(process)?;
    let reads: HashSet<SourceVarId> = pm
        .implicit_reads(process.body(), true)
        .into_iter()
        .map(|atom| atom.id)
        .collect();
    let unread: Vec<_> = process
        .events()
        .iter()
        .filter(|event| pm.id(event.signal()).is_none_or(|id| !reads.contains(&id)))
        .collect();
    match unread.as_slice() {
        [only] if only.signal() == clock.signal() && only.edge() == clock.edge() => {
            Some(ff::Clock {
                signal: clock.signal().to_owned(),
                rising: clock.edge() == sv::ir::FfEdge::Pos,
                alone: process.events().len() == 1,
            })
        }
        _ => None,
    }
}
