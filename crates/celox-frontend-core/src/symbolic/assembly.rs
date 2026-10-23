use num_bigint::BigUint;
use std::collections::{BTreeMap, BTreeSet};

use crate::symbolic::artifact::{
    FfPart, NbaSwitch, RelocationModule, SimModule, SplitCommit, SymbolicGlueAddr as GlueAddr,
    SymbolicRtl,
};
use crate::{
    FrontendLookup, FrontendTrace, FrontendTraceOptions, FusedSirOptimizationHints, HashMap,
    HashSet, InstancePath, ParallelScheduleOptions, ParserError, ScheduledRtl, ScheduledRtlOutput,
    SourceAddr, SourceLocation, SourceVarId, VariableInfo, flattening,
};
use celox_design::{
    BitAccess, CombDerivedClock, CounterKind, CounterSlots, DomainKind, ElaboratedDesign,
    EventTopology, EventWatch, ExternFunction, GateOp, HostRequestSite, InitialStateData,
    InitialStateValue, InstanceId, ModuleId, NbaSite, ProcessSlots, RegionedAbsoluteAddrBase,
    RegionedStateAddr, RuntimeCombObserver, RuntimeErrorInfo, RuntimeEventKind, RuntimeEventSite,
    RuntimeSchema, STABLE_REGION, StateAddr, StateObjectId, TriggerSet, VarAtomBase,
    VariableMetadata, WORKING_REGION, WaitSite,
};
use celox_sir::{
    BasicBlock, ExecutionUnit, LaneUnit, ParallelSirProgram, SIRInstruction, SIROffset,
    SIRTerminator, SirProgram,
};
use celox_slt::{
    CombObserver, FfAccessSummary, LogicPath, LogicPathId, LogicPathTarget, NodeId, SLTNodeArena,
    scheduler::{self, SchedulerError},
};

type AbsoluteAddr = SourceAddr;
type RegionedAbsoluteAddr = RegionedAbsoluteAddrBase<SourceVarId>;

/// Per canonical event, the split commit of each of its apply units, in
/// their order; `None` for a unit whose frontend does not split it.
type SplitCommits = HashMap<AbsoluteAddr, Vec<Option<SplitCommit<RegionedAbsoluteAddr>>>>;
type RegionedVarAddr = celox_design::RegionedVarAddrBase<SourceVarId>;

/// Runtime IDs assigned while a module instance is relocated into the global
/// design. Source adapters use these IDs only when rebuilding an optimized FF
/// action inside the shared comb/FF scheduler.
#[derive(Clone, Debug)]
pub struct FfRuntimeRelocation {
    pub error_codes: HashMap<i64, i64>,
    pub event_site_base: u32,
}

/// Source-neutral description of one FF action offered to an adapter-specific
/// lowering implementation.
#[derive(Clone)]
pub struct FusedFfAction {
    pub id: usize,
    pub instance_id: InstanceId,
    pub module_id: ModuleId,
    pub trigger: TriggerSet<SourceVarId>,
    pub summary: FfAccessSummary<RegionedAbsoluteAddrBase<SourceVarId>>,
    pub runtime: FfRuntimeRelocation,
}

/// Adapter hook for source-aware FF lowering used by shared-clock scheduling,
/// with or without a preceding comb phase. The scheduler and all identities
/// crossing this trait remain source neutral.
pub trait FusedFfLoweringFactory {
    fn create(
        &self,
        actions: Vec<FusedFfAction>,
    ) -> Result<
        Box<
            dyn scheduler::ClockFfLowering<
                    RegionedAbsoluteAddrBase<SourceVarId>,
                    Error = ParserError,
                > + '_,
        >,
        ParserError,
    >;
}

fn elaborated_scope_name(
    root_name: &str,
    path: &InstancePath,
    expanded: &HashMap<InstancePath, InstanceId>,
    indexed_instances: &HashSet<InstanceId>,
) -> String {
    let mut prefix = Vec::with_capacity(path.0.len());
    let segments = path
        .0
        .iter()
        .map(|(name, index)| {
            prefix.push((name.clone(), *index));
            let indexed = expanded
                .get(&InstancePath(prefix.clone()))
                .is_some_and(|id| indexed_instances.contains(id));
            if indexed {
                format!("{name}[{index}]")
            } else {
                name.clone()
            }
        })
        .collect::<Vec<_>>();
    if segments.is_empty() {
        root_name.to_string()
    } else {
        format!("{root_name}.{}", segments.join("."))
    }
}

fn flatten_with_trace(
    module: &SimModule,
    path: &InstancePath,
    instance_ids: &HashMap<InstancePath, InstanceId>,
    global_boundaries: &HashMap<AbsoluteAddr, BTreeSet<usize>>,
    unpacked_element_widths: &HashMap<AbsoluteAddr, usize>,
    arena: &mut SLTNodeArena<AbsoluteAddr>,
    bindings: &PackageBindings,
    trace_opts: &FrontendTraceOptions,
    mut trace: Option<&mut FrontendTrace>,
) -> Result<RelocationModule, celox_slt::SLTNodeFactsError> {
    let flattened = flattening::flatten_module(
        module,
        path,
        instance_ids,
        global_boundaries,
        unpacked_element_widths,
        arena,
        &|instance_id, var_id| bindings.locate(instance_id, var_id),
    )?;

    if let Some(trace) = trace.as_deref_mut()
        && trace_opts.pre_atomized_comb_blocks
    {
        match &mut trace.pre_atomized_comb_blocks {
            Some((blocks, trace_arena)) => {
                blocks.extend(flattened.pre_atomized_comb_blocks);
                *trace_arena = arena.clone();
            }
            slot @ None => *slot = Some((flattened.pre_atomized_comb_blocks, arena.clone())),
        }
    }

    if let Some(trace) = trace
        && trace_opts.atomized_comb_blocks
    {
        match &mut trace.atomized_comb_blocks {
            Some((blocks, trace_arena)) => {
                blocks.extend(flattened.relocation.comb_blocks.iter().cloned());
                *trace_arena = arena.clone();
            }
            slot @ None => {
                *slot = Some((flattened.relocation.comb_blocks.clone(), arena.clone()));
            }
        }
    }

    Ok(flattened.relocation)
}

fn remap_for_fold_runtime_event_sites<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    start: usize,
    runtime_event_site_map: &HashMap<u32, u32>,
) -> Result<(), ParserError> {
    arena
        .remap_for_fold_effect_sites(start..arena.len(), |site_id, fatal_error_code| {
            Ok(runtime_event_site_map.get(&site_id).map(|&global_site| {
                (
                    global_site,
                    fatal_error_code.map(|_| i64::from(global_site)),
                )
            }))
        })
        .map_err(|error| {
            ParserError::illegal_context(
                "ForFold runtime-event relocation",
                error.to_string(),
                None,
            )
        })
}

/// Where the variables of each instance live. A module variable bound to a
/// package variable (IEEE 1800-2023 26.2) is that package's one object, in
/// the package's instance; every other variable belongs to its instance.
#[derive(Default)]
pub(crate) struct PackageBindings {
    targets: HashMap<AbsoluteAddr, AbsoluteAddr>,
}

impl PackageBindings {
    fn new(
        instance_modules: &HashMap<InstanceId, ModuleId>,
        modules: &HashMap<ModuleId, SimModule>,
        package_instances: &HashMap<ModuleId, InstanceId>,
    ) -> Result<Self, ParserError> {
        let mut targets = HashMap::default();
        for (&instance_id, module_id) in instance_modules {
            let module = &modules[module_id];
            for (&var_id, binding) in &module.package_bindings {
                let package_instance =
                    package_instances.get(&binding.package).ok_or_else(|| {
                        ParserError::illegal_context(
                            "package variable binding",
                            format!(
                                "module `{}` binds a variable to a module that is not a package",
                                module.name
                            ),
                            None,
                        )
                    })?;
                let package = &modules[&binding.package];
                let (Some(variable), Some(target)) = (
                    module.variables.get(&var_id),
                    package.variables.get(&binding.var_id),
                ) else {
                    return Err(ParserError::illegal_context(
                        "package variable binding",
                        format!(
                            "module `{}` binds an unknown variable of package `{}`",
                            module.name, package.name
                        ),
                        None,
                    ));
                };
                if variable.metadata != target.metadata {
                    return Err(ParserError::illegal_context(
                        "package variable binding",
                        format!(
                            "module `{}` binds a variable to `{}` of package `{}` with another \
                             type",
                            module.name,
                            target.path.join("."),
                            package.name
                        ),
                        None,
                    ));
                }
                targets.insert(
                    AbsoluteAddr {
                        instance_id,
                        var_id,
                    },
                    AbsoluteAddr {
                        instance_id: *package_instance,
                        var_id: binding.var_id,
                    },
                );
            }
        }
        Ok(Self { targets })
    }

    /// The object variable `var_id` of instance `instance_id` denotes.
    pub(crate) fn locate(&self, instance_id: InstanceId, var_id: SourceVarId) -> AbsoluteAddr {
        let address = AbsoluteAddr {
            instance_id,
            var_id,
        };
        self.targets.get(&address).copied().unwrap_or(address)
    }

    pub(crate) fn locate_regioned(
        &self,
        region: u32,
        instance_id: InstanceId,
        var_id: SourceVarId,
    ) -> RegionedAbsoluteAddr {
        let address = self.locate(instance_id, var_id);
        RegionedAbsoluteAddr {
            region,
            instance_id: address.instance_id,
            var_id: address.var_id,
        }
    }

    /// Whether the variable is bound to a package variable, and so has no
    /// state object of its own.
    fn is_bound(&self, address: &AbsoluteAddr) -> bool {
        self.targets.contains_key(address)
    }
}

/// Reject a package variable that two flip-flop processes write, that the
/// initial writes of two instances write, or that a continuous driver and
/// any other process write. Instances of one module are separate drivers.
/// Overlapping continuous drivers are rejected by the scheduler.
fn check_package_variable_drivers(
    bindings: &PackageBindings,
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
    package_instances: &HashMap<ModuleId, InstanceId>,
    comb_blocks: &[celox_slt::LogicPath<AbsoluteAddr>],
) -> Result<(), ParserError> {
    let packages: HashSet<AbsoluteAddr> = bindings.targets.values().copied().collect();
    if packages.is_empty() {
        return Ok(());
    }
    let width = |address: &AbsoluteAddr| {
        modules[&instance_modules[&address.instance_id]].variables[&address.var_id]
            .metadata
            .width
    };
    let error = |address: &AbsoluteAddr| {
        let package = &modules[&instance_modules[&address.instance_id]];
        ParserError::unsupported(
            1146,
            crate::LoweringPhase::SimulatorParser,
            "package variable drivers",
            format!(
                "multiple drivers of package variable `{}::{}`",
                package.name,
                package.variables[&address.var_id].path.join(".")
            ),
            None,
        )
    };
    // Units of one clock are merged across instances when relocated, so
    // writers are told apart before: one per trigger group of an instance.
    let mut ff_writes: Vec<((InstanceId, usize), AbsoluteAddr, BitAccess)> = Vec::new();
    let mut instances = instance_modules.iter().collect::<Vec<_>>();
    instances.sort_unstable_by_key(|(instance, _)| instance.0);
    for (&instance, module_id) in instances {
        let module = &modules[module_id];
        let units = module.eval_apply_ff_blocks.iter().chain(
            module
                .eval_only_ff_blocks
                .iter()
                .filter(|(trigger, _)| !module.eval_apply_ff_blocks.contains_key(*trigger)),
        );
        for (group, (_, unit)) in units.enumerate() {
            for block in unit.blocks.values() {
                for instruction in &block.instructions {
                    let (address, offset, bits) = match instruction {
                        SIRInstruction::Store(address, offset, bits, ..)
                        | SIRInstruction::Commit(_, address, offset, bits, _) => {
                            (bindings.locate(instance, address.var_id), offset, *bits)
                        }
                        _ => continue,
                    };
                    if !packages.contains(&address) {
                        continue;
                    }
                    let access = offset
                        .constant_bit_offset()
                        .zip(bits.checked_sub(1))
                        .and_then(|(lsb, tail)| Some(BitAccess::new(lsb, lsb.checked_add(tail)?)))
                        .unwrap_or_else(|| BitAccess::new(0, width(&address).saturating_sub(1)));
                    ff_writes.push(((instance, group), address, access));
                }
            }
        }
    }
    // Initial writes of module instances: constant ones folded into initial
    // values, and initial processes. The package's own declaration
    // initializers are not drivers.
    let mut initial_writes: Vec<((InstanceId, usize), AbsoluteAddr, BitAccess)> = Vec::new();
    for (&instance, module_id) in instance_modules {
        let module = &modules[module_id];
        if package_instances.contains_key(module_id) {
            continue;
        }
        for initial in &module.initial_memory_values {
            let address = bindings.locate(instance, initial.address);
            if !packages.contains(&address) {
                continue;
            }
            // The bits the initial value writes.
            let accesses = match &initial.data {
                celox_design::InitialStateData::Packed { written_mask, .. } => {
                    let bits = written_mask.bits() as usize;
                    written_mask
                        .trailing_zeros()
                        .map(|lsb| vec![BitAccess::new(lsb as usize, bits.saturating_sub(1))])
                        .unwrap_or_default()
                }
                celox_design::InitialStateData::Writes(runs) => runs
                    .iter()
                    .filter(|run| run.bit_width > 0)
                    .map(|run| BitAccess::new(run.bit_offset, run.bit_offset + run.bit_width - 1))
                    .collect(),
            };
            for access in accesses {
                initial_writes.push(((instance, usize::MAX), address, access));
            }
        }
        for (index, process) in module.processes.iter().enumerate() {
            for block in process.kernel.blocks.values() {
                for instruction in &block.instructions {
                    let (SIRInstruction::Store(address, ..)
                    | SIRInstruction::Commit(_, address, ..)) = instruction
                    else {
                        continue;
                    };
                    let address = bindings.locate(instance, address.var_id);
                    if packages.contains(&address) {
                        let access = BitAccess::new(0, width(&address).saturating_sub(1));
                        initial_writes.push(((instance, index), address, access));
                    }
                }
            }
        }
    }
    let conflict =
        |writes: &[((InstanceId, usize), AbsoluteAddr, BitAccess)],
         distinct: &dyn Fn(&(InstanceId, usize), &(InstanceId, usize)) -> bool| {
            writes
                .iter()
                .enumerate()
                .find_map(|(index, (writer, address, access))| {
                    writes[index + 1..]
                        .iter()
                        .any(|(other, other_address, other_access)| {
                            distinct(writer, other)
                                && other_address == address
                                && other_access.overlaps(access)
                        })
                        .then_some(*address)
                })
        };
    // Two flip-flop processes, or initial writes of two instances, race.
    if let Some(address) = conflict(&ff_writes, &|left, right| left != right) {
        return Err(error(&address));
    }
    if let Some(address) = conflict(&initial_writes, &|left, right| left.0 != right.0) {
        return Err(error(&address));
    }
    // A continuous driver overlaps every other write.
    for path in comb_blocks {
        let Some(target) = path.target.var() else {
            continue;
        };
        if ff_writes
            .iter()
            .chain(&initial_writes)
            .any(|(_, address, access)| *address == target.id && access.overlaps(&target.access))
        {
            return Err(error(&target.id));
        }
    }
    Ok(())
}

