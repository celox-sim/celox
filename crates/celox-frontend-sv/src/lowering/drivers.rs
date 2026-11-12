//! The variables each process of a module writes.

use super::*;

/// The variables a process kernel of `module` may write after it first
/// suspends. Such a write can land between the evaluation of an edge
/// process and the NBA region that publishes its nonblocking assignments:
/// an inactive-region resumption (`#0`), or a process a domain's observed
/// blocking result wakes (IEEE 1800-2023 4.4.2.4, 4.5). The code before the
/// first suspension of an `initial` runs at time zero before any domain
/// evaluates, so its writes, such as memory initialization, are not
/// included.
///
/// Processes of other modules that write variables of `module` through
/// hierarchical names (`foreign_writes`) may run in that window as well, so
/// those variables are included whatever region the writes run in.
pub(super) fn kernel_window_writes(
    module: &sv::ir::Module,
    foreign_writes: &HashMap<String, hierarchy::ForeignWrites>,
) -> HashSet<String> {
    let subroutines = module.subroutines();
    let scan = DriverScan::of(module);
    let mut names = HashSet::default();
    for process in module
        .initial_processes()
        .iter()
        .filter(|process| !process.is_initializer() && !process.is_final())
    {
        let body = process.body();
        let first = if process.repeats() {
            0
        } else {
            body.iter()
                .position(|stmt| stmt_may_suspend(subroutines, stmt))
                .unwrap_or(body.len())
        };
        kernel_written_names(
            &scan,
            &body[first..],
            &mut names,
            &mut HashSet::default(),
            0,
            &mut None,
        );
    }
    names.extend(foreign_writes.keys().cloned());
    names
}

/// Whether `stmt` may suspend its process: a timing control, or a call of a
/// task, in it or in a subroutine it calls.
pub(super) fn stmt_may_suspend(subroutines: &[sv::ir::Subroutine], stmt: &sv::ir::Stmt) -> bool {
    let mut found = false;
    let mut calls = Vec::new();
    stmt.walk(&mut |stmt| {
        found |= matches!(
            stmt,
            sv::ir::Stmt::Delay { .. } | sv::ir::Stmt::WaitEvent(_) | sv::ir::Stmt::Wait(_)
        );
        procedural::stmt_calls(stmt, &mut calls);
    });
    let mut visited = HashSet::default();
    let mut seen_defaults = procedural::SeenDefaults::default();
    while !found && let Some((callee, args)) = calls.pop() {
        let sv::ir::Callee::Subroutine(name) = callee else {
            continue;
        };
        let Some(subroutine) = subroutines
            .iter()
            .find(|subroutine| subroutine.name == name)
        else {
            continue;
        };
        found |= subroutine.is_task;
        procedural::default_calls(subroutine, &args, &mut calls, &mut seen_defaults);
        if visited.insert(name) {
            for stmt in &subroutine.body {
                stmt.walk(&mut |stmt| procedural::stmt_calls(stmt, &mut calls));
            }
        }
    }
    found
}

