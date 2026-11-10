//! Event-domain ordering, cascades and derived clocks of a flattened design.

use super::*;

/// Event-domain ordering and cascade structure of a flattened design.
pub(super) struct ClockAnalysis {
    pub(super) topological_clocks: Vec<AbsoluteAddr>,
    pub(super) cascaded_clocks: BTreeSet<AbsoluteAddr>,
    /// Each event to the derived domains an occurrence of it can trigger,
    /// transitively, in `topological_clocks` order.
    pub(super) derived_events: BTreeMap<AbsoluteAddr, Vec<AbsoluteAddr>>,
    /// Each event to the clocks computed combinationally from its signal.
    pub(super) combinational_derived: BTreeMap<AbsoluteAddr, Vec<CombDerivedClock<AbsoluteAddr>>>,
    /// Events whose combinationally derived domains may run before them in
    /// turn instead of together.
    pub(super) independent_derived: BTreeSet<AbsoluteAddr>,
    /// Events whose signal combinational logic reads.
    pub(super) combinationally_read: BTreeSet<AbsoluteAddr>,
    /// Events whose own domain does not read their signal.
    pub(super) level_unread_by_domain: BTreeSet<AbsoluteAddr>,
}

pub(super) fn analyze_clock_dependencies(
    bindings: &PackageBindings,
    eval_apply_ffs: &mut HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    eval_only_ffs: &mut HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    apply_ffs: &mut HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    comb_blocks: &[LogicPath<AbsoluteAddr>],
    arena: &SLTNodeArena<AbsoluteAddr>,
    clock_domains: &HashMap<AbsoluteAddr, AbsoluteAddr>,
    expanded: &HashMap<InstancePath, InstanceId>,
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
) -> ClockAnalysis {
    // Build static clock dependency graph & Topo Sort
    let mut clock_deps: BTreeMap<AbsoluteAddr, BTreeSet<AbsoluteAddr>> = BTreeMap::new();
    let mut unique_clocks: BTreeSet<AbsoluteAddr> = BTreeSet::new();

    // 1. Identify all variables written by FFs (direct sequential outputs)
    let mut ff_outputs: BTreeSet<AbsoluteAddr> = BTreeSet::new();
    // Each FF output to the event domains that store it.
    let mut ff_writers: BTreeMap<AbsoluteAddr, BTreeSet<AbsoluteAddr>> = BTreeMap::new();
    unique_clocks.extend(eval_apply_ffs.keys().copied());

    // Include event-typed signals even when no FF is directly driven by them.
    // A testbench clock may only feed a combinationally gated clock, in which
    // case it would otherwise be absent from the dependency graph entirely.
    for id in expanded.values() {
        let module_id = &instance_modules[id];
        let sim_module = &modules[module_id];
        for (var_id, var) in &sim_module.variables {
            let kind = var.metadata.kind;
            if !matches!(
                kind,
                DomainKind::ClockPosedge
                    | DomainKind::ClockNegedge
                    | DomainKind::ResetAsyncHigh
                    | DomainKind::ResetAsyncLow
            ) {
                continue;
            }
            let addr = bindings.locate(*id, *var_id);
            let canonical = clock_domains.get(&addr).copied().unwrap_or(addr);
            unique_clocks.insert(canonical);
            eval_apply_ffs.entry(canonical).or_default();
            eval_only_ffs.entry(canonical).or_default();
            apply_ffs.entry(canonical).or_default();
        }
    }

    for (domain_clock, eus) in &*eval_apply_ffs {
        for eu in eus {
            for bb in eu.blocks.values() {
                // The domain's stores, and what its calls write.
                let targets = bb
                    .instructions
                    .iter()
                    .filter(|inst| {
                        matches!(
                            inst,
                            SIRInstruction::Store(..) | SIRInstruction::Call { .. }
                        )
                    })
                    .flat_map(super::super::written_objects);
                for target_addr in targets {
                    // Direct sequential dependency: the target is driven by this clock
                    let abs = target_addr.absolute_addr();
                    let canonical_target = clock_domains.get(&abs).copied().unwrap_or(abs);

                    ff_outputs.insert(abs);
                    ff_writers.entry(abs).or_default().insert(*domain_clock);

                    if canonical_target != *domain_clock
                        && unique_clocks.contains(&canonical_target)
                    {
                        clock_deps
                            .entry(canonical_target)
                            .or_default()
                            .insert(*domain_clock);
                    }
                }
            }
        }
    }

    // 2. Build combinational dependency graph (target -> sources)
    let acd_timing = tracing::enabled!(tracing::Level::DEBUG);
    let acd_start = acd_timing.then(std::time::Instant::now);
    let mut comb_deps: BTreeMap<AbsoluteAddr, BTreeSet<AbsoluteAddr>> = BTreeMap::new();
    for path in comb_blocks {
        let Some(target) = path.target.var() else {
            continue;
        };
        let target_abs = target.id;
        let mut sources = HashSet::default();
        if matches!(path.target, celox_slt::LogicPathTarget::Process { .. }) {
            sources.extend(path.sources.iter().cloned());
        } else {
            crate::flattening::collect_inputs(path.expr, arena, &mut sources);
        }
        for source in sources {
            comb_deps.entry(target_abs).or_default().insert(source.id);
        }
    }
    if let Some(s) = acd_start {
        tracing::debug!(
            "[acd] comb_deps build ({} blocks): {:?}",
            comb_deps.len(),
            s.elapsed()
        );
    }

    // Record clock-to-clock dependencies through combinational logic.  FF
    // propagation below finds divided clocks, but a plain gated clock such as
    // `gated_clk = clk & enable` has no FF source and needs this separate walk.
    fn collect_upstream_clocks(
        node: AbsoluteAddr,
        target_clock: AbsoluteAddr,
        comb_deps: &BTreeMap<AbsoluteAddr, BTreeSet<AbsoluteAddr>>,
        clock_domains: &HashMap<AbsoluteAddr, AbsoluteAddr>,
        clocks: &BTreeSet<AbsoluteAddr>,
        visited: &mut BTreeSet<AbsoluteAddr>,
        found: &mut BTreeSet<AbsoluteAddr>,
    ) {
        if !visited.insert(node) {
            return;
        }
        let Some(sources) = comb_deps.get(&node) else {
            return;
        };
        for source in sources {
            let canonical = clock_domains.get(source).copied().unwrap_or(*source);
            if canonical != target_clock && clocks.contains(&canonical) {
                found.insert(canonical);
            }
            collect_upstream_clocks(
                *source,
                target_clock,
                comb_deps,
                clock_domains,
                clocks,
                visited,
                found,
            );
        }
    }

    // Which derived domains each event can trigger directly: a clock computed
    // from the event's signal, or from state the event's domain writes.
    let mut trigger_edges: BTreeMap<AbsoluteAddr, BTreeSet<AbsoluteAddr>> = BTreeMap::new();
    let mut comb_derived_clocks: BTreeMap<AbsoluteAddr, BTreeSet<AbsoluteAddr>> = BTreeMap::new();
    let combinational_inputs = clock_cone_inputs(comb_blocks, arena, &unique_clocks);
    for target_clock in &unique_clocks {
        let mut sources = BTreeSet::new();
        let mut upstream = BTreeSet::new();
        collect_upstream_clocks(
            *target_clock,
            *target_clock,
            &comb_deps,
            clock_domains,
            &unique_clocks,
            &mut upstream,
            &mut sources,
        );
        for source in &sources {
            trigger_edges
                .entry(*source)
                .or_default()
                .insert(*target_clock);
            comb_derived_clocks
                .entry(*source)
                .or_default()
                .insert(*target_clock);
        }
        for node in &upstream {
            for writer in ff_writers.get(node).into_iter().flatten() {
                if writer != target_clock {
                    trigger_edges
                        .entry(*writer)
                        .or_default()
                        .insert(*target_clock);
                }
            }
        }
        if !sources.is_empty() {
            clock_deps.entry(*target_clock).or_default().extend(sources);
        }
    }

    // 3. Propagate FF outputs through combinational graph to find all derived variables
    let fp_start = acd_timing.then(std::time::Instant::now);
    let mut derived_from_ff: BTreeSet<AbsoluteAddr> = ff_outputs.clone();
    let mut changed = true;
    let mut fp_rounds = 0u32;
    while changed {
        changed = false;
        fp_rounds += 1;
        for (target, sources) in &comb_deps {
            if !derived_from_ff.contains(target) {
                // If any source is derived from an FF, the target is too
                if sources.iter().any(|s| derived_from_ff.contains(s)) {
                    derived_from_ff.insert(*target);
                    changed = true;
                }
            }
        }
    }
    if let Some(s) = fp_start {
        tracing::debug!(
            "[acd] fixpoint: {fp_rounds} rounds, {} entries, {:?}",
            comb_deps.len(),
            s.elapsed()
        );
    }

    // 4. Any clock domain that is derived from an FF is a cascaded clock!
    // We add them to a special "pseudo-domain" or just add themselves to trigger cascade marking.
    for clk in &unique_clocks {
        if derived_from_ff.contains(clk) {
            // Self-dependency ensures it appears in `clock_deps.keys()`
            clock_deps.entry(*clk).or_default().insert(*clk);
        }
    }

    // Topologically sort the clock domains
    // Sources (no dependencies) should be evaluated first.
    let mut topological_clocks = Vec::new();
    let mut visited = BTreeSet::new();
    let mut temp_visited = BTreeSet::new();

    fn topo_visit(
        node: AbsoluteAddr,
        deps: &BTreeMap<AbsoluteAddr, BTreeSet<AbsoluteAddr>>,
        visited: &mut BTreeSet<AbsoluteAddr>,
        temp_visited: &mut BTreeSet<AbsoluteAddr>,
        result: &mut Vec<AbsoluteAddr>,
    ) {
        if visited.contains(&node) {
            return;
        }
        if temp_visited.contains(&node) {
            // Cycle detected in clock generation, ignore and break cycle for now
            return;
        }
        temp_visited.insert(node);

        if let Some(node_deps) = deps.get(&node) {
            for &dep in node_deps {
                topo_visit(dep, deps, visited, temp_visited, result);
            }
        }

        temp_visited.remove(&node);
        visited.insert(node);
        result.push(node);
    }

    // Ensure all unique clocks mapped in eval_apply_ffs are present in the topo sort
    for &clk in &unique_clocks {
        if !visited.contains(&clk) {
            topo_visit(
                clk,
                &clock_deps,
                &mut visited,
                &mut temp_visited,
                &mut topological_clocks,
            );
        }
    }

    // Include other potential event signals (like synchronous resets) so they can be scheduled
    for id in expanded.values() {
        let module_id = &instance_modules[id];
        let sim_module = &modules[module_id];
        for (var_id, var) in &sim_module.variables {
            let kind = var.metadata.kind;
            let is_trigger = matches!(
                kind,
                DomainKind::ClockPosedge
                    | DomainKind::ClockNegedge
                    | DomainKind::ResetAsyncHigh
                    | DomainKind::ResetAsyncLow
            );
            if is_trigger {
                let addr = bindings.locate(*id, *var_id);
                let canonical = clock_domains.get(&addr).copied().unwrap_or(addr);
                // Add empty execution units so it becomes a valid event domain for scheduling
                eval_apply_ffs.entry(canonical).or_default();
                eval_only_ffs.entry(canonical).or_default();
                apply_ffs.entry(canonical).or_default();

                if !visited.contains(&canonical) {
                    topo_visit(
                        canonical,
                        &clock_deps,
                        &mut visited,
                        &mut temp_visited,
                        &mut topological_clocks,
                    );
                }
            }
        }
    }

    let mut cascaded_clocks: BTreeSet<AbsoluteAddr> = BTreeSet::new();
    for (target, sources) in &clock_deps {
        cascaded_clocks.insert(*target);
        for source in sources {
            cascaded_clocks.insert(*source);
        }
    }

    // Close the direct edges transitively: a domain triggered by the event
    // may itself write the source of another derived clock.
    let order: HashMap<AbsoluteAddr, usize> = topological_clocks
        .iter()
        .enumerate()
        .map(|(index, clock)| (*clock, index))
        .collect();
    let mut derived_events = BTreeMap::new();
    for event in trigger_edges.keys() {
        let mut reached = BTreeSet::new();
        let mut pending = vec![*event];
        while let Some(source) = pending.pop() {
            for target in trigger_edges.get(&source).into_iter().flatten() {
                if target != event && reached.insert(*target) {
                    pending.push(*target);
                }
            }
        }
        // Clocks computed from the event's own signal change with the event
        // itself; they are judged at its edge, not after its commit.
        let combinational = comb_derived_clocks.get(event);
        let mut targets: Vec<_> = reached
            .into_iter()
            .filter(|target| !combinational.is_some_and(|clocks| clocks.contains(target)))
            .collect();
        targets.sort_by_key(|target| order.get(target).copied().unwrap_or(usize::MAX));
        if !targets.is_empty() {
            derived_events.insert(*event, targets);
        }
    }

    // Combinational definitions by target, for the gate programs.
    let var_widths: HashMap<AbsoluteAddr, usize> = instance_modules
        .iter()
        .flat_map(|(&instance, module)| {
            modules[module]
                .variables
                .iter()
                .map(move |(var_id, var)| (bindings.locate(instance, *var_id), var.metadata.width))
        })
        .collect();
    let mut definitions: HashMap<AbsoluteAddr, Option<&LogicPath<AbsoluteAddr>>> =
        HashMap::default();
    for path in comb_blocks {
        let Some(target) = path.target.var() else {
            continue;
        };
        let whole = var_widths
            .get(&target.id)
            .is_some_and(|&width| target.access.lsb == 0 && target.access.msb + 1 == width);
        let simple = whole && path.local_inputs.is_empty() && path.previous_sources.is_empty();
        definitions
            .entry(target.id)
            .and_modify(|definition| *definition = None)
            .or_insert(simple.then_some(path));
    }
    let gates = GateBuilder {
        arena,
        definitions: &definitions,
        var_widths: &var_widths,
        clock_domains,
    };
    // The state each domain reads and writes, or `None` when it has side
    // effects whose order is observable.
    let access = |domain: &AbsoluteAddr| {
        let mut reads = BTreeSet::new();
        let mut writes = BTreeSet::new();
        for unit in eval_apply_ffs.get(domain).into_iter().flatten() {
            for block in unit.blocks.values() {
                for instruction in &block.instructions {
                    // Records, captures and external calls, a callee's too.
                    if instruction.is_host_interaction() {
                        return None;
                    }
                    reads.extend(
                        super::super::read_objects(instruction)
                            .map(|address| address.absolute_addr()),
                    );
                    writes.extend(
                        super::super::written_objects(instruction)
                            .map(|address| address.absolute_addr()),
                    );
                }
            }
        }
        Some((reads, writes))
    };
    // A domain reads its own clock's level when a flip-flop samples it as
    // data. An external call may read anything.
    let canonical = |address: AbsoluteAddr| clock_domains.get(&address).copied().unwrap_or(address);
    let level_unread_by_domain: BTreeSet<AbsoluteAddr> = eval_apply_ffs
        .iter()
        .filter(|(domain, units)| {
            let domain = canonical(**domain);
            !units
                .iter()
                .flat_map(|unit| unit.blocks.values())
                .flat_map(|block| &block.instructions)
                .any(|instruction| {
                    instruction.host_effects().extern_calls
                        || super::super::read_objects(instruction)
                            .any(|address| canonical(address.absolute_addr()) == domain)
                })
        })
        .map(|(domain, _)| canonical(*domain))
        .collect();
    let mut independent_derived = BTreeSet::new();
    let combinationally_read: BTreeSet<AbsoluteAddr> = comb_deps
        .values()
        .flatten()
        .map(|source| clock_domains.get(source).copied().unwrap_or(*source))
        .filter(|source| unique_clocks.contains(source))
        .collect();
    let combinational_derived = comb_derived_clocks
        .into_iter()
        .map(|(event, clocks)| {
            let mut clocks: Vec<_> = clocks.into_iter().collect();
            clocks.sort_by_key(|clock| order.get(clock).copied().unwrap_or(usize::MAX));
            // Run in turn, the derived domains in order and then the event's
            // own, each must find the state the earlier ones left untouched.
            let independent = (|| {
                let mut written = BTreeSet::new();
                for domain in clocks.iter().chain(std::iter::once(&event)) {
                    let (reads, writes) = access(domain)?;
                    if !reads.is_disjoint(&written) || !writes.is_disjoint(&written) {
                        return None;
                    }
                    written.extend(writes);
                }
                Some(())
            })()
            .is_some();
            if independent {
                independent_derived.insert(event);
            }
            let clocks = clocks
                .into_iter()
                .map(|clock| CombDerivedClock {
                    clock,
                    gate: gates.program(event, clock),
                    inputs: combinational_inputs.get(&clock).cloned(),
                })
                .collect();
            (event, clocks)
        })
        .collect();

    ClockAnalysis {
        topological_clocks,
        cascaded_clocks,
        derived_events,
        combinational_derived,
        independent_derived,
        combinationally_read,
        level_unread_by_domain,
    }
}