fn create_absolute_addr(
    instance_path: &[(String, usize)],
    var_path: &[String],
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
    expanded: &HashMap<InstancePath, InstanceId>,
) -> AbsoluteAddr {
    let instance_path = InstancePath(instance_path.to_vec());
    let instance_id = expanded[&instance_path];
    let module_id = instance_modules[&instance_id];
    let module = &modules[&module_id];
    let var_id = *module
        .variables
        .iter()
        .find(|(_, variable)| variable.path == var_path)
        .unwrap()
        .0;
    AbsoluteAddr {
        instance_id,
        var_id,
    }
}
fn parse_ignored_loops(
    ignored_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
    )],
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
    expanded: &HashMap<InstancePath, InstanceId>,
) -> HashSet<(AbsoluteAddr, AbsoluteAddr)> {
    let mut res = HashSet::default();

    for ((from_instance_path, from_var_path), (to_instance_path, to_var_path)) in ignored_loops {
        let from = create_absolute_addr(
            from_instance_path,
            from_var_path,
            instance_modules,
            modules,
            expanded,
        );
        let to = create_absolute_addr(
            to_instance_path,
            to_var_path,
            instance_modules,
            modules,
            expanded,
        );
        res.insert((from, to));
    }
    res
}
fn parse_true_loops(
    true_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
        usize,
    )],
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
    expanded: &HashMap<InstancePath, InstanceId>,
) -> HashMap<(AbsoluteAddr, AbsoluteAddr), usize> {
    let mut res = HashMap::default();

    for ((from_instance_path, from_var_path), (to_instance_path, to_var_path), max_iter) in
        true_loops
    {
        let from = create_absolute_addr(
            from_instance_path,
            from_var_path,
            instance_modules,
            modules,
            expanded,
        );
        let to = create_absolute_addr(
            to_instance_path,
            to_var_path,
            instance_modules,
            modules,
            expanded,
        );
        res.insert((from, to), *max_iter);
    }
    res
}

fn scheduler_source_locations(
    error: &SchedulerError<AbsoluteAddr>,
    modules: &HashMap<ModuleId, SimModule>,
    instance_modules: &HashMap<InstanceId, ModuleId>,
) -> Vec<SourceLocation> {
    let blocks = match error {
        SchedulerError::CombinationalLoop { blocks } => blocks,
        SchedulerError::MultipleDriver { blocks } => blocks,
        SchedulerError::InvalidDependencyGraph => return Vec::new(),
    };
    let mut seen = HashSet::default();
    blocks
        .iter()
        .filter_map(|block| {
            let addr = block.target.var()?.id;
            if !seen.insert(addr) {
                return None;
            }
            let module_id = instance_modules.get(&addr.instance_id)?;
            let module = modules.get(module_id)?;
            let var = module.variables.get(&addr.var_id)?;
            var.source.clone()
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub fn schedule_symbolic_rtl(
    symbolic: SymbolicRtl,
    fused_ff_factory: Option<&dyn FusedFfLoweringFactory>,
    ignored_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
    )],
    true_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
        usize,
    )],
    four_state: bool,
    parallel: &ParallelScheduleOptions,
    trace_opts: &FrontendTraceOptions,
    trace: Option<&mut FrontendTrace>,
) -> Result<ScheduledRtlOutput, ParserError> {
    schedule_symbolic_rtl_reporting_loops(
        symbolic,
        fused_ff_factory,
        ignored_loops,
        true_loops,
        four_state,
        parallel,
        trace_opts,
        trace,
        &mut Vec::new(),
    )
}

/// A range a block of a combinational loop writes, in the module that
/// declares it.
pub type LoopMember = (ModuleId, VarAtomBase<SourceVarId>);