/// Adds the names of the variables `body` writes, directly or in the
/// subroutines it calls, to `names` (see [`DriverScan`]).
///
/// With `refs`, an actual of a `ref` formal is not written by the call but
/// recorded there with the formal: it is written when the formal is (see
/// `ProcModule::written_names`).
pub(super) fn kernel_written_names(
    scan: &impl CallDefinitions,
    body: &[sv::ir::Stmt],
    names: &mut HashSet<String>,
    visited: &mut HashSet<String>,
    depth: usize,
    refs: &mut Option<Vec<(String, Vec<String>)>>,
) {
    if depth > procedural::MAX_CALL_DEPTH {
        return;
    }
    let mut calls = Vec::new();
    for stmt in body {
        stmt.walk(&mut |stmt| {
            match stmt {
                sv::ir::Stmt::Assign { lhs, .. } => {
                    names.insert(lhs.name().to_string());
                }
                sv::ir::Stmt::AssignConcat { parts, .. } => {
                    names.extend(parts.iter().map(|part| part.name().to_string()));
                }
                sv::ir::Stmt::SystemTask { task, args, .. } => {
                    if let Some(destination) = procedural::readmem_destination(*task, args) {
                        names.insert(destination.to_string());
                    }
                }
                _ => {}
            }
            procedural::stmt_calls(stmt, &mut calls);
        });
    }
    let mut seen_defaults = procedural::SeenDefaults::default();
    while let Some((callee, args)) = calls.pop() {
        if let sv::ir::Callee::Object(op) = &callee {
            names.extend(
                procedural::written_object_arguments(op, &args)
                    .iter()
                    .map(|lhs| lhs.name().to_string()),
            );
        }
        let name = match callee {
            sv::ir::Callee::System(function) => {
                names.extend(
                    procedural::written_system_arguments(function, &args)
                        .iter()
                        .map(|lvalue| lvalue.name().to_string()),
                );
                continue;
            }
            sv::ir::Callee::String(_)
            | sv::ir::Callee::Object(_)
            | sv::ir::Callee::Checked { .. }
            | sv::ir::Callee::Lookup { .. }
            | sv::ir::Callee::Stream(_) => continue,
            sv::ir::Callee::Subroutine(name) => name,
        };
        if let Some(import) = scan.dpi_import(&name) {
            names.extend(
                procedural::written_dpi_arguments(import, &args)
                    .iter()
                    .map(|lvalue| lvalue.name().to_string()),
            );
            continue;
        }
        let Some(subroutine) = scan.subroutine(&name) else {
            continue;
        };
        procedural::default_calls(subroutine, &args, &mut calls, &mut seen_defaults);
        for (param, arg) in subroutine.params.iter().zip(&args) {
            if param.direction.is_written()
                && let Some(lvalues) = arg.as_ref().and_then(procedural::lvalue_from_expr)
            {
                let actuals = lvalues.iter().map(|lvalue| lvalue.name().to_string());
                match refs {
                    Some(refs) if param.direction.is_reference() => {
                        refs.push((param.name.clone(), actuals.collect()));
                    }
                    _ => names.extend(actuals),
                }
            }
        }
        if visited.insert(name) {
            kernel_written_names(scan, &subroutine.body, names, visited, depth + 1, refs);
        }
    }
}

/// Where a scan of a body finds the subroutines and DPI-C imports it calls.
pub(super) trait CallDefinitions {
    fn subroutine(&self, name: &str) -> Option<&sv::ir::Subroutine>;
    fn dpi_import(&self, name: &str) -> Option<&sv::ir::DpiImport>;
}

impl CallDefinitions for DriverScan<'_> {
    fn subroutine(&self, name: &str) -> Option<&sv::ir::Subroutine> {
        self.subroutines
            .iter()
            .find(|subroutine| subroutine.name == name)
    }

    fn dpi_import(&self, name: &str) -> Option<&sv::ir::DpiImport> {
        self.dpi_imports.iter().find(|import| import.name() == name)
    }
}

impl CallDefinitions for procedural::ProcModule<'_> {
    fn subroutine(&self, name: &str) -> Option<&sv::ir::Subroutine> {
        self.subroutines.get(name)
    }

    fn dpi_import(&self, name: &str) -> Option<&sv::ir::DpiImport> {
        self.dpi_imports.get(name)
    }
}

/// Walks a body for the variables it writes: by assignments, and through the
/// output arguments and bodies of the subroutines it calls, which write for
/// the calling process (IEEE 1800-2023 9.2.2.2).
pub(super) struct DriverScan<'a> {
    subroutines: &'a [sv::ir::Subroutine],
    dpi_imports: &'a [sv::ir::DpiImport],
}

impl<'a> DriverScan<'a> {
    pub(super) fn of(module: &'a sv::ir::Module) -> Self {
        Self {
            subroutines: module.subroutines(),
            dpi_imports: module.dpi_imports(),
        }
    }