/// Complete read dependencies for proving a clock unaffected by an update.
/// Activation dependencies omit reads of a process's own outputs; those reads
/// still matter here. Include all procedural loads, previous values and local
/// expressions. An external call makes its outputs opaque.
pub(super) fn clock_cone_inputs(
    paths: &[LogicPath<AbsoluteAddr>],
    arena: &SLTNodeArena<AbsoluteAddr>,
    clocks: &BTreeSet<AbsoluteAddr>,
) -> BTreeMap<AbsoluteAddr, Vec<AbsoluteAddr>> {
    let mut dependencies: HashMap<AbsoluteAddr, HashSet<AbsoluteAddr>> = HashMap::default();
    let mut opaque = HashSet::default();
    for path in paths {
        let Some(target) = path.target.var() else {
            continue;
        };
        let mut inputs = path.sources.clone();
        inputs.extend(path.previous_sources.iter().cloned());
        inputs.extend(path.address_sources.iter().cloned());
        flattening::collect_inputs(path.expr, arena, &mut inputs);
        for expression in path
            .local_inputs
            .iter()
            .map(|(_, node)| *node)
            .chain(path.pre_lower_nodes.iter().copied())
        {
            flattening::collect_inputs(expression, arena, &mut inputs);
        }
        let reads = dependencies.entry(target.id).or_default();
        reads.extend(inputs.into_iter().map(|input| input.id));
        if let LogicPathTarget::Process { unit, .. } = &path.target {
            for instruction in unit.blocks.values().flat_map(|block| &block.instructions) {
                reads.extend(super::super::read_objects(instruction).copied());
                // An external call, a callee's too.
                if instruction.host_effects().extern_calls {
                    opaque.insert(target.id);
                }
            }
        }
    }
    clocks
        .iter()
        .filter_map(|&clock| {
            let mut reached = BTreeSet::new();
            let mut pending = vec![clock];
            while let Some(address) = pending.pop() {
                if !reached.insert(address) {
                    continue;
                }
                if opaque.contains(&address) {
                    return None;
                }
                pending.extend(dependencies.get(&address).into_iter().flatten().copied());
            }
            Some((clock, reached.into_iter().collect()))
        })
        .collect()
}

