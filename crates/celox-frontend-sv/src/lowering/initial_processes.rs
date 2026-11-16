//! Lowering of declaration initializers and of `initial` and `final` procedures.

use super::*;

/// Process kind and index identify a thread without merging the lowering paths.
/// Each instance owns an initialization RNG seeded with the same default
/// (IEEE 1800-2023 18.14.1); relocation keeps sibling RNG states independent.
pub(super) fn process_random_seeds(module: &sv::ir::Module) -> HashMap<(u8, usize), u32> {
    let mut processes: Vec<_> = module
        .ff_processes()
        .iter()
        .enumerate()
        .map(|(i, p)| (p.creation_order(), 0u8, i))
        .chain(
            module
                .initial_processes()
                .iter()
                .enumerate()
                .filter(|(_, p)| !p.is_synthetic())
                .map(|(i, p)| (p.creation_order(), 1, i)),
        )
        .chain(
            module
                .comb_processes()
                .iter()
                .enumerate()
                .map(|(i, p)| (p.creation_order(), 2, i)),
        )
        .collect();
    processes.sort_by_key(|&(order, kind, i)| (order, kind, i));
    let mut state = 0x4d59_5df4u32;
    processes
        .into_iter()
        .map(|(_, kind, index)| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((kind, index), state)
        })
        .collect()
}

/// The source procedure of a process kernel, for diagnostics.
/// The synthetic process that groups a module's delayed flip-flop
/// nonblocking sites for timed publication. Its kernel is empty: the
/// domains' commit units stage the updates and set the sites' pending
/// flags, and publication queues them for their due times.
pub(super) fn delayed_publication_process(
    variables: &mut HashMap<SourceVarId, SvVariable>,
    index: usize,
) -> SymbolicProcess {
    let mut next_id = SourceVarId(
        variables
            .keys()
            .map(|id| id.0 + 1)
            .max()
            .unwrap_or_default(),
    );
    let mut declare = |slot: &str, width: usize| {
        let id = next_var_id(&mut next_id);
        variables.insert(
            id,
            SvVariable {
                float: None,
                value_kind: celox_design::ValueKind::Bits,
                path: vec![format!("$ff-nba[{index}]"), slot.to_string()],
                width,
                signed: false,
                is_4state: false,
                is_unpacked_struct: false,
                packed_ranges: vec![(width as i128 - 1, 0)],
                array_dims: Vec::new(),
                unpacked_ranges: Vec::new(),
                domain_kind: DomainKind::Other,
                kind: VariableKind::Variable,
                type_kind: PortTypeKind::Bit,
                source: None,
                hidden: true,
                force_control: None,
                sampled_from: None,
                history_of: None,
                activation_scratch: false,
            },
        );
        id
    };
    let resume = declare("resume", PROCESS_RESUME_WIDTH);
    let status = declare("status", celox_design::PROCESS_STATUS_WIDTH);
    let delay = declare("delay", celox_design::PROCESS_DELAY_WIDTH);
    // The kernel only marks itself done: it never runs the sites.
    let mut kernel = SIRBuilder::new();
    let code = kernel.alloc_bit(celox_design::PROCESS_STATUS_WIDTH, false);
    kernel.emit(SIRInstruction::Imm(
        code,
        SIRValue::new(celox_design::ProcessStatus::Done.code()),
    ));
    kernel.emit(SIRInstruction::Store(
        RegionedVarAddrBase {
            region: STABLE_REGION,
            var_id: status,
        },
        SIROffset::Static(0),
        celox_design::PROCESS_STATUS_WIDTH,
        code,
        Vec::new(),
        Vec::new(),
    ));
    SymbolicProcess {
        kernel: seal_builder(kernel),
        frame: None,
        slots: ProcessSlots {
            kind: celox_design::ProcessKind::Regular,
            requires_timed_simulation: false,
            const_delays: true,
            program: None,
            resume,
            status,
            delay,
            call_top: None,
        },
        call_resumes: Vec::new(),
        origin: celox_design::ProcessOrigin {
            construct: "flip-flop delayed nonblocking publication".to_string(),
            location: None,
            scope: None,
        },
    }
}

