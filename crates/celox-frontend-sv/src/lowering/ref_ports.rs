//! Elaborated validation and cross-instance driver analysis for SystemVerilog
//! `ref` and `const ref` ports (IEEE 1800-2023 13.5.2, 23.3.3.2).

use celox_design::ModuleId;
use celox_frontend_core::{ParserError, symbolic::artifact::HierarchicalStart};
use celox_sv_analyzer as sv;
use fxhash::{FxHashMap as HashMap, FxHashSet as HashSet};

use super::{
    DriverScan, FrontendError, LoweredSvModuleKey, SvSignalType, condition_is_active,
    expand_structure_connections, hierarchy, module_constants_with_overrides,
    module_parameter_types, net_driver_ranges_overlap, net_lvalue_range, procedural,
    runtime_select_window, signal_type_from_sv, unrolled_select_bits,
};

type InstPath = Vec<(String, usize)>;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum ObjectKey {
    InstanceVar(InstPath, String),
    PackageVar(String, String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum DriverKind {
    Combinational,
    AlwaysFf,
    AlwaysPlain,
    InitialExplicit,
}

impl DriverKind {
    fn is_exclusive(self) -> bool {
        matches!(self, Self::Combinational | Self::AlwaysFf)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum ModuleDriver {
    ContinuousAssign(usize, usize),
    CombProcess(usize),
    ChildPort(usize, usize),
    FfProcess(usize),
    InitialProcess(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ModuleWrite {
    driver: ModuleDriver,
    kind: DriverKind,
    range: Option<(i128, i128)>,
}

#[derive(Default)]
struct ModuleWrites {
    vars: HashMap<String, Vec<ModuleWrite>>,
}

impl ModuleWrites {
    fn record(
        &mut self,
        name: String,
        driver: ModuleDriver,
        kind: DriverKind,
        range: Option<(i128, i128)>,
    ) {
        let entry = ModuleWrite {
            driver,
            kind,
            range,
        };
        let list = self.vars.entry(name).or_default();
        if !list.contains(&entry) {
            list.push(entry);
        }
    }
}

#[derive(Default)]
pub(super) struct RefPortAnalysis {
    pub(super) targets: HashMap<ModuleId, HashSet<String>>,
    pub(super) same_instance_aliased: HashMap<ModuleId, HashSet<String>>,
    pub(super) same_instance_alias_groups: HashMap<ModuleId, Vec<Vec<String>>>,
    /// Per-module distinct alias partitions observed across its elaborated
    /// instances (`partition -> alias_groups -> formal_names`), including an
    /// empty partition when an instance binds every `ref` port separately.
    pub(super) same_instance_alias_partitions: HashMap<ModuleId, Vec<Vec<Vec<String>>>>,
}

fn ir_module<'a>(
    module_id: ModuleId,
    keys: &HashMap<ModuleId, &LoweredSvModuleKey>,
    analyses: &'a HashMap<ModuleId, sv::Ir>,
    discovery_analyses: &'a HashMap<ModuleId, sv::Ir>,
    forced_discovery: &'a HashMap<ModuleId, sv::Ir>,
) -> Option<&'a sv::ir::Module> {
    let name = keys.get(&module_id)?.name.as_str();
    analyses
        .get(&module_id)
        .or_else(|| discovery_analyses.get(&module_id))
        .or_else(|| forced_discovery.get(&module_id))
        .and_then(|ir| ir.modules().iter().find(|module| module.name() == name))
}

fn root_ident(expr: &sv::ir::Expr) -> Option<&str> {
    match expr {
        sv::ir::Expr::Ident(name) => Some(name.as_str()),
        sv::ir::Expr::Select { expr, .. } | sv::ir::Expr::Resize { expr, .. } => root_ident(expr),
        _ => None,
    }
}

fn collect_expr_idents(expr: &sv::ir::Expr, out: &mut Vec<String>) {
    match expr {
        sv::ir::Expr::Ident(name) => out.push(name.clone()),
        sv::ir::Expr::Select { expr, .. } | sv::ir::Expr::Resize { expr, .. } => {
            collect_expr_idents(expr, out);
        }
        sv::ir::Expr::Concat(parts) => {
            for part in parts {
                collect_expr_idents(part, out);
            }
        }
        _ => {}
    }
}

fn find_parent_var<'a>(
    module: &'a sv::ir::Module,
    name: &str,
) -> Option<(&'a sv::ir::Type, bool, bool)> {
    if let Some(port) = module.ports().iter().find(|port| port.name() == name) {
        return Some((port.r#type(), port.is_net(), port.is_const()));
    }
    if let Some(signal) = module.signals().iter().find(|signal| signal.name() == name) {
        return Some((signal.r#type(), signal.is_net(), signal.is_const()));
    }
    None
}

fn is_const_var(module: &sv::ir::Module, name: &str) -> bool {
    find_parent_var(module, name).is_some_and(|(_, _, is_const)| is_const)
}

#[derive(Clone, Copy)]
enum PhysicalBits {
    Empty,
    WholeOrRuntime,
    Bits(i128, i128),
}

fn normalize_constant_bits(info: Option<&SvSignalType>, msb: i128, lsb: i128) -> PhysicalBits {
    let Some(info) = info else {
        return PhysicalBits::Bits(msb.min(lsb), msb.max(lsb));
    };
    let (msb_pos, lsb_pos) = if info.array_dims.is_empty() {
        match info.packed_ranges.as_slice() {
            [(left, right)] if left >= right => {
                let Some((a, b)) = msb.checked_sub(*right).zip(lsb.checked_sub(*right)) else {
                    return PhysicalBits::WholeOrRuntime;
                };
                (a, b)
            }
            [(_, right)] => {
                let Some((a, b)) = right.checked_sub(msb).zip(right.checked_sub(lsb)) else {
                    return PhysicalBits::WholeOrRuntime;
                };
                (a, b)
            }
            _ => (msb, lsb),
        }
    } else {
        (msb, lsb)
    };
    let low = msb_pos.min(lsb_pos);
    let high = msb_pos.max(lsb_pos);
    let Some(top) = i128::try_from(info.width)
        .ok()
        .and_then(|w| w.checked_sub(1))
    else {
        return PhysicalBits::Bits(low, high);
    };
    let start = low.max(0);
    let end = high.min(top);
    if start > end {
        PhysicalBits::Empty
    } else {
        PhysicalBits::Bits(start, end)
    }
}

fn record_normalized_physical(
    bits: PhysicalBits,
    name: String,
    driver: ModuleDriver,
    kind: DriverKind,
    result: &mut ModuleWrites,
) {
    match bits {
        PhysicalBits::Empty => {}
        PhysicalBits::WholeOrRuntime => result.record(name, driver, kind, None),
        PhysicalBits::Bits(start, end) => result.record(name, driver, kind, Some((start, end))),
    }
}

#[allow(clippy::too_many_arguments)]
fn record_lvalue_write(
    lvalue: &sv::ir::LValue,
    loops: &procedural::LoopValues,
    driver: ModuleDriver,
    kind: DriverKind,
    signal_info: &HashMap<&str, SvSignalType>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    result: &mut ModuleWrites,
) {
    let name = lvalue.name();
    let info = signal_info.get(name);
    if let Some((msb, lsb)) = net_lvalue_range(lvalue, constants, parameter_types) {
        let bits = normalize_constant_bits(info, msb, lsb);
        record_normalized_physical(bits, name.to_owned(), driver, kind, result);
        return;
    }
    let element = match lvalue {
        sv::ir::LValue::Select {
            window: Some(window),
            ..
        } if window.at.is_none() => Some((i128::from(window.lsb), i128::from(window.msb))),
        sv::ir::LValue::Select { lsb, .. } => info
            .filter(|info| !info.array_dims.is_empty())
            .and_then(|info| {
                let elements = info.array_dims.iter().product::<usize>();
                (elements != 0).then(|| (info.width / elements, info.width))
            })
            .and_then(|(element_width, width)| {
                let window =
                    runtime_select_window(lsb, element_width, width, constants, parameter_types)?;
                Some((window.lsb as i128, window.msb as i128))
            }),
        _ => None,
    };
    let normalize_unrolled = info.is_some_and(|i| i.array_dims.is_empty())
        && !matches!(
            lvalue,
            sv::ir::LValue::Select {
                window: Some(_),
                ..
            }
        );
    match unrolled_select_bits(lvalue, None, element, loops, constants, parameter_types) {
        Some(ranges) => {
            for (low, high) in ranges {
                let bits = if normalize_unrolled {
                    normalize_constant_bits(info, high, low)
                } else {
                    PhysicalBits::Bits(low, high)
                };
                record_normalized_physical(bits, name.to_owned(), driver, kind, result);
            }
        }
        None => result.record(name.to_owned(), driver, kind, element),
    }
}

fn collect_expr_ranges(
    expr: &sv::ir::Expr,
    signal_info: &HashMap<&str, SvSignalType>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    out: &mut Vec<(String, PhysicalBits)>,
) {
    match expr {
        sv::ir::Expr::Ident(name) => out.push((name.clone(), PhysicalBits::WholeOrRuntime)),
        sv::ir::Expr::Resize { expr, .. } => {
            collect_expr_ranges(expr, signal_info, constants, parameter_types, out);
        }
        sv::ir::Expr::Select { expr, msb, lsb, .. } => {
            if let sv::ir::Expr::Ident(name) = expr.as_ref() {
                if let (Some(msb_val), Some(lsb_val)) = (
                    sv::typecheck::eval_const_expr_with_types(msb, constants, parameter_types),
                    sv::typecheck::eval_const_expr_with_types(lsb, constants, parameter_types),
                ) {
                    let bits =
                        normalize_constant_bits(signal_info.get(name.as_str()), msb_val, lsb_val);
                    out.push((name.clone(), bits));
                } else {
                    out.push((name.clone(), PhysicalBits::WholeOrRuntime));
                }
            } else {
                let mut idents = Vec::new();
                collect_expr_idents(expr, &mut idents);
                out.extend(
                    idents
                        .into_iter()
                        .map(|name| (name, PhysicalBits::WholeOrRuntime)),
                );
            }
        }
        sv::ir::Expr::Concat(parts) => {
            for part in parts {
                collect_expr_ranges(part, signal_info, constants, parameter_types, out);
            }
        }
        _ => {}
    }
}

fn collect_module_writes<'a>(
    module_id: ModuleId,
    keys: &HashMap<ModuleId, &LoweredSvModuleKey>,
    analyses: &HashMap<ModuleId, sv::Ir>,
    discovery_analyses: &HashMap<ModuleId, sv::Ir>,
    forced_discovery: &HashMap<ModuleId, sv::Ir>,
    elaborated: &HashMap<ModuleId, hierarchy::ElaboratedModule>,
    module_ids: &HashMap<LoweredSvModuleKey, ModuleId>,
    cache: &'a mut HashMap<ModuleId, ModuleWrites>,
) -> &'a ModuleWrites {
    if cache.contains_key(&module_id) {
        return &cache[&module_id];
    }
    let mut result = ModuleWrites::default();
    if let Some(module) = ir_module(
        module_id,
        keys,
        analyses,
        discovery_analyses,
        forced_discovery,
    ) {
        let constants = module_constants_with_overrides(module, &[]);
        let parameter_types = module_parameter_types(module);
        let mut signal_info: HashMap<&str, SvSignalType> = HashMap::default();
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
            if !signal_info.contains_key(name)
                && let Ok(info) = signal_type_from_sv(r#type, &constants, &parameter_types)
            {
                signal_info.insert(name, info);
            }
        }
        let scan = DriverScan::with_written_names(module);
        let locals: HashMap<&str, &sv::ir::Type> = module
            .locals()
            .iter()
            .map(|local| (local.name.as_str(), &local.r#type))
            .collect();
        let local_type = |name: &str| {
            let info = signal_type_from_sv(locals.get(name)?, &constants, &parameter_types).ok()?;
            (info.float.is_none() && info.array_dims.is_empty())
                .then_some((info.width, info.signed))
        };
        let bounds = procedural::LoopBounds {
            constants: &constants,
            parameter_types: &parameter_types,
            variable_type: &local_type,
            calls: &scan,
        };
        let outside = procedural::LoopValues::new();

        for (proc_idx, process) in module.comb_processes().iter().enumerate() {
            if !condition_is_active(process.condition(), &constants, &parameter_types) {
                continue;
            }
            if process.kind() == sv::ir::CombProcessKind::ContinuousAssign
                && !process.assignments().is_empty()
            {
                for (assign_idx, assignment) in process.assignments().iter().enumerate() {
                    record_lvalue_write(
                        assignment.lhs_value(),
                        &outside,
                        ModuleDriver::ContinuousAssign(proc_idx, assign_idx),
                        DriverKind::Combinational,
                        &signal_info,
                        &constants,
                        &parameter_types,
                        &mut result,
                    );
                }
            } else {
                let driver = if process.kind() == sv::ir::CombProcessKind::ContinuousAssign {
                    ModuleDriver::ContinuousAssign(proc_idx, 0)
                } else {
                    ModuleDriver::CombProcess(proc_idx)
                };
                let kind = if process.kind() == sv::ir::CombProcessKind::AlwaysStar {
                    DriverKind::AlwaysPlain
                } else {
                    DriverKind::Combinational
                };
                scan.body_in_loops(
                    &mut |lvalue, loops| {
                        record_lvalue_write(
                            lvalue,
                            loops,
                            driver,
                            kind,
                            &signal_info,
                            &constants,
                            &parameter_types,
                            &mut result,
                        );
                    },
                    process.body(),
                    &bounds,
                );
            }
        }
        // Child output/inout port connections drive parent variables continuously.
        if let Some(elab) = elaborated.get(&module_id) {
            for (inst_idx, instance) in elab.instances().iter().enumerate() {
                let Some(&child_id) = module_ids.get(&LoweredSvModuleKey::instance_key(instance))
                else {
                    continue;
                };
                let Some(child_mod) = ir_module(
                    child_id,
                    keys,
                    analyses,
                    discovery_analyses,
                    forced_discovery,
                ) else {
                    continue;
                };
                let connections = expand_structure_connections(
                    &instance.port_connections,
                    module,
                    child_mod,
                    None,
                )
                .ok();
                let conns = connections.as_deref().unwrap_or(&instance.port_connections);
                for (conn_idx, conn) in conns.iter().enumerate() {
                    let Some(expr) = conn.actual_expr.as_ref() else {
                        continue;
                    };
                    if child_mod.ports().iter().any(|p| {
                        p.name() == conn.formal
                            && matches!(
                                p.direction(),
                                sv::ir::PortDirection::Output | sv::ir::PortDirection::Inout
                            )
                    }) {
                        let mut ranges = Vec::new();
                        collect_expr_ranges(
                            expr,
                            &signal_info,
                            &constants,
                            &parameter_types,
                            &mut ranges,
                        );
                        for (name, bits) in ranges {
                            record_normalized_physical(
                                bits,
                                name,
                                ModuleDriver::ChildPort(inst_idx, conn_idx),
                                DriverKind::Combinational,
                                &mut result,
                            );
                        }
                    }
                }
            }
        }

        for (proc_idx, process) in module.ff_processes().iter().enumerate() {
            let kind = if process.is_always_ff() {
                DriverKind::AlwaysFf
            } else {
                DriverKind::AlwaysPlain
            };
            scan.body_in_loops(
                &mut |lvalue, loops| {
                    record_lvalue_write(
                        lvalue,
                        loops,
                        ModuleDriver::FfProcess(proc_idx),
                        kind,
                        &signal_info,
                        &constants,
                        &parameter_types,
                        &mut result,
                    );
                },
                process.body(),
                &bounds,
            );
        }
        for (proc_idx, process) in module.initial_processes().iter().enumerate() {
            if process.is_initializer()
                || process.is_final()
                || !condition_is_active(process.condition(), &constants, &parameter_types)
            {
                continue;
            }
            let kind = if !process.repeats() {
                DriverKind::InitialExplicit
            } else {
                match process.site().map(|site| site.construct) {
                    Some("always_ff") => DriverKind::AlwaysFf,
                    Some("always_comb" | "always_latch") => DriverKind::Combinational,
                    _ => DriverKind::AlwaysPlain,
                }
            };
            scan.body_in_loops(
                &mut |lvalue, loops| {
                    record_lvalue_write(
                        lvalue,
                        loops,
                        ModuleDriver::InitialProcess(proc_idx),
                        kind,
                        &signal_info,
                        &constants,
                        &parameter_types,
                        &mut result,
                    );
                },
                process.body(),
                &bounds,
            );
        }
    }
    cache.entry(module_id).or_insert(result)
}

fn validate_const_output_connections(
    instance_connections: &[super::LoweredSvPortConnection],
    parent_mod: &sv::ir::Module,
    child_mod: &sv::ir::Module,
) -> Result<(), FrontendError> {
    for connection in instance_connections {
        let Some(expr) = connection.actual_expr.as_ref() else {
            continue;
        };
        let Some(port) = child_mod
            .ports()
            .iter()
            .find(|port| port.name() == connection.formal)
        else {
            continue;
        };
        if !matches!(
            port.direction(),
            sv::ir::PortDirection::Output | sv::ir::PortDirection::Inout
        ) {
            continue;
        }
        let mut idents = Vec::new();
        collect_expr_idents(expr, &mut idents);
        for name in idents {
            if is_const_var(parent_mod, &name) {
                return Err(ParserError::illegal_context(
                    "systemverilog const ref port",
                    format!(
                        "const variable or const ref port `{name}` cannot connect to {} port `{}` of `{}`",
                        if port.direction() == sv::ir::PortDirection::Output {
                            "output"
                        } else {
                            "inout"
                        },
                        port.name(),
                        child_mod.name(),
                    ),
                    None,
                )
                .into());
            }
        }
    }
    Ok(())
}

fn non_variable_ref_port_error(
    parent_mod: &sv::ir::Module,
    actual_expr: &sv::ir::Expr,
    port: &sv::ir::Port,
    child_mod: &sv::ir::Module,
) -> FrontendError {
    if let sv::ir::Expr::Select { expr: inner, .. } = actual_expr
        && let Some(root) = root_ident(inner)
        && let Some((ty, is_net, _)) = find_parent_var(parent_mod, root)
        && !is_net
        && (!ty.unpacked_ranges().is_empty() || ty.container().is_some())
    {
        return sv::AnalyzerError::Unsupported(format!(
            "ref port `{}` of `{}` connection to an unpacked array or container element",
            port.name(),
            child_mod.name(),
        ))
        .into();
    }
    ParserError::illegal_context(
        "systemverilog ref port",
        format!(
            "ref port `{}` of `{}` requires a variable (IEEE 1800-2023 23.3.3.2)",
            port.name(),
            child_mod.name(),
        ),
        None,
    )
    .into()
}

#[allow(clippy::too_many_arguments)]
fn validate_ref_port_binding(
    actual_name: &str,
    actual_type: &sv::ir::Type,
    actual_is_net: bool,
    actual_is_const: bool,
    port: &sv::ir::Port,
    child_mod: &sv::ir::Module,
    array_element: Option<(usize, usize)>,
    design: &sv::Design,
) -> Result<(), FrontendError> {
    if actual_is_net {
        return Err(ParserError::illegal_context(
            "systemverilog ref port",
            format!(
                "net `{actual_name}` cannot connect to ref port `{}` of `{}` (IEEE 1800-2023 23.3.3.2)",
                port.name(),
                child_mod.name(),
            ),
            None,
        )
        .into());
    }
    if actual_is_const && !port.is_const() {
        return Err(ParserError::illegal_context(
            "systemverilog ref port",
            format!(
                "const variable or const ref port `{actual_name}` cannot connect to non-const ref port `{}` of `{}` (IEEE 1800-2023 13.5.2, 23.3.3.2)",
                port.name(),
                child_mod.name(),
            ),
            None,
        )
        .into());
    }
    match design.check_ref_port_type_equivalent(actual_type, port.r#type()) {
        Ok(()) => Ok(()),
        Err(sv::RefPortTypeError::QueueBoundsDiffer) => {
            Err(sv::AnalyzerError::Unsupported(format!(
                "ref port `{}` of `{}` binding queues with different bounds",
                port.name(),
                child_mod.name(),
            ))
            .into())
        }
        Err(sv::RefPortTypeError::NotEquivalent) => {
            if let Some((_, count)) = array_element
                && actual_type
                    .unpacked_ranges()
                    .first()
                    .and_then(|r| r.resolved_bounds())
                    .is_some_and(|(l, r)| l.abs_diff(r) as usize + 1 == count)
                && actual_type.unpacked_ranges().len() == port.r#type().unpacked_ranges().len() + 1
            {
                return Err(sv::AnalyzerError::Unsupported(format!(
                    "instance array element-by-element ref port connection on `{}`",
                    port.name(),
                ))
                .into());
            }
            Err(ParserError::illegal_context(
                "systemverilog ref port",
                format!(
                    "ref port `{}` of `{}` requires an actual of an equivalent type (IEEE 1800-2023 6.22.2, 23.3.3.2)",
                    port.name(),
                    child_mod.name(),
                ),
                None,
            )
            .into())
        }
    }
}

/// Validates all `ref` / `const ref` port bindings across the elaborated
/// hierarchy, rejects forced-alias and multi-driver conflicts, updates
/// `foreign_writes` / `foreign_references`, and returns aliased variable names
/// per specialization for wait/comb sensitivity tracking and same-instance
/// lowering decisions.
#[allow(clippy::too_many_arguments)]
pub(super) fn analyze(
    root_id: ModuleId,
    keys: &HashMap<ModuleId, &LoweredSvModuleKey>,
    module_ids: &HashMap<LoweredSvModuleKey, ModuleId>,
    elaborated: &HashMap<ModuleId, hierarchy::ElaboratedModule>,
    analyses: &HashMap<ModuleId, sv::Ir>,
    discovery_analyses: &HashMap<ModuleId, sv::Ir>,
    forced_discovery: &HashMap<ModuleId, sv::Ir>,
    references: &HashMap<ModuleId, Vec<hierarchy::HierarchicalReference>>,
    virtual_references: &HashMap<ModuleId, Vec<hierarchy::HierarchicalReference>>,
    design: &sv::Design,
    forced: &HashMap<ModuleId, Vec<String>>,
    foreign_writes: &mut HashMap<ModuleId, HashMap<String, hierarchy::ForeignWrites>>,
    foreign_references: &mut HashMap<ModuleId, HashSet<String>>,
) -> Result<RefPortAnalysis, FrontendError> {
    if let Some(root_mod) = ir_module(
        root_id,
        keys,
        analyses,
        discovery_analyses,
        forced_discovery,
    ) && let Some(port) = root_mod
        .ports()
        .iter()
        .find(|port| port.direction() == sv::ir::PortDirection::Ref)
    {
        return Err(sv::AnalyzerError::Unsupported(format!(
            "top-level ref port `{}` of `{}`",
            port.name(),
            root_mod.name()
        ))
        .into());
    }

    let mut ref_edges: HashMap<(InstPath, String), (InstPath, String)> = HashMap::default();
    let mut inst_modules: HashMap<InstPath, ModuleId> = HashMap::default();
    let mut queue: Vec<(InstPath, ModuleId)> = vec![(Vec::new(), root_id)];
    inst_modules.insert(Vec::new(), root_id);

    while let Some((parent_path, parent_id)) = queue.pop() {
        if parent_path.len() > module_ids.len() {
            continue;
        }
        let Some(parent_mod) = ir_module(
            parent_id,
            keys,
            analyses,
            discovery_analyses,
            forced_discovery,
        ) else {
            continue;
        };
        let Some(parent_elab) = elaborated.get(&parent_id) else {
            continue;
        };
        let mut positions: HashMap<&str, usize> = HashMap::default();
        for instance in parent_elab.instances() {
            let position = positions
                .entry(instance.instance_name.as_str())
                .or_default();
            let index = instance.array_index_base + *position;
            *position += 1;

            let Some(&child_id) = module_ids.get(&LoweredSvModuleKey::instance_key(instance))
            else {
                continue;
            };
            let Some(child_mod) = ir_module(
                child_id,
                keys,
                analyses,
                discovery_analyses,
                forced_discovery,
            ) else {
                continue;
            };

            let mut child_path = parent_path.clone();
            child_path.push((instance.instance_name.clone(), index));
            inst_modules.insert(child_path.clone(), child_id);
            queue.push((child_path.clone(), child_id));

            validate_const_output_connections(&instance.port_connections, parent_mod, child_mod)?;

            // Declared aggregate unpacked structure `ref` ports must be
            // explicitly connected to a variable of an equivalent type; exclude
            // synthetic intermediate ports synthesized for nested structures.
            let mut synthetic_struct_ports: HashSet<String> = HashSet::default();
            for port in child_mod.ports() {
                if port.r#type().structure_identity().is_some() {
                    for (member, _) in port.r#type().structure_leaves() {
                        let mut prefix = port.name().to_string();
                        for segment in member.split('.') {
                            prefix.push('.');
                            prefix.push_str(segment);
                            synthetic_struct_ports.insert(prefix.clone());
                        }
                    }
                }
            }
            for port in child_mod.ports() {
                if port.direction() != sv::ir::PortDirection::Ref
                    || port.r#type().structure_identity().is_none()
                    || synthetic_struct_ports.contains(port.name())
                {
                    continue;
                }
                let conn = instance
                    .port_connections
                    .iter()
                    .find(|c| c.formal == port.name());
                let (actual_name, actual_expr) = match conn {
                    None
                    | Some(super::LoweredSvPortConnection {
                        actual_expr: None,
                        bound_pattern: None,
                        ..
                    }) => {
                        return Err(ParserError::illegal_context(
                            "systemverilog ref port",
                            format!(
                                "ref port `{}` of `{}` cannot be left unconnected (IEEE 1800-2023 23.3.3.2)",
                                port.name(),
                                child_mod.name(),
                            ),
                            None,
                        )
                        .into());
                    }
                    Some(super::LoweredSvPortConnection {
                        bound_pattern: Some(_),
                        ..
                    }) => {
                        return Err(ParserError::illegal_context(
                            "systemverilog ref port",
                            format!(
                                "ref port `{}` of `{}` requires a variable (IEEE 1800-2023 23.3.3.2)",
                                port.name(),
                                child_mod.name(),
                            ),
                            None,
                        )
                        .into());
                    }
                    Some(super::LoweredSvPortConnection {
                        actual,
                        actual_expr: Some(actual_expr),
                        ..
                    }) => (actual.as_str(), actual_expr),
                };
                let Some((actual_type, actual_is_net, actual_is_const)) =
                    find_parent_var(parent_mod, actual_name)
                else {
                    return Err(non_variable_ref_port_error(
                        parent_mod,
                        actual_expr,
                        port,
                        child_mod,
                    ));
                };
                let is_named_struct_var = actual_expr
                    == &sv::ir::Expr::Ident(actual_name.to_owned())
                    || (!actual_type.structure_leaves().is_empty()
                        && actual_expr
                            == &sv::ir::Expr::Concat(
                                actual_type
                                    .structure_leaves()
                                    .iter()
                                    .map(|(member, _)| {
                                        sv::ir::Expr::Ident(format!("{actual_name}.{member}"))
                                    })
                                    .collect(),
                            ));
                if !is_named_struct_var {
                    return Err(non_variable_ref_port_error(
                        parent_mod,
                        actual_expr,
                        port,
                        child_mod,
                    ));
                }
                validate_ref_port_binding(
                    actual_name,
                    actual_type,
                    actual_is_net,
                    actual_is_const,
                    port,
                    child_mod,
                    instance.array_element,
                    design,
                )?;
            }

            let connections = expand_structure_connections(
                &instance.port_connections,
                parent_mod,
                child_mod,
                None,
            )?;

            for port in child_mod.ports() {
                if port.direction() != sv::ir::PortDirection::Ref
                    || port.r#type().structure_identity().is_some()
                {
                    continue;
                }
                let conn = connections.iter().find(|c| c.formal == port.name());
                let Some(connection) = conn else {
                    return Err(ParserError::illegal_context(
                        "systemverilog ref port",
                        format!(
                            "ref port `{}` of `{}` cannot be left unconnected (IEEE 1800-2023 23.3.3.2)",
                            port.name(),
                            child_mod.name(),
                        ),
                        None,
                    )
                    .into());
                };
                if connection.bound_pattern.is_some() {
                    return Err(ParserError::illegal_context(
                        "systemverilog ref port",
                        format!(
                            "ref port `{}` of `{}` requires a variable (IEEE 1800-2023 23.3.3.2)",
                            port.name(),
                            child_mod.name(),
                        ),
                        None,
                    )
                    .into());
                }
                let Some(actual_expr) = connection.actual_expr.as_ref() else {
                    return Err(ParserError::illegal_context(
                        "systemverilog ref port",
                        format!(
                            "ref port `{}` of `{}` cannot be left unconnected (IEEE 1800-2023 23.3.3.2)",
                            port.name(),
                            child_mod.name(),
                        ),
                        None,
                    )
                    .into());
                };
                let sv::ir::Expr::Ident(actual_name) = actual_expr else {
                    return Err(non_variable_ref_port_error(
                        parent_mod,
                        actual_expr,
                        port,
                        child_mod,
                    ));
                };
                let Some((actual_type, actual_is_net, actual_is_const)) =
                    find_parent_var(parent_mod, actual_name)
                else {
                    return Err(ParserError::illegal_context(
                        "systemverilog ref port",
                        format!(
                            "ref port `{}` of `{}` requires a variable (IEEE 1800-2023 23.3.3.2)",
                            port.name(),
                            child_mod.name(),
                        ),
                        None,
                    )
                    .into());
                };
                validate_ref_port_binding(
                    actual_name,
                    actual_type,
                    actual_is_net,
                    actual_is_const,
                    port,
                    child_mod,
                    instance.array_element,
                    design,
                )?;
                ref_edges.insert(
                    (child_path.clone(), port.name().to_string()),
                    (parent_path.clone(), actual_name.clone()),
                );
            }
        }
    }

    if ref_edges.is_empty() {
        return Ok(RefPortAnalysis::default());
    }

    // Group all elaborated `(InstPath, var_name)` members—including package and
    // hierarchical/virtual aliases—by their canonical owner object, keeping the
    // equivalence classes touched by at least one `ref` port binding.
    let mut alias_edges: HashMap<ObjectKey, ObjectKey> = HashMap::default();
    for ((child_path, child_var), (parent_path, parent_var)) in &ref_edges {
        alias_edges.insert(
            ObjectKey::InstanceVar(child_path.clone(), child_var.clone()),
            ObjectKey::InstanceVar(parent_path.clone(), parent_var.clone()),
        );
    }
    let mut inst_entries: Vec<_> = inst_modules
        .iter()
        .map(|(path, &mod_id)| (path.clone(), mod_id))
        .collect();
    inst_entries.sort_by(|a, b| a.0.cmp(&b.0));
    for (inst_path, mod_id) in inst_entries {
        if let Some(module) =
            ir_module(mod_id, keys, analyses, discovery_analyses, forced_discovery)
        {
            for signal in module.signals() {
                if let Some((package, variable)) = signal.package_variable()
                    && package != sv::ASSERTION_CONTROL_PACKAGE
                {
                    alias_edges
                        .entry(ObjectKey::InstanceVar(
                            inst_path.clone(),
                            signal.name().to_string(),
                        ))
                        .or_insert_with(|| {
                            ObjectKey::PackageVar(package.to_string(), variable.to_string())
                        });
                }
            }
        }
        for reference in references
            .get(&mod_id)
            .into_iter()
            .flatten()
            .chain(virtual_references.get(&mod_id).into_iter().flatten())
        {
            if reference.signal.local || reference.signal.constant.is_some() {
                continue;
            }
            let mut target_path = match reference.start {
                HierarchicalStart::Root => Vec::new(),
                HierarchicalStart::Ancestor(levels) => {
                    inst_path[..inst_path.len().saturating_sub(levels)].to_vec()
                }
            };
            target_path.extend(reference.path.iter().cloned());
            let Some(&target_mod_id) = inst_modules.get(&target_path) else {
                continue;
            };
            let Some((_, target_var)) = reference
                .targets
                .iter()
                .find(|(module, _)| *module == target_mod_id)
            else {
                continue;
            };
            alias_edges
                .entry(ObjectKey::InstanceVar(
                    inst_path.clone(),
                    reference.signal.name.clone(),
                ))
                .or_insert_with(|| ObjectKey::InstanceVar(target_path, target_var.clone()));
        }
    }

    let mut groups: HashMap<ObjectKey, Vec<(InstPath, String)>> = HashMap::default();
    let mut sources: Vec<_> = alias_edges.keys().cloned().collect();
    sources.sort();
    for source in sources {
        let mut root = alias_edges[&source].clone();
        let mut steps = 0usize;
        while let Some(next) = alias_edges.get(&root) {
            steps += 1;
            if next == &source || steps > alias_edges.len() {
                break;
            }
            root = next.clone();
        }
        if let ObjectKey::InstanceVar(inst_path, var_name) = source {
            groups.entry(root).or_default().push((inst_path, var_name));
        }
    }
    groups.retain(|_, members| members.iter().any(|member| ref_edges.contains_key(member)));

    let mut writes_cache = HashMap::default();
    let mut targets: HashMap<ModuleId, HashSet<String>> = HashMap::default();
    let mut same_instance_aliased: HashMap<ModuleId, HashSet<String>> = HashMap::default();
    let mut same_instance_alias_groups: HashMap<ModuleId, Vec<Vec<String>>> = HashMap::default();
    let mut inst_partitions: HashMap<InstPath, Vec<Vec<String>>> = HashMap::default();
    // Aggregate ForeignWrites across separate alias groups sharing a
    // `(ModuleId, var_name)` via component-wise max rather than summing across
    // unrelated instances.
    let mut best_foreign: HashMap<(ModuleId, String), hierarchy::ForeignWrites> =
        HashMap::default();

    struct GroupWrite {
        member_idx: usize,
        driver: (InstPath, ModuleDriver),
        kind: DriverKind,
        range: Option<(i128, i128)>,
    }

    let mut roots: Vec<_> = groups.into_iter().collect();
    roots.sort_by(|a, b| a.0.cmp(&b.0));

    for (root, mut members) in roots {
        let root_name = match &root {
            ObjectKey::InstanceVar(_, name) => name.clone(),
            ObjectKey::PackageVar(package, variable) => format!("{package}::{variable}"),
        };
        if let ObjectKey::InstanceVar(inst_path, var_name) = root {
            let member = (inst_path, var_name);
            if !members.contains(&member) {
                members.insert(0, member);
            }
        }

        let mut vars_by_inst: HashMap<&InstPath, Vec<&str>> = HashMap::default();
        for (inst_path, var_name) in &members {
            let mod_id = inst_modules[inst_path];
            if forced
                .get(&mod_id)
                .is_some_and(|names| names.contains(var_name))
                || ir_module(mod_id, keys, analyses, discovery_analyses, forced_discovery)
                    .is_some_and(|m| {
                        m.signals().iter().any(|signal| {
                            signal
                                .force_control()
                                .is_some_and(|ctrl| ctrl.target == *var_name)
                        })
                    })
            {
                return Err(sv::AnalyzerError::Unsupported(format!(
                    "force or release of `{root_name}`, which is aliased by a ref port"
                ))
                .into());
            }
            foreign_references
                .entry(mod_id)
                .or_default()
                .insert(var_name.clone());
            targets.entry(mod_id).or_default().insert(var_name.clone());
            let list = vars_by_inst.entry(inst_path).or_default();
            if !list.contains(&var_name.as_str()) {
                list.push(var_name.as_str());
            }
        }
        for (inst_path, var_names) in vars_by_inst {
            if var_names.len() > 1 {
                let mod_id = inst_modules[inst_path];
                let mut group: Vec<String> = var_names.into_iter().map(str::to_owned).collect();
                group.sort();
                same_instance_aliased
                    .entry(mod_id)
                    .or_default()
                    .extend(group.iter().cloned());
                let groups = same_instance_alias_groups.entry(mod_id).or_default();
                if !groups.contains(&group) {
                    groups.push(group.clone());
                }
                inst_partitions
                    .entry(inst_path.clone())
                    .or_default()
                    .push(group);
            }
        }

        let mut group_writes: Vec<GroupWrite> = Vec::new();
        for (member_idx, (inst_path, var_name)) in members.iter().enumerate() {
            let mod_id = inst_modules[inst_path];
            let writes = collect_module_writes(
                mod_id,
                keys,
                analyses,
                discovery_analyses,
                forced_discovery,
                elaborated,
                module_ids,
                &mut writes_cache,
            );
            if let Some(entries) = writes.vars.get(var_name) {
                for &w in entries {
                    group_writes.push(GroupWrite {
                        member_idx,
                        driver: (inst_path.clone(), w.driver),
                        kind: w.kind,
                        range: w.range,
                    });
                }
            }
        }

        let has_conflict = group_writes.iter().enumerate().any(|(i, left)| {
            group_writes[i + 1..].iter().any(|right| {
                left.driver != right.driver
                    && (left.kind.is_exclusive() || right.kind.is_exclusive())
                    && net_driver_ranges_overlap(left.range, right.range)
            })
        });
        if has_conflict {
            return Err(sv::AnalyzerError::Unsupported(format!(
                "multiple variable drivers for `{root_name}` through ref ports"
            ))
            .into());
        }

        for (index, (inst_path, var_name)) in members.iter().enumerate() {
            let mod_id = inst_modules[inst_path];
            let own_writes: Vec<&GroupWrite> = group_writes
                .iter()
                .filter(|w| w.member_idx == index)
                .collect();
            let own_drivers: HashSet<&(InstPath, ModuleDriver)> =
                own_writes.iter().map(|w| &w.driver).collect();
            let other_writes: Vec<&GroupWrite> = group_writes
                .iter()
                .filter(|w| !own_drivers.contains(&w.driver))
                .collect();
            if other_writes.is_empty() {
                continue;
            }
            let other_repeating_drivers: HashSet<&(InstPath, ModuleDriver)> = other_writes
                .iter()
                .filter(|w| w.kind != DriverKind::InitialExplicit)
                .map(|w| &w.driver)
                .collect();
            let repeating_overlap = other_writes.iter().enumerate().any(|(i, left)| {
                left.kind != DriverKind::InitialExplicit
                    && other_writes[i + 1..].iter().any(|right| {
                        right.kind != DriverKind::InitialExplicit
                            && left.driver != right.driver
                            && net_driver_ranges_overlap(left.range, right.range)
                    })
            });
            let repeating = if other_repeating_drivers.is_empty() {
                0
            } else if repeating_overlap {
                other_repeating_drivers.len()
            } else {
                1
            };
            let overlapping_non_exclusive = other_writes.iter().any(|fw| {
                !fw.kind.is_exclusive()
                    && own_writes
                        .iter()
                        .any(|ow| net_driver_ranges_overlap(fw.range, ow.range))
            });
            // `has_conflict` above already validated per-instance bit-range
            // driver exclusivity across the alias group; only propagate the
            // per-signal flags consumed by lowering (`repeating` and
            // `overlapping_non_exclusive`) so disjoint instances cannot
            // manufacture a whole-variable conflict.
            let entry = best_foreign.entry((mod_id, var_name.clone())).or_default();
            entry.repeating = entry.repeating.max(repeating);
            entry.overlapping_non_exclusive |= overlapping_non_exclusive;
        }
    }

    for ((mod_id, var_name), foreign) in best_foreign {
        let entry = foreign_writes
            .entry(mod_id)
            .or_default()
            .entry(var_name)
            .or_default();
        entry.repeating = entry.repeating.max(foreign.repeating);
        entry.overlapping_non_exclusive |= foreign.overlapping_non_exclusive;
    }

    let mut same_instance_alias_partitions: HashMap<ModuleId, Vec<Vec<Vec<String>>>> =
        HashMap::default();
    for (inst_path, &mod_id) in &inst_modules {
        if !same_instance_aliased.contains_key(&mod_id) {
            continue;
        }
        let mut partition = inst_partitions.get(inst_path).cloned().unwrap_or_default();
        partition.sort();
        let partitions = same_instance_alias_partitions.entry(mod_id).or_default();
        if !partitions.contains(&partition) {
            partitions.push(partition);
        }
    }
    for partitions in same_instance_alias_partitions.values_mut() {
        partitions.sort();
        partitions.dedup();
    }

    Ok(RefPortAnalysis {
        targets,
        same_instance_aliased,
        same_instance_alias_groups,
        same_instance_alias_partitions,
    })
}