/// [`schedule_symbolic_rtl`], which also reports the ranges the blocks of a
/// combinational loop write when the scheduler rejects one.
#[allow(clippy::too_many_arguments)]
pub fn schedule_symbolic_rtl_reporting_loops(
    symbolic: SymbolicRtl,
    fused_ff_factory: Option<&dyn FusedFfLoweringFactory>,
    ignored_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
    )],
    true_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
        usize,
    )],
    four_state: bool,
    parallel: &ParallelScheduleOptions,
    trace_opts: &FrontendTraceOptions,
    mut trace: Option<&mut FrontendTrace>,
    loop_members: &mut Vec<LoopMember>,
) -> Result<ScheduledRtlOutput, ParserError> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_SOURCE_PROGRAM: AtomicU64 = AtomicU64::new(1);
    let _diagnostic = tracing::trace_span!(target: "celox::parallel", "source_program",
        source_program = NEXT_SOURCE_PROGRAM
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("source program identity exhausted").to_string())
    .entered();
    let SymbolicRtl {
        modules,
        module_names,
        root_id,
        packages,
    } = symbolic;
    let time_precision = modules
        .values()
        .filter_map(|module| module.time_precision)
        .min()
        .unwrap_or_default();
    let flatten_timing = trace_opts.phase_timing;
    macro_rules! timed_sub {
        ($label:expr, $body:expr) => {{
            if flatten_timing {
                let start = std::time::Instant::now();
                let result = $body;
                tracing::debug!("[flatten] {}: {:?}", $label, start.elapsed());
                result
            } else {
                $body
            }
        }};
    }

    if let Some(t) = trace.as_deref_mut()
        && trace_opts.sim_modules
    {
        t.sim_modules = Some(modules.clone());
    }

    let (mut expanded, mut instance_modules, indexed_instances) =
        timed_sub!("expand_hierarchy", expand_hierarchy(&root_id, &modules));
    // Each package with variables is one instance outside the hierarchy,
    // named by a path segment no instance name can take.
    let mut package_instances = HashMap::default();
    for (name, module_id) in &packages {
        let id = InstanceId(instance_modules.len());
        expanded.insert(InstancePath(vec![(format!("{name}::"), 0)]), id);
        instance_modules.insert(id, *module_id);
        package_instances.insert(*module_id, id);
    }
    let instance_time_scales = instance_modules
        .iter()
        .map(|(&instance, module)| (instance, modules[module].time_scale))
        .collect::<HashMap<_, _>>();
    let bindings = &PackageBindings::new(&instance_modules, &modules, &package_instances)?;
    let global_boundaries = timed_sub!(
        "propagate_boundaries",
        propagate_boundaries(bindings, &expanded, &instance_modules, &modules)
    );
    let unpacked_element_widths = instance_modules
        .iter()
        .flat_map(|(&instance_id, &module_id)| {
            modules[&module_id]
                .variables
                .iter()
                .filter_map(move |(&var_id, variable)| {
                    let element_count = variable
                        .metadata
                        .array_dims
                        .iter()
                        .try_fold(1usize, |total, &dim| total.checked_mul(dim))?;
                    let element_width = variable.metadata.width.checked_div(element_count)?;
                    (element_count > 1 && element_width > 0)
                        .then_some((bindings.locate(instance_id, var_id), element_width))
                })
        })
        .collect::<HashMap<_, _>>();

    let clock_domains = timed_sub!(
        "unify_clock_domains",
        unify_clock_domains(bindings, &expanded, &instance_modules, &modules)
    );
    let mut parallel_ff_units = HashMap::default();
    let mut split_commits = SplitCommits::default();
    let (
        mut global_arena,
        mut eval_apply_ffs,
        mut eval_only_ffs,
        mut apply_ffs,
        _ff_access_summaries,
        ff_runtime_relocations,
        mut comb_blocks,
        mut comb_observers,
        mut runtime_errors,
        runtime_event_sites,
        extern_functions,
        next_runtime_error_code,
        mut processes,
    ) = timed_sub!(
        "relocate_units",
        relocate_units(
            bindings,
            &expanded,
            &instance_modules,
            &modules,
            &module_names[&root_id],
            &indexed_instances,
            &global_boundaries,
            &unpacked_element_widths,
            &clock_domains,
            parallel.enabled().then_some(&mut parallel_ff_units),
            &mut split_commits,
            trace_opts,
            &mut trace,
        )
    )?;
    let mut timing = relocate_timing(bindings, &instance_modules, &modules, &processes);
    let variables: HashMap<AbsoluteAddr, (usize, bool)> = instance_modules
        .iter()
        .flat_map(|(&instance, module)| {
            modules[module].variables.iter().map(move |(&var_id, var)| {
                (
                    bindings.locate(instance, var_id),
                    (var.metadata.width, var.metadata.is_4state),
                )
            })
        })
        .collect();
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
        let counted = counted_signals(&timing, &event_addresses);
        choose_comb_alternatives(
            bindings,
            &instance_modules,
            &modules,
            &counted,
            &mut comb_blocks,
        );
        // Count sensitivity occurrences before any optimizer can coalesce
        // consecutive writes whose final value is unchanged.
        let mut probes = Vec::new();
        for path in &comb_blocks {
            if let LogicPathTarget::Process {
                sensitivity_counters,
                ..
            } = &path.target
            {
                for &(source, counter) in sensitivity_counters {
                    if !probes.contains(&(source, counter)) {
                        probes.push((source, counter));
                    }
                }
            }
        }
        for observer in &comb_observers {
            for &(source, counter) in &observer.sensitivity_counters {
                if !probes.contains(&(source, counter)) {
                    probes.push((source, counter));
                }
            }
        }
        // A direct wire/port connection carries occurrences as well as its
        // settled value. Preserve them when a kernel writes the upstream
        // object twice before the copy path is evaluated. Only equal-width,
        // unindexed copies are transparent here; expressions remain scheduled
        // combinational evaluations.
        let mut next_probe = 0;
        while next_probe < probes.len() {
            let (source, counter) = probes[next_probe];
            next_probe += 1;
            for path in &comb_blocks {
                let LogicPathTarget::Var(target) = &path.target else {
                    continue;
                };
                if !path.transparent_copy
                    || target.id != source.id
                    || !target.access.overlaps(&source.access)
                {
                    continue;
                }
                let celox_slt::SLTNode::Input {
                    variable,
                    access,
                    index,
                    ..
                } = global_arena.get(path.expr)
                else {
                    continue;
                };
                if !index.is_empty()
                    || (four_state && variables[variable].1 != variables[&target.id].1)
                    || access.msb - access.lsb != target.access.msb - target.access.lsb
                {
                    continue;
                }
                let lsb = source.access.lsb.max(target.access.lsb);
                let msb = source.access.msb.min(target.access.msb);
                let upstream = VarAtomBase::new(
                    *variable,
                    access.lsb + lsb - target.access.lsb,
                    access.lsb + msb - target.access.lsb,
                );
                if !probes.contains(&(upstream, counter)) {
                    probes.push((upstream, counter));
                }
            }
        }
        declaration_probes = probes.clone();
        occurrence_probes = probes.clone();
        // Signals a flip-flop evaluation may store twice in its working
        // state. Each store is an update event (IEEE 1800-2023 4.3, 10.4.2)
        // that the commit of the final value alone would not show; the
        // working state is seeded with the stable value first, so a store
        // compares with the value its update replaces.
        let ff_multi_stored = ff_multi_stored(&eval_apply_ffs, &eval_only_ffs, &parallel_ff_units);
        for (source, counter) in probes {
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
            let stable = |address| RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, address);
            let code = |address: &RegionedAbsoluteAddr| {
                (address.region == STABLE_REGION && address.absolute_addr() == source.id).then(
                    || crate::watch::WatchCode {
                        signal: stable(source.id),
                        width,
                        offset: offset.clone(),
                        four_state,
                        counters: vec![(CounterKind::Changes, stable(counter))],
                    },
                )
            };
            for process in &mut processes {
                crate::watch::instrument_unit(&mut process.kernel, code);
            }
            for units in eval_apply_ffs
                .values_mut()
                .chain(eval_only_ffs.values_mut())
                .chain(apply_ffs.values_mut())
            {
                for unit in units {
                    crate::watch::instrument_unit(unit, code);
                }
            }
            for parts in parallel_ff_units.values_mut() {
                for (_, part) in parts {
                    crate::watch::instrument_unit(&mut part.evaluate, code);
                    crate::watch::instrument_unit(&mut part.apply, code);
                }
            }
            if ff_multi_stored.contains(&source.id) {
                let working =
                    |address| RegionedAbsoluteAddr::from_absolute_addr(WORKING_REGION, address);
                let code = |address: &RegionedAbsoluteAddr| {
                    (address.region == WORKING_REGION && address.absolute_addr() == source.id).then(
                        || crate::watch::WatchCode {
                            signal: working(source.id),
                            width,
                            offset: offset.clone(),
                            four_state,
                            counters: vec![(CounterKind::Changes, stable(counter))],
                        },
                    )
                };
                for_each_ff_evaluation(
                    &mut eval_apply_ffs,
                    &mut eval_only_ffs,
                    &mut parallel_ff_units,
                    |unit| instrument_stores(unit, code),
                );
            }
            for path in &mut comb_blocks {
                if let LogicPathTarget::Process { unit, .. } = &mut path.target {
                    crate::watch::instrument_unit(unit, |address| {
                        (*address == source.id).then(|| crate::watch::WatchCode {
                            signal: source.id,
                            width,
                            offset: offset.clone(),
                            four_state,
                            counters: vec![(CounterKind::Changes, counter)],
                        })
                    });
                }
            }
        }
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
            &timing,
            counted,
            &variables,
            four_state,
            &mut processes,
            &mut comb_blocks,
            ff_units,
        );
        // A counted signal without pending slots (a design without waits)
        // is counted at its commit, which sees only the final value. Count
        // each store of an evaluation that may store it twice as well.
        let pending: HashSet<AbsoluteAddr> = timing
            .pending_slots
            .iter()
            .map(|slots| slots.signal)
            .collect();
        for (signal, slots) in &counted {
            if !ff_multi_stored.contains(signal) || pending.contains(signal) {
                continue;
            }
            let (width, is_4state) = variables[signal];
            let regioned = RegionedAbsoluteAddr::from_absolute_addr;
            let code = |address: &RegionedAbsoluteAddr| {
                (address.region == WORKING_REGION && address.absolute_addr() == *signal).then(
                    || crate::watch::WatchCode {
                        signal: regioned(WORKING_REGION, *signal),
                        offset: celox_sir::SIROffset::Static(0),
                        width,
                        four_state: four_state && is_4state,
                        counters: CounterKind::ALL
                            .into_iter()
                            .map(|kind| (kind, regioned(STABLE_REGION, slots.counter(kind))))
                            .collect(),
                    },
                )
            };
            for_each_ff_evaluation(
                &mut eval_apply_ffs,
                &mut eval_only_ffs,
                &mut parallel_ff_units,
                |unit| instrument_stores(unit, code),
            );
        }
        counted
    };
    // Combinational values and the variables they are computed from.
    let comb_sources = comb_sources(&comb_blocks, &global_arena);
    check_package_variable_drivers(
        bindings,
        &instance_modules,
        &modules,
        &package_instances,
        &comb_blocks,
    )?;
    let ignored_loops = parse_ignored_loops(ignored_loops, &instance_modules, &modules, &expanded);
    let true_loops = parse_true_loops(true_loops, &instance_modules, &modules, &expanded);

    flattening::refine_cyclic_logic_paths(
        &mut comb_blocks,
        &global_boundaries,
        &unpacked_element_widths,
        &mut global_arena,
    )?;

    // Build reset -> clock mapping with AbsoluteAddr
    let mut reset_clock_map: HashMap<AbsoluteAddr, AbsoluteAddr> = HashMap::default();
    for id in expanded.values() {
        let module_id = &instance_modules[id];
        let sim_module = &modules[module_id];
        for (reset_var_id, clock_var_id) in &sim_module.reset_clock_map {
            let reset_addr = bindings.locate(*id, *reset_var_id);
            let clock_addr = bindings.locate(*id, *clock_var_id);
            // Use canonical clock domain if available
            let canonical_clock = clock_domains
                .get(&clock_addr)
                .copied()
                .unwrap_or(clock_addr);
            let canonical_reset = clock_domains
                .get(&reset_addr)
                .copied()
                .unwrap_or(reset_addr);
            reset_clock_map.insert(canonical_reset, canonical_clock);
        }
    }

    let ClockAnalysis {
        topological_clocks,
        cascaded_clocks,
        derived_events,
        combinational_derived,
        independent_derived,
        combinationally_read,
    } = timed_sub!(
        "analyze_clock_dependencies",
        analyze_clock_dependencies(
            bindings,
            &mut eval_apply_ffs,
            &mut eval_only_ffs,
            &mut apply_ffs,
            &comb_blocks,
            &global_arena,
            &clock_domains,
            &expanded,
            &instance_modules,
            &modules,
        )
    );

    if let Some(t) = trace.as_deref_mut()
        && trace_opts.flattened_comb_blocks
    {
        t.flattened_comb_blocks = Some((comb_blocks.clone(), global_arena.clone()));
    }

    // Constant variable inlining: detect variables whose every LogicPath
    // is a constant, then replace all Input references with Constant nodes.
    // This eliminates Store→Load roundtrips for compile-time constants
    // (e.g. genvar-expanded parity-check matrices).
    celox_slt::const_inline::inline_constant_variables(&mut comb_blocks, &mut global_arena)?;
    apply_always_comb_previous_source_ordering(&mut comb_blocks);

    let var_widths: HashMap<AbsoluteAddr, usize> = instance_modules
        .iter()
        .flat_map(|(&inst_id, &mod_id)| {
            modules[&mod_id]
                .variables
                .iter()
                .map(move |(var_id, var)| (bindings.locate(inst_id, *var_id), var.metadata.width))
        })
        .collect();
    let var_signedness: HashMap<AbsoluteAddr, bool> = instance_modules
        .iter()
        .flat_map(|(&inst_id, &mod_id)| {
            modules[&mod_id]
                .variables
                .iter()
                .map(move |(var_id, var)| (bindings.locate(inst_id, *var_id), var.signed))
        })
        .collect();

    build_comb_observer_capture_paths(
        &mut comb_blocks,
        &mut comb_observers,
        &runtime_event_sites,
        &mut global_arena,
        &unpacked_element_widths,
    )?;
    for (site_id, site) in runtime_event_sites.iter().enumerate() {
        if !matches!(site.kind, RuntimeEventKind::AssertFatal) {
            continue;
        }
        runtime_errors
            .entry(site_id as i64)
            .or_insert_with(|| RuntimeErrorInfo {
                message: site
                    .template
                    .clone()
                    .unwrap_or_else(|| "assertion failed".to_string()),
                signals: Vec::new(),
            });
    }

    celox_slt::verify_symbolic_roots(
        &global_arena,
        &comb_blocks,
        &comb_observers,
        &var_widths,
        &var_signedness,
    )
    .map_err(|error| ParserError::SltVerify {
        phase: "after flattening symbolic logic",
        error,
    })?;

    let fused_inputs = if fused_ff_factory.is_some() {
        let actions = build_fused_ff_actions(
            bindings,
            &modules,
            &instance_modules,
            &clock_domains,
            &ff_runtime_relocations,
        );
        let mut clock_arena = SLTNodeArena::<RegionedAbsoluteAddr>::new();
        let mut clock_node_cache = HashMap::default();
        let clock_comb_blocks = comb_blocks
            .iter()
            .map(|path| {
                path.map_addr(
                    &global_arena,
                    &mut clock_arena,
                    &mut clock_node_cache,
                    &|addr| RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, *addr),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let clock_var_widths = var_widths
            .iter()
            .map(|(&addr, &width)| {
                (
                    RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, addr),
                    width,
                )
            })
            .collect::<HashMap<_, _>>();
        let clock_unpacked_element_widths = unpacked_element_widths
            .iter()
            .map(|(&addr, &element_width)| {
                (
                    RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, addr),
                    element_width,
                )
            })
            .collect::<HashMap<_, _>>();
        let clock_ignored_loops = ignored_loops
            .iter()
            .map(|&(from, to)| {
                (
                    RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, from),
                    RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, to),
                )
            })
            .collect::<HashSet<_>>();
        let clock_true_loops = true_loops
            .iter()
            .map(|(&(from, to), &limit)| {
                (
                    (
                        RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, from),
                        RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, to),
                    ),
                    limit,
                )
            })
            .collect::<HashMap<_, _>>();
        Some((
            actions,
            clock_arena,
            clock_comb_blocks,
            clock_var_widths,
            clock_unpacked_element_widths,
            clock_ignored_loops,
            clock_true_loops,
        ))
    } else {
        None
    };

    // Stores to event signals update shared trigger bytes; their writers stay
    // in one lane of a partitioned schedule.
    let lane0_objects = parallel.enabled().then(|| {
        let events = topological_clocks.iter().copied().collect::<HashSet<_>>();
        events
            .iter()
            .copied()
            .chain(
                clock_domains
                    .iter()
                    .filter(|(_, canonical)| events.contains(canonical))
                    .map(|(alias, _)| *alias),
            )
            .collect::<HashSet<_>>()
    });
    let mut persistent_comb_state = HashSet::default();
    let mut comb_startups = BTreeMap::new();
    for path in &comb_blocks {
        if let LogicPathTarget::Process { unit, startup, .. } = &path.target {
            if let Some(startup) = startup {
                comb_startups.entry(startup.initialized).or_insert_with(|| {
                    let mut startup = startup.clone();
                    // Transparent ports/wires already have their upstream
                    // declaration value before wildcard processes begin
                    // waiting. Their unconnected net default (Z) is not a
                    // startup event. Resolve only direct equal-type copies;
                    // constant/expression continuous drivers still run at
                    // time zero and can wake the process.
                    for (source, _) in &mut startup.inputs {
                        let mut seen = HashSet::default();
                        while seen.insert(*source) {
                            let upstream = comb_blocks.iter().find_map(|path| {
                                if !path.transparent_copy {
                                    return None;
                                }
                                let LogicPathTarget::Var(target) = &path.target else {
                                    return None;
                                };
                                if target.id != source.id
                                    || target.access.lsb > source.access.lsb
                                    || target.access.msb < source.access.msb
                                {
                                    return None;
                                }
                                let celox_slt::SLTNode::Input {
                                    variable,
                                    access,
                                    index,
                                    ..
                                } = global_arena.get(path.expr)
                                else {
                                    return None;
                                };
                                if !index.is_empty()
                                    || access.msb - access.lsb
                                        != target.access.msb - target.access.lsb
                                    || (four_state
                                        && variables[variable].1 != variables[&target.id].1)
                                {
                                    return None;
                                }
                                Some(VarAtomBase::new(
                                    *variable,
                                    access.lsb + source.access.lsb - target.access.lsb,
                                    access.lsb + source.access.msb - target.access.lsb,
                                ))
                            });
                            let Some(upstream) = upstream else {
                                break;
                            };
                            *source = upstream;
                        }
                    }
                    startup
                });
            }
            for block in unit.blocks.values() {
                for inst in &block.instructions {
                    if let SIRInstruction::Store(address, ..) = inst {
                        persistent_comb_state.insert(*address);
                    }
                }
            }
        }
    }
    let parallel_comb_blocks = parallel.enabled().then(|| comb_blocks.clone());
    let sched_start = flatten_timing.then(std::time::Instant::now);
    let schedule = match scheduler::sort_with_unpacked_element_widths(
        comb_blocks,
        &global_arena,
        &ignored_loops,
        &true_loops,
        four_state,
        &var_widths,
        &unpacked_element_widths,
        next_runtime_error_code,
    ) {
        Ok(schedule) => schedule,
        Err(error) => {
            let (err_vars, err_path_idx) = module_variables(&modules);
            let frontend_lookup = FrontendLookup {
                instance_ids: expanded.clone(),
                instance_module: instance_modules.clone(),
                indexed_instances: indexed_instances.clone(),
                module_variables: err_vars,
                module_var_path_index: err_path_idx,
                module_names: module_names.clone(),
                source_to_state: HashMap::default(),
                state_to_source: HashMap::default(),
                event_aliases: HashMap::default(),
                package_instances: package_instances.values().copied().collect(),
            };
            let source_locations = scheduler_source_locations(&error, &modules, &instance_modules);
            if let SchedulerError::CombinationalLoop { blocks } = &error {
                loop_members.extend(blocks.iter().filter_map(|block| {
                    let var = block.target.var()?;
                    Some((
                        *instance_modules.get(&var.id.instance_id)?,
                        VarAtomBase::new(var.id.var_id, var.access.lsb, var.access.msb),
                    ))
                }));
            }
            let mut target_arena = SLTNodeArena::new();
            let error = error.map_addr(&global_arena, &mut target_arena, &|addr| {
                frontend_lookup.get_path(addr)
            })?;
            return Err(if source_locations.is_empty() {
                ParserError::Scheduler(error)
            } else {
                ParserError::SchedulerWithLocation {
                    error,
                    source_locations,
                }
            });
        }
    };
    let object_sizes = super::parallel::ObjectSizes {
        widths: &var_widths,
        element_widths: &unpacked_element_widths,
    };
    let parallel_comb = match parallel_comb_blocks {
        Some(paths) => {
            let first_code = runtime_errors
                .keys()
                .chain(schedule.runtime_errors.keys())
                .copied()
                .max()
                .map_or(next_runtime_error_code, |code| code + 1)
                .max(next_runtime_error_code);
            let options = scheduler::LanePartitionOptions {
                lanes: parallel.lanes,
                synchronization_cost: parallel.synchronization_cost,
                lane0_targets: lane0_objects.clone().unwrap_or_default(),
                minimum_speedup_percent: parallel.minimum_speedup_percent,
                hierarchy: Some({
                    let parents = instance_parents(&expanded);
                    let extra_costs = super::parallel::instance_ff_costs(
                        &parallel_ff_units,
                        parents.len(),
                        object_sizes,
                    );
                    scheduler::InstanceHierarchy {
                        instance_of: |address: &AbsoluteAddr| address.instance_id.0,
                        parents,
                        extra_costs,
                    }
                }),
            };
            let partitioned = scheduler::sort_lanes(
                paths,
                &global_arena,
                &ignored_loops,
                &true_loops,
                four_state,
                &var_widths,
                &unpacked_element_widths,
                first_code,
                &options,
            )
            .map_err(|error| {
                ParserError::illegal_context(
                    "lane-partitioned combinational schedule",
                    format!("{error}"),
                    None,
                )
            })?;
            if let Some(partitioned) = &partitioned {
                tracing::debug!(
                    "[parallel] comb lanes={} units={} estimated_makespan={} total_cost={}",
                    parallel.lanes,
                    partitioned.units.len(),
                    partitioned.estimated_makespan,
                    partitioned.total_cost
                );
            }
            partitioned
        }
        None => None,
    };
    // The comb schedule no longer refers to SLT nodes. Release its arena before
    // constructing the clock-fused schedules, whose arena already owns its
    // remapped nodes.
    drop(global_arena);
    if let Some(s) = sched_start {
        tracing::debug!("[flatten] scheduler::sort: {:?}", s.elapsed());
    }
    runtime_errors.extend(schedule.runtime_errors);
    let stable_unit = |eu: ExecutionUnit<AbsoluteAddr>| ExecutionUnit {
        entry_block_id: eu.entry_block_id,
        blocks: eu
            .blocks
            .into_iter()
            .map(|(id, bb)| {
                (
                    id,
                    BasicBlock {
                        id: bb.id,
                        params: bb.params,
                        instructions: bb
                            .instructions
                            .into_iter()
                            .map(|inst| {
                                inst.into_map_addr(|addr| RegionedAbsoluteAddr {
                                    region: STABLE_REGION,
                                    instance_id: addr.instance_id,
                                    var_id: addr.var_id,
                                })
                            })
                            .collect(),
                        terminator: bb.terminator,
                    },
                )
            })
            .collect(),
        register_map: eu.register_map,
    };
    let eval_comb: Vec<ExecutionUnit<RegionedAbsoluteAddr>> = schedule
        .execution_units
        .into_iter()
        .map(stable_unit)
        .collect();
    let parallel_eval_comb = parallel_comb
        .map(|partitioned| {
            runtime_errors.extend(partitioned.runtime_errors);
            partitioned
                .units
                .into_iter()
                .map(|unit| LaneUnit::new(unit.lane, stable_unit(unit.unit)))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut eval_comb_apply_ffs = HashMap::default();
    let mut fused_direct_ff_writes = HashMap::default();
    if let (
        Some(factory),
        Some((
            actions,
            clock_arena,
            clock_comb_blocks,
            clock_var_widths,
            clock_unpacked_element_widths,
            clock_ignored_loops,
            clock_true_loops,
        )),
    ) = (fused_ff_factory, fused_inputs)
    {
        let mut fused_schedule_cache = HashMap::<
            Vec<usize>,
            (
                Option<Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
                Vec<ExecutionUnit<RegionedAbsoluteAddr>>,
                Vec<VarAtomBase<RegionedAbsoluteAddr>>,
            ),
        >::default();
        let schedule = |paths, actions| {
            let mut ff_lowering = factory.create(actions)?;
            match scheduler::sort_clock(
                paths,
                &clock_arena,
                &clock_ignored_loops,
                &clock_true_loops,
                four_state,
                &clock_var_widths,
                &clock_unpacked_element_widths,
                next_runtime_error_code,
                ff_lowering.as_mut(),
            ) {
                Ok(schedule) => Ok(schedule),
                Err(scheduler::ClockSortError::Lowering(error)) => Err(error),
                Err(scheduler::ClockSortError::Scheduler(error)) => {
                    let mut error_arena = SLTNodeArena::new();
                    let error =
                        error.map_addr(&clock_arena, &mut error_arena, &|addr| addr.to_string())?;
                    Err(ParserError::Scheduler(error))
                }
            }
        };
        for (trigger, actions) in actions {
            let action_ids = actions.iter().map(|action| action.id).collect::<Vec<_>>();
            if let Some((ff_only, units, direct_ff_writes)) = fused_schedule_cache.get(&action_ids)
            {
                if let Some(ff_only) = ff_only {
                    eval_apply_ffs.insert(trigger, ff_only.clone());
                }
                eval_comb_apply_ffs.insert(trigger, units.clone());
                fused_direct_ff_writes.insert(trigger, direct_ff_writes.clone());
                continue;
            }
            let fused_start = flatten_timing.then(std::time::Instant::now);
            // The ordinary tick already settled comb. Reuse the existing
            // old-state dependency planner with no comb paths: acyclic readers
            // run before writers, and only cyclic/aliased ranges need staging.
            // Keep the merged eval-then-apply fallback for adapters without
            // source-aware lowering and the existing single-action fast path.
            let ff_only = if actions.len() > 1 {
                Some(schedule(Vec::new(), actions.clone())?.execution_units)
            } else {
                None
            };
            let fused = schedule(clock_comb_blocks.clone(), actions)?;
            if let Some(start) = fused_start {
                tracing::debug!("[flatten] scheduler::sort_clock: {:?}", start.elapsed());
            }
            let direct_ff_writes = fused.direct_ff_writes;
            let units = fused.execution_units;
            fused_schedule_cache.insert(
                action_ids,
                (ff_only.clone(), units.clone(), direct_ff_writes.clone()),
            );
            if let Some(ff_only) = ff_only {
                eval_apply_ffs.insert(trigger, ff_only);
            }
            eval_comb_apply_ffs.insert(trigger, units);
            fused_direct_ff_writes.insert(trigger, direct_ff_writes);
        }
    }

    if let Some(t) = trace
        && trace_opts.scheduled_units
    {
        t.scheduled_units = Some(eval_comb.clone());
    }

    // The unified function is the normal fast path.  Split evaluator/apply
    // functions are needed only when scheduling can evaluate several active
    // domains or must cascade through a derived clock.
    let active_ff_domains = eval_apply_ffs
        .values()
        .filter(|units| !units.is_empty())
        .count();
    // With process kernels, a round in which a zero delay is pending after
    // domains triggered evaluates them before the inactive region and
    // publishes them in the NBA region (IEEE 1800-2023 4.5). Only those
    // rounds use the split units; the fused units stay the default.
    let needs_split_path =
        active_ff_domains > 1 || !cascaded_clocks.is_empty() || !processes.is_empty();
    let parallel_sir = parallel.enabled().then(|| {
        let mut events = eval_apply_ffs
            .iter()
            .filter(|(_, units)| !units.is_empty())
            .map(|(event, _)| *event)
            .collect::<Vec<_>>();
        events.sort_unstable();
        let eval_apply_ffs = super::parallel::plan_parallel_ff_kernels(
            &events,
            parallel_ff_units,
            lane0_objects
                .as_ref()
                .expect("parallel scheduling computes lane-0 objects"),
            &parallel_eval_comb,
            object_sizes,
            parallel,
        );
        for (event, kernel) in &eval_apply_ffs {
            let lanes = kernel
                .units()
                .map(|unit| unit.lane)
                .collect::<BTreeSet<_>>();
            tracing::debug!(
                "[parallel] event={event} evaluations={} applications={} lanes={lanes:?}",
                kernel.evaluations.len(),
                kernel.applications.len(),
            );
        }
        ParallelSirProgram {
            lanes: parallel.lanes,
            eval_comb: parallel_eval_comb,
            eval_apply_ffs,
        }
    });
    // With process kernels, a domain whose blocking results another unit
    // observes commits them with its evaluation, in the active region, and
    // only its nonblocking results in the NBA region (IEEE 1800-2023 4.5,
    // 10.4). Every other domain commits both together, as before.
    let mut active_commits = Vec::new();
    if !processes.is_empty() {
        let events: HashSet<AbsoluteAddr> = eval_apply_ffs
            .keys()
            .copied()
            .chain(clock_domains.keys().copied())
            .collect();
        active_commits = split_observed_commits(
            &mut eval_only_ffs,
            &mut apply_ffs,
            split_commits,
            &comb_sources,
            processes.iter().map(|process| &process.kernel),
            timing
                .wait_sites
                .iter()
                .flat_map(|site| site.terms.iter().map(|(signal, _)| *signal)),
            &events,
            &clock_domains,
        );
    }
    let (eval_only_ffs, apply_ffs) = if needs_split_path {
        (eval_only_ffs, apply_ffs)
    } else {
        (HashMap::default(), HashMap::default())
    };

    let (mod_vars, mod_path_idx) = module_variables(&modules);
    // A variable bound to a package variable has no state object of its own:
    // its initial writes, such as a constant `initial` assignment, go to the
    // package's object. Declaration initializers of packages come first
    // (IEEE 1800-2023 10.5).
    let mut initial_instances = instance_modules.iter().collect::<Vec<_>>();
    initial_instances.sort_unstable_by_key(|(instance, module)| {
        (!package_instances.contains_key(*module), instance.0)
    });
    let declaration_values: Vec<InitialStateValue<AbsoluteAddr>> = initial_instances
        .iter()
        .flat_map(|&(&instance_id, module_id)| {
            modules[module_id]
                .declaration_initial_memory_values
                .iter()
                .filter(move |init| {
                    !bindings.is_bound(&AbsoluteAddr {
                        instance_id,
                        var_id: init.address,
                    })
                })
                .map(move |init| InitialStateValue {
                    address: bindings.locate(instance_id, init.address),
                    data: init.data.clone(),
                })
        })
        .collect();
    let folded_initials: Vec<InitialStateValue<AbsoluteAddr>> = initial_instances
        .iter()
        .flat_map(|&(&instance_id, module_id)| {
            modules[module_id]
                .folded_initial_memory_values
                .iter()
                .map(move |init| InitialStateValue {
                    address: bindings.locate(instance_id, init.address),
                    data: init.data.clone(),
                })
        })
        .collect();
    let mut initial_memory_values: Vec<InitialStateValue<AbsoluteAddr>> = initial_instances
        .into_iter()
        .flat_map(|(&instance_id, module_id)| {
            modules[module_id]
                .initial_memory_values
                .iter()
                .map(move |init| InitialStateValue {
                    address: bindings.locate(instance_id, init.address),
                    data: init.data.clone(),
                })
        })
        .collect();
    let state_objects: HashMap<AbsoluteAddr, VariableMetadata> = instance_modules
        .iter()
        .flat_map(|(&instance_id, module_id)| {
            modules[module_id]
                .variables
                .iter()
                .map(move |(&var_id, variable)| {
                    (
                        AbsoluteAddr {
                            instance_id,
                            var_id,
                        },
                        variable.metadata.clone(),
                    )
                })
        })
        .filter(|(address, _)| !bindings.is_bound(address))
        .collect();
    let declaration_startup = processes
        .iter()
        .any(|process| process.slots.is_initializer)
        .then(|| {
            let targets: BTreeSet<_> = folded_initials.iter().map(|init| init.address).collect();
            let mut reset = targets
                .iter()
                .map(|&address| InitialStateValue {
                    address,
                    data: InitialStateData::Packed {
                        value: BigUint::default(),
                        mask: BigUint::default(),
                        written_mask: (BigUint::from(1u8) << state_objects[&address].width) - 1u8,
                    },
                })
                .collect::<Vec<_>>();
            reset.extend(
                declaration_values
                    .iter()
                    .filter(|init| targets.contains(&init.address))
                    .cloned(),
            );
            celox_design::DeclarationStartup {
                reset,
                ordinary: folded_initials,
                snapshots: comb_startups
                    .values()
                    .flat_map(|startup| startup.inputs.iter().copied())
                    .collect(),
                counter_snapshots: comb_startups
                    .values()
                    .flat_map(|startup| startup.counters.iter().copied())
                    .collect(),
                probes: declaration_probes,
            }
        });
    // Arm wildcard processes against declaration-time state, before continuous
    // drivers, folded initial statements, or the first host write can change
    // their inputs. The resulting constants use the ordinary initialization
    // path on every backend, including native images.
    initial_memory_values.extend(super::comb_startup::initial_values(
        comb_startups.values(),
        &declaration_values,
        &state_objects,
        four_state,
    ));
    let runtime_comb_observers: Vec<RuntimeCombObserver<AbsoluteAddr>> = comb_observers
        .iter()
        .map(|observer| RuntimeCombObserver {
            site_id: observer.site_id,
            activation_group: observer.activation_group,
            sensitivity: observer.sensitivity.clone(),
            occurrence_counters: observer
                .sensitivity_counters
                .iter()
                .map(|&(_, counter)| counter)
                .collect(),
            written_inputs: observer.written_inputs.clone(),
        })
        .collect();
    // Nonblocking sites that can stage their value instead of queueing it
    // (`NbaSite::pending`). Staged values are published after the queue,
    // so a staged target must be one whose update order nothing observes:
    // no wait, counter, event domain or comb observer reads it, directly or
    // through combinational logic.
    if !timing.nba_switches.is_empty() {
        let observed = {
            let mut observed: HashSet<AbsoluteAddr> = timing
                .wait_sites
                .iter()
                .flat_map(|site| site.terms.iter().map(|(signal, _)| *signal))
                .chain(timing.counter_slots.iter().map(|slots| slots.signal))
                .chain(
                    clock_domains
                        .iter()
                        .flat_map(|(&alias, &canonical)| [alias, canonical]),
                )
                .chain(topological_clocks.iter().copied())
                .chain(
                    runtime_comb_observers
                        .iter()
                        .flat_map(|observer| observer.sensitivity.iter().map(|atom| atom.id)),
                )
                .collect();
            for ffs in [
                &eval_apply_ffs,
                &eval_comb_apply_ffs,
                &eval_only_ffs,
                &apply_ffs,
            ] {
                observed.extend(ffs.keys().copied());
            }
            through_comb(observed, &comb_sources)
        };
        // The ports of the top module, which the host drives.
        let top_ports: HashSet<AbsoluteAddr> = expanded
            .get(&InstancePath(Vec::new()))
            .map(|&root| {
                modules[&instance_modules[&root]]
                    .variables
                    .iter()
                    .filter(|(_, variable)| {
                        matches!(
                            variable.kind,
                            crate::VariableKind::Input
                                | crate::VariableKind::Output
                                | crate::VariableKind::Inout
                        )
                    })
                    .map(|(&var_id, _)| bindings.locate(root, var_id))
                    .collect()
            })
            .unwrap_or_default();
        let staged = staged_nba_sites(
            &timing,
            eval_comb
                .iter()
                .chain(eval_apply_ffs.values().flatten())
                .chain(eval_comb_apply_ffs.values().flatten())
                .chain(eval_only_ffs.values().flatten())
                .chain(apply_ffs.values().flatten())
                .chain(processes.iter().map(|process| &process.kernel)),
            &top_ports,
            &observed,
        );
        select_nba_paths(&mut timing, &staged, &mut processes);
    }
    let mut process_kernels = Vec::with_capacity(processes.len());
    let mut process_slots = Vec::with_capacity(processes.len());
    let mut process_origins = Vec::with_capacity(processes.len());
    for process in processes {
        process_kernels.push(process.kernel);
        process_slots.push(process.slots);
        process_origins.push(process.origin);
    }
    let TimingMetadata {
        wait_sites,
        nba_sites,
        host_request_sites,
        time_slots,
        ..
    } = timing;
    let source_sir = SirProgram {
        eval_apply_ffs,
        eval_comb_apply_ffs,
        eval_only_ffs,
        apply_ffs,
        eval_comb,
        parallel: parallel_sir,
        processes: process_kernels,
    };
    let mut source_addresses = state_objects.keys().copied().collect::<Vec<_>>();
    source_addresses.sort_unstable();
    let mut source_to_state = HashMap::default();
    for (index, source) in source_addresses.into_iter().enumerate() {
        let object = StateObjectId(u32::try_from(index).map_err(|_| {
            ParserError::illegal_context(
                "design state projection",
                "flattened state object count exceeds u32",
                None,
            )
        })?);
        let state = StateAddr {
            instance_id: source.instance_id,
            var_id: object,
        };
        source_to_state.insert(source, state);
    }
    for (alias, target) in &bindings.targets {
        source_to_state.insert(*alias, source_to_state[target]);
    }
    let project = |source: AbsoluteAddr| source_to_state[&source];
    let project_regioned = |source: RegionedAbsoluteAddr| RegionedStateAddr {
        region: source.region,
        instance_id: source.instance_id,
        var_id: source_to_state[&source.absolute_addr()].var_id,
    };

    let mut sir = source_sir.into_map_addr(project, project_regioned);
    if !four_state {
        celox_sir::two_state::canonicalize_program(&mut sir);
    }
    let state_objects: HashMap<StateAddr, VariableMetadata> = state_objects
        .into_iter()
        .map(|(address, metadata)| (project(address), metadata))
        .collect();
    let initial_state = initial_memory_values
        .into_iter()
        .map(|initial| InitialStateValue {
            address: project(initial.address),
            data: initial.data,
        })
        .collect();
    let events = EventTopology {
        aliases: clock_domains
            .into_iter()
            .map(|(alias, canonical)| (project(alias), project(canonical)))
            .collect(),
        ordered_events: topological_clocks.into_iter().map(project).collect(),
        cascaded_events: cascaded_clocks.into_iter().map(project).collect(),
        derived_events: derived_events
            .into_iter()
            .map(|(event, targets)| (project(event), targets.into_iter().map(project).collect()))
            .collect(),
        independent_derived: independent_derived.into_iter().map(project).collect(),
        combinationally_read: combinationally_read.into_iter().map(project).collect(),
        combinational_derived: combinational_derived
            .into_iter()
            .map(|(event, clocks)| {
                let clocks = clocks
                    .into_iter()
                    .map(|derived| CombDerivedClock {
                        clock: project(derived.clock),
                        // A gate reading an address without a state object
                        // falls back to settling the design.
                        gate: derived.gate.and_then(|gate| {
                            gate.into_iter()
                                .map(|op| match op {
                                    GateOp::Load {
                                        address,
                                        lsb,
                                        width,
                                    } => source_to_state.get(&address).map(|state| GateOp::Load {
                                        address: *state,
                                        lsb,
                                        width,
                                    }),
                                    op => {
                                        Some(op.map_addr(|_| {
                                            unreachable!("only loads have addresses")
                                        }))
                                    }
                                })
                                .collect()
                        }),
                    })
                    .collect();
                (project(event), clocks)
            })
            .collect(),
        reset_clocks: reset_clock_map
            .into_iter()
            .map(|(reset, clock)| (project(reset), project(clock)))
            .collect(),
    };
    let event_aliases = events.aliases.clone();
    let runtime_errors = runtime_errors
        .into_iter()
        .map(|(code, info)| {
            (
                code,
                RuntimeErrorInfo {
                    message: info.message,
                    signals: info.signals.into_iter().map(project).collect(),
                },
            )
        })
        .collect();
    let comb_observers = runtime_comb_observers
        .into_iter()
        .map(|observer| RuntimeCombObserver {
            site_id: observer.site_id,
            activation_group: observer.activation_group,
            sensitivity: observer
                .sensitivity
                .into_iter()
                .map(|atom| VarAtomBase {
                    id: project(atom.id),
                    access: atom.access,
                })
                .collect(),
            occurrence_counters: observer
                .occurrence_counters
                .into_iter()
                .map(project)
                .collect(),
            written_inputs: observer.written_inputs.into_iter().map(project).collect(),
        })
        .collect();
    let direct_ff_writes = fused_direct_ff_writes
        .into_iter()
        .map(|(source, writes)| {
            (
                project(source),
                writes
                    .into_iter()
                    .map(|write| VarAtomBase {
                        id: project_regioned(write.id),
                        access: write.access,
                    })
                    .collect(),
            )
        })
        .collect();

    let mut rtl_writes = HashSet::default();
    for unit in sir
        .eval_comb
        .iter()
        .chain(sir.eval_apply_ffs.values().flatten())
        .chain(sir.eval_comb_apply_ffs.values().flatten())
        .chain(sir.eval_only_ffs.values().flatten())
        .chain(sir.apply_ffs.values().flatten())
        .chain(&sir.processes)
    {
        for block in unit.blocks.values() {
            for instruction in &block.instructions {
                let (address, offset, width) = match instruction {
                    SIRInstruction::Store(address, offset, width, ..)
                    | SIRInstruction::Commit(_, address, offset, width, _) => {
                        (address.absolute_addr(), offset, *width)
                    }
                    _ => continue,
                };
                let access = offset
                    .constant_bit_offset()
                    .and_then(|lsb| {
                        width
                            .checked_sub(1)
                            .and_then(|tail| lsb.checked_add(tail))
                            .map(|msb| BitAccess::new(lsb, msb))
                    })
                    .or_else(|| {
                        state_objects
                            .get(&address)
                            .and_then(|object| object.width.checked_sub(1))
                            .map(|msb| BitAccess::new(0, msb))
                    });
                if let Some(access) = access {
                    rtl_writes.insert(VarAtomBase {
                        id: address,
                        access,
                    });
                }
            }
        }
    }

    let process_slots = process_slots
        .into_iter()
        .map(|slots| slots.map(project))
        .collect();
    let watches: Vec<EventWatch<StateAddr>> = {
        let event_kinds = ElaboratedDesign {
            time_precision,
            instance_time_scales: HashMap::default(),
            state_objects: state_objects.clone(),
            events: events.clone(),
            initial_state: Vec::new(),
        }
        .event_kinds();
        counted
            .values()
            .map(|slots| {
                let signal = project(slots.signal);
                let object = &state_objects[&signal];
                let canonical = events.canonical(signal);
                let domains = event_kinds
                    .get(&canonical)
                    .filter(|_| events.ordered_events.contains(&signal) || canonical != signal)
                    .map(|&kind| (canonical, CounterKind::for_domain(kind)))
                    .into_iter()
                    .collect();
                EventWatch {
                    signal,
                    width: object.width,
                    four_state: object.is_4state,
                    changes: Some(project(slots.changes)),
                    rises: Some(project(slots.rises)),
                    falls: Some(project(slots.falls)),
                    domains,
                }
            })
            .collect()
    };
    let wait_sites: Vec<WaitSite<StateAddr>> =
        wait_sites.iter().map(|site| site.map(project)).collect();
    let nba_sites: Vec<NbaSite<StateAddr>> =
        nba_sites.iter().map(|site| site.map(project)).collect();
    let host_request_sites: Vec<HostRequestSite<StateAddr>> = host_request_sites
        .iter()
        .map(|site| site.map(project))
        .collect();
    let active_commits: Vec<StateAddr> = active_commits.into_iter().map(project).collect();
    let time_slots: Vec<StateAddr> = time_slots.into_iter().map(project).collect();
    // The runtime reads counters, the signals waits watch and staged
    // nonblocking values directly; no unit loads a wait's signal.
    let mut timing_read_roots: HashSet<StateAddr> = watches
        .iter()
        .flat_map(|watch| {
            watch
                .counters()
                .map(|(_, counter)| counter)
                .collect::<Vec<_>>()
        })
        .chain(
            wait_sites
                .iter()
                .flat_map(|site| site.terms.iter().map(|(signal, _)| *signal)),
        )
        .chain(
            nba_sites
                .iter()
                .flat_map(|site| [Some(site.value), site.offset, site.pending])
                .flatten(),
        )
        .chain(host_request_sites.iter().flat_map(|site| site.slots()))
        .collect();
    let declaration_startup = declaration_startup.map(|startup| startup.map(project));
    let occurrence_probes: Vec<(VarAtomBase<StateAddr>, StateAddr)> = occurrence_probes
        .into_iter()
        .map(|(source, counter)| {
            (
                VarAtomBase {
                    id: project(source.id),
                    access: source.access,
                },
                project(counter),
            )
        })
        .collect();
    // The runtime counts the occurrences its own writes cause.
    timing_read_roots.extend(
        occurrence_probes
            .iter()
            .flat_map(|(source, counter)| [source.id, *counter]),
    );
    if let Some(startup) = &declaration_startup {
        timing_read_roots.extend(
            startup
                .reset
                .iter()
                .chain(&startup.ordinary)
                .map(|value| value.address),
        );
        timing_read_roots.extend(
            startup
                .snapshots
                .iter()
                .flat_map(|(source, saved)| [source.id, *saved]),
        );
        timing_read_roots.extend(
            startup
                .counter_snapshots
                .iter()
                .flat_map(|&(counter, saved)| [counter, saved]),
        );
        timing_read_roots.extend(
            startup
                .probes
                .iter()
                .flat_map(|(source, counter)| [source.id, *counter]),
        );
    }
    // A package variable's state object maps back to the package's own
    // variable, not to one of its aliases.
    let state_to_source = source_to_state
        .iter()
        .filter(|(source, _)| !bindings.is_bound(source))
        .map(|(source, state)| (*state, *source))
        .collect();

    let persistent_comb_state = persistent_comb_state.into_iter().map(project).collect();
    let scheduled = ScheduledRtl {
        sir,
        design: ElaboratedDesign {
            time_precision,
            instance_time_scales,
            state_objects,
            events,
            initial_state,
        },
        frontend_lookup: FrontendLookup {
            instance_ids: expanded,
            instance_module: instance_modules,
            indexed_instances,
            module_variables: mod_vars,
            module_var_path_index: mod_path_idx,
            module_names,
            source_to_state,
            state_to_source,
            event_aliases,
            package_instances: package_instances.values().copied().collect(),
        },
        runtime_schema: RuntimeSchema {
            runtime_errors,
            runtime_event_sites,
            extern_functions,
            comb_observers,
            testbench_read_roots: timing_read_roots,
            rtl_writes,
            comb_writes: Default::default(),
            persistent_comb_state,
            declaration_startup,
            dead_store_policy: Default::default(),
            live_signals: Default::default(),
            processes: process_slots,
            process_origins,
            watches,
            occurrence_probes,
            wait_sites,
            nba_sites,
            host_request_sites,
            active_commits,
            time_slots,
        },
    };

    if tracing::enabled!(target: "celox::parallel", tracing::Level::TRACE) {
        for source in scheduled.frontend_lookup.source_to_state.keys() {
            tracing::trace!(target: "celox::parallel", event = "source_signal",
                source_address = %source, name = scheduled.frontend_lookup.get_path(source),
                instance = source.instance_id.0, source_object = source.var_id.0);
        }
    }

    Ok(ScheduledRtlOutput {
        scheduled,
        fused_optimization_hints: FusedSirOptimizationHints { direct_ff_writes },
    })
}

fn module_variables(
    modules: &HashMap<ModuleId, SimModule>,
) -> (
    HashMap<ModuleId, HashMap<SourceVarId, VariableInfo>>,
    HashMap<ModuleId, HashMap<Vec<String>, Option<SourceVarId>>>,
) {
    let mut res = HashMap::default();
    let mut path_index = HashMap::default();
    for (id, module) in modules {
        let mut variables = HashMap::default();
        let mut paths: HashMap<Vec<String>, Option<SourceVarId>> = HashMap::default();
        for (&source_id, variable) in &module.variables {
            // A package variable belongs to the package's instance.
            if module.package_bindings.contains_key(&source_id) {
                continue;
            }
            // Only module-scope variables are externally addressable. Locals
            // may share the same VarPath, but must not make a legal
            // hierarchical or public lookup appear ambiguous.
            if variable.module_affiliated {
                match paths.entry(variable.path.clone()) {
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(Some(source_id));
                    }
                    std::collections::hash_map::Entry::Occupied(mut e) => {
                        // Duplicate visible VarPath — mark as ambiguous.
                        e.insert(None);
                    }
                }
            }
            variables.insert(
                source_id,
                VariableInfo {
                    id: source_id,
                    path: variable.path.clone(),
                    var_kind: variable.kind,
                    signed: variable.signed,
                    packed_dims: variable.packed_dims.clone(),
                    unpacked_ranges: variable.unpacked_ranges.clone(),
                    metadata: variable.metadata.clone(),
                },
            );
        }
        res.insert(*id, variables);
        path_index.insert(*id, paths);
    }
    (res, path_index)
}