/// Builds [`GateOp`] programs: a derived clock's combinational cone with its
/// source event's signal as a parameter and state variables as loads.
pub(super) struct GateBuilder<'a> {
    pub(super) arena: &'a SLTNodeArena<AbsoluteAddr>,
    /// The single whole-variable combinational definition of a variable, or
    /// `None` when it has several or a procedural one.
    pub(super) definitions: &'a HashMap<AbsoluteAddr, Option<&'a LogicPath<AbsoluteAddr>>>,
    pub(super) var_widths: &'a HashMap<AbsoluteAddr, usize>,
    pub(super) clock_domains: &'a HashMap<AbsoluteAddr, AbsoluteAddr>,
}

/// A gate program under construction.
#[derive(Default)]
pub(super) struct GateProgram {
    pub(super) ops: Vec<GateOp<AbsoluteAddr>>,
    pub(super) stack: usize,
    pub(super) max_stack: usize,
}

impl GateProgram {
    /// Larger cones settle the design instead.
    pub(super) const MAX_OPS: usize = 64;
    pub(super) const MAX_STACK: usize = 16;

    pub(super) fn push(&mut self, op: GateOp<AbsoluteAddr>, pops: usize) -> Option<()> {
        if self.ops.len() >= Self::MAX_OPS {
            return None;
        }
        self.ops.push(op);
        self.stack = self.stack.checked_sub(pops)? + 1;
        self.max_stack = self.max_stack.max(self.stack);
        Some(())
    }
}