    /// Calls `record` with each assignment target of `body`.
    pub(super) fn body(&self, record: &mut dyn FnMut(&sv::ir::LValue), body: &[sv::ir::Stmt]) {
        self.body_at(record, body, &mut HashSet::default(), 0);
    }

    fn body_at(
        &self,
        record: &mut dyn FnMut(&sv::ir::LValue),
        body: &[sv::ir::Stmt],
        visited: &mut HashSet<String>,
        depth: usize,
    ) {
        // Deeper calls are rejected when the processes are lowered.
        if depth > procedural::MAX_CALL_DEPTH {
            return;
        }
        let mut calls = Vec::new();
        for stmt in body {
            stmt.walk(&mut |stmt| {
                match stmt {
                    sv::ir::Stmt::Assign { lhs, .. } => record(lhs),
                    sv::ir::Stmt::AssignConcat { parts, .. } => parts.iter().for_each(&mut *record),
                    _ => {}
                }
                procedural::stmt_calls(stmt, &mut calls);
            });
        }
        let mut seen_defaults = procedural::SeenDefaults::default();
        while let Some((callee, args)) = calls.pop() {
            if let sv::ir::Callee::Object(op) = &callee {
                for lhs in procedural::written_object_arguments(op, &args) {
                    record(&lhs);
                }
            }
            let name = match callee {
                sv::ir::Callee::System(function) => {
                    for lvalue in procedural::written_system_arguments(function, &args) {
                        record(&lvalue);
                    }
                    continue;
                }
                sv::ir::Callee::String(_)
                | sv::ir::Callee::Object(_)
                | sv::ir::Callee::Checked { .. }
                | sv::ir::Callee::Lookup { .. }
                | sv::ir::Callee::Stream(_) => continue,
                sv::ir::Callee::Subroutine(name) => name,
            };
            if let Some(import) = self.dpi_import(&name) {
                for lvalue in procedural::written_dpi_arguments(import, &args) {
                    record(&lvalue);
                }
                continue;
            }
            let Some(subroutine) = self.subroutine(&name) else {
                continue;
            };
            procedural::default_calls(subroutine, &args, &mut calls, &mut seen_defaults);
            for (param, arg) in subroutine.params.iter().zip(&args) {
                if param.direction.is_written()
                    && let Some(lvalues) = arg.as_ref().and_then(procedural::lvalue_from_expr)
                {
                    lvalues.iter().for_each(&mut *record);
                }
            }
            // A subroutine body writes a module variable for each caller;
            // its own formals and locals have module-unique names.
            if visited.insert(name) {
                self.body_at(record, &subroutine.body, visited, depth + 1);
            }
        }
    }
}

