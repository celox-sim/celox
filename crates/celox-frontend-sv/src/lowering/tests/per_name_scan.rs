use super::super::*;

/// Records the ranges of `signal_name` that `body` writes: by assignments,
/// and through the output arguments and bodies of the subroutines it calls,
/// which write for the calling process (IEEE 1800-2023 9.2.2.2).
fn body_driver_ranges(
    drivers: &mut Vec<(usize, Option<(i128, i128)>)>,
    body: &[sv::ir::Stmt],
    driver_id: usize,
    scan: &DriverScan<'_>,
) {
    let mut visited = HashSet::default();
    scan.body(drivers, body, driver_id, &mut visited, 0);
}

/// What `body_driver_ranges` looks for and in which module.
struct DriverScan<'a> {
    signal_name: &'a str,
    subroutines: &'a [sv::ir::Subroutine],
    /// The bits an assignment target drives, when they are known.
    range: &'a dyn Fn(&sv::ir::LValue) -> Option<(i128, i128)>,
}

impl DriverScan<'_> {
    fn body(
        &self,
        drivers: &mut Vec<(usize, Option<(i128, i128)>)>,
        body: &[sv::ir::Stmt],
        driver_id: usize,
        visited: &mut HashSet<String>,
        depth: usize,
    ) {
        // Deeper calls are rejected when the processes are lowered.
        if depth > procedural::MAX_CALL_DEPTH {
            return;
        }
        let record = |drivers: &mut Vec<_>, lvalue: &sv::ir::LValue| {
            if lvalue.name() == self.signal_name {
                drivers.push((driver_id, (self.range)(lvalue)));
            }
        };
        let mut calls = Vec::new();
        for stmt in body {
            stmt.walk(&mut |stmt| {
                match stmt {
                    sv::ir::Stmt::Assign { lhs, .. } => record(drivers, lhs),
                    sv::ir::Stmt::AssignConcat { parts, .. } => {
                        parts.iter().for_each(|part| record(drivers, part))
                    }
                    _ => {}
                }
                procedural::stmt_calls(stmt, &mut calls);
            });
        }
        let mut seen_defaults = procedural::SeenDefaults::default();
        while let Some((callee, args)) = calls.pop() {
            if let sv::ir::Callee::Object(op) = &callee {
                for lhs in procedural::written_object_arguments(op, &args) {
                    record(drivers, &lhs);
                }
            }
            let name = match callee {
                sv::ir::Callee::System(function) => {
                    for lvalue in procedural::written_system_arguments(function, &args) {
                        record(drivers, &lvalue);
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
            let Some(subroutine) = self
                .subroutines
                .iter()
                .find(|subroutine| subroutine.name == name)
            else {
                continue;
            };
            procedural::default_calls(subroutine, &args, &mut calls, &mut seen_defaults);
            for (param, arg) in subroutine.params.iter().zip(&args) {
                if param.direction.is_written()
                    && let Some(lvalues) = arg.as_ref().and_then(procedural::lvalue_from_expr)
                {
                    lvalues.iter().for_each(|lvalue| record(drivers, lvalue));
                }
            }
            // A subroutine body writes a module variable for each caller;
            // its own formals and locals have module-unique names.
            if visited.insert(name) {
                self.body(drivers, &subroutine.body, driver_id, visited, depth + 1);
            }
        }
    }
}

pub(super) fn local_driver_ranges(
    module: &sv::ir::Module,
    signal_name: &str,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Vec<(usize, Option<(i128, i128)>)> {
    // The element a run-time select inside one element of an array drives.
    let shape = module
        .signals()
        .iter()
        .map(|signal| (signal.name(), signal.r#type()))
        .chain(
            module
                .ports()
                .iter()
                .map(|port| (port.name(), port.r#type())),
        )
        .find(|(name, _)| *name == signal_name)
        .and_then(|(_, r#type)| signal_type_from_sv(r#type, constants, parameter_types).ok())
        .and_then(|info| {
            let elements = info.array_dims.iter().product::<usize>();
            (!info.array_dims.is_empty() && elements != 0)
                .then(|| (info.width / elements, info.width))
        });
    let lvalue_range = |lvalue: &sv::ir::LValue| {
        net_lvalue_range(lvalue, constants, parameter_types).or_else(|| {
            let (sv::ir::LValue::Select { lsb, .. }, Some((element_width, width))) =
                (lvalue, shape)
            else {
                return None;
            };
            let window =
                runtime_select_window(lsb, element_width, width, constants, parameter_types)?;
            Some((window.lsb as i128, window.msb as i128))
        })
    };
    let scan = DriverScan {
        signal_name,
        subroutines: module.subroutines(),
        range: &lvalue_range,
    };
    let mut drivers = Vec::new();
    let mut driver_id = 0;
    for process in module.comb_processes() {
        if condition_is_active(process.condition(), constants, parameter_types) {
            body_driver_ranges(&mut drivers, process.body(), driver_id, &scan);
        }
        driver_id += 1;
    }
    for process in module.ff_processes() {
        body_driver_ranges(&mut drivers, process.body(), driver_id, &scan);
        driver_id += 1;
    }
    // Ordinary always processes with timing (including edge processes
    // converted after timed-task classification) retain their driver identity.
    // One-shot initial blocks and declaration initializers are not drivers.
    for process in module.initial_processes().iter().filter(|p| p.repeats()) {
        if condition_is_active(process.condition(), constants, parameter_types) {
            body_driver_ranges(&mut drivers, process.body(), driver_id, &scan);
        }
        driver_id += 1;
    }
    drivers
}

/// The ranges of `signal_name` that continuous assignments write.
pub(super) fn continuous_driver_ranges(
    module: &sv::ir::Module,
    signal_name: &str,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Vec<(usize, Option<(i128, i128)>)> {
    let range = |lvalue: &sv::ir::LValue| net_lvalue_range(lvalue, constants, parameter_types);
    let scan = DriverScan {
        signal_name,
        subroutines: module.subroutines(),
        range: &range,
    };
    let mut drivers = Vec::new();
    for (driver_id, process) in module.comb_processes().iter().enumerate() {
        if process.kind() == sv::ir::CombProcessKind::ContinuousAssign
            && condition_is_active(process.condition(), constants, parameter_types)
        {
            body_driver_ranges(&mut drivers, process.body(), driver_id, &scan);
        }
    }
    drivers
}

/// The ranges of `signal_name` that `initial` processes and variable
/// declaration initializers write.
pub(super) fn initial_driver_ranges(
    module: &sv::ir::Module,
    signal_name: &str,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Vec<(usize, Option<(i128, i128)>)> {
    let range = |lvalue: &sv::ir::LValue| net_lvalue_range(lvalue, constants, parameter_types);
    let scan = DriverScan {
        signal_name,
        subroutines: module.subroutines(),
        range: &range,
    };
    let mut drivers = Vec::new();
    for (driver_id, process) in module.initial_processes().iter().enumerate() {
        if !process.is_scheduled_edge()
            && condition_is_active(process.condition(), constants, parameter_types)
        {
            body_driver_ranges(&mut drivers, process.body(), driver_id, &scan);
        }
    }
    drivers
}
