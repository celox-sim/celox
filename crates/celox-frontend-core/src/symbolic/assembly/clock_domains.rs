//! Unifying the clock domains of signals wired to one another.

use super::*;

pub(super) fn unify_clock_domains(
    bindings: &PackageBindings,
    expanded: &HashMap<InstancePath, InstanceId>,
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
) -> HashMap<AbsoluteAddr, AbsoluteAddr> {
    let mut drive_graph: HashMap<AbsoluteAddr, Vec<AbsoluteAddr>> = HashMap::default();
    // The edge each clock is sensed at: `true` rising, `false` falling.
    let mut polarities: HashMap<AbsoluteAddr, bool> = HashMap::default();
    // Every range definition by its target, to follow single bits through
    // wiring.
    let mut wires: HashMap<AbsoluteAddr, Vec<Wire<'_>>> = HashMap::default();

    for (path, id) in expanded {
        let module_id = &instance_modules[id];
        let sim_module = &modules[module_id];

        let width = |module: &SimModule, var: SourceVarId| {
            module
                .variables
                .get(&var)
                .map(|variable| variable.metadata.width)
        };
        let mut record_polarity = |module: &SimModule, instance: InstanceId, var: SourceVarId| {
            let polarity = module
                .variables
                .get(&var)
                .and_then(|variable| celox_design::edges::polarity(variable.metadata.kind));
            if let Some(polarity) = polarity {
                polarities.insert(bindings.locate(instance, var), polarity);
            }
        };

        for logic_path in &sim_module.comb_blocks {
            // A definition reading a value from before its process ran
            // is not a wire: `g = v[0]; v = {2{clk}};` makes `g` the old
            // level of `clk`.
            if !logic_path.previous_sources.is_empty() {
                continue;
            }
            if let Some(target) = logic_path.target.var() {
                wires
                    .entry(bindings.locate(*id, target.id))
                    .or_default()
                    .push(Wire {
                        access: target.access,
                        expr: logic_path.expr,
                        arena: WireArena::Comb(&sim_module.arena, *id),
                    });
            }
        }
        // Internal aliases (e.g. `assign clk_internal = clk_port;`)
        for logic_path in &sim_module.comb_blocks {
            // Only unify whole-variable copies. Gated clocks and part-selects
            // such as `let divided = counter[0];` are clocks of their own: the
            // runtime edge-senses the canonical variable as a whole.
            if let Some((target, source)) = whole_variable_copy(
                &sim_module.arena,
                logic_path,
                |target| width(sim_module, target),
                |source| width(sim_module, source),
            ) {
                record_polarity(sim_module, *id, target);
                record_polarity(sim_module, *id, source);
                drive_graph
                    .entry(bindings.locate(*id, source))
                    .or_default()
                    .push(bindings.locate(*id, target));
            }
        }
        for (inst_name, glue_blocks) in &sim_module.glue_blocks {
            for (idx, glue_block) in glue_blocks.iter().enumerate() {
                let mut child_path = path.0.clone();
                child_path.push((inst_name.clone(), sim_module.instance_index(inst_name, idx)));
                let child_id = expanded[&InstancePath(child_path)];
                let child_module = &modules[&glue_block.module_id];
                let glue_width = |var: GlueAddr| match var {
                    GlueAddr::Parent(var) => width(sim_module, var),
                    GlueAddr::Child(var) => width(child_module, var),
                };
                for (_, logic_path) in glue_block
                    .input_ports
                    .iter()
                    .chain(&glue_block.output_ports)
                {
                    if let Some(target) = logic_path.target.var() {
                        let located = match target.id {
                            GlueAddr::Parent(var) => bindings.locate(*id, var),
                            GlueAddr::Child(var) => bindings.locate(child_id, var),
                        };
                        wires.entry(located).or_default().push(Wire {
                            access: target.access,
                            expr: logic_path.expr,
                            arena: WireArena::Glue(&glue_block.arena, *id, child_id),
                        });
                    }
                }

                // Inputs: Parent -> Child (Parent drives Child)
                for (_, logic_path) in &glue_block.input_ports {
                    if let Some((GlueAddr::Child(child_var), GlueAddr::Parent(parent_var))) =
                        whole_variable_copy(&glue_block.arena, logic_path, glue_width, glue_width)
                    {
                        record_polarity(child_module, child_id, child_var);
                        record_polarity(sim_module, *id, parent_var);
                        drive_graph
                            .entry(bindings.locate(*id, parent_var))
                            .or_default()
                            .push(bindings.locate(child_id, child_var));
                    }
                }
                // Outputs: Child -> Parent (Child drives Parent)
                for (parent_vars, logic_path) in &glue_block.output_ports {
                    let [parent_var] = parent_vars.as_slice() else {
                        continue;
                    };
                    let parent_width = width(sim_module, *parent_var);
                    if let Some((_, GlueAddr::Child(child_var))) = whole_variable_copy(
                        &glue_block.arena,
                        logic_path,
                        |_| parent_width,
                        glue_width,
                    ) {
                        record_polarity(child_module, child_id, child_var);
                        record_polarity(sim_module, *id, *parent_var);
                        drive_graph
                            .entry(bindings.locate(child_id, child_var))
                            .or_default()
                            .push(bindings.locate(*id, *parent_var));
                    }
                }
            }
        }
    }

    // A one-bit variable wired to a bit of a vector that is in turn wired
    // to a one-bit variable, through bit selects, concatenations,
    // replications and port connections, is a copy of that variable: a
    // clock gated in a cell, collected into a vector and replicated to the
    // clock inputs of many instances is the cell's output clock.
    let one_bit = |addr: AbsoluteAddr| {
        modules[&instance_modules[&addr.instance_id]]
            .variables
            .get(&addr.var_id)
            .is_some_and(|variable| {
                variable.metadata.width == 1 && variable.metadata.array_dims.is_empty()
            })
    };
    let resolve = |(addr, bit): (AbsoluteAddr, usize)| {
        let wire = wires
            .get(&addr)?
            .iter()
            .find(|wire| wire.access.lsb <= bit && bit <= wire.access.msb)?;
        let bit = bit - wire.access.lsb;
        match wire.arena {
            WireArena::Comb(arena, instance) => wired_bit(arena, wire.expr, bit)
                .map(|(var, bit)| (bindings.locate(instance, var), bit)),
            WireArena::Glue(arena, parent, child) => {
                wired_bit(arena, wire.expr, bit).map(|(var, bit)| match var {
                    GlueAddr::Parent(var) => (bindings.locate(parent, var), bit),
                    GlueAddr::Child(var) => (bindings.locate(child, var), bit),
                })
            }
        }
    };
    let mut targets = wires
        .keys()
        .copied()
        .filter(|&addr| one_bit(addr))
        .collect::<Vec<_>>();
    targets.sort_unstable();
    for target in targets {
        let mut node = (target, 0);
        for _ in 0..MAX_WIRED_BIT_STEPS {
            let Some(next) = resolve(node) else {
                break;
            };
            if one_bit(next.0) {
                if next.0 != target {
                    for addr in [target, next.0] {
                        let polarity = modules[&instance_modules[&addr.instance_id]]
                            .variables
                            .get(&addr.var_id)
                            .and_then(|variable| {
                                celox_design::edges::polarity(variable.metadata.kind)
                            });
                        if let Some(polarity) = polarity {
                            polarities.insert(addr, polarity);
                        }
                    }
                    drive_graph.entry(next.0).or_default().push(target);
                }
                break;
            }
            node = next;
        }
    }

    // A copy sensed at the opposite edge of its source is a clock of its own:
    // `always_ff @(negedge d)` with `assign c = clk; assign d = c;` runs when
    // `clk` falls, not in `clk`'s rising domain. A copy sensed at no edge
    // takes the edge of its drivers, and a root sensed at no edge the edge of
    // its first copy sensed at one. Copies that join edges of both kinds are
    // cut apart.
    let drivers = {
        let mut drivers: HashMap<AbsoluteAddr, Vec<AbsoluteAddr>> = HashMap::default();
        for (src, sinks) in &drive_graph {
            for sink in sinks {
                drivers.entry(*sink).or_default().push(*src);
            }
        }
        drivers
    };
    let root_polarity = |root: AbsoluteAddr| {
        polarities.get(&root).copied().or_else(|| {
            let mut stack = vec![root];
            let mut visited = HashSet::default();
            while let Some(addr) = stack.pop() {
                if !visited.insert(addr) {
                    continue;
                }
                if let Some(&polarity) = polarities.get(&addr) {
                    return Some(polarity);
                }
                if let Some(sinks) = drive_graph.get(&addr) {
                    stack.extend(sinks.iter().rev());
                }
            }
            None
        })
    };
    let effective_polarity = |addr: AbsoluteAddr| {
        let mut current = addr;
        let mut visited = HashSet::default();
        loop {
            if let Some(&polarity) = polarities.get(&current) {
                return Some(polarity);
            }
            match drivers.get(&current).and_then(|sources| sources.first()) {
                Some(&next) if visited.insert(next) => current = next,
                _ => return root_polarity(current),
            }
        }
    };
    let drive_graph: HashMap<AbsoluteAddr, Vec<AbsoluteAddr>> = drive_graph
        .iter()
        .map(|(src, sinks)| {
            let src_polarity = effective_polarity(*src);
            let sinks = sinks
                .iter()
                .copied()
                .filter(|sink| match (src_polarity, effective_polarity(*sink)) {
                    (Some(a), Some(b)) => a == b,
                    _ => true,
                })
                .collect();
            (*src, sinks)
        })
        .collect();

    // Resolve Canonical Clock Domains: Find the root driver for each connected component
    let mut clock_domains: HashMap<AbsoluteAddr, AbsoluteAddr> = HashMap::default();

    // Reverse the drive graph to find roots (Sink -> Sources)
    let mut reverse_drive_graph: HashMap<AbsoluteAddr, Vec<AbsoluteAddr>> = HashMap::default();
    for (src, sinks) in &drive_graph {
        for sink in sinks {
            reverse_drive_graph.entry(*sink).or_default().push(*src);
        }
    }

    // Collect all unique addresses involved in any drive relationship
    let mut all_addrs = HashSet::default();
    for src in drive_graph.keys() {
        all_addrs.insert(*src);
    }
    for sinks in drive_graph.values() {
        for sink in sinks {
            all_addrs.insert(*sink);
        }
    }

    // Assign each address its canonical root driver
    for addr in all_addrs {
        let mut current = addr;
        let mut visited = HashSet::default();
        // Traverse upwards towards the root driver
        while let Some(sources) = reverse_drive_graph.get(&current) {
            if sources.is_empty() {
                break;
            }
            // In a valid hardware design, a clock net usually has 1 driver.
            // If multiple, we just pick the first for canonicalization.
            let next = sources[0];
            if visited.contains(&next) {
                break; // Prevent infinite loop in case of bad combinational loop
            }
            visited.insert(next);
            current = next;
        }
        clock_domains.insert(addr, current);
    }
    clock_domains
}