pub(super) fn gate_u32(value: usize) -> Option<u32> {
    u32::try_from(value).ok()
}

impl GateBuilder<'_> {
    pub(super) const MAX_DEPTH: usize = 32;

    pub(super) fn program(
        &self,
        source: AbsoluteAddr,
        clock: AbsoluteAddr,
    ) -> Option<Vec<GateOp<AbsoluteAddr>>> {
        if self.var_widths.get(&clock) != Some(&1) {
            return None;
        }
        let definition = (*self.definitions.get(&clock)?)?;
        let mut program = GateProgram::default();
        self.emit(source, definition.expr, 0, &mut program)?;
        (program.stack == 1 && program.max_stack <= GateProgram::MAX_STACK).then_some(program.ops)
    }

    pub(super) fn width(&self, node: NodeId) -> Option<usize> {
        self.arena
            .node_width(node)
            .filter(|width| (1..=64).contains(width))
    }

    pub(super) fn emit(
        &self,
        source: AbsoluteAddr,
        node: NodeId,
        depth: usize,
        program: &mut GateProgram,
    ) -> Option<()> {
        use celox_design::{BinaryOp, UnaryOp};
        use celox_slt::SLTNode;
        if depth > Self::MAX_DEPTH {
            return None;
        }
        let result = self.width(node)?;
        match self.arena.get(node) {
            SLTNode::Input {
                variable,
                index,
                access,
                ..
            } => {
                if !index.is_empty() {
                    return None;
                }
                let bits = access.msb.checked_sub(access.lsb)? + 1;
                let canonical = self.clock_domains.get(variable).unwrap_or(variable);
                if *canonical == source {
                    return if access.lsb == 0 && bits == 1 {
                        program.push(GateOp::Source, 0)
                    } else {
                        None
                    };
                }
                let var_width = *self.var_widths.get(variable)?;
                match self.definitions.get(variable) {
                    Some(Some(definition)) => {
                        self.emit(source, definition.expr, depth + 1, program)?;
                        if access.lsb != 0 || bits != var_width {
                            program.push(
                                GateOp::Slice {
                                    lsb: gate_u32(access.lsb)?,
                                    width: gate_u32(bits)?,
                                },
                                1,
                            )?;
                        }
                        Some(())
                    }
                    Some(None) => None,
                    None if var_width <= 64 => program.push(
                        GateOp::Load {
                            address: *variable,
                            lsb: gate_u32(access.lsb)?,
                            width: gate_u32(bits)?,
                        },
                        0,
                    ),
                    None => None,
                }
            }
            SLTNode::Constant(payload, mask, _, _) => {
                if mask.bits() != 0 {
                    return None;
                }
                program.push(GateOp::Const(u64::try_from(payload).ok()?), 0)
            }
            &SLTNode::Binary(lhs, op, rhs) => {
                let (left, right) = (self.width(lhs)?, self.width(rhs)?);
                let same = left == right && left == result;
                let op = match op {
                    BinaryOp::And if same => GateOp::And,
                    BinaryOp::Or if same => GateOp::Or,
                    BinaryOp::Xor if same => GateOp::Xor,
                    BinaryOp::Eq if left == right => GateOp::Eq,
                    BinaryOp::Ne if left == right => GateOp::Ne,
                    BinaryOp::LogicAnd => GateOp::LogicAnd,
                    BinaryOp::LogicOr => GateOp::LogicOr,
                    _ => return None,
                };
                self.emit(source, lhs, depth + 1, program)?;
                self.emit(source, rhs, depth + 1, program)?;
                program.push(op, 2)
            }
            &SLTNode::Unary(op, operand) => {
                let operand_width = self.width(operand)?;
                let op = match op {
                    UnaryOp::Ident if operand_width == result => None,
                    UnaryOp::BitNot if operand_width == result => Some(GateOp::Not {
                        width: gate_u32(result)?,
                    }),
                    UnaryOp::LogicNot => Some(GateOp::LogicNot),
                    UnaryOp::And => Some(GateOp::ReduceAnd {
                        width: gate_u32(operand_width)?,
                    }),
                    UnaryOp::Or => Some(GateOp::ReduceOr),
                    UnaryOp::Xor => Some(GateOp::ReduceXor),
                    _ => return None,
                };
                self.emit(source, operand, depth + 1, program)?;
                match op {
                    Some(op) => program.push(op, 1),
                    None => Some(()),
                }
            }
            SLTNode::Slice { expr, access } => {
                let bits = access.msb.checked_sub(access.lsb)? + 1;
                self.emit(source, *expr, depth + 1, program)?;
                program.push(
                    GateOp::Slice {
                        lsb: gate_u32(access.lsb)?,
                        width: gate_u32(bits)?,
                    },
                    1,
                )
            }
            &SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                if self.width(then_expr)? != result || self.width(else_expr)? != result {
                    return None;
                }
                self.emit(source, cond, depth + 1, program)?;
                self.emit(source, then_expr, depth + 1, program)?;
                self.emit(source, else_expr, depth + 1, program)?;
                program.push(GateOp::Mux, 3)
            }
            _ => None,
        }
    }
}