pub(super) fn condition_is_active(
    condition: Option<&sv::ir::ConstExpr>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> bool {
    condition.is_none_or(|condition| {
        sv::typecheck::eval_const_expr_with_types(condition, constants, parameter_types)
            .is_none_or(|value| value != 0)
    })
}

/// One assignment target a process of a module writes, with the drivers it
/// counts as.
pub(super) struct DriverWrite {
    /// The driver among the processes, `always` blocks and edge processes
    /// that [`ModuleDrivers::local`] counts, if the process is one of them.
    pub(super) local: Option<usize>,
    /// The continuous assignment, if the process is one.
    pub(super) continuous: Option<usize>,
    /// The `initial` process or declaration initializer, if it is one.
    pub(super) initial: Option<usize>,
    /// The bits the target drives, when its select is constant.
    pub(super) range: Option<(i128, i128)>,
    /// The bits of the element of an array a run-time select inside one
    /// element drives, when the select is not constant.
    pub(super) element: Option<(i128, i128)>,
}

/// Whether two continuous assignments of `module` assign the same name, the
/// cheap precondition of a net with several drivers.
pub(super) fn continuous_targets_repeat(module: &sv::ir::Module) -> bool {
    let mut seen = HashSet::default();
    module
        .comb_processes()
        .iter()
        .filter(|process| process.kind() == sv::ir::CombProcessKind::ContinuousAssign)
        .any(|process| {
            let mut names = HashSet::default();
            for statement in process.body() {
                match statement {
                    sv::ir::Stmt::Assign { lhs, .. } => {
                        names.insert(lhs.name());
                    }
                    sv::ir::Stmt::AssignConcat { parts, .. } => {
                        names.extend(parts.iter().map(sv::ir::LValue::name));
                    }
                    _ => {}
                }
            }
            names.into_iter().any(|name| !seen.insert(name))
        })
}

/// Repeated zero-delay aliases or identical literal drivers of a declared net
/// have one effective value (IEEE 1800-2023 6.6.1, 10.3.1). Keep the authored
/// process indices so removing a driver does not renumber the random streams
/// of later processes. Only whole-net identifier reads and identical literals
/// qualify: calls, slices, variables and conditional assignments retain their
/// independent driver identities. In particular, `uwire` drivers must not merge.
pub(super) fn redundant_net_drivers(module: &sv::ir::Module) -> HashSet<usize> {
    let nets: HashSet<_> = module
        .signals()
        .iter()
        .filter(|signal| {
            signal.is_net() && !signal.is_unresolved_net() && signal.hierarchical().is_none()
        })
        .map(|signal| signal.name())
        .chain(
            module
                .ports()
                .iter()
                .filter(|port| port.is_net() && !port.is_unresolved_net())
                .map(|port| port.name()),
        )
        .collect();
    let mut drivers = HashSet::default();
    let mut redundant = HashSet::default();
    for (index, process) in module.comb_processes().iter().enumerate() {
        if process.kind() != sv::ir::CombProcessKind::ContinuousAssign
            || process.condition().is_some()
        {
            continue;
        }
        let [
            sv::ir::Stmt::Assign {
                lhs: sv::ir::LValue::Ident(target),
                rhs,
                nonblocking: false,
            },
        ] = process.body()
        else {
            continue;
        };
        let (literal, source) = match rhs {
            sv::ir::Expr::Ident(source) => (false, source),
            sv::ir::Expr::Literal(source) => (true, source),
            _ => continue,
        };
        if nets.contains(target.as_str())
            && !drivers.insert((target.as_str(), literal, source.as_str()))
        {
            redundant.insert(index);
        }
    }
    redundant
}

/// The drivers of the variables of a module, by variable name, found by one
/// walk of each process. Each query returns the drivers in process order.
pub(super) struct ModuleDrivers {
    pub(super) writes: HashMap<String, Vec<DriverWrite>>,
}

impl ModuleDrivers {
    pub(super) fn new(
        module: &sv::ir::Module,
        constants: &HashMap<String, i128>,
        parameter_types: &HashMap<String, (usize, bool)>,
    ) -> Self {
        let mut types = HashMap::default();
        for (name, r#type) in module
            .signals()
            .iter()
            .map(|signal| (signal.name(), signal.r#type()))
            .chain(
                module
                    .ports()
                    .iter()
                    .map(|port| (port.name(), port.r#type())),
            )
        {
            types.entry(name).or_insert(r#type);
        }
        // The element width and the width of each array written at a
        // run-time element.
        let mut shapes: HashMap<String, Option<(usize, usize)>> = HashMap::default();
        let mut writes: HashMap<String, Vec<DriverWrite>> = HashMap::default();
        let scan = DriverScan::of(module);
        let mut walk = |body: &[sv::ir::Stmt],
                        local: Option<usize>,
                        continuous: Option<usize>,
                        initial: Option<usize>| {
            if local.is_none() && continuous.is_none() && initial.is_none() {
                return;
            }
            scan.body(
                &mut |lvalue| {
                    let name = lvalue.name();
                    let range = net_lvalue_range(lvalue, constants, parameter_types);
                    let element = match lvalue {
                        sv::ir::LValue::Select { lsb, .. } if range.is_none() => {
                            let shape = *shapes.entry(name.to_string()).or_insert_with(|| {
                                types
                                    .get(name)
                                    .and_then(|r#type| {
                                        signal_type_from_sv(r#type, constants, parameter_types).ok()
                                    })
                                    .and_then(|info| {
                                        let elements = info.array_dims.iter().product::<usize>();
                                        (!info.array_dims.is_empty() && elements != 0)
                                            .then(|| (info.width / elements, info.width))
                                    })
                            });
                            shape.and_then(|(element_width, width)| {
                                let window = runtime_select_window(
                                    lsb,
                                    element_width,
                                    width,
                                    constants,
                                    parameter_types,
                                )?;
                                Some((window.lsb as i128, window.msb as i128))
                            })
                        }
                        _ => None,
                    };
                    writes
                        .entry(name.to_string())
                        .or_default()
                        .push(DriverWrite {
                            local,
                            continuous,
                            initial,
                            range,
                            element,
                        });
                },
                body,
            );
        };
        let comb = module.comb_processes();
        let ff = module.ff_processes();
        let redundant = redundant_net_drivers(module);
        for (index, process) in comb.iter().enumerate() {
            if redundant.contains(&index) {
                continue;
            }
            let active = condition_is_active(process.condition(), constants, parameter_types);
            walk(
                process.body(),
                active.then_some(index),
                (active && process.kind() == sv::ir::CombProcessKind::ContinuousAssign)
                    .then_some(index),
                None,
            );
        }
        for (index, process) in ff.iter().enumerate() {
            walk(process.body(), Some(comb.len() + index), None, None);
        }
        // Ordinary always processes with timing (including edge processes
        // converted after timed-task classification) retain their driver
        // identity. One-shot initial blocks and declaration initializers are
        // not drivers.
        let mut repeating = comb.len() + ff.len();
        for (index, process) in module.initial_processes().iter().enumerate() {
            let active = condition_is_active(process.condition(), constants, parameter_types);
            let local = process.repeats().then(|| {
                repeating += 1;
                repeating - 1
            });
            walk(
                process.body(),
                local.filter(|_| active),
                None,
                (active && !process.is_scheduled_edge()).then_some(index),
            );
        }
        Self { writes }
    }

    fn drivers(
        &self,
        signal_name: &str,
        driver: impl Fn(&DriverWrite) -> Option<(usize, Option<(i128, i128)>)>,
    ) -> Vec<(usize, Option<(i128, i128)>)> {
        self.writes
            .get(signal_name)
            .map_or_else(Vec::new, |writes| {
                writes.iter().filter_map(driver).collect()
            })
    }

    /// The ranges of `signal_name` that the processes of the module write,
    /// other than `initial` blocks and declaration initializers.
    pub(super) fn local(&self, signal_name: &str) -> Vec<(usize, Option<(i128, i128)>)> {
        self.drivers(signal_name, |write| {
            Some((write.local?, write.range.or(write.element)))
        })
    }

    /// The ranges of `signal_name` that continuous assignments write.
    pub(super) fn continuous(&self, signal_name: &str) -> Vec<(usize, Option<(i128, i128)>)> {
        self.drivers(signal_name, |write| Some((write.continuous?, write.range)))
    }

    /// The ranges of `signal_name` that `initial` processes and variable
    /// declaration initializers write.
    pub(super) fn initial(&self, signal_name: &str) -> Vec<(usize, Option<(i128, i128)>)> {
        self.drivers(signal_name, |write| Some((write.initial?, write.range)))
    }
}

/// Where process `driver` of [`ModuleDrivers::local`] is written, as
/// `path:line` or `path:line:column`.
pub(super) fn driver_site(module: &sv::ir::Module, driver: usize) -> Option<String> {
    let combinational = module.comb_processes().len();
    let edge = combinational + module.ff_processes().len();
    if let Some(process) = module.comb_processes().get(driver) {
        return process
            .site()
            .map(|site| format!("{}:{}", site.logical_path.display(), site.line));
    }
    if driver < edge {
        return module.ff_processes()[driver - combinational]
            .site()
            .map(ToString::to_string);
    }
    module
        .initial_processes()
        .iter()
        .filter(|process| process.repeats())
        .nth(driver - edge)
        .and_then(|process| process.site())
        .map(ToString::to_string)
}

/// Whether process `driver` of [`ModuleDrivers::local`] runs as a process
/// whose writes store into a variable as they execute, so that it may write
/// a variable that other such processes write too (IEEE 1800-2023 6.5): an
/// ordinary `always` run as a process. An `always_ff` is the only process
/// that writes its variables (9.2.2.4).
pub(super) fn shares_variables(module: &sv::ir::Module, driver: usize) -> bool {
    let edge = module.comb_processes().len() + module.ff_processes().len();
    driver >= edge
        && module
            .initial_processes()
            .iter()
            .filter(|process| process.repeats())
            .nth(driver - edge)
            .is_some_and(|process| {
                process
                    .site()
                    .is_none_or(|site| site.construct != "always_ff")
            })
}

/// Whether process `driver` of [`ModuleDrivers::local`] must be the only writer of
/// the variables it writes: an `always_comb` or continuous assignment, an
/// edge procedure, or an `always_ff` run as a process, but not another
/// ordinary `always` process (IEEE 1800-2023 6.5, 9.2.2).
pub(super) fn exclusive_driver(module: &sv::ir::Module, driver: usize) -> bool {
    let edge = module.comb_processes().len() + module.ff_processes().len();
    driver < edge || {
        let process = module
            .initial_processes()
            .iter()
            .filter(|process| process.repeats())
            .nth(driver - edge);
        process.is_some_and(|process| {
            process.is_scheduled_edge()
                || process
                    .site()
                    .is_some_and(|site| site.construct == "always_ff")
        })
    }
}

/// Whether process `driver` of [`ModuleDrivers::local`] is one the standard lets
/// no other process write to the variables it writes: `always_comb`,
/// `always_latch`, `always_ff` or a continuous assignment (IEEE 1800-2023
/// 6.5, 9.2.2).
pub(super) fn restricted_driver(module: &sv::ir::Module, driver: usize) -> bool {
    let combinational = module.comb_processes().len();
    let edge = combinational + module.ff_processes().len();
    if let Some(process) = module.comb_processes().get(driver) {
        return process.kind() != sv::ir::CombProcessKind::AlwaysStar;
    }
    if driver < edge {
        return module.ff_processes()[driver - combinational].is_always_ff();
    }
    module
        .initial_processes()
        .iter()
        .filter(|process| process.repeats())
        .nth(driver - edge)
        .and_then(|process| process.site())
        .is_some_and(|site| matches!(site.construct, "always_ff" | "always_comb" | "always_latch"))
}

pub(super) fn net_lvalue_range(
    lvalue: &sv::ir::LValue,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<(i128, i128)> {
    let sv::ir::LValue::Select { msb, lsb, .. } = lvalue else {
        return None;
    };
    let msb = sv::typecheck::eval_const_expr_with_types(msb, constants, parameter_types)?;
    let lsb = sv::typecheck::eval_const_expr_with_types(lsb, constants, parameter_types)?;
    Some((msb.min(lsb), msb.max(lsb)))
}

pub(super) fn net_driver_ranges_overlap(
    left: Option<(i128, i128)>,
    right: Option<(i128, i128)>,
) -> bool {
    match (left, right) {
        (Some((left_start, left_end)), Some((right_start, right_end))) => {
            left_start <= right_end && right_start <= left_end
        }
        _ => true,
    }
}