/// The most wiring hops followed from a one-bit variable to its source.
pub(super) const MAX_WIRED_BIT_STEPS: usize = 64;

/// The node arena of a range definition, with the instances its
/// addresses resolve in.
#[derive(Clone, Copy)]
pub(super) enum WireArena<'a> {
    Comb(&'a SLTNodeArena<SourceVarId>, InstanceId),
    /// A port connection: the parent's and the child's instance.
    Glue(&'a SLTNodeArena<GlueAddr>, InstanceId, InstanceId),
}

/// One range definition of a variable: `access` of the target takes `expr`.
pub(super) struct Wire<'a> {
    pub(super) access: BitAccess,
    pub(super) expr: NodeId,
    pub(super) arena: WireArena<'a>,
}

/// The variable bit that bit `bit` of `node` is wired to: a read of a
/// variable, through bit selects and concatenations (replications are
/// concatenations), with no operator in between.
pub(super) fn wired_bit<A: Copy + std::hash::Hash + Eq>(
    arena: &SLTNodeArena<A>,
    mut node: NodeId,
    mut bit: usize,
) -> Option<(A, usize)> {
    loop {
        match arena.get(node) {
            celox_slt::SLTNode::Input {
                variable,
                index,
                access,
                ..
            } if index.is_empty() => {
                return (bit <= access.msb - access.lsb).then_some((*variable, access.lsb + bit));
            }
            celox_slt::SLTNode::Slice { expr, access } => {
                if bit > access.msb - access.lsb {
                    return None;
                }
                bit += access.lsb;
                node = *expr;
            }
            celox_slt::SLTNode::Concat(parts) => {
                // Parts run from the most significant one.
                let mut offset = 0;
                let (part, inner) = parts.iter().rev().find_map(|&(part, width)| {
                    let found = (bit < offset + width).then_some((part, bit - offset));
                    offset += width;
                    found
                })?;
                node = part;
                bit = inner;
            }
            _ => return None,
        }
    }
}