/// The parent of every expanded instance, indexed by instance id, with
/// `usize::MAX` for the root.
fn instance_parents(expanded: &HashMap<InstancePath, InstanceId>) -> Vec<usize> {
    let count = expanded
        .values()
        .map(|instance| instance.0 + 1)
        .max()
        .unwrap_or(0);
    let mut parents = vec![usize::MAX; count];
    for (path, instance) in expanded {
        if let Some((_, parent_path)) = path.0.split_last()
            && let Some(parent) = expanded.get(&InstancePath(parent_path.to_vec()))
        {
            parents[instance.0] = parent.0;
        }
    }
    parents
}

fn expand_hierarchy(
    top: &ModuleId,
    modules: &HashMap<ModuleId, SimModule>,
) -> (
    HashMap<InstancePath, InstanceId>,
    HashMap<InstanceId, ModuleId>,
    HashSet<InstanceId>,
) {
    let mut expanded = HashMap::default();
    let mut instance_modules = HashMap::default();
    let mut indexed_instances = HashSet::default();
    let mut instance_id = 0;
    let path = vec![];
    let id = InstanceId(instance_id);
    instance_modules.insert(id, *top);
    expanded.insert(InstancePath(path.clone()), id);
    instance_id += 1;
    expand(
        top,
        path,
        modules,
        &mut expanded,
        &mut instance_modules,
        &mut indexed_instances,
        &mut instance_id,
    );
    (expanded, instance_modules, indexed_instances)
}