fn process_origin(process: &sv::ir::InitialProcess) -> celox_design::ProcessOrigin {
    let construct = match process.site() {
        Some(site) => site.construct,
        None if process.is_initializer() => "declaration initializer",
        None if process.is_final() => "final",
        None if process.repeats() => "always",
        None => "initial",
    };
    celox_design::ProcessOrigin {
        construct: construct.to_string(),
        location: process.site().map(ToString::to_string),
        scope: None,
    }
}

/// Lower declaration initializers and procedural kernels. Final procedures
/// retain kernels but execute only at simulation termination (9.2.3); they
/// must never contribute folded initial state.
///
/// A block whose writes have constant values defines initial state, with
/// the hidden variables used while executing it discarded. Declaration
/// initializers come first, and blocks start from the values they define.
/// A block that reads design state or runs a system task becomes a process
/// kernel; such kernels start at time zero in declaration order, initializers
/// first.
#[allow(clippy::too_many_arguments)]
pub(super) fn lower_initial_processes(
    module: &sv::ir::Module,
    tables: &procedural::ModuleTables,
    variables: &mut HashMap<SourceVarId, SvVariable>,
    name_to_id: &mut HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    four_state: bool,
    runtime_event_sites: &mut Vec<RuntimeEventSite>,
    runtime_errors: &mut HashMap<i64, RuntimeErrorInfo<SourceVarId>>,
    extern_functions: &mut Vec<ExternFunction>,
    warnings: &mut Vec<crate::warning::PendingWarning>,
    mut calls: Option<&mut recursion::CallGraph>,
) -> Result<
    (
        Vec<InitialStateValue<SourceVarId>>,
        Vec<InitialStateValue<SourceVarId>>,
        Vec<InitialStateValue<SourceVarId>>,
        Vec<SymbolicProcess>,
        Vec<WaitSite<SourceVarId>>,
        Vec<NbaSite<SourceVarId>>,
        Vec<NbaSwitch<SourceVarId>>,
        Vec<celox_design::HostRequestSite<SourceVarId>>,
        Vec<(SourceVarId, Vec<sv::ir::Stmt>)>,
    ),
    sv::AnalyzerError,