/// The `(target, source)` of a path that copies one whole variable into
/// another variable of the same width, unchanged.
///
/// Only such a copy is the same signal as its source, so only it may share the
/// source's clock domain. A part-select, an element of an array, a resized
/// read, or an assignment to part of the target is a different value, whose
/// edges are not the edges of the source variable as a whole.
pub(super) fn whole_variable_copy<A: Copy + std::hash::Hash + Eq>(
    arena: &SLTNodeArena<A>,
    path: &LogicPath<A>,
    target_width: impl Fn(A) -> Option<usize>,
    source_width: impl Fn(A) -> Option<usize>,
) -> Option<(A, A)> {
    let target = path.target.var()?;
    let [source] = path.sources.iter().collect::<Vec<_>>()[..] else {
        return None;
    };
    let width = source_width(source.id).filter(|&width| width > 0)?;
    let whole = |access: &BitAccess| access.lsb == 0 && access.msb + 1 == width;
    let reads_whole_source = |mut node: NodeId| loop {
        match arena.get(node) {
            celox_slt::SLTNode::Input {
                variable,
                index,
                access,
                ..
            } => break *variable == source.id && index.is_empty() && whole(access),
            celox_slt::SLTNode::Slice { expr, access } if whole(access) => node = *expr,
            _ => break false,
        }
    };
    (target_width(target.id) == Some(width)
        && whole(&target.access)
        && whole(&source.access)
        && reads_whole_source(path.expr))
    .then_some((target.id, source.id))
}