fn extend_boundaries(
    boundaries: &mut HashMap<AbsoluteAddr, BTreeSet<usize>>,
    source: AbsoluteAddr,
    target: AbsoluteAddr,
) -> bool {
    if source == target || boundaries.get(&source).is_none_or(BTreeSet::is_empty) {
        return false;
    }
    boundaries.entry(target).or_default();
    let [Some(source), Some(target)] = boundaries.get_disjoint_mut([&source, &target]) else {
        unreachable!("distinct boundary keys were inserted before lookup");
    };
    let old_len = target.len();
    target.extend(source.iter().copied());
    target.len() != old_len
}

fn propagate_boundaries(
    bindings: &PackageBindings,
    expanded: &HashMap<InstancePath, InstanceId>,
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
) -> HashMap<AbsoluteAddr, BTreeSet<usize>> {
    let mut current_boundaries = HashMap::default();

    // Initialize with local boundaries
    for id in expanded.values() {
        let module_id = &instance_modules[id];
        let sim_module = &modules[module_id];
        for (var_id, boundaries) in &sim_module.comb_boundaries {
            // Every alias of a package variable adds its boundaries.
            let addr = bindings.locate(*id, *var_id);
            current_boundaries
                .entry(addr)
                .or_insert_with(BTreeSet::new)
                .extend(boundaries.iter().copied());
        }
    }

    // Propagate boundaries
    let mut changed = true;
    while changed {
        changed = false;
        for (path, id) in expanded {
            let module_id = &instance_modules[id];
            let sim_module = &modules[module_id];

            for (inst_name, glue_blocks) in &sim_module.glue_blocks {
                for (idx, glue_block) in glue_blocks.iter().enumerate() {
                    let mut child_path = path.0.clone();
                    child_path.push((inst_name.clone(), sim_module.instance_index(inst_name, idx)));
                    let child_id = expanded[&InstancePath(child_path)];

                    // Propagate from Parent to Child (Input Ports)
                    for (parent_vars, child_addr) in &glue_block.input_ports {
                        if let Some(target) = child_addr.target.var()
                            && let GlueAddr::Child(child_var_id) = target.id
                        {
                            let child_abs = bindings.locate(child_id, child_var_id);

                            // Collect boundaries from all parent variables connected to this port
                            for parent_var in parent_vars {
                                let parent_abs = bindings.locate(*id, *parent_var);
                                changed |= extend_boundaries(
                                    &mut current_boundaries,
                                    parent_abs,
                                    child_abs,
                                );
                            }
                        }
                    }

                    // Propagate from Child to Parent (Output Ports)
                    for (parent_vars, logic_path) in &glue_block.output_ports {
                        // logic_path.target is Parent. logic_path.sources contains Child.
                        for source in &logic_path.sources {
                            if let GlueAddr::Child(child_var_id) = source.id {
                                let child_abs = bindings.locate(child_id, child_var_id);

                                // Child -> Parent
                                for parent_var in parent_vars {
                                    let parent_abs = bindings.locate(*id, *parent_var);
                                    changed |= extend_boundaries(
                                        &mut current_boundaries,
                                        child_abs,
                                        parent_abs,
                                    );
                                }

                                // Parent -> Child (Sink -> Source propagation)
                                // If the parent wire connected to this output has boundaries (e.g. used in slices),
                                // those boundaries should propagate to the child output port so it drives them appropriately.
                                for parent_var in parent_vars {
                                    let parent_abs = bindings.locate(*id, *parent_var);
                                    changed |= extend_boundaries(
                                        &mut current_boundaries,
                                        parent_abs,
                                        child_abs,
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    current_boundaries
}

fn expand(
    target: &ModuleId,
    path: Vec<(String, usize)>,
    modules: &HashMap<ModuleId, SimModule>,
    expanded: &mut HashMap<InstancePath, InstanceId>,
    instance_modules: &mut HashMap<InstanceId, ModuleId>,
    indexed_instances: &mut HashSet<InstanceId>,
    instance_id: &mut usize,
) {
    let module = &modules[target];
    for (inst_name, gbs) in &module.glue_blocks {
        // Generate loops can elaborate several scalar declarations under the
        // same flattened name. Keep those scopes distinct until generate
        // hierarchy segments become part of InstancePath.
        let indexed = module.indexed_instance_names.contains(inst_name) || gbs.len() > 1;
        for (idx, gb) in gbs.iter().enumerate() {
            let mut path = path.clone();
            path.push((inst_name.clone(), module.instance_index(inst_name, idx)));
            let id = InstanceId(*instance_id);
            expanded.insert(InstancePath(path.clone()), id);
            instance_modules.insert(id, gb.module_id);
            if indexed {
                indexed_instances.insert(id);
            }
            *instance_id += 1;
            expand(
                &gb.module_id,
                path,
                modules,
                expanded,
                instance_modules,
                indexed_instances,
                instance_id,
            );
        }
    }
}

fn relocate_executation_unit_with_errors<A, B>(
    eu: &ExecutionUnit<A>,
    f: &impl Fn(&A) -> B,
    runtime_error_codes: &HashMap<i64, i64>,
    runtime_event_sites: &HashMap<u32, u32>,
    extern_functions: &HashMap<u32, u32>,
) -> ExecutionUnit<B> {
    ExecutionUnit {
        entry_block_id: eu.entry_block_id,
        blocks: eu
            .blocks
            .iter()
            .map(|(id, block)| {
                (
                    *id,
                    BasicBlock {
                        id: block.id,
                        instructions: block
                            .instructions
                            .iter()
                            .map(|inst| match inst {
                                SIRInstruction::RuntimeEvent { site_id, args } => {
                                    SIRInstruction::RuntimeEvent {
                                        site_id: runtime_event_sites
                                            .get(site_id)
                                            .copied()
                                            .unwrap_or(*site_id),
                                        args: args.clone(),
                                    }
                                }
                                SIRInstruction::CombCaptureEvent {
                                    site_id,
                                    args,
                                    fatal_error_code,
                                    consume_enabled,
                                } => SIRInstruction::CombCaptureEvent {
                                    site_id: runtime_event_sites
                                        .get(site_id)
                                        .copied()
                                        .unwrap_or(*site_id),
                                    args: args.clone(),
                                    fatal_error_code: *fatal_error_code,
                                    consume_enabled: *consume_enabled,
                                },
                                SIRInstruction::ExternCall { dst, func, args } => {
                                    SIRInstruction::ExternCall {
                                        dst: *dst,
                                        // The heap helper is outside every
                                        // module's extern function table.
                                        func: if *func == celox_sir::extern_abi::HEAP_HELPER_ID {
                                            *func
                                        } else {
                                            extern_functions[func]
                                        },
                                        args: args.clone(),
                                    }
                                }
                                _ => inst.map_addr(f),
                            })
                            .collect(),
                        params: block.params.clone(),
                        terminator: match block.terminator {
                            SIRTerminator::Error(code) => SIRTerminator::Error(
                                runtime_error_codes.get(&code).copied().unwrap_or(code),
                            ),
                            ref terminator => terminator.clone(),
                        },
                    },
                )
            })
            .collect(),
        register_map: eu.register_map.clone(),
    }
}

fn unify_clock_domains(
    bindings: &PackageBindings,
    expanded: &HashMap<InstancePath, InstanceId>,
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
) -> HashMap<AbsoluteAddr, AbsoluteAddr> {
    let mut drive_graph: HashMap<AbsoluteAddr, Vec<AbsoluteAddr>> = HashMap::default();
    // The edge each clock is sensed at: `true` rising, `false` falling.
    let mut polarities: HashMap<AbsoluteAddr, bool> = HashMap::default();

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

/// The `(target, source)` of a path that copies one whole variable into
/// another variable of the same width, unchanged.
///
/// Only such a copy is the same signal as its source, so only it may share the
/// source's clock domain. A part-select, an element of an array, a resized
/// read, or an assignment to part of the target is a different value, whose
/// edges are not the edges of the source variable as a whole.
fn whole_variable_copy<A: Copy + std::hash::Hash + Eq>(
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

fn build_fused_ff_actions(
    bindings: &PackageBindings,
    modules: &HashMap<ModuleId, SimModule>,
    instance_modules: &HashMap<InstanceId, ModuleId>,
    clock_domains: &HashMap<AbsoluteAddr, AbsoluteAddr>,
    runtime_relocations: &HashMap<InstanceId, FfRuntimeRelocation>,
) -> HashMap<AbsoluteAddr, Vec<FusedFfAction>> {
    let mut instances = instance_modules.iter().collect::<Vec<_>>();
    instances.sort_unstable_by_key(|(instance, _)| instance.0);
    let mut result = HashMap::<AbsoluteAddr, Vec<FusedFfAction>>::default();
    let mut next_action_id = 0usize;

    for (&instance_id, &module_id) in instances {
        let module = &modules[&module_id];
        let mut summaries = module.ff_access_summaries.iter().collect::<Vec<_>>();
        summaries.sort_unstable_by_key(|(trigger, _)| (*trigger).clone());
        for (trigger, summary) in summaries {
            let relocate = |address: RegionedVarAddr| {
                bindings.locate_regioned(address.region, instance_id, address.var_id)
            };
            let summary = FfAccessSummary {
                reads: summary
                    .reads
                    .iter()
                    .map(|read| VarAtomBase {
                        id: relocate(read.id),
                        access: read.access,
                    })
                    .collect(),
                writes: summary
                    .writes
                    .iter()
                    .map(|write| VarAtomBase {
                        id: bindings.locate_regioned(STABLE_REGION, instance_id, write.id.var_id),
                        access: write.access,
                    })
                    .collect(),
                dynamic_writes: summary
                    .dynamic_writes
                    .iter()
                    .map(|address| {
                        bindings.locate_regioned(STABLE_REGION, instance_id, address.var_id)
                    })
                    .collect(),
            };
            let action = FusedFfAction {
                id: next_action_id,
                instance_id,
                module_id,
                trigger: trigger.clone(),
                summary,
                runtime: runtime_relocations[&instance_id].clone(),
            };
            next_action_id += 1;
            let clock = bindings.locate(instance_id, trigger.clock);
            let clock = clock_domains.get(&clock).copied().unwrap_or(clock);
            result.entry(clock).or_default().push(action.clone());
            for &reset_id in &trigger.resets {
                let reset = bindings.locate(instance_id, reset_id);
                let reset = clock_domains.get(&reset).copied().unwrap_or(reset);
                result.entry(reset).or_default().push(action.clone());
            }
        }
    }
    result
}

/// always_comb starts after every initial procedure has reached its first
/// suspension or completion (IEEE 1800-2023 9.2.2.2). Initial status is zero
/// until that happens; all control slots are ordinary checkpointed state.
fn after_initial_processes(
    unit: &ExecutionUnit<AbsoluteAddr>,
    statuses: &[AbsoluteAddr],
) -> ExecutionUnit<AbsoluteAddr> {
    use celox_sir::{SIRBuilder, SIROffset, SIRValue};
    let mut b = SIRBuilder::new();
    let mut ready = b.alloc_bit(1, false);
    b.emit(SIRInstruction::Imm(ready, SIRValue::new(1u8)));
    for status in statuses {
        let value = b.alloc_bit(celox_design::PROCESS_STATUS_WIDTH, false);
        b.emit(SIRInstruction::Load(
            value,
            *status,
            SIROffset::Static(0),
            celox_design::PROCESS_STATUS_WIDTH,
        ));
        let started = b.alloc_bit(1, false);
        b.emit(SIRInstruction::Unary(
            started,
            celox_design::UnaryOp::Or,
            value,
        ));
        let next = b.alloc_bit(1, false);
        b.emit(SIRInstruction::Binary(
            next,
            ready,
            celox_design::BinaryOp::And,
            started,
        ));
        ready = next;
    }
    let execute = b.new_block();
    let done = b.new_block();
    b.seal_block(SIRTerminator::Branch {
        cond: ready,
        true_block: (execute, vec![]),
        false_block: (done, vec![]),
    });
    b.switch_to_block(execute);
    b.inline_execution_unit(unit);
    b.seal_block(SIRTerminator::Jump(done, vec![]));
    b.switch_to_block(done);
    b.flush_eu()
        .expect("process activation gate has instructions")
}

fn relocate_units(
    bindings: &PackageBindings,
    expanded: &HashMap<InstancePath, InstanceId>,
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
    root_name: &str,
    indexed_instances: &HashSet<InstanceId>,
    global_boundaries: &HashMap<AbsoluteAddr, std::collections::BTreeSet<usize>>,
    unpacked_element_widths: &HashMap<AbsoluteAddr, usize>,
    clock_domains: &HashMap<AbsoluteAddr, AbsoluteAddr>,
    parallel_ff_units: Option<
        &mut HashMap<AbsoluteAddr, Vec<(InstanceId, FfPart<RegionedAbsoluteAddr>)>>,
    >,
    split_commits: &mut SplitCommits,
    trace_opts: &crate::FrontendTraceOptions,
    trace: &mut Option<&mut crate::FrontendTrace>,
) -> Result<
    (
        SLTNodeArena<AbsoluteAddr>,
        HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
        HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
        HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
        HashMap<AbsoluteAddr, Vec<FfAccessSummary<RegionedAbsoluteAddr>>>,
        HashMap<InstanceId, FfRuntimeRelocation>,
        Vec<celox_slt::LogicPath<AbsoluteAddr>>,
        Vec<CombObserver<AbsoluteAddr>>,
        HashMap<i64, RuntimeErrorInfo<AbsoluteAddr>>,
        Vec<RuntimeEventSite>,
        Vec<ExternFunction>,
        i64,
        Vec<RelocatedProcess>,
    ),
    ParserError,
> {
    let mut global_arena = SLTNodeArena::<AbsoluteAddr>::new();
    let mut processes = Vec::new();
    let mut eval_apply_ffs: HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>> =
        HashMap::default();
    let mut eval_only_ffs: HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>> =
        HashMap::default();
    let mut apply_ffs: HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>> =
        HashMap::default();
    let mut ff_access_summaries: HashMap<AbsoluteAddr, Vec<FfAccessSummary<RegionedAbsoluteAddr>>> =
        HashMap::default();
    let mut ff_runtime_relocations = HashMap::default();
    let mut comb_blocks = Vec::new();
    let mut comb_observers = Vec::new();
    let mut runtime_errors = HashMap::default();
    let mut runtime_event_sites = Vec::new();
    let mut extern_functions = Vec::<ExternFunction>::new();
    let mut extern_function_indices = HashMap::<String, usize>::default();
    let mut next_runtime_error_code = 2000;
    let mut parallel_ff_units = parallel_ff_units;
    let mut initial_statuses: Vec<_> = expanded
        .values()
        .flat_map(|id| {
            modules[&instance_modules[id]]
                .processes
                .iter()
                .map(|process| bindings.locate(*id, process.slots.status))
        })
        .collect();
    initial_statuses.sort();
    initial_statuses.dedup();

    for (path, id) in expanded {
        let module_id = &instance_modules[id];
        let sim_module = &modules[module_id];
        let runtime_event_site_base = u32::try_from(runtime_event_sites.len()).map_err(|_| {
            ParserError::illegal_context(
                "FF runtime-event relocation",
                "runtime event site count exceeds u32",
                None,
            )
        })?;
        let relocate_ff_summary = |summary: &FfAccessSummary<RegionedVarAddr>| {
            let relocate_addr =
                |addr: RegionedVarAddr| bindings.locate_regioned(addr.region, *id, addr.var_id);
            FfAccessSummary {
                reads: summary
                    .reads
                    .iter()
                    .map(|read| VarAtomBase {
                        id: relocate_addr(read.id),
                        access: read.access,
                    })
                    .collect(),
                writes: summary
                    .writes
                    .iter()
                    .map(|write| VarAtomBase {
                        id: relocate_addr(write.id),
                        access: write.access,
                    })
                    .collect(),
                dynamic_writes: summary
                    .dynamic_writes
                    .iter()
                    .copied()
                    .map(relocate_addr)
                    .collect(),
            }
        };
        for (trigger_set, summary) in &sim_module.ff_access_summaries {
            let clock_addr = bindings.locate(*id, trigger_set.clock);
            let canonical_clock = clock_domains
                .get(&clock_addr)
                .copied()
                .unwrap_or(clock_addr);
            ff_access_summaries
                .entry(canonical_clock)
                .or_default()
                .push(relocate_ff_summary(summary));
            for &reset in &trigger_set.resets {
                let reset_addr = bindings.locate(*id, reset);
                let canonical_reset = clock_domains
                    .get(&reset_addr)
                    .copied()
                    .unwrap_or(reset_addr);
                ff_access_summaries
                    .entry(canonical_reset)
                    .or_default()
                    .push(relocate_ff_summary(summary));
            }
        }
        let mut runtime_error_codes = HashMap::default();
        for (&local_code, info) in &sim_module.runtime_errors {
            let global_code = next_runtime_error_code;
            next_runtime_error_code += 1;
            runtime_error_codes.insert(local_code, global_code);
            runtime_errors.insert(
                global_code,
                RuntimeErrorInfo {
                    message: info.message.clone(),
                    signals: info
                        .signals
                        .iter()
                        .filter(|var_id| sim_module.variables.contains_key(var_id))
                        .map(|&var_id| bindings.locate(*id, var_id))
                        .collect(),
                },
            );
        }
        ff_runtime_relocations.insert(
            *id,
            FfRuntimeRelocation {
                error_codes: runtime_error_codes.clone(),
                event_site_base: runtime_event_site_base,
            },
        );
        // Extern functions are shared by C symbol name across instances, and
        // every import of one name must declare the same prototype.
        let mut extern_function_map = HashMap::default();
        for (local, function) in sim_module.extern_functions.iter().enumerate() {
            let global = match extern_function_indices.get(&function.name).copied() {
                Some(global) if extern_functions[global] != *function => {
                    return Err(ParserError::ExternSignatureMismatch {
                        name: function.name.clone(),
                        first: extern_functions[global].to_string(),
                        second: function.to_string(),
                    });
                }
                Some(global) => global,
                None => {
                    extern_function_indices.insert(function.name.clone(), extern_functions.len());
                    extern_functions.push(function.clone());
                    extern_functions.len() - 1
                }
            };
            extern_function_map.insert(local as u32, global as u32);
        }
        let mut runtime_event_site_map = HashMap::default();
        let scope = elaborated_scope_name(root_name, path, expanded, indexed_instances);
        for (local_site, site) in sim_module.runtime_event_sites.iter().enumerate() {
            let global_site = runtime_event_sites.len() as u32;
            runtime_event_site_map.insert(local_site as u32, global_site);
            let mut site = site.clone();
            site.scope = Some(scope.clone());
            runtime_event_sites.push(site);
        }

        // Lane-partitioning units of every trigger group: its independently
        // lowered parts when present, otherwise its whole evaluate/apply pair.
        if let Some(parallel_ff_units) = parallel_ff_units.as_deref_mut() {
            let relocate = |unit: &ExecutionUnit<RegionedVarAddr>| {
                relocate_executation_unit_with_errors(
                    unit,
                    &|addr| bindings.locate_regioned(addr.region, *id, addr.var_id),
                    &runtime_error_codes,
                    &runtime_event_site_map,
                    &extern_function_map,
                )
            };
            let mut trigger_sets = sim_module.eval_only_ff_blocks.keys().collect::<Vec<_>>();
            trigger_sets.sort_unstable();
            for trigger_set in trigger_sets {
                let parts = match sim_module.parallel_ff_parts.get(trigger_set) {
                    Some(parts) => parts
                        .iter()
                        .map(|part| part.map_units(relocate))
                        .collect::<Vec<_>>(),
                    None => {
                        let Some(apply) = sim_module.apply_ff_blocks.get(trigger_set) else {
                            continue;
                        };
                        vec![FfPart {
                            evaluate: relocate(&sim_module.eval_only_ff_blocks[trigger_set]),
                            apply: relocate(apply),
                        }]
                    }
                };
                let canonical = |var_id| {
                    let address = bindings.locate(*id, var_id);
                    clock_domains.get(&address).copied().unwrap_or(address)
                };
                let events = std::iter::once(canonical(trigger_set.clock))
                    .chain(trigger_set.resets.iter().map(|&reset| canonical(reset)))
                    .collect::<BTreeSet<_>>();
                for event in events {
                    parallel_ff_units
                        .entry(event)
                        .or_default()
                        .extend(parts.iter().cloned().map(|part| (*id, part)));
                }
            }
        }

        let arena_start = global_arena.len();
        let mut relocated_module = flatten_with_trace(
            sim_module,
            path,
            expanded,
            global_boundaries,
            unpacked_element_widths,
            &mut global_arena,
            bindings,
            trace_opts,
            trace.as_deref_mut(),
        )?;
        remap_for_fold_runtime_event_sites(
            &mut global_arena,
            arena_start,
            &runtime_event_site_map,
        )?;
        for observer in &mut relocated_module.comb_observers {
            observer.site_id = runtime_event_site_map[&observer.site_id];
            observer.activation_group = runtime_event_site_map[&observer.activation_group];
        }
        for path in &mut relocated_module.comb_blocks {
            if let celox_slt::LogicPathTarget::Process {
                unit,
                after_initials,
                ..
            } = &mut path.target
            {
                **unit = relocate_executation_unit_with_errors(
                    unit,
                    &|addr| *addr,
                    &runtime_error_codes,
                    &runtime_event_site_map,
                    &extern_function_map,
                );
                if *after_initials && !initial_statuses.is_empty() {
                    **unit = after_initial_processes(unit, &initial_statuses);
                }
            }
        }
        comb_blocks.extend(relocated_module.comb_blocks);
        comb_observers.extend(relocated_module.comb_observers);

        // Relocate sequential blocks for this instance
        for (trigger_set, eu) in &sim_module.eval_apply_ff_blocks {
            let clock_addr = bindings.locate(*id, trigger_set.clock);
            let canonical_addr = clock_domains
                .get(&clock_addr)
                .copied()
                .unwrap_or(clock_addr);

            eval_apply_ffs.entry(canonical_addr).or_default().push(
                relocate_executation_unit_with_errors(
                    eu,
                    &|addr| bindings.locate_regioned(addr.region, *id, addr.var_id),
                    &runtime_error_codes,
                    &runtime_event_site_map,
                    &extern_function_map,
                ),
            );

            for &reset in &trigger_set.resets {
                let reset_addr = bindings.locate(*id, reset);
                let canonical_addr = clock_domains
                    .get(&reset_addr)
                    .copied()
                    .unwrap_or(reset_addr);
                eval_apply_ffs.entry(canonical_addr).or_default().push(
                    relocate_executation_unit_with_errors(
                        eu,
                        &|addr| bindings.locate_regioned(addr.region, *id, addr.var_id),
                        &runtime_error_codes,
                        &runtime_event_site_map,
                        &extern_function_map,
                    ),
                );
            }
        }

        for (trigger_set, eu) in &sim_module.eval_only_ff_blocks {
            let clock_addr = bindings.locate(*id, trigger_set.clock);
            let canonical_addr = clock_domains
                .get(&clock_addr)
                .copied()
                .unwrap_or(clock_addr);
            eval_only_ffs.entry(canonical_addr).or_default().push(
                relocate_executation_unit_with_errors(
                    eu,
                    &|addr| bindings.locate_regioned(addr.region, *id, addr.var_id),
                    &runtime_error_codes,
                    &runtime_event_site_map,
                    &extern_function_map,
                ),
            );

            for &reset in &trigger_set.resets {
                let reset_addr = bindings.locate(*id, reset);
                let canonical_addr = clock_domains
                    .get(&reset_addr)
                    .copied()
                    .unwrap_or(reset_addr);
                eval_only_ffs.entry(canonical_addr).or_default().push(
                    relocate_executation_unit_with_errors(
                        eu,
                        &|addr| bindings.locate_regioned(addr.region, *id, addr.var_id),
                        &runtime_error_codes,
                        &runtime_event_site_map,
                        &extern_function_map,
                    ),
                );
            }
        }

        for (index, process) in sim_module.processes.iter().enumerate() {
            processes.push((
                (*id, index),
                RelocatedProcess {
                    kernel: relocate_executation_unit_with_errors(
                        &process.kernel,
                        &|addr| bindings.locate_regioned(addr.region, *id, addr.var_id),
                        &runtime_error_codes,
                        &runtime_event_site_map,
                        &extern_function_map,
                    ),
                    slots: process.slots.map(|var_id| bindings.locate(*id, var_id)),
                    origin: celox_design::ProcessOrigin {
                        scope: Some(scope.clone()),
                        ..process.origin.clone()
                    },
                    key: (*id, index),
                },
            ));
        }

        for (trigger_set, eu) in &sim_module.apply_ff_blocks {
            let clock_addr = bindings.locate(*id, trigger_set.clock);
            let canonical_addr = clock_domains
                .get(&clock_addr)
                .copied()
                .unwrap_or(clock_addr);
            // The split commit of each apply unit, in the same order.
            let split = sim_module
                .split_commit_ff_blocks
                .get(trigger_set)
                .map(|split| SplitCommit {
                    active: relocate_executation_unit_with_errors(
                        &split.active,
                        &|addr| bindings.locate_regioned(addr.region, *id, addr.var_id),
                        &runtime_error_codes,
                        &runtime_event_site_map,
                        &extern_function_map,
                    ),
                    nba: relocate_executation_unit_with_errors(
                        &split.nba,
                        &|addr| bindings.locate_regioned(addr.region, *id, addr.var_id),
                        &runtime_error_codes,
                        &runtime_event_site_map,
                        &extern_function_map,
                    ),
                });
            // Pushed like the apply units below: the clock, then each reset.
            split_commits
                .entry(canonical_addr)
                .or_default()
                .push(split.clone());
            for &reset in &trigger_set.resets {
                let reset_addr = bindings.locate(*id, reset);
                let canonical_addr = clock_domains
                    .get(&reset_addr)
                    .copied()
                    .unwrap_or(reset_addr);
                split_commits
                    .entry(canonical_addr)
                    .or_default()
                    .push(split.clone());
            }
            apply_ffs.entry(canonical_addr).or_default().push(
                relocate_executation_unit_with_errors(
                    eu,
                    &|addr| bindings.locate_regioned(addr.region, *id, addr.var_id),
                    &runtime_error_codes,
                    &runtime_event_site_map,
                    &extern_function_map,
                ),
            );

            for &reset in &trigger_set.resets {
                let reset_addr = bindings.locate(*id, reset);
                let canonical_addr = clock_domains
                    .get(&reset_addr)
                    .copied()
                    .unwrap_or(reset_addr);
                apply_ffs.entry(canonical_addr).or_default().push(
                    relocate_executation_unit_with_errors(
                        eu,
                        &|addr| bindings.locate_regioned(addr.region, *id, addr.var_id),
                        &runtime_error_codes,
                        &runtime_event_site_map,
                        &extern_function_map,
                    ),
                );
            }
        }
    }
    // A canonical event may collect multiple reset groups or instances. The
    // ordinary tick path must evaluate every group against pre-edge state
    // before any group publishes its nonblocking assignments. Concatenating
    // each module's eval+apply unit instead makes behavior depend on map order.
    for (event, units) in &mut eval_apply_ffs {
        if units.len() > 1 {
            let evaluations = eval_only_ffs.get(event).ok_or_else(|| {
                ParserError::illegal_context(
                    "FF event assembly",
                    "shared event has no evaluation phase",
                    None,
                )
            })?;
            let applications = apply_ffs.get(event).ok_or_else(|| {
                ParserError::illegal_context(
                    "FF event assembly",
                    "shared event has no application phase",
                    None,
                )
            })?;
            let phases = evaluations.iter().chain(applications).collect::<Vec<_>>();
            // Keep the staging stores and their commits in the same optimizer
            // unit: WORKING-region stores are otherwise local dead stores.
            *units = vec![celox_sir::merge_sir_eu_refs(&phases).0];
        }
    }
    Ok((
        global_arena,
        eval_apply_ffs,
        eval_only_ffs,
        apply_ffs,
        ff_access_summaries,
        ff_runtime_relocations,
        comb_blocks,
        comb_observers,
        runtime_errors,
        runtime_event_sites,
        extern_functions,
        next_runtime_error_code,
        {
            // Instances are numbered in elaboration order, so this is the
            // order processes start in at time zero.
            processes.sort_unstable_by_key(|(key, _)| *key);
            processes.into_iter().map(|(_, process)| process).collect()
        },
    ))
}

/// A process kernel placed in one instance.
struct RelocatedProcess {
    kernel: ExecutionUnit<RegionedAbsoluteAddr>,
    slots: ProcessSlots<AbsoluteAddr>,
    origin: celox_design::ProcessOrigin,
    /// The instance and the module-local index of the process.
    key: (InstanceId, usize),
}

/// Counter slots, wait sites, nonblocking sites and time slots of the whole
/// design.
struct TimingMetadata {
    counter_slots: Vec<CounterSlots<AbsoluteAddr>>,
    pending_slots: Vec<crate::watch::PendingSlots<AbsoluteAddr>>,
    wait_sites: Vec<WaitSite<AbsoluteAddr>>,
    nba_sites: Vec<NbaSite<AbsoluteAddr>>,
    host_request_sites: Vec<HostRequestSite<AbsoluteAddr>>,
    time_slots: Vec<AbsoluteAddr>,
    nba_switches: Vec<NbaSwitch<AbsoluteAddr>>,
}

/// For every combinational value, the variables it is computed from.
fn comb_sources(
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

/// The variables `unit` loads.
fn unit_loads(unit: &ExecutionUnit<RegionedAbsoluteAddr>, loads: &mut HashSet<AbsoluteAddr>) {
    for block in unit.blocks.values() {
        for instruction in &block.instructions {
            if let SIRInstruction::Load(_, addr, ..) = instruction {
                loads.insert(addr.absolute_addr());
            }
        }
    }
}

/// `reads` closed over combinational logic: with a value, the variables it
/// is computed from.
fn through_comb(
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
fn split_observed_commits<'k>(
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

/// The switches of the switched nonblocking sites that may stage their
/// value: the site's target has no other writer (no store or commit of any
/// unit, no other nonblocking site, not a top-level port the host drives)
/// and is not in `observed`.
fn staged_nba_sites<'u>(
    timing: &TimingMetadata,
    units: impl Iterator<Item = &'u ExecutionUnit<RegionedAbsoluteAddr>>,
    top_ports: &HashSet<AbsoluteAddr>,
    observed: &HashSet<AbsoluteAddr>,
) -> HashSet<AbsoluteAddr> {
    let mut written = HashSet::default();
    for unit in units {
        for block in unit.blocks.values() {
            for instruction in &block.instructions {
                match instruction {
                    SIRInstruction::Store(dst, ..) | SIRInstruction::Commit(_, dst, ..) => {
                        written.insert(dst.absolute_addr());
                    }
                    _ => {}
                }
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
fn select_nba_paths(
    timing: &mut TimingMetadata,
    staged: &HashSet<AbsoluteAddr>,
    processes: &mut [RelocatedProcess],
) {
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
fn fold_nba_switches(
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

/// Place the timing metadata of every instance. Sites name the global index
/// of their process; a kernel numbers its sites in the order they appear
/// here, which is the module's order.
fn relocate_timing(
    bindings: &PackageBindings,
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
    processes: &[RelocatedProcess],
) -> TimingMetadata {
    let process_index: HashMap<(InstanceId, usize), usize> = processes
        .iter()
        .enumerate()
        .map(|(index, process)| (process.key, index))
        .collect();
    let mut instances = instance_modules.iter().collect::<Vec<_>>();
    instances.sort_unstable_by_key(|(instance, _)| **instance);
    let mut metadata = TimingMetadata {
        counter_slots: Vec::new(),
        pending_slots: Vec::new(),
        wait_sites: Vec::new(),
        nba_sites: Vec::new(),
        host_request_sites: Vec::new(),
        time_slots: Vec::new(),
        nba_switches: Vec::new(),
    };
    for (&instance, module) in instances {
        let module = &modules[module];
        let locate = |var_id| bindings.locate(instance, var_id);
        metadata
            .counter_slots
            .extend(module.counter_slots.iter().map(|slots| slots.map(locate)));
        metadata
            .pending_slots
            .extend(module.pending_slots.iter().map(|slots| slots.map(locate)));
        metadata
            .wait_sites
            .extend(module.wait_sites.iter().map(|site| WaitSite {
                process: process_index[&(instance, site.process)],
                ..site.map(locate)
            }));
        metadata
            .nba_sites
            .extend(module.nba_sites.iter().map(|site| NbaSite {
                process: process_index[&(instance, site.process)],
                ..site.map(locate)
            }));
        metadata
            .host_request_sites
            .extend(
                module
                    .host_request_sites
                    .iter()
                    .map(|site| HostRequestSite {
                        process: process_index[&(instance, site.process)],
                        ..site.map(locate)
                    }),
            );
        metadata.time_slots.extend(module.time_slot.map(locate));
        metadata.nba_switches.extend(
            module
                .nba_switches
                .iter()
                .map(|switch| switch.map(|&var| locate(var))),
        );
    }
    metadata
}

type FfUnits = HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>;
type FfParts = HashMap<AbsoluteAddr, Vec<(InstanceId, FfPart<RegionedAbsoluteAddr>)>>;

/// The signals some flip-flop evaluation may store twice in its working
/// state in one execution.
fn ff_multi_stored(
    eval_apply_ffs: &FfUnits,
    eval_only_ffs: &FfUnits,
    parallel_ff_units: &FfParts,
) -> HashSet<AbsoluteAddr> {
    let working = |address: &RegionedAbsoluteAddr| {
        (address.region == WORKING_REGION).then(|| address.absolute_addr())
    };
    eval_apply_ffs
        .values()
        .chain(eval_only_ffs.values())
        .flatten()
        .chain(
            parallel_ff_units
                .values()
                .flatten()
                .map(|(_, part)| &part.evaluate),
        )
        .flat_map(|unit| crate::watch::multi_store_only_candidates(unit, working))
        .collect()
}

/// [`crate::watch::instrument_unit`] for the stores alone: the commits that
/// seed a flip-flop evaluation's working state are not writes of the
/// process.
fn instrument_stores(
    unit: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    watch_for: impl Fn(&RegionedAbsoluteAddr) -> Option<crate::watch::WatchCode<RegionedAbsoluteAddr>>,
) {
    crate::watch::instrument_writes(unit, |instruction| match instruction {
        SIRInstruction::Store(address, ..) => watch_for(address),
        _ => None,
    });
}

/// Run `f` on every flip-flop evaluation.
fn for_each_ff_evaluation(
    eval_apply_ffs: &mut FfUnits,
    eval_only_ffs: &mut FfUnits,
    parallel_ff_units: &mut FfParts,
    mut f: impl FnMut(&mut ExecutionUnit<RegionedAbsoluteAddr>),
) {
    for unit in eval_apply_ffs
        .values_mut()
        .chain(eval_only_ffs.values_mut())
        .flatten()
    {
        f(unit);
    }
    for (_, part) in parallel_ff_units.values_mut().flatten() {
        f(&mut part.evaluate);
    }
}

/// Counted signals: those a unit may store twice between two runtime checks
/// (a frontend declared counters for them) and that a flip-flop domain or a
/// wait watches, by signal.
fn counted_signals(
    timing: &TimingMetadata,
    event_addresses: &HashSet<AbsoluteAddr>,
) -> BTreeMap<AbsoluteAddr, CounterSlots<AbsoluteAddr>> {
    let waited: HashSet<AbsoluteAddr> = timing
        .wait_sites
        .iter()
        .flat_map(|site| site.terms.iter().map(|(signal, _)| *signal))
        .collect();
    timing
        .counter_slots
        .iter()
        .filter(|slots| event_addresses.contains(&slots.signal) || waited.contains(&slots.signal))
        .map(|slots| (slots.signal, *slots))
        .collect()
}

/// Keep, for every instance of a process in `comb_alternatives`, either
/// its imperative unit, when a variable it may store twice is `counted`, or
/// its symbolic definitions, and remove the paths of the other form from
/// `comb_blocks`. Paths of other drivers of the same variables stay.
fn choose_comb_alternatives(
    bindings: &PackageBindings,
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
    counted: &BTreeMap<AbsoluteAddr, CounterSlots<AbsoluteAddr>>,
    comb_blocks: &mut Vec<LogicPath<AbsoluteAddr>>,
) {
    let mut imperative = HashSet::default();
    let mut symbolic = HashMap::<AbsoluteAddr, Vec<_>>::default();
    for (&instance, module) in instance_modules {
        for alternative in &modules[module].comb_alternatives {
            let locate = |var_id| bindings.locate(instance, var_id);
            if alternative
                .candidates
                .iter()
                .any(|&candidate| counted.contains_key(&locate(candidate)))
            {
                for target in &alternative.targets {
                    symbolic
                        .entry(locate(target.id))
                        .or_default()
                        .push(target.access);
                }
            } else {
                imperative.insert(locate(alternative.process));
            }
        }
    }
    if imperative.is_empty() && symbolic.is_empty() {
        return;
    }
    // Flattening may have split a definition at the boundaries of its
    // readers, so a path of the process is any piece of one of its ranges.
    // No other driver writes those bits: the SV frontend rejects overlapping
    // drivers (`validate_variable_driver_ranges`).
    comb_blocks.retain(|path| match &path.target {
        LogicPathTarget::Var(target) => !symbolic
            .get(&target.id)
            .is_some_and(|ranges| ranges.iter().any(|range| range.overlaps(&target.access))),
        LogicPathTarget::Process { process, .. } => !imperative.contains(process),
        LogicPathTarget::CombCaptureEvent { .. } => true,
    });
}

/// Instrument the process kernels, combinational process units and
/// flip-flop units that write a `counted` signal, and return `counted`; no
/// other unit changes.
fn instrument_counted_signals<'u>(
    timing: &TimingMetadata,
    counted: BTreeMap<AbsoluteAddr, CounterSlots<AbsoluteAddr>>,
    variables: &HashMap<AbsoluteAddr, (usize, bool)>,
    four_state: bool,
    processes: &mut [RelocatedProcess],
    comb_blocks: &mut [LogicPath<AbsoluteAddr>],
    ff_units: impl Iterator<Item = &'u mut ExecutionUnit<RegionedAbsoluteAddr>>,
) -> BTreeMap<AbsoluteAddr, CounterSlots<AbsoluteAddr>> {
    if counted.is_empty() {
        return counted;
    }
    let code = |signal: AbsoluteAddr| {
        let slots = counted.get(&signal)?;
        let (width, is_4state) = variables[&signal];
        Some((slots, width, four_state && is_4state))
    };
    for process in processes {
        crate::watch::instrument_unit(&mut process.kernel, |address| {
            if address.region != STABLE_REGION {
                return None;
            }
            let (slots, width, four_state) = code(address.absolute_addr())?;
            let stable =
                |var: AbsoluteAddr| RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, var);
            Some(crate::watch::WatchCode {
                signal: stable(slots.signal),
                offset: celox_sir::SIROffset::Static(0),
                width,
                four_state,
                counters: CounterKind::ALL
                    .into_iter()
                    .map(|kind| (kind, stable(slots.counter(kind))))
                    .collect(),
            })
        });
    }
    for path in comb_blocks {
        if let LogicPathTarget::Process { unit, .. } = &mut path.target {
            crate::watch::instrument_unit(unit, |address| {
                let (slots, width, four_state) = code(*address)?;
                Some(crate::watch::WatchCode {
                    signal: slots.signal,
                    offset: celox_sir::SIROffset::Static(0),
                    width,
                    four_state,
                    counters: CounterKind::ALL
                        .into_iter()
                        .map(|kind| (kind, slots.counter(kind)))
                        .collect(),
                })
            });
        }
    }
    let stable = |var: AbsoluteAddr| RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, var);
    let working = |var: AbsoluteAddr| RegionedAbsoluteAddr::from_absolute_addr(WORKING_REGION, var);
    let watch = |signal: AbsoluteAddr| {
        let (slots, width, four_state) = code(signal)?;
        Some(crate::watch::WatchCode {
            signal: stable(slots.signal),
            offset: celox_sir::SIROffset::Static(0),
            width,
            four_state,
            counters: CounterKind::ALL
                .into_iter()
                .map(|kind| (kind, stable(slots.counter(kind))))
                .collect(),
        })
    };
    // A flip-flop writes its next state, and a commit publishes it. Repeated
    // writes are held back until the commit; a single write is counted at
    // the commit itself.
    let pending: Vec<_> = timing
        .pending_slots
        .iter()
        .filter_map(|slots| {
            Some(crate::watch::PendingCode {
                watch: watch(slots.signal)?,
                write: working(slots.staged.unwrap_or(slots.signal)),
                staged: slots.staged.is_some(),
                value: stable(slots.value),
                covered: stable(slots.covered),
                first: stable(slots.first),
                changed: stable(slots.changed),
                written: stable(slots.written),
                edges: vec![
                    (CounterKind::Rises, stable(slots.rises)),
                    (CounterKind::Falls, stable(slots.falls)),
                ],
            })
        })
        .collect();
    let held = |staged: bool| -> HashSet<AbsoluteAddr> {
        timing
            .pending_slots
            .iter()
            .filter(|slots| slots.staged.is_some() == staged)
            .map(|slots| slots.signal)
            .collect()
    };
    let (held_next, held_staged) = (held(false), held(true));
    for unit in ff_units {
        crate::watch::instrument_writes(unit, |instruction| match instruction {
            // The commit of the next state; a seed copies the other way.
            SIRInstruction::Commit(source, destination, ..)
                if source.region == WORKING_REGION
                    && destination.region == STABLE_REGION
                    && source.absolute_addr() == destination.absolute_addr()
                    && !held_next.contains(&destination.absolute_addr()) =>
            {
                watch(destination.absolute_addr())
            }
            // The merge of staged nonblocking values.
            SIRInstruction::Store(destination, ..)
                if destination.region == STABLE_REGION
                    && !held_staged.contains(&destination.absolute_addr()) =>
            {
                watch(destination.absolute_addr())
            }
            _ => None,
        });
        crate::watch::instrument_pending(unit, &pending);
    }
    counted
}

fn build_comb_observer_capture_paths(
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
    for observer_idx in 0..observers.len() {
        let observer = &observers[observer_idx];
        let has_statement_position_dependency =
            observer_has_statement_position_dependency(comb_blocks, observer);
        let order_before = observer_order_before(comb_blocks, observer);
        let order_after = observer_order_after(comb_blocks, observer);
        let trigger_paths = if has_statement_position_dependency {
            observer_trigger_paths(comb_blocks, observer)
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
                .filter(|atom| !observer_statement_position_overlaps(comb_blocks, observer, atom))
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
            .filter(|atom| !observer_statement_position_overlaps(comb_blocks, observer, atom))
            .collect();
        for (_, node) in &observer.local_inputs {
            let mut local_sources = HashSet::default();
            crate::flattening::collect_inputs(*node, arena, &mut local_sources);
            sources.extend(
                local_sources
                    .into_iter()
                    .filter(|atom| !observer_written_input_overlaps(observer, atom))
                    .filter(|atom| !local_input_ids.contains(&atom.id))
                    .filter(|atom| {
                        !observer_statement_position_overlaps(comb_blocks, observer, atom)
                    }),
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
        let fatal_error_code = matches!(
            sites[observer.site_id as usize].kind,
            RuntimeEventKind::AssertFatal
        )
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
                let member_fatal_error_code = matches!(
                    sites[member.site_id as usize].kind,
                    RuntimeEventKind::AssertFatal
                )
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

fn apply_always_comb_previous_source_ordering(comb_blocks: &mut [LogicPath<AbsoluteAddr>]) {
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

fn observer_written_input_overlaps(
    observer: &CombObserver<AbsoluteAddr>,
    atom: &VarAtomBase<AbsoluteAddr>,
) -> bool {
    observer
        .written_input_atoms
        .iter()
        .any(|written| written.id == atom.id && written.access.overlaps(&atom.access))
}

fn observer_statement_position_overlaps(
    paths: &[LogicPath<AbsoluteAddr>],
    observer: &CombObserver<AbsoluteAddr>,
    atom: &VarAtomBase<AbsoluteAddr>,
) -> bool {
    atom_overlaps_any(atom, observer_affected_by_preceding_writes(paths, observer))
}

fn observer_has_statement_position_dependency(
    paths: &[LogicPath<AbsoluteAddr>],
    observer: &CombObserver<AbsoluteAddr>,
) -> bool {
    if observer.preceding_writes.is_empty() {
        return false;
    }
    let affected = observer_affected_by_preceding_writes(paths, observer);
    observer
        .position_inputs
        .iter()
        .chain(observer.observed_inputs.iter())
        .any(|input| atom_overlaps_any(input, &affected))
}

fn observer_trigger_paths(
    paths: &[LogicPath<AbsoluteAddr>],
    observer: &CombObserver<AbsoluteAddr>,
) -> Vec<LogicPathId> {
    let mut seen_targets = HashSet::default();
    let affected = observer_affected_by_preceding_writes(paths, observer);
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
                || !atom_overlaps_any(target, &affected)
                || !seen_targets.insert((target.id, target.access.lsb, target.access.msb))
            {
                return None;
            }
            Some(LogicPathId(idx))
        })
        .collect()
}

fn observer_pre_lower_nodes(
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

fn capture_contains_nested_snapshot(node: NodeId, arena: &SLTNodeArena<AbsoluteAddr>) -> bool {
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

fn annotate_comb_capture_enable_sites(
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

fn observer_order_after(
    paths: &[LogicPath<AbsoluteAddr>],
    observer: &CombObserver<AbsoluteAddr>,
) -> HashSet<LogicPathId> {
    let mut result = HashSet::default();
    if !observer_has_statement_position_dependency(paths, observer) {
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

fn observer_order_before(
    paths: &[LogicPath<AbsoluteAddr>],
    observer: &CombObserver<AbsoluteAddr>,
) -> HashSet<LogicPathId> {
    let preceding_writes = observer.preceding_writes.iter().collect::<Vec<_>>();
    let affected_by_preceding_writes = observer_has_statement_position_dependency(paths, observer)
        .then(|| observer_affected_by_preceding_writes(paths, observer));
    let mut result = HashSet::default();
    for (idx, path) in paths.iter().enumerate() {
        let Some(target) = path.target.var() else {
            continue;
        };
        let already_written = preceding_writes
            .iter()
            .any(|written| target.id == written.id && target.access.overlaps(&written.access));
        let is_later_observed_write = observer_written_input_overlaps(observer, target);
        let is_later_affected_write = affected_by_preceding_writes
            .as_ref()
            .is_some_and(|affected| atom_overlaps_any(target, affected));
        if !already_written && (is_later_observed_write || is_later_affected_write) {
            result.insert(LogicPathId(idx));
        }
    }
    result
}

/// Place a trigger-capture between the write which activated it and each
/// immediate dataflow consumer of that write. Transitive consumers remain
/// ordered by the ordinary LogicPath dependency graph.
fn direct_consumers_of_path_target(
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

fn observer_affected_by_preceding_writes(
    paths: &[LogicPath<AbsoluteAddr>],
    observer: &CombObserver<AbsoluteAddr>,
) -> HashSet<VarAtomBase<AbsoluteAddr>> {
    let mut affected: HashSet<VarAtomBase<AbsoluteAddr>> =
        observer.preceding_writes.iter().copied().collect();
    let mut changed = true;
    while changed {
        changed = false;
        for path in paths {
            let Some(target) = path.target.var() else {
                continue;
            };
            if !path
                .sources
                .iter()
                .any(|source| atom_overlaps_any(source, &affected))
            {
                continue;
            }
            if affected.insert(*target) {
                changed = true;
            }
        }
    }
    affected
}

fn atom_overlaps_any<A: Eq + std::hash::Hash + Copy>(
    atom: &VarAtomBase<A>,
    atoms: impl IntoIterator<Item = impl std::borrow::Borrow<VarAtomBase<A>>>,
) -> bool {
    atoms.into_iter().any(|other| {
        let other = other.borrow();
        atom.id == other.id && atom.access.overlaps(&other.access)
    })
}

/// Event-domain ordering and cascade structure of a flattened design.
struct ClockAnalysis {
    topological_clocks: Vec<AbsoluteAddr>,
    cascaded_clocks: BTreeSet<AbsoluteAddr>,
    /// Each event to the derived domains an occurrence of it can trigger,
    /// transitively, in `topological_clocks` order.
    derived_events: BTreeMap<AbsoluteAddr, Vec<AbsoluteAddr>>,
    /// Each event to the clocks computed combinationally from its signal.
    combinational_derived: BTreeMap<AbsoluteAddr, Vec<CombDerivedClock<AbsoluteAddr>>>,
    /// Events whose combinationally derived domains may run before them in
    /// turn instead of together.
    independent_derived: BTreeSet<AbsoluteAddr>,
    /// Events whose signal combinational logic reads.
    combinationally_read: BTreeSet<AbsoluteAddr>,
}

fn analyze_clock_dependencies(
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
                for inst in &bb.instructions {
                    if let SIRInstruction::Store(target_addr, ..) = inst {
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
                    match instruction {
                        SIRInstruction::Load(_, address, ..) => {
                            reads.insert(address.absolute_addr());
                        }
                        SIRInstruction::Store(address, ..) => {
                            writes.insert(address.absolute_addr());
                        }
                        SIRInstruction::Commit(source, target, ..) => {
                            reads.insert(source.absolute_addr());
                            writes.insert(target.absolute_addr());
                        }
                        SIRInstruction::RuntimeEvent { .. }
                        | SIRInstruction::CombCaptureEvent { .. }
                        | SIRInstruction::CombCaptureEnableIfChanged { .. }
                        | SIRInstruction::ExternCall { .. } => return None,
                        _ => {}
                    }
                }
            }
        }
        Some((reads, writes))
    };
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
    }
}

/// Builds [`GateOp`] programs: a derived clock's combinational cone with its
/// source event's signal as a parameter and state variables as loads.
struct GateBuilder<'a> {
    arena: &'a SLTNodeArena<AbsoluteAddr>,
    /// The single whole-variable combinational definition of a variable, or
    /// `None` when it has several or a procedural one.
    definitions: &'a HashMap<AbsoluteAddr, Option<&'a LogicPath<AbsoluteAddr>>>,
    var_widths: &'a HashMap<AbsoluteAddr, usize>,
    clock_domains: &'a HashMap<AbsoluteAddr, AbsoluteAddr>,
}

/// A gate program under construction.
#[derive(Default)]
struct GateProgram {
    ops: Vec<GateOp<AbsoluteAddr>>,
    stack: usize,
    max_stack: usize,
}

impl GateProgram {
    /// Larger cones settle the design instead.
    const MAX_OPS: usize = 64;
    const MAX_STACK: usize = 16;

    fn push(&mut self, op: GateOp<AbsoluteAddr>, pops: usize) -> Option<()> {
        if self.ops.len() >= Self::MAX_OPS {
            return None;
        }
        self.ops.push(op);
        self.stack = self.stack.checked_sub(pops)? + 1;
        self.max_stack = self.max_stack.max(self.stack);
        Some(())
    }
}

fn gate_u32(value: usize) -> Option<u32> {
    u32::try_from(value).ok()
}

impl GateBuilder<'_> {
    const MAX_DEPTH: usize = 32;

    fn program(
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

    fn width(&self, node: NodeId) -> Option<usize> {
        self.arena
            .node_width(node)
            .filter(|width| (1..=64).contains(width))
    }

    fn emit(
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