> {
    if module.initial_processes().is_empty() {
        return Ok(Default::default());
    }
    let mut values = Vec::new();
    let mut declaration_values = Vec::new();
    let mut folded_values = Vec::new();
    let mut runtime = Vec::new();
    // The unit of the call graph of each runtime process.
    let mut runtime_units = Vec::new();
    let mut pm = procedural::ProcModule::new(
        module,
        tables,
        variables,
        name_to_id,
        constants,
        parameter_types,
        four_state,
        warnings,
    );
    pm.calls = calls.as_deref_mut();
    let mut arena = SLTNodeArena::new();
    let mut seed = comb::Store::default();
    let random_seeds = process_random_seeds(module);
    for (index, process) in module.initial_processes().iter().enumerate() {
        if let Some(condition) = process.condition() {
            let condition =
                sv::typecheck::eval_const_expr_with_types(condition, constants, parameter_types)
                    .ok_or_else(|| {
                        procedural::unsupported("unknown conditional-generate condition")
                    })?;
            if condition == 0 {
                continue;
            }
        }
        let random_seed = random_seeds
            .get(&(1, index))
            .copied()
            .unwrap_or(0x4d59_5df4);
        let unit = pm.call_unit();
        // An `always` process never finishes, so it is never folded into
        // initial state. A program's `initial` procedure runs in the reactive
        // region set (IEEE 1800-2023 24.3.1), after the design's processes of
        // time zero, so it is not folded either.
        if process.is_final()
            || process.repeats()
            || (module.is_program() && !process.is_initializer())
        {
            runtime.push((process, random_seed));
            runtime_units.push(unit);
            continue;
        }
        // A process that cannot be folded runs as a kernel instead.
        pm.attempt_calls();
        let mut comb = comb::Comb::new(&mut pm, &mut arena);
        let folded = comb.lower_initial(process.body(), process.is_initializer(), &seed);
        pm.end_attempt_calls(folded.is_ok());
        match folded {
            Ok((written, store)) => {
                if process.is_initializer() {
                    declaration_values.extend(written.iter().cloned());
                    seed = store;
                } else {
                    folded_values.extend(written.iter().cloned());
                }
                values.extend(written);
            }
            Err(_) => {
                runtime.push((process, random_seed));
                runtime_units.push(unit);
            }
        }
    }
    let created = std::mem::take(&mut pm.created);
    for id in created {
        if let Some(variable) = variables.remove(&id) {
            name_to_id.remove(&variable.path.join("."));
        }
    }
    if runtime.is_empty() {
        return Ok((
            values,
            declaration_values,
            folded_values,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ));
    }

    let mut next_id = SourceVarId(
        variables
            .keys()
            .map(|id| id.0 + 1)
            .max()
            .unwrap_or_default(),
    );
    let mut slots: Vec<_> = runtime
        .iter()
        .enumerate()
        .map(|(index, (process, _))| {
            let mut declare = |slot: &str, width: usize| {
                let id = next_var_id(&mut next_id);
                variables.insert(
                    id,
                    SvVariable {
                        float: None,

                        value_kind: celox_design::ValueKind::Bits,
                        path: vec![format!("$initial[{index}]"), slot.to_string()],
                        width,
                        signed: false,
                        is_4state: false,
                        is_unpacked_struct: false,
                        packed_ranges: vec![(width as i128 - 1, 0)],
                        array_dims: Vec::new(),
                        unpacked_ranges: Vec::new(),
                        domain_kind: DomainKind::Other,
                        kind: VariableKind::Variable,
                        type_kind: PortTypeKind::Bit,
                        source: None,
                        // Control slots are not signals of the design.
                        hidden: true,
                        force_control: None,
                        sampled_from: None,
                        history_of: None,
                        activation_scratch: false,
                    },
                );
                id
            };
            ProcessSlots {
                kind: process.kind(),
                requires_timed_simulation: process.is_scheduled_edge() || process.is_final(),
                const_delays: false,
                program: None,
                resume: declare("resume", PROCESS_RESUME_WIDTH),
                status: declare("status", PROCESS_STATUS_WIDTH),
                delay: declare("delay", PROCESS_DELAY_WIDTH),
                call_top: None,
            }
        })
        .collect();
    // Every `initial` kernel of a program instance names the status slot of
    // the first, the instance's identity for `$exit` and its end (IEEE
    // 1800-2023 24.3, 24.7).
    if module.is_program() {
        let program = slots
            .iter()
            .zip(&runtime)
            .find(|(_, (process, _))| !process.is_initializer() && !process.is_final())
            .map(|(slots, _)| slots.status);
        for (slots, (process, _)) in slots.iter_mut().zip(&runtime) {
            if !process.is_initializer() && !process.is_final() {
                slots.program = program;
            }
        }
    }
    // The kernels add their event sites, runtime errors and extern functions
    // after those of the sequential processes. Their hidden variables are
    // state the kernels keep.
    let mut pm = procedural::ProcModule::new(
        module,
        tables,
        variables,
        name_to_id,
        constants,
        parameter_types,
        four_state,
        warnings,
    );
    pm.calls = calls;
    pm.runtime.event_sites = std::mem::take(runtime_event_sites);
    pm.runtime.errors = std::mem::take(runtime_errors);
    pm.externs.functions = std::mem::take(extern_functions);
    let mut wait_sites = Vec::new();
    let mut nba_sites = Vec::new();
    let mut nba_switches = Vec::new();
    let mut host_request_sites = Vec::new();
    let processes = runtime
        .into_iter()
        .zip(slots)
        .enumerate()
        .map(|(index, ((process, random_seed), slots))| {
            pm.resume_call_unit(runtime_units[index]);
            // `always @(list) statement` waits for a change (IEEE 1800-2023
            // 9.4.2). In two-state mode its outputs would then start as a
            // real 0, so it runs once at time zero, after the time-zero
            // statements of the initial blocks, before it waits, as
            // Verilator settles it: `#0; forever begin statement; @(list); end`.
            let settled;
            let body = match process.level_sensitive().filter(|_| !four_state) {
                Some((items, statements)) => {
                    let mut looped = statements.to_vec();
                    looped.push(sv::ir::Stmt::WaitEvent(items.to_vec()));
                    settled = [
                        sv::ir::Stmt::Delay {
                            amount: sv::ir::Expr::Literal("0".to_string()),
                            ticks_per_unit: 1,
                            ticks_per_precision: 1,
                            precision_steps_per_tick: 1,
                        },
                        sv::ir::Stmt::Loop {
                            kind: sv::ir::LoopKind::Forever,
                            init: Vec::new(),
                            condition: None,
                            step: Vec::new(),
                            body: looped,
                        },
                    ];
                    &settled[..]
                }
                None => process.body(),
            };
            let startup_inactive = !four_state
                && process.level_sensitive().is_some_and(|(_, body)| {
                    body.iter()
                        .all(|stmt| !stmt_may_suspend(module.subroutines(), stmt))
                });
            let mut kernel = ff::Ff::new(&mut pm, random_seed).lower_initial_kernel(
                body,
                slots,
                index,
                process.is_final(),
            )?;
            if startup_inactive {
                // This level-sensitive body contains no timing controls. Its
                // synthetic #0 executes once at time zero in the reference
                // loop, before any compiled slot can start. Every subsequent
                // suspension is an event wait.
                kernel.slots.const_delays = true;
            }
            wait_sites.extend(kernel.wait_sites);
            nba_sites.extend(kernel.nba_sites);
            nba_switches.extend(kernel.nba_switches);
            host_request_sites.extend(kernel.host_request_sites.into_iter().map(|mut site| {
                site.process = celox_design::SiteProcess::own(index);
                site
            }));
            Ok(SymbolicProcess {
                kernel: kernel.unit,
                frame: kernel.frame,
                slots: kernel.slots,
                call_resumes: kernel.call_resumes,
                origin: process_origin(process),
            })
        })
        .collect::<Result<Vec<_>, sv::AnalyzerError>>();
    // A kernel that can suspend inside a call lists the requests of the
    // module's function units after its own.
    if let Ok(processes) = &processes {
        for (index, process) in processes.iter().enumerate() {
            if process.slots.call_top.is_some() {
                // And its waits, after its own (see `Ff::function_wait_word`).
                wait_sites.extend(pm.function_sites.wait.iter().map(|site| {
                    celox_design::WaitSite {
                        process: index,
                        ..site.clone()
                    }
                }));
                nba_sites.extend(
                    pm.function_sites
                        .nba
                        .iter()
                        .map(|site| celox_design::NbaSite {
                            process: index,
                            ..site.clone()
                        }),
                );
                host_request_sites.extend(pm.function_sites.host_request.iter().map(|site| {
                    celox_design::HostRequestSite {
                        process: celox_design::SiteProcess::function(index),
                        ..site.clone()
                    }
                }));
            }
        }
    }
    *runtime_event_sites = std::mem::take(&mut pm.runtime.event_sites);
    *runtime_errors = std::mem::take(&mut pm.runtime.errors);
    *extern_functions = std::mem::take(&mut pm.externs.functions);
    Ok((
        values,
        declaration_values,
        folded_values,
        processes?,
        wait_sites,
        nba_sites,
        nba_switches,
        host_request_sites,
        std::mem::take(&mut pm.event_assignments),
    ))
}
